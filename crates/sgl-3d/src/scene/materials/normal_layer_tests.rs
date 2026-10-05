//! Scrolling normal layers at the frame's time, through the real opaque
//! stage (the G-buffer normal reflections read) and a real ray hit.
use crate::asset::{Asset, CpuMesh, Image, Material, Vertex};
use crate::renderer::Renderer;
use crate::settings::{
    AmbientOcclusionQuality, ScreenSpaceReflections, Settings, WorldSpaceReflections,
};
use crate::test_support;
use crate::{Camera, FrameInput, NormalLayer, Scene, SceneError, perspective};
use glam::{DVec2, Mat4, Vec2, Vec3};

const SIZE: [u32; 2] = [64, 64];
/// The plane's distance in front of the camera, and half its side.
const DEPTH: f32 = 3.;
const HALF: f32 = 2.;
/// Texels on each side of the ramp map.
const TEXELS: u8 = 16;
/// 101 hours: there `f32` seconds step by a thirty-second of a second,
/// about two 60 Hz frames.
const LATE: f64 = 101. * 3600.;

/// A normal map whose red rises 12 a texel from 40 along U and whose green
/// rises 9 a texel from 60 along V, blue 200 throughout: between the first
/// and last texel centres, its filtered tangent-space X is linear in U and
/// its Y in V.
fn ramp() -> Image {
    Image::Rgba8(image::RgbaImage::from_fn(
        TEXELS.into(),
        TEXELS.into(),
        |x, y| image::Rgba([40 + 12 * x as u8, 60 + 9 * y as u8, 200, 255]),
    ))
}

/// The ramp's filtered tangent-space normal at texture coordinate `at`, which
/// must lie between its first and last texel centres on both axes.
fn ramp_at(at: DVec2) -> Vec3 {
    let texels = f64::from(TEXELS);
    let texel = at * texels - 0.5;
    assert!(
        texel.min_element() >= 0. && texel.max_element() <= texels - 1.,
        "{at} lies where the ramp wraps; move the fixture's points"
    );
    let unit = |byte: f64| (byte / 255. * 2. - 1.) as f32;
    Vec3::new(
        unit(40. + 12. * texel.x),
        unit(60. + 9. * texel.y),
        unit(200.),
    )
}

/// The normal `layers` give a surface facing +Z whose U runs along +X and V
/// along +Y, at material UV `uv` and `seconds`, from their documented
/// meaning: each layer draws the map `scale` times per unit of UV, its
/// pattern moved `velocity × seconds` across the surface, and the layers'
/// height fields add, so their slopes, the tangent-space X and Y over Z
/// scaled by the strength, add.
fn expected(layers: &[NormalLayer; 2], uv: DVec2, seconds: f64) -> Vec3 {
    let slope: Vec2 = layers
        .iter()
        .map(|layer| {
            let velocity = DVec2::from(layer.velocity.map(f64::from));
            let at = ((uv - velocity * seconds) * f64::from(layer.scale)).rem_euclid(DVec2::ONE);
            let normal = ramp_at(at);
            normal.truncate() / normal.z * layer.strength
        })
        .sum();
    slope.extend(1.).normalize()
}

/// A square facing +Z at `DEPTH` in front of the camera, `HALF` metres
/// from its centre to each side, its UV rising along +X and +Y, drawn with
/// `material` and the ramp map.
fn plane(material: Material) -> Asset {
    let mut asset = test_support::cube();
    asset.meshes = vec![CpuMesh {
        vertices: [(-1., -1.), (1., -1.), (1., 1.), (-1., 1.)]
            .map(|(x, y)| Vertex {
                position: [x * HALF, y * HALF, -DEPTH],
                normal: [0., 0., 1.],
                uv: [(x + 1.) / 2., (y + 1.) / 2.],
                color: [1.; 4],
                lightmap_uv: [0.; 2],
                lightmap_bounds: [0., 0., 1., 1.],
                tangent: [0.; 4],
            })
            .to_vec(),
        indices: vec![0, 1, 2, 0, 2, 3],
        material: 0,
        deformation: Default::default(),
    }];
    asset.materials = vec![material];
    asset.images = vec![ramp()];
    asset
}

/// The pixel nearest the plane's point at UV `uv`, and the UV and position
/// of the plane where that pixel's centre sees it.
fn pixel(projection: Mat4, uv: Vec2) -> (usize, DVec2, Vec3) {
    let point = ((uv * 2. - 1.) * HALF).extend(-DEPTH);
    let ndc = projection.project_point3(point);
    let [x, y] = [ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5].map(|t| (t * SIZE[0] as f32) as u32);
    let centre = [x, y].map(|p| (p as f32 + 0.5) / SIZE[0] as f32 * 2. - 1.);
    let seen = Vec3::new(
        centre[0] / projection.x_axis.x * DEPTH,
        -centre[1] / projection.y_axis.y * DEPTH,
        -DEPTH,
    );
    let uv = (seen.truncate() / HALF + 1.) / 2.;
    ((y * SIZE[0] + x) as usize, uv.as_dvec2(), seen)
}

