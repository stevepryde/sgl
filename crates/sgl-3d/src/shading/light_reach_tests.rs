//! Which lobes a point light reaches: the production light a surface takes
//! from a scene light (`scene_light_sample` and `surface_direct_light`,
//! from the scene's light record on the GPU, unshadowed), judged by what
//! the lobes' equations require of it: a coat lobe depends on the coat's
//! normal alone, and a sphere's highlight changes continuously as its
//! centre crosses the horizon while part of it stays above.
use crate::{Light, LightShape, Scene};
use glam::DVec3;

/// One receiver at the origin, lit by `light`.
#[derive(Clone, Copy)]
struct Receiver {
    light: usize,
    normal: DVec3,
    coat_normal: DVec3,
    view: DVec3,
    /// The base's diffuse colour and F0, which F90 follows (1, or 0 with
    /// an F0 of 0).
    diffuse: f64,
    f0: f64,
    coat: f64,
}

/// The light each receiver takes.
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
                r.normal.as_vec3().extend(r.light as f32).to_array(),
                r.coat_normal.as_vec3().extend(r.coat as f32).to_array(),
                r.view.as_vec3().extend(0.).to_array(),
                [r.diffuse as f32, r.f0 as f32, 0., 0.],
            ]
        })
        .collect();
    let observation = r#"
struct Case { n:vec4<f32>,coat:vec4<f32>,v:vec4<f32>,f:vec4<f32> }
@group(0) @binding(3) var<storage,read> cases:array<Case>;
@group(0) @binding(4) var<storage,read_write> result:array<vec4<f32>>;
@compute @workgroup_size(64) fn observe(@builtin(global_invocation_id) id:vec3<u32>) {
 if id.x>=arrayLength(&cases) { return; }
 let c=cases[id.x];
 let index=u32(c.n.w);
 var surface:Surface;
 surface.position=vec3(0.);
 surface.normal=c.n.xyz;
 surface.geometry_normal=c.coat.xyz;
 surface.coat_normal=c.coat.xyz;
 surface.view=c.v.xyz;
 surface.roughness=.4;
 surface.coat=c.coat.w;
 surface.coat_roughness=.2;
 surface.dielectric_f0=vec3(c.f.y);
 surface.specular=1.;
 var reflectance:SurfaceReflectance;
 reflectance.diffuse=vec3(c.f.x);
 reflectance.f0=vec3(c.f.y);
 reflectance.f90=select(0.,1.,c.f.y>0.);
 reflectance.multiscatter=vec3(1.);
 reflectance.coat_fresnel=.04*c.coat.w;
 let light=scene_light_sample(index,surface.position,surface.normal,surface.coat_normal,surface.geometry_normal,vec2(0.),SHADOW_RECEIVER_CAMERA,false,surface.position,0.);
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

/// A white point light of `radius` at `position`.
fn point(position: DVec3, radius: f32) -> Light {
    Light {
        position: position.as_vec3(),
        shape: LightShape::Point { radius },
        range: 20.,
        ..Default::default()
    }
}

/// `direction` reflected about unit `normal`.
fn mirrored(direction: DVec3, normal: DVec3) -> DVec3 {
    normal * 2. * normal.dot(direction) - direction
}

// Plausible defect: a point light dropped wherever the mapped base normal
// faces away from it, so a clearcoat over normal-mapped flakes loses the
// coat's highlight there (#521). The oracle is the coat lobe's own
// equation, which reads the coat normal and not the base's: a base normal
// turned away from the light, under which the base's diffuse light, sheen
// and specular take nothing, leaves the light of the same coat over a base
// that faces the light but reflects nothing.
#[test]
fn a_coat_takes_a_point_light_its_flake_normal_faces_away_from() {
    let up = DVec3::Y;
    let to_light = DVec3::new(0., 1., 1.).normalize();
    let lights = [point(to_light * 2., 0.)];
    let view = mirrored(to_light, up);
    // Turned 60° away from the light: its cosine to it is negative.
    let flake = DVec3::new(0., 60f64.to_radians().cos(), -60f64.to_radians().sin());
    assert!(flake.dot(to_light) < 0.);
    let receivers = [
        Receiver {
            light: 0,
            normal: flake,
            coat_normal: up,
            view,
            diffuse: 0.5,
            f0: 0.04,
            coat: 1.,
        },
        Receiver {
            light: 0,
            normal: up,
            coat_normal: up,
            view,
            diffuse: 0.,
            f0: 0.,
            coat: 1.,
        },
    ];
    let Some(light) = observe(&lights, &receivers) else {
        return;
    };
    let (flaked, coat_alone) = (light[0], light[1]);
    assert!(coat_alone > 0., "the coat alone takes {coat_alone}");
    assert!(
        (flaked - coat_alone).abs() <= 1e-5 * coat_alone,
        "over a flake facing away {flaked}, the coat alone {coat_alone}"
    );
}

// Plausible defect: a sphere light dropped as soon as its centre passes
// below the surface's horizon, though part of it stays above and its
// highlight (the representative point on the sphere, LightShape::Point)
// still reaches the surface, so the highlight pops off. The oracle is the
// highlight's continuity: just below the horizon a sphere a tenth of its
// distance across lights a specular surface as it does just above, and
// halfway to its rim below it still lights it. Near the horizon the
// highlight's representative point stands about 0.087 above it here (the
// sphere's 0.1 along the reflection's rise), so the lobe's cosine there,
// and the light, change by about 2δ / 0.087 between elevations ±δ: 0.5%
// at δ = 0.0002, held within 1%.
#[test]
fn a_sphere_light_below_the_horizon_keeps_its_highlight() {
    let up = DVec3::Y;
    let distance = 2.;
    // Radius over distance: 0.1.
    let radius = 0.2;
    let toward = |elevation: f64| DVec3::new(0., elevation.sin(), elevation.cos());
    let elevations = [0.0002, -0.0002, -0.05];
    let lights = elevations.map(|elevation| point(toward(elevation) * distance, radius));
    // The view 30° above the horizon opposite the light, whose reflection
    // rises toward the sphere.
    let view = mirrored(toward(30f64.to_radians()), up);
    let receivers: Vec<Receiver> = (0..elevations.len())
        .map(|light| Receiver {
            light,
            normal: up,
            coat_normal: up,
            view,
            diffuse: 0.,
            f0: 0.9,
            coat: 0.,
        })
        .collect();
    let Some(light) = observe(&lights, &receivers) else {
        return;
    };
    let (above, below, halfway) = (light[0], light[1], light[2]);
    assert!(above > 0., "just above the horizon {above}");
    assert!(
        (above - below).abs() <= 0.01 * above,
        "just above the horizon {above}, just below {below}"
    );
    assert!(
        halfway > 0.,
        "halfway to its rim below the horizon {halfway}"
    );
}
