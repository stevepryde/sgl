//! Rectangle lights' shading judged by an independent numerical integration:
//! the production light a surface takes from a scene light
//! (`scene_light_sample` and `surface_direct_light`, from the scene's light
//! record and the LTC table on the GPU, unshadowed) against a midpoint
//! quadrature, in f64, of Lambert diffuse and GGX with height-correlated
//! Smith masking and Schlick Fresnel (the BRDF selfshadow/ltc_code fits),
//! layered under a coat as `surface_direct_brdf` layers it, over the
//! rectangle the light's description gives and faded by its range window.
//! It uses no LTC table, fitted matrix or edge formula.
use crate::{Light, LightShape, Scene};
use glam::{DVec3, Vec3};
use std::f64::consts::PI;

/// One receiver: where it is, how it faces and what it reflects.
#[derive(Clone, Copy, Debug)]
struct Receiver {
    light: usize,
    position: DVec3,
    normal: DVec3,
    view: DVec3,
    /// Perceptual roughness.
    rough: f64,
    f0: f64,
    diffuse: f64,
    /// The coat's strength and perceptual roughness, and its Fresnel toward
    /// the view, which takes from the base.
    coat: f64,
    coat_rough: f64,
    coat_fresnel: f64,
}

/// The rectangle `light` describes: its centre, unit normal, and half
/// extents along two unit axes in its plane.
fn rectangle(light: &Light) -> (DVec3, DVec3, DVec3, DVec3) {
    let LightShape::Rect {
        direction,
        width_axis,
        width,
        height,
    } = light.shape
    else {
        unreachable!()
    };
    let normal = direction.as_dvec3().normalize();
    let width_axis = width_axis.as_dvec3();
    let across = (width_axis - normal * normal.dot(width_axis)).normalize();
    let up = normal.cross(across);
    (
        light.position.as_dvec3(),
        normal,
        across * (width as f64 / 2.),
        up * (height as f64 / 2.),
    )
}

/// `light`'s area in square metres.
fn area(light: &Light) -> f64 {
    let LightShape::Rect { width, height, .. } = light.shape else {
        unreachable!()
    };
    width as f64 * height as f64
}

/// The light `light` brings to `receiver`, per unit of its colour: its
/// luminance (intensity over area) times the BRDF and the cosine at the
/// receiver, integrated over its face by `steps`² midpoints.
fn reference(light: &Light, receiver: &Receiver, steps: usize) -> f64 {
    let (center, light_normal, half_width, half_height) = rectangle(light);
    let area = area(light);
    let luminance = light.intensity as f64 / area;
    let (n, v) = (receiver.normal, receiver.view);
    let nv = n.dot(v);
    // GGX with height-correlated Smith and Schlick Fresnel, over N.L.
    let ggx = |rough: f64, f0: f64, l: DVec3| {
        let alpha = rough * rough;
        let a2 = alpha * alpha;
        let lambda = |cosine: f64| {
            let tan2 = (1. - cosine * cosine).max(0.) / (cosine * cosine);
            ((1. + a2 * tan2).sqrt() - 1.) / 2.
        };
        let h = (v + l).normalize();
        let nh = n.dot(h);
        let d = a2 / (PI * (1. + (a2 - 1.) * nh * nh).powi(2));
        let g = 1. / (1. + lambda(nv) + lambda(n.dot(l)));
        let fresnel = f0 + (1. - f0) * (1. - v.dot(h)).clamp(0., 1.).powi(5);
        d * g * fresnel / (4. * nv)
    };
    // Bevy's range window at the distance to the centre.
    let factor = (center - receiver.position).length_squared() / (light.range as f64).powi(2);
    let window = (1. - factor * factor).clamp(0., 1.).powi(2);
    let mut sum = 0.;
    for i in 0..steps {
        for j in 0..steps {
            let s = (i as f64 + 0.5) / steps as f64 * 2. - 1.;
            let t = (j as f64 + 0.5) / steps as f64 * 2. - 1.;
            let point = center + half_width * s + half_height * t;
            let offset = point - receiver.position;
            let distance2 = offset.length_squared();
            let l = offset / distance2.sqrt();
            let emitted = (-l).dot(light_normal);
            let nl = n.dot(l);
            if emitted <= 0. || nl <= 0. {
                continue;
            }
            let solid_angle = emitted * (area / (steps * steps) as f64) / distance2;
            let base = receiver.diffuse / PI * nl + ggx(receiver.rough, receiver.f0, l);
            let layered = if receiver.coat > 0. {
                base * (1. - receiver.coat_fresnel)
                    + receiver.coat * ggx(receiver.coat_rough, 0.04, l)
            } else {
                base
            };
            sum += layered * solid_angle;
        }
    }
    sum * luminance * window
}