/// The world normal a ray from the camera finds at each of `points`, at
/// `seconds`.
fn ray_normals(
    (device, queue): (&wgpu::Device, &wgpu::Queue),
    (renderer, scene): (&mut Renderer, &mut Scene),
    (input, settings): (&FrameInput, &Settings),
    points: &[Vec3],
) -> Vec<Vec3> {
    let observation = format!(
        r#"
@group(3) @binding(0) var<storage,read_write> output:array<vec4<f32>>;
@compute @workgroup_size(1) fn observe() {{
 let points=array<vec3<f32>,{count}>({points});
 for (var i=0u;i<{count}u;i++) {{
  let ray=SceneRay(vec4(0.,0.,0.,0.),vec4(normalize(points[i]),100.));
  let hit=scene_decode_hit(scene_trace_nearest(ray,SCENE_SIDES_AS_RASTER),ray.origin.xyz,ray.direction.xyz);
  output[i]=vec4(ray_normal(hit,scene_material(hit.material_word)),0.);
 }}
}}
"#,
        count = points.len(),
        points = points
            .iter()
            .map(|p| format!("vec3({},{},{})", p.x, p.y, p.z))
            .collect::<Vec<_>>()
            .join(","),
    );
    test_support::observe_ray_hits(
        device,
        queue,
        renderer,
        scene,
        input,
        settings,
        &observation,
        points.len(),
    )
    .into_iter()
    .map(|n| Vec3::from_slice(&n[..3]))
    .collect()
}

/// A renderer whose frames trace world-space rays, and a scene holding the
/// plane drawn with `layers` scrolling the ramp map.
fn fixture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    layers: [NormalLayer; 2],
) -> (Renderer, Scene, Settings, crate::MaterialId) {
    let settings = Settings {
        ambient_occlusion: AmbientOcclusionQuality::Off,
        // World-space rays need a screen-space method to fill in.
        screen_space_reflections: ScreenSpaceReflections::Half,
        world_space_reflections: WorldSpaceReflections::Moving,
        ..Settings::default()
    };
    let renderer = Renderer::for_test(device, queue, SIZE, &settings);
    let mut scene = Scene::new(device, queue);
    let material = Material {
        base: [0.5, 0.5, 0.5, 1.],
        metallic: 0.,
        roughness: 0.5,
        normal_texture: Some(0),
        normal_layers: Some(layers),
        ..Default::default()
    };
    let (ids, _) = test_support::add_static(device, queue, &mut scene, plane(material));
    (renderer, scene, settings, ids.materials[0])
}

fn input(seconds: f64) -> FrameInput {
    let mut input = FrameInput::new(Camera {
        view: Mat4::IDENTITY,
        projection: perspective(1., 1., 0.1),
        eye: Vec3::ZERO,
    });
    input.elapsed_seconds = seconds;
    input
}

// Plausible defects: a layer moving against its velocity, or by its velocity
// in its own texture's units rather than across the surface; its scale or
// strength left out or applied to the other layer; the layers' normals
// averaged rather than their slopes added; raster and ray hits evaluating
// them differently, or a ray hit not seeing them move; the frame's time
// reaching the GPU as `f32` seconds, which 101 hours in step by a
// thirty-second of a second, so a layer lands short of where it should be.
// The oracle is the layers' documented meaning (`expected`) over a ramp map
// whose filtered normal is linear in its coordinates, at two points, a
// second apart and 101 hours later, with speeds of whole repeats per hour,
// which SGL3D keeps as given.
#[test]
fn layers_move_along_their_velocities_and_add_their_slopes() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let layers = [
        NormalLayer {
            velocity: [0.25, 0.],
            scale: 1.,
            strength: 1.,
        },
        NormalLayer {
            velocity: [0., -0.125],
            scale: 2.,
            strength: 0.5,
        },
    ];
    let (mut renderer, mut scene, settings, _) = fixture(&device, &queue, layers);
    let projection = input(0.).camera.projection;
    let points = [Vec2::new(0.45, 0.2), Vec2::new(0.7, 0.35)].map(|uv| pixel(projection, uv));
    for seconds in [0.5123, 1.5123, LATE + 0.5123] {
        let input = input(seconds);
        let mut frame = renderer.prepare_test_frame(&device, &queue, &mut scene, &input, &settings);
        let mut encoder = device.create_command_encoder(&Default::default());
        let fused = renderer.test_fused_supported();
        renderer.encode_test_opaque(&device, &queue, &mut encoder, &scene, &mut frame, fused);
        queue.submit([encoder.finish()]);
        scene.finish_frame();
        let normals = test_support::read(&device, &queue, renderer.targets().normal.texture(), 8);
        let rays = ray_normals(
            (&device, &queue),
            (&mut renderer, &mut scene),
            (&input, &settings),
            &points.map(|(_, _, seen)| seen),
        );
        for ((at, uv, _), ray) in points.iter().zip(rays) {
            let expected = expected(&layers, *uv, seconds);
            let raster = [0, 1].map(|c| test_support::half(&normals[at * 8 + c * 2..]));
            let encoded = test_support::octahedral(expected);
            let label = format!("{seconds} s at UV {uv}");
            assert!(
                (0..2).all(|c| (raster[c] - encoded[c]).abs() < 3e-3),
                "{label}: raster normal {raster:?}, expected {encoded:?} ({expected})"
            );
            assert!(
                ray.distance(expected) < 1e-3,
                "{label}: ray normal {ray}, expected {expected}"
            );
        }
    }
}