/// Unit `direction` turned `angle` radians from `normal` toward `toward`.
fn tilted(normal: DVec3, toward: DVec3, angle: f64) -> DVec3 {
    let across = (toward - normal * normal.dot(toward)).normalize();
    (normal * angle.cos() + across * angle.sin()).normalize()
}

/// The light each receiver takes, through the production scene-light
/// sample and direct light, with no shadow.
fn observe(lights: &[Light], receivers: &[Receiver]) -> Option<Vec<f64>> {
    let (device, queue) = crate::test_support::device()?;
    let mut scene = Scene::new(&device, &queue);
    for light in lights {
        scene.add_light(&device, &queue, *light).unwrap();
    }
    let cases: Vec<[[f32; 4]; 4]> = receivers
        .iter()
        .map(|r| {
            [
                r.normal.as_vec3().extend(r.rough as f32).to_array(),
                r.view.as_vec3().extend(r.coat_rough as f32).to_array(),
                r.position.as_vec3().extend(r.light as f32).to_array(),
                [r.f0, r.diffuse, r.coat, r.coat_fresnel].map(|value| value as f32),
            ]
        })
        .collect();
    let observation = r#"
struct Case { n:vec4<f32>,v:vec4<f32>,p:vec4<f32>,f:vec4<f32> }
@group(0) @binding(3) var<storage,read> cases:array<Case>;
@group(0) @binding(4) var<storage,read_write> result:array<vec4<f32>>;
@compute @workgroup_size(64) fn observe(@builtin(global_invocation_id) id:vec3<u32>) {
 if id.x>=arrayLength(&cases) { return; }
 let c=cases[id.x];
 let index=u32(c.p.w);
 var surface:Surface;
 surface.position=c.p.xyz;
 surface.normal=c.n.xyz;
 surface.geometry_normal=c.n.xyz;
 surface.view=c.v.xyz;
 surface.roughness=c.n.w;
 surface.coat=c.f.z;
 surface.coat_roughness=c.v.w;
 var reflectance:SurfaceReflectance;
 reflectance.diffuse=vec3(c.f.y);
 reflectance.f0=vec3(c.f.x);
 reflectance.coat_fresnel=c.f.w;
 let light=scene_light_sample(index,c.p.xyz,c.n.xyz,c.n.xyz,vec2(0.),SHADOW_RECEIVER_CAMERA);
 result[id.x]=vec4(surface_direct_light(surface,reflectance,light),0.);
}
"#;
    let rows = crate::test_support::observe_surface(
        &device,
        &queue,
        &scene,
        bytemuck::cast_slice(&cases),
        observation,
        cases.len().div_ceil(64) as u32,
        cases.len(),
    );
    Some(rows.iter().map(|row| row[0] as f64).collect())
}

/// A white rectangle light of `intensity` candela.
fn rect(position: Vec3, direction: Vec3, width_axis: Vec3, width: f32, height: f32) -> Light {
    Light {
        position,
        shape: LightShape::Rect {
            direction,
            width_axis,
            width,
            height,
        },
        color: [1.; 3],
        intensity: width * height * 3.,
        range: 100.,
        baked: false,
        specular: 1.,
        casts_shadow: false,
        ..Default::default()
    }
}

#[test]
fn rect_lights_match_numerical_integration() {
    let lights = [
        // A panel overhead, its width along x.
        rect(Vec3::new(0., 3., 0.), Vec3::NEG_Y, Vec3::X, 2., 0.5),
        // A tilted strip, its width axis given off its plane.
        rect(
            Vec3::new(0.4, 2.5, -0.3),
            Vec3::new(0.3, -1., 0.2),
            Vec3::new(0.1, 0.2, 1.),
            4.,
            0.3,
        ),
        // A large panel close by.
        rect(
            Vec3::new(0., 1.2, 0.),
            Vec3::new(0., -1., 0.1),
            Vec3::Z,
            3.,
            3.,
        ),
    ];
    let mut receivers = Vec::new();
    for light in 0..lights.len() {
        for position in [DVec3::ZERO, DVec3::new(1.5, 0., 0.8)] {
            // Facing up, and tilted far enough that the horizon clips the
            // nearer lights.
            for tilt in [0., 0.5, 1.2] {
                let normal = tilted(DVec3::Y, DVec3::new(1., 0., 0.3), tilt);
                // Views along the normal and toward grazing.
                for view_angle in [0., 0.6, 1.2] {
                    let view = tilted(normal, DVec3::new(-0.4, 0., 1.), view_angle);
                    for (rough, f0, diffuse, coat) in [
                        (0.35, 0.04, 0.8, 0.),
                        (0.6, 0.9, 0., 0.),
                        (0.9, 0.04, 0.5, 0.),
                        (0.6, 0.04, 0.5, 1.),
                        (0.6, 0.9, 0., 1.),
                    ] {
                        receivers.push(Receiver {
                            light,
                            position,
                            normal,
                            view,
                            rough,
                            f0,
                            diffuse,
                            coat,
                            coat_rough: 0.3,
                            coat_fresnel: 0.05,
                        });
                    }
                }
            }
        }
    }
    // Behind each face, turned partly toward it: the face is one-sided and
    // lights none.
    let behind = receivers.len();
    for (light, description) in lights.iter().enumerate() {
        let LightShape::Rect { direction, .. } = description.shape else {
            unreachable!()
        };
        let facing = direction.as_dvec3().normalize();
        let normal = tilted(facing, facing.any_orthonormal_vector(), 1.2);
        receivers.push(Receiver {
            light,
            position: description.position.as_dvec3() - facing,
            normal,
            view: normal,
            rough: 0.5,
            f0: 0.04,
            diffuse: 0.8,
            coat: 0.,
            coat_rough: 0.3,
            coat_fresnel: 0.,
        });
    }
    let Some(observed) = observe(&lights, &receivers) else {
        return;
    };
    // The fit's accuracy: within 4% where the face is above a dielectric
    // receiver's horizon and the view is away from grazing, and everywhere
    // within 30% plus 1% of the face's luminance, which takes the fit's
    // grazing glossy tails and the sphere that stands in for the horizon's
    // clipping.
    let mut worst = [0f64; 2];
    for (receiver, &observed) in receivers[behind..].iter().zip(&observed[behind..]) {
        assert_eq!(observed, 0., "{receiver:?}: lit from behind the face");
    }
    for (receiver, observed) in receivers[..behind].iter().zip(observed) {
        let light = &lights[receiver.light];
        let expected = reference(light, receiver, 160);
        let error = (observed - expected).abs();
        let luminance = light.intensity as f64 / area(light);
        let ordinary = receiver.normal.angle_between(DVec3::Y) < 0.6
            && receiver.view.angle_between(receiver.normal) < 0.7
            && receiver.f0 < 0.5;
        let bounds = [
            if ordinary {
                0.04 * expected
            } else {
                f64::INFINITY
            },
            0.3 * expected + 0.01 * luminance,
        ];
        for (worst, bound) in worst.iter_mut().zip(bounds) {
            *worst = worst.max(error / bound);
        }
        assert!(
            error <= bounds[0] && error <= bounds[1],
            "{receiver:?}: observed {observed}, expected {expected}"
        );
    }
    eprintln!("rect lights: worst error over its bounds {worst:?}");
}