// Plausible defects: speeds kept as given, so the layers jump where the
// animation's period wraps; the frame's time reduced in `f32`, or reaching
// the GPU as `f32` seconds, so frames 101 hours in stall or jump. The
// oracle is each layer's velocity: from one 60 Hz frame to the next it
// moves its pattern `velocity / 60` across the surface, which over the
// ramp map moves the slope linearly, at every frame about the 101st hour,
// within the documented rounding of its speed to whole repeats per hour.
#[test]
fn layers_move_smoothly_across_the_hours() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    // 904.68 and -929.52 repeats an hour, before rounding.
    let layers = [
        NormalLayer {
            velocity: [0.2513, 0.],
            scale: 1.,
            strength: 1.,
        },
        NormalLayer {
            velocity: [0., -0.1291],
            scale: 2.,
            strength: 0.5,
        },
    ];
    let (mut renderer, mut scene, settings, _) = fixture(&device, &queue, layers);
    let (_, _, seen) = pixel(input(0.).camera.projection, Vec2::new(0.45, 0.2));
    let slopes: Vec<Vec2> = (-2..=2)
        .map(|frame| {
            let input = input(LATE + f64::from(frame) / 60.);
            let normal = ray_normals(
                (&device, &queue),
                (&mut renderer, &mut scene),
                (&input, &settings),
                &[seen],
            )[0];
            normal.truncate() / normal.z
        })
        .collect();
    // The ramp's X and Y rise per unit of texture coordinate, over its Z.
    let z = 200. / 255. * 2. - 1.;
    let rise = Vec2::new(12., 9.) / 255. * 2. * f32::from(TEXELS) / z;
    // Layer 0 moves U and layer 1 V; each slope falls by the layer's
    // movement in texture coordinates, times its strength.
    let step = Vec2::new(
        -rise.x * layers[0].velocity[0] * layers[0].scale * layers[0].strength,
        -rise.y * layers[1].velocity[1] * layers[1].scale * layers[1].strength,
    ) / 60.;
    for (frame, pair) in slopes.windows(2).enumerate() {
        let moved = pair[1] - pair[0];
        assert!(
            (moved - step).abs().cmple(step.abs() * 0.05).all(),
            "frame {frame} to the next: the slope moved {moved}, expected {step}"
        );
    }
}

// Plausible defects: layers accepted on a material whose normal map is
// missing or does not repeat, so they scroll nothing or smear its edge, or
// with values that reach the shaders as NaN, a collapsed map or a speed whose
// repeats per hour `f32` cannot hold, so the hour's end jumps; a refused edit
// applied anyway. The oracles are the documented conditions and the
// refusal's contract: nothing changes.
#[test]
fn invalid_normal_layers_are_refused() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let layers = [NormalLayer::default(); 2];
    let (_, mut scene, _, material) = fixture(&device, &queue, layers);
    let mut clamped = plane(Material {
        normal_texture: Some(0),
        normal_layers: Some(layers),
        ..Default::default()
    });
    clamped.materials[0].wrap[1] = gltf::texture::WrappingMode::ClampToEdge;
    let mut unmapped = clamped.clone();
    unmapped.materials[0].wrap[1] = gltf::texture::WrappingMode::Repeat;
    unmapped.materials[0].normal_texture = None;
    for asset in [clamped, unmapped] {
        assert!(matches!(
            scene.add_asset(&device, &queue, asset),
            Err(SceneError::InvalidNormalLayers)
        ));
    }
    let before = scene.material(material).unwrap();
    for invalid in [
        NormalLayer {
            velocity: [f32::NAN, 0.],
            ..NormalLayer::default()
        },
        NormalLayer {
            scale: 0.,
            ..NormalLayer::default()
        },
        NormalLayer {
            scale: f32::INFINITY,
            ..NormalLayer::default()
        },
        NormalLayer {
            strength: f32::NAN,
            ..NormalLayer::default()
        },
        // 5000 repeats a second: 18 million an hour, past 2^24.
        NormalLayer {
            velocity: [0., 5000.],
            ..NormalLayer::default()
        },
        NormalLayer {
            velocity: [f32::MAX, 0.],
            scale: 2.,
            ..NormalLayer::default()
        },
    ] {
        let mut values = before;
        values.normal_layers = Some([NormalLayer::default(), invalid]);
        assert!(
            matches!(
                scene.set_material(&queue, material, values),
                Err(SceneError::InvalidNormalLayers)
            ),
            "{invalid:?}"
        );
        assert_eq!(scene.material(material).unwrap(), before);
    }
}
