//! The dynamic GI volume in real frames, observed through the one sample
//! every view shades with (`dynamic_gi_irradiance`) and the frame's pixels:
//! an open volume holds a uniform environment's radiance and the hemisphere
//! fill's irradiance, and a closed room's emitters' with their bounces,
//! damped, while light outside the room reaches no probe inside but through
//! its shadow opacity or as a light that casts none, and the backs of
//! single-sided walls keep the sky out of a room seen from within and an
//! inside-out box's light in; a material that does not emit into GI gives
//! the probes none of its own light, yet blocks their rays and reflects a
//! lamp's light; its share fades over the spacing past its extent; the
//! one determination puts it below charts and above ambient cubes, and in
//! place of the frame's ambient; the probes continue across renderer resets,
//! abandoned frames and render origin moves, light probe captures as the
//! last submitted frame left them, and restart for another placement or
//! scene or after a frame that did not run them; and the scene refuses a
//! placement that is not a lattice or does not fit the device.
use crate::renderer::Renderer;
use crate::settings::{self, DynamicGiQuality, Settings};
use crate::shading::gbuffer;
use crate::{
    Backdrop, Camera, DynamicGiVolume, EnvironmentId, FrameInput, HemisphereLight, InstanceState,
    Mobility, Scene, test_support,
};
use glam::{Mat4, Vec3};

const SIZE: [u32; 2] = [64, 64];

fn settings(dynamic_gi: DynamicGiQuality) -> Settings {
    Settings {
        antialiasing: settings::Antialiasing::Off,
        bloom: settings::Bloom::Off,
        dynamic_gi,
        ..Settings::default()
    }
}

/// A camera at `eye` looking down -Z, with no light, fill or environment.
fn input(eye: Vec3) -> FrameInput {
    let mut input = FrameInput::new(Camera {
        view: Mat4::from_translation(-eye),
        projection: crate::perspective(1., 1., 0.1),
        eye,
    });
    input.backdrop = Backdrop::Color([0.; 3]);
    input.hemisphere_light = HemisphereLight {
        sky_color: [0.; 3],
        ground_color: [0.; 3],
        intensity: 0.,
    };
    input
}

/// An environment of uniform radiance `radiance`.
fn uniform_environment(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    scene: &mut Scene,
    radiance: [f32; 3],
) -> EnvironmentId {
    let texel: Vec<u8> = radiance
        .iter()
        .chain(&[1.])
        .flat_map(|&value| test_support::to_half(value).to_le_bytes())
        .collect();
    scene
        .add_environment(device, queue, &test_support::environment([255; 4], &texel))
        .unwrap()
}

/// `frames` frames of `scene` seen as `input`, each submitted and finished.
fn render(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    renderer: &mut Renderer,
    scene: &mut Scene,
    input: &FrameInput,
    settings: &Settings,
    frames: usize,
) {
    let output = crate::view::targets::target(device, "dynamic GI frames", SIZE, gbuffer::COLOR);
    for _ in 0..frames {
        let mut encoder = device.create_command_encoder(&Default::default());
        renderer.render(
            device,
            queue,
            &mut encoder,
            scene,
            input,
            settings,
            &output,
            None,
        );
        queue.submit([encoder.finish()]);
        renderer.finish_frame(scene);
    }
}

/// `dynamic_gi_irradiance` at each (position, normal) of `queries`, seen
/// along the normal, with the last frame's camera group 0, as a moving
/// instance's receiver there takes it, the volume's sample of open space
/// too: irradiance / PI in rgb, the volume's share in a.
fn irradiance(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    renderer: &Renderer,
    queries: &[(Vec3, Vec3)],
) -> Vec<[f32; 4]> {
    irradiance_seen(device, queue, renderer, &along(queries), Receiver::Moving)
}

/// Each (position, normal) of `queries`, seen along the normal.
fn along(queries: &[(Vec3, Vec3)]) -> Vec<(Vec3, Vec3, Vec3)> {
    queries
        .iter()
        .map(|&(position, normal)| (position, normal, normal))
        .collect()
}

/// A receiver on a static instance, which skips probes with no surface in
/// their cell, or on a moving one, which weighs them.
#[derive(Clone, Copy)]
enum Receiver {
    Static,
    Moving,
}

/// `dynamic_gi_irradiance` at each (position, normal, view toward the
/// viewer) of `queries` as `receiver` takes it.
fn irradiance_seen(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    renderer: &Renderer,
    queries: &[(Vec3, Vec3, Vec3)],
    receiver: Receiver,
) -> Vec<[f32; 4]> {
    let moving = matches!(receiver, Receiver::Moving);
    observe(
        device,
        queue,
        renderer,
        queries,
        &format!(
            "dynamic_gi_irradiance(queries[id.x].position.xyz,normalize(queries[id.x].normal.xyz),normalize(queries[id.x].view.xyz),false,{moving})"
        ),
    )
}

/// `expression` at each (position, normal, view) of `queries`, as
/// `irradiance` observes the sample.
fn observe(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    renderer: &Renderer,
    queries: &[(Vec3, Vec3, Vec3)],
    expression: &str,
) -> Vec<[f32; 4]> {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("dynamic GI observation"),
        source: wgpu::ShaderSource::Wgsl(
            format!(
                r#"{library}
{INDIRECT}
struct Query {{ position:vec4<f32>, normal:vec4<f32>, view:vec4<f32>, answer:vec4<f32> }}
@group(3) @binding(0) var<storage,read_write> queries:array<Query>;
@compute @workgroup_size(1) fn observe(@builtin(global_invocation_id) id:vec3<u32>) {{
 queries[id.x].answer={expression};
}}
"#,
                library = crate::shading::lit_compute_library(),
            )
            .into(),
        ),
    });
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("dynamic GI observation"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only: false },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("dynamic GI observation"),
        layout: Some(
            &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("dynamic GI observation"),
                bind_group_layouts: &[Some(renderer.test_lit_layout()), None, None, Some(&layout)],
                immediate_size: 0,
            }),
        ),
        module: &shader,
        entry_point: Some("observe"),
        compilation_options: Default::default(),
        cache: None,
    });
    let words: Vec<f32> = queries
        .iter()
        .flat_map(|(position, normal, view)| {
            [
                position.extend(1.).to_array(),
                normal.extend(0.).to_array(),
                view.extend(0.).to_array(),
                [0.; 4],
            ]
        })
        .flatten()
        .collect();
    let buffer = crate::scene::buffer(
        device,
        "dynamic GI queries",
        bytemuck::cast_slice(&words),
        wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
    );
    let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: buffer.as_entire_binding(),
        }],
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_compute_pass(&Default::default());
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, renderer.test_camera_lit(), &[]);
        pass.set_bind_group(3, &group, &[]);
        pass.dispatch_workgroups(queries.len() as u32, 1, 1);
    }
    queue.submit([encoder.finish()]);
    let words = test_support::read_words(device, queue, &buffer);
    words
        .chunks_exact(16)
        .map(|query| std::array::from_fn(|channel| f32::from_bits(query[12 + channel])))
        .collect()
}

/// The fixtures' surfaces for `indirect`.
const INDIRECT: &str = r#"
fn indirect_of(position:vec3<f32>,moving:bool,lightmapped:bool)->vec4<f32> {
 var s:Surface;
 s.position=position;
 s.normal=vec3(0.,1.,0.);
 s.view=s.normal;
 s.moving=moving;
 s.baked=lightmapped;
 s.front=true;
 for (var face=0u;face<6u;face++) {
  s.baked_irradiance[face]=vec4(.25);
 }
 let indirect=surface_indirect_diffuse(s,s.normal,false);
 return select(vec4(indirect.baked,0.),indirect.dynamic_gi,indirect.dynamic_gi.a>0.);
}
"#;

/// Points and normals throughout a volume over -3..3 on each axis.
fn queries() -> Vec<(Vec3, Vec3)> {
    let points = [
        Vec3::new(0.3, -0.7, 1.1),
        Vec3::new(-2.6, 2.2, -1.9),
        Vec3::new(2.9, 0.1, 2.4),
    ];
    let normals = [
        Vec3::X,
        Vec3::NEG_X,
        Vec3::Y,
        Vec3::NEG_Y,
        Vec3::Z,
        Vec3::NEG_Z,
        Vec3::new(1., 2., -3.),
    ];
    points
        .iter()
        .flat_map(|&point| normals.iter().map(move |&normal| (point, normal)))
        .collect()
}

const VOLUME: DynamicGiVolume = DynamicGiVolume {
    origin: Vec3::splat(-3.),
    spacing: Vec3::splat(2.),
    probes: [4, 4, 4],
};

fn close(actual: f32, expected: f32, tolerance: f32) -> bool {
    (actual - expected).abs() <= tolerance
}

// Nothing to hit: every ray takes the environment's radiance, which a
// cosine-weighted mean over any hemisphere leaves as it is, so the volume
// holds that radiance as its irradiance / PI everywhere, in every direction,
// from the first frame.
#[test]
fn an_open_volume_holds_a_uniform_environments_radiance() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    let radiance = [0.5, 0.25, 0.125];
    let environment = uniform_environment(&device, &queue, &mut scene, radiance);
    scene.set_dynamic_gi_volume(&device, Some(VOLUME)).unwrap();
    let mut input = input(Vec3::new(0., 0., 8.));
    input.environment = Some(environment);
    let settings = settings(DynamicGiQuality::High);
    let mut renderer = Renderer::for_test(&device, &queue, SIZE, &settings);
    render(
        &device,
        &queue,
        &mut renderer,
        &mut scene,
        &input,
        &settings,
        1,
    );
    for (query, answer) in queries()
        .iter()
        .zip(irradiance(&device, &queue, &renderer, &queries()))
    {
        assert_eq!(answer[3], 1., "{query:?}: the volume's share");
        for channel in 0..3 {
            assert!(
                close(
                    answer[channel],
                    radiance[channel],
                    radiance[channel] * 0.005
                ),
                "{query:?}: {answer:?}"
            );
        }
    }
}

// Nothing to hit, no environment and a hemisphere fill of red sky and blue
// ground: the rays' radiance is the field whose irradiance the fill is, so
// facing up the volume holds the sky's irradiance alone, facing down the
// ground's, and facing sideways half of each (pbr_hemisphere: their mix by
// n.y / 2 + 1 / 2, times the intensity).
#[test]
fn an_open_volume_holds_the_hemisphere_fills_irradiance() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    scene.set_dynamic_gi_volume(&device, Some(VOLUME)).unwrap();
    let mut input = input(Vec3::new(0., 0., 8.));
    input.hemisphere_light = HemisphereLight {
        sky_color: [1., 0., 0.],
        ground_color: [0., 0., 1.],
        intensity: std::f32::consts::PI,
    };
    let settings = settings(DynamicGiQuality::High);
    let mut renderer = Renderer::for_test(&device, &queue, SIZE, &settings);
    render(
        &device,
        &queue,
        &mut renderer,
        &mut scene,
        &input,
        &settings,
        1,
    );
    let point = Vec3::new(0.3, -0.7, 1.1);
    let normals = [Vec3::Y, Vec3::NEG_Y, Vec3::X, Vec3::NEG_Z];
    let queries: Vec<_> = normals.iter().map(|&normal| (point, normal)).collect();
    for (normal, answer) in normals
        .iter()
        .zip(irradiance(&device, &queue, &renderer, &queries))
    {
        let up = normal.y * 0.5 + 0.5;
        assert!(close(answer[0], up, 0.02), "{normal}: {answer:?}");
        assert!(close(answer[2], 1. - up, 0.02), "{normal}: {answer:?}");
    }
}

/// A static instance of `test_support::cube` with `material`'s changes,
/// posed by `pose`.
fn add_cube(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    scene: &mut Scene,
    pose: Mat4,
    mobility: Mobility,
    material: impl FnOnce(&mut crate::asset::Material),
) {
    let mut cube = test_support::cube();
    material(&mut cube.materials[0]);
    let ids = scene.add_asset(device, queue, cube).unwrap();
    scene
        .add_instance(
            device,
            queue,
            InstanceState {
                model: ids.model,
                pose,
                visible: true,
                capture_visible: true,
            },
            mobility,
        )
        .unwrap();
}

/// A closed grey room ten metres across, its walls double-sided, about the
/// volume.
fn add_room(device: &wgpu::Device, queue: &wgpu::Queue, scene: &mut Scene) {
    add_cube(
        device,
        queue,
        scene,
        Mat4::from_scale(Vec3::splat(10.)),
        Mobility::Static,
        |material| {
            material.base = [0.5, 0.5, 0.5, 1.];
            material.metallic = 0.;
        },
    );
}

// A point light and the sun outside a closed room light the outside of its
// walls and, through them, the inside faces turned toward them: a probe
// hit takes the visibility of a light that casts a shadow from a ray, so
// none of it reaches a probe inside. At shadow opacity 0 no ray is cast and
// it does, and a light that casts no shadow lights the hit unoccluded, as
// it lights every other receiver.
#[test]
fn light_outside_a_closed_room_reaches_its_probes_only_without_a_shadow() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut maxima = Vec::new();
    for (casts_shadow, shadow_opacity) in [(true, 1.), (true, 0.), (false, 1.)] {
        let mut scene = Scene::new(&device, &queue);
        add_room(&device, &queue, &mut scene);
        scene
            .add_light(
                &device,
                &queue,
                crate::Light {
                    position: Vec3::new(7., 0., 0.),
                    intensity: 200.,
                    range: 30.,
                    casts_shadow,
                    shadow_opacity,
                    ..Default::default()
                },
            )
            .unwrap();
        scene.set_dynamic_gi_volume(&device, Some(VOLUME)).unwrap();
        let mut input = input(Vec3::new(0., 0., 4.));
        input.directional_lights[0] = Some(crate::DirectionalLight {
            direction: Vec3::new(-1., -0.2, 0.1),
            illuminance: 20.,
            shadow: casts_shadow.then(crate::DirectionalShadow::default),
            shadow_opacity,
            ..Default::default()
        });
        let settings = settings(DynamicGiQuality::High);
        let mut renderer = Renderer::for_test(&device, &queue, SIZE, &settings);
        render(
            &device,
            &queue,
            &mut renderer,
            &mut scene,
            &input,
            &settings,
            3,
        );
        let answers = irradiance(&device, &queue, &renderer, &queries());
        assert!(answers.iter().all(|answer| answer[3] == 1.));
        maxima.push(
            answers
                .iter()
                .flat_map(|answer| answer[..3].to_vec())
                .fold(0f32, f32::max),
        );
    }
    assert_eq!(maxima[0], 0., "light leaked into the room");
    assert!(maxima[1] > 0.01, "{maxima:?}");
    assert!(maxima[2] > 0.01, "{maxima:?}");
}

/// `asset` turned inside out and single-sided: each triangle's winding and
/// each normal reversed, so its faces are seen from within, as a room built
/// to be seen from inside is.
fn inward(mut asset: crate::asset::Asset) -> crate::asset::Asset {
    for mesh in &mut asset.meshes {
        for triangle in mesh.indices.chunks_exact_mut(3) {
            triangle.swap(1, 2);
        }
        for vertex in &mut mesh.vertices {
            vertex.normal = vertex.normal.map(|n| -n);
        }
    }
    for material in &mut asset.materials {
        material.double_sided = false;
    }
    asset
}

/// A static instance of `asset`, posed by `pose`.
fn add_static(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    scene: &mut Scene,
    asset: crate::asset::Asset,
    pose: Mat4,
) {
    let ids = scene.add_asset(device, queue, asset).unwrap();
    scene
        .add_instance(
            device,
            queue,
            InstanceState {
                model: ids.model,
                pose,
                visible: true,
                capture_visible: true,
            },
            Mobility::Static,
        )
        .unwrap();
}

// A room of black single-sided walls facing inward, under a bright sky,
// with probes inside it and beyond its walls. A probe outside sees the
// walls' backs, which take no light and shorten its depth, so a receiver
// inside weighs it as occluded and takes only the dark room; were the
// probe ray to pass through the walls' backs, the probe would see the
// room's far side as unoccluded and bring the sky in. A receiver near a
// wall and facing it weighs the probes behind it by the wrap weight and
// those beyond the wall by their visibility, so the dark room's probes
// outweigh the sky's, where Wicked's hard backface test and floor tied
// them all (0.35 of the sky). The walls reflect nothing, so no bounce from
// them carries light either way.
#[test]
fn an_inward_facing_room_keeps_the_sky_outside() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    let environment = uniform_environment(&device, &queue, &mut scene, [1.; 3]);
    let mut room = inward(test_support::cube());
    room.materials[0].base = [0., 0., 0., 1.];
    room.materials[0].metallic = 0.;
    // Walls at ±3; probes at -4, -2, 0, 2 and 4 on each axis.
    add_static(
        &device,
        &queue,
        &mut scene,
        room,
        Mat4::from_scale(Vec3::splat(6.)),
    );
    scene
        .set_dynamic_gi_volume(
            &device,
            Some(DynamicGiVolume {
                origin: Vec3::splat(-4.),
                spacing: Vec3::splat(2.),
                probes: [5, 5, 5],
            }),
        )
        .unwrap();
    let mut input = input(Vec3::new(0., 0., 1.));
    input.environment = Some(environment);
    let settings = settings(DynamicGiQuality::High);
    let mut renderer = Renderer::for_test(&device, &queue, SIZE, &settings);
    render(
        &device,
        &queue,
        &mut renderer,
        &mut scene,
        &input,
        &settings,
        3,
    );
    // Inside, 0.3 m from a wall: facing along it, so the probes beyond the
    // wall lie in front of the receiver, and facing it, seen obliquely from
    // inside the room.
    let along = |position, normal| (position, normal, normal);
    let facing =
        |position, normal: Vec3, across: Vec3| (position, normal, normal * 0.5 + across * 0.866);
    let queries = [
        along(Vec3::new(2.7, 0.3, 0.1), Vec3::Y),
        along(Vec3::new(-2.6, 0.2, 1.1), Vec3::Z),
        along(Vec3::new(0.3, 2.7, -0.4), Vec3::X),
        along(Vec3::new(0.4, -2.7, 0.9), Vec3::NEG_Z),
        facing(Vec3::new(2.7, 0.3, 0.1), Vec3::X, Vec3::Z),
        facing(Vec3::new(-2.7, 0.2, 1.1), Vec3::NEG_X, Vec3::NEG_Z),
        facing(Vec3::new(0.3, 2.7, -0.4), Vec3::Y, Vec3::X),
        facing(Vec3::new(0.4, -2.7, 0.9), Vec3::NEG_Y, Vec3::NEG_X),
    ];
    for (query, answer) in queries.iter().zip(irradiance_seen(
        &device,
        &queue,
        &renderer,
        &queries,
        Receiver::Static,
    )) {
        assert_eq!(answer[3], 1., "{query:?}");
        assert!(
            answer[..3].iter().all(|&channel| channel < 0.01),
            "{query:?}: {answer:?}"
        );
    }
}

// A black floor under an open sky of radiance 1: a receiver on it facing
// up takes the whole sky above, irradiance / PI 1, from the probes above
// it. The wrap weight lets the probes below the floor weigh too, which the
// floor hides from the receiver; the self-shadow bias tests visibility
// from above the floor, where those probes are occluded and the probes
// above are not, so the floor does not shadow itself.
#[test]
fn a_floor_under_an_open_sky_takes_all_of_it() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    let environment = uniform_environment(&device, &queue, &mut scene, [1.; 3]);
    let mut slab = test_support::cube();
    slab.materials[0].base = [0., 0., 0., 1.];
    slab.materials[0].metallic = 0.;
    // Its top at y = 0; probes at -1, 1 and 3.
    add_static(
        &device,
        &queue,
        &mut scene,
        slab,
        Mat4::from_translation(Vec3::new(0., -0.1, 0.))
            * Mat4::from_scale(Vec3::new(20., 0.2, 20.)),
    );
    scene
        .set_dynamic_gi_volume(
            &device,
            Some(DynamicGiVolume {
                origin: Vec3::new(-4., -1., -4.),
                spacing: Vec3::splat(2.),
                probes: [5, 3, 5],
            }),
        )
        .unwrap();
    let mut input = input(Vec3::new(0., 1.5, 1.));
    input.environment = Some(environment);
    let settings = settings(DynamicGiQuality::High);
    let mut renderer = Renderer::for_test(&device, &queue, SIZE, &settings);
    render(
        &device,
        &queue,
        &mut renderer,
        &mut scene,
        &input,
        &settings,
        3,
    );
    let queries: Vec<_> = [
        Vec3::new(0.3, 0., 0.1),
        Vec3::new(-1.3, 0., 1.1),
        Vec3::new(1.7, 0., -0.6),
        Vec3::new(-0.9, 0., -1.8),
    ]
    .into_iter()
    .flat_map(|point| [Vec3::Y, Vec3::new(0.866, 0.5, 0.)].map(|view| (point, Vec3::Y, view)))
    .collect();
    for (query, answer) in queries.iter().zip(irradiance_seen(
        &device,
        &queue,
        &renderer,
        &queries,
        Receiver::Static,
    )) {
        assert!(
            answer[..3].iter().all(|&channel| close(channel, 1., 0.01)),
            "{query:?}: {answer:?}"
        );
    }
}

// A closed room of grey single-sided walls facing inward, under a bright
// sky, holds no light: nothing within it glows, so the probes inside take
// none from their first frame on. Probes beyond its walls see their backs
// and weigh nothing, and the first frame's hits, about which no probe has
// yet been blended, take the volume's zero rather than the sky's fallback,
// which the bounce would otherwise carry about the room for seconds (0.74
// of the sky after three frames).
#[test]
fn a_closed_room_holds_no_light_from_the_sky_beyond_it() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    let environment = uniform_environment(&device, &queue, &mut scene, [1.; 3]);
    let mut room = inward(test_support::cube());
    room.materials[0].base = [0.8, 0.8, 0.8, 1.];
    room.materials[0].metallic = 0.;
    // Walls at ±3; probes at -4, -2, 0, 2 and 4 on each axis.
    add_static(
        &device,
        &queue,
        &mut scene,
        room,
        Mat4::from_scale(Vec3::splat(6.)),
    );
    scene
        .set_dynamic_gi_volume(
            &device,
            Some(DynamicGiVolume {
                origin: Vec3::splat(-4.),
                spacing: Vec3::splat(2.),
                probes: [5, 5, 5],
            }),
        )
        .unwrap();
    let mut input = input(Vec3::new(0., 0., 1.));
    input.environment = Some(environment);
    let settings = settings(DynamicGiQuality::High);
    let mut renderer = Renderer::for_test(&device, &queue, SIZE, &settings);
    render(
        &device,
        &queue,
        &mut renderer,
        &mut scene,
        &input,
        &settings,
        3,
    );
    // Through the room and on its walls, away from its edges.
    let queries = [
        (Vec3::new(0.7, 0.3, 0.1), Vec3::X),
        (Vec3::new(-0.7, 0.2, 1.1), Vec3::NEG_X),
        (Vec3::new(1., -1., 0.5), Vec3::Y),
        (Vec3::new(2.999, 0.3, 0.1), Vec3::NEG_X),
        (Vec3::new(0.3, 2.999, -0.4), Vec3::NEG_Y),
        (Vec3::new(0.4, -2.999, 0.9), Vec3::Y),
    ];
    for (query, answer) in queries.iter().zip(irradiance_seen(
        &device,
        &queue,
        &renderer,
        &along(&queries),
        Receiver::Static,
    )) {
        assert!(
            answer[..3].iter().all(|&channel| channel < 1e-3),
            "{query:?}: {answer:?}"
        );
    }
}

// Probes inside a closed box see only its faces' backs, every one of their
// fixed rays meeting one, so the first frame classifies them inactive and
// from then on each traces the fewest rays and its fixed rays, where a
// probe whose light has only just started traces nearly the most.
#[test]
fn probes_inside_closed_geometry_trace_the_fewest_rays() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    let environment = uniform_environment(&device, &queue, &mut scene, [1.; 3]);
    let mut solid = test_support::cube();
    solid.materials[0].double_sided = false;
    add_static(
        &device,
        &queue,
        &mut scene,
        solid,
        Mat4::from_scale(Vec3::splat(4.)),
    );
    scene
        .set_dynamic_gi_volume(
            &device,
            Some(DynamicGiVolume {
                origin: Vec3::splat(-1.),
                spacing: Vec3::ONE,
                probes: [3, 3, 3],
            }),
        )
        .unwrap();
    let mut input = input(Vec3::new(0., 0., 6.));
    input.environment = Some(environment);
    let settings = settings(DynamicGiQuality::High);
    let mut renderer = Renderer::for_test(&device, &queue, SIZE, &settings);
    let mut rays = || {
        render(
            &device,
            &queue,
            &mut renderer,
            &mut scene,
            &input,
            &settings,
            1,
        );
        renderer.test_dynamic_gi().test_traced_rays(&device, &queue)
    };
    assert_eq!(rays(), 27 * STARTING_RAYS, "the start");
    assert_eq!(rays(), 27 * SETTLED_RAYS, "inactive");
}

// Probes 2 m beyond the walls of a room of single-sided walls facing inward
// see about a quarter of the walls' backs, near the threshold of their
// class, and lie far enough from every face that none moves them. Their
// fixed rays meet the same faces every cycle, so while nothing moves each
// probe's share, and so its class, holds still, where the share of its
// rotated rays would wander across the threshold.
#[test]
fn a_probes_class_holds_still_while_what_it_sees_does() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    let environment = uniform_environment(&device, &queue, &mut scene, [1.; 3]);
    let mut room = inward(test_support::cube());
    room.materials[0].base = [0., 0., 0., 1.];
    room.materials[0].metallic = 0.;
    add_static(
        &device,
        &queue,
        &mut scene,
        room,
        Mat4::from_scale(Vec3::splat(6.)),
    );
    scene
        .set_dynamic_gi_volume(
            &device,
            Some(DynamicGiVolume {
                origin: Vec3::splat(-5.),
                spacing: Vec3::splat(2.5),
                probes: [5, 5, 5],
            }),
        )
        .unwrap();
    let mut input = input(Vec3::new(0., 0., 1.));
    input.environment = Some(environment);
    let settings = settings(DynamicGiQuality::High);
    let mut renderer = Renderer::for_test(&device, &queue, SIZE, &settings);
    render(
        &device,
        &queue,
        &mut renderer,
        &mut scene,
        &input,
        &settings,
        16,
    );
    let first = renderer
        .test_dynamic_gi()
        .test_backface_shares(&device, &queue);
    for _ in 0..40 {
        render(
            &device,
            &queue,
            &mut renderer,
            &mut scene,
            &input,
            &settings,
            1,
        );
        let shares = renderer
            .test_dynamic_gi()
            .test_backface_shares(&device, &queue);
        assert_eq!(shares, first);
    }
    // Several of them lie about the threshold.
    let near = first.iter().filter(|&&share| (share - 0.25).abs() < 0.05);
    assert!(near.count() >= 4, "{first:?}");
}

// A room of black single-sided walls facing inward, under a bright sky. A
// probe diagonally beyond its edge or corner sees few of the walls' backs,
// so it passes the first phase of its class, but it holds no surface in its
// cell: dormant, it lights no static receiver, so a receiver on a wall
// within a tenth of a metre of another, or facing its wall head-on within
// the self-shadow bias, takes nothing from beyond the walls, from the first
// frame's class and from the fixed rays' alike.
#[test]
fn a_static_receiver_by_a_rooms_edge_takes_nothing_from_beyond_its_walls() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    let environment = uniform_environment(&device, &queue, &mut scene, [1.; 3]);
    let mut room = inward(test_support::cube());
    room.materials[0].base = [0., 0., 0., 1.];
    room.materials[0].metallic = 0.;
    // Walls at ±3; probes at -4, -2, 0, 2 and 4 on each axis.
    add_static(
        &device,
        &queue,
        &mut scene,
        room,
        Mat4::from_scale(Vec3::splat(6.)),
    );
    scene
        .set_dynamic_gi_volume(
            &device,
            Some(DynamicGiVolume {
                origin: Vec3::splat(-4.),
                spacing: Vec3::splat(2.),
                probes: [5, 5, 5],
            }),
        )
        .unwrap();
    let mut input = input(Vec3::new(0., 0., 1.));
    input.environment = Some(environment);
    let settings = settings(DynamicGiQuality::High);
    let mut renderer = Renderer::for_test(&device, &queue, SIZE, &settings);
    let queries = [
        // On a wall, by an edge or a corner.
        (Vec3::new(2.999, 2.8, 0.1), Vec3::NEG_X),
        (Vec3::new(2.9, 2.999, -1.3), Vec3::NEG_Y),
        (Vec3::new(-2.999, -2.9, 2.9), Vec3::X),
        (Vec3::new(1.1, -2.999, 2.95), Vec3::Y),
        // 0.3 m from a wall, facing it.
        (Vec3::new(2.7, 0.3, 0.1), Vec3::X),
        (Vec3::new(-2.7, 0.2, 1.1), Vec3::NEG_X),
        (Vec3::new(0.3, 2.7, -0.4), Vec3::Y),
        (Vec3::new(0.4, -2.7, 0.9), Vec3::NEG_Y),
    ];
    for frames in [3, 60] {
        render(
            &device,
            &queue,
            &mut renderer,
            &mut scene,
            &input,
            &settings,
            frames,
        );
        for (query, answer) in queries.iter().zip(irradiance_seen(
            &device,
            &queue,
            &renderer,
            &along(&queries),
            Receiver::Static,
        )) {
            assert!(
                answer[..3].iter().all(|&channel| channel < 1e-3),
                "{query:?} after {frames} frames: {answer:?}"
            );
        }
    }
}

// Under an open sky, with nothing near them, every probe is dormant: no
// static receiver takes it, but a moving instance that appears among them
// is lit by them from its first frame, the sky's radiance at its top.
#[test]
fn a_moving_instance_in_open_space_is_lit_from_its_first_frame() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    let environment = uniform_environment(&device, &queue, &mut scene, [0.5; 3]);
    scene.set_dynamic_gi_volume(&device, Some(VOLUME)).unwrap();
    let mut input = input(Vec3::new(0., 0., 8.));
    input.environment = Some(environment);
    let settings = settings(DynamicGiQuality::High);
    let mut renderer = Renderer::for_test(&device, &queue, SIZE, &settings);
    render(
        &device,
        &queue,
        &mut renderer,
        &mut scene,
        &input,
        &settings,
        30,
    );
    add_cube(
        &device,
        &queue,
        &mut scene,
        Mat4::from_translation(Vec3::new(0.3, -0.2, 0.4)),
        Mobility::Moving,
        |material| {
            material.base = [0.5, 0.5, 0.5, 1.];
            material.metallic = 0.;
        },
    );
    render(
        &device,
        &queue,
        &mut renderer,
        &mut scene,
        &input,
        &settings,
        1,
    );
    let top = [(Vec3::new(0.3, 0.301, 0.4), Vec3::Y)];
    let moving = irradiance_seen(&device, &queue, &renderer, &along(&top), Receiver::Moving);
    assert!(
        moving[0][3] == 1. && close(moving[0][0], 0.5, 0.005),
        "{moving:?}"
    );
    // A static receiver there would take nothing of them.
    let fixed = irradiance_seen(&device, &queue, &renderer, &along(&top), Receiver::Static);
    assert_eq!(fixed[0][3], 0., "{fixed:?}");
}

// Under an open sky every probe is dormant, so once started each traces the
// fewest rays and its fixed rays; but the probes whose cells a moving
// instance reaches into trace as active ones, where its light changes, here
// a box the rays do not see, so it gives them no surface of its own.
#[test]
fn dormant_probes_trace_the_fewest_rays_but_about_a_moving_instance() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let second_frame = |moving: bool| {
        let mut scene = Scene::new(&device, &queue);
        let environment = uniform_environment(&device, &queue, &mut scene, [0.5; 3]);
        if moving {
            let ids = scene
                .add_asset(&device, &queue, test_support::cube())
                .unwrap();
            scene
                .add_instance(
                    &device,
                    &queue,
                    InstanceState {
                        model: ids.model,
                        pose: Mat4::IDENTITY,
                        visible: true,
                        capture_visible: false,
                    },
                    Mobility::Moving,
                )
                .unwrap();
        }
        // Probes at -4, -2, 0, 2 and 4: the box's bounds reach into the
        // cells of the 27 at -2, 0 and 2.
        scene
            .set_dynamic_gi_volume(
                &device,
                Some(DynamicGiVolume {
                    origin: Vec3::splat(-4.),
                    spacing: Vec3::splat(2.),
                    probes: [5, 5, 5],
                }),
            )
            .unwrap();
        let mut input = input(Vec3::new(0., 0., 10.));
        input.environment = Some(environment);
        let settings = settings(DynamicGiQuality::High);
        let mut renderer = Renderer::for_test(&device, &queue, SIZE, &settings);
        let mut rays = || {
            render(
                &device,
                &queue,
                &mut renderer,
                &mut scene,
                &input,
                &settings,
                1,
            );
            renderer.test_dynamic_gi().test_traced_rays(&device, &queue)
        };
        assert_eq!(rays(), 125 * STARTING_RAYS, "the start");
        rays()
    };
    let open = second_frame(false);
    assert_eq!(open, 125 * SETTLED_RAYS, "dormant");
    // Just started, their light is far from settled, so each traces many.
    let about = second_frame(true);
    assert!(
        about >= 98 * SETTLED_RAYS + 27 * 4 * SETTLED_RAYS,
        "{about}"
    );
}

/// An open floor under a sky and a sun that casts shadows, whose cascades
/// follow the camera, a box on the floor and probes 2 m apart above it: a
/// scene whose volume converges in a few seconds.
fn floor_scene(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
) -> (Scene, FrameInput, (crate::InstanceId, InstanceState)) {
    let mut scene = Scene::new(device, queue);
    let environment = uniform_environment(device, queue, &mut scene, [0.5; 3]);
    add_cube(
        device,
        queue,
        &mut scene,
        Mat4::from_translation(Vec3::new(0., -3.5, 0.)) * Mat4::from_scale(Vec3::new(30., 1., 30.)),
        Mobility::Static,
        |material| {
            material.base = [0.5, 0.5, 0.5, 1.];
            material.metallic = 0.;
        },
    );
    let mut cube = test_support::cube();
    cube.materials[0].base = [0.6, 0.2, 0.2, 1.];
    cube.materials[0].metallic = 0.;
    let model = scene.add_asset(device, queue, cube).unwrap().model;
    let state = InstanceState {
        model,
        pose: Mat4::from_translation(Vec3::new(1., -2.5, 0.)),
        visible: true,
        capture_visible: true,
    };
    let instance = scene
        .add_instance(device, queue, state, Mobility::Moving)
        .unwrap();
    // Probes at y = -2, 0, 2 and 4.
    scene
        .set_dynamic_gi_volume(
            device,
            Some(DynamicGiVolume {
                origin: Vec3::new(-4., -2., -4.),
                spacing: Vec3::splat(2.),
                probes: [5, 4, 5],
            }),
        )
        .unwrap();
    let mut input = input(Vec3::new(0., 0.5, 3.5));
    input.environment = Some(environment);
    input.directional_lights[0] = Some(crate::DirectionalLight {
        direction: Vec3::new(0.3, -1., 0.2),
        illuminance: 3.,
        shadow: Some(crate::DirectionalShadow::default()),
        ..Default::default()
    });
    (scene, input, (instance, state))
}

/// An edit to a scene or its frame.
type Edit<'a> = dyn Fn(&mut Scene, &mut FrameInput) + 'a;

/// Renders frames of `scene` until one traces no ray, at most `most`, and
/// returns how many it took.
fn render_until_paused(
    (device, queue): (&wgpu::Device, &wgpu::Queue),
    renderer: &mut Renderer,
    scene: &mut Scene,
    input: &FrameInput,
    settings: &Settings,
    most: usize,
) -> Option<usize> {
    (1..=most).find(|_| {
        render(device, queue, renderer, scene, input, settings, 1);
        renderer.test_dynamic_gi().test_traced_rays(device, queue) == 0
    })
}

// Once its light has converged (RTXGI's probe variability stops falling),
// a volume traces nothing while what its light follows holds still: its
// light then holds exactly, and a camera that moves, with the sun's
// cascades about it, changes nothing of it.
// Each edit to what it follows starts it again, and it pauses again once
// its light has settled: an instance moved, the sun turned, the hemisphere
// fill and the environment's intensity.
#[test]
fn a_converged_volume_traces_nothing_until_what_its_light_follows_changes() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let gpu = (&device, &queue);
    let (mut scene, mut input, (instance, state)) = floor_scene(&device, &queue);
    let settings = settings(DynamicGiQuality::High);
    let mut renderer = Renderer::for_test(&device, &queue, SIZE, &settings);
    let first = render_until_paused(gpu, &mut renderer, &mut scene, &input, &settings, 400);
    assert!(first.is_some_and(|frames| frames > 32), "{first:?}");
    let queries = [
        (Vec3::new(0.3, -2.9, 0.4), Vec3::Y),
        (Vec3::new(1.51, -2.5, 0.1), Vec3::X),
    ];
    let paused = irradiance(&device, &queue, &renderer, &queries);
    for eye in [Vec3::new(2., 1., 3.), Vec3::new(-3., 0., -1.)] {
        input.camera.eye = eye;
        input.camera.view = Mat4::from_translation(-eye);
        render(
            &device,
            &queue,
            &mut renderer,
            &mut scene,
            &input,
            &settings,
            10,
        );
        assert_eq!(
            renderer.test_dynamic_gi().test_traced_rays(&device, &queue),
            0,
            "a camera that moves"
        );
    }
    assert_eq!(irradiance(&device, &queue, &renderer, &queries), paused);
    let edits: [(&str, &Edit<'_>); 4] = [
        ("an instance moved", &|scene, _| {
            let moved = InstanceState {
                pose: Mat4::from_translation(Vec3::new(-1., -2.5, 0.5)),
                ..state
            };
            scene.set_instance(&queue, instance, moved).unwrap();
        }),
        ("the sun's direction", &|_, input| {
            if let Some(sun) = &mut input.directional_lights[0] {
                sun.direction = Vec3::new(-0.4, -1., 0.1);
            }
        }),
        ("the hemisphere fill", &|_, input| {
            input.hemisphere_light.intensity = 0.5;
        }),
        ("the environment's intensity", &|_, input| {
            input.diffuse_environment.intensity = 0.5;
        }),
    ];
    for (edit, apply) in edits {
        apply(&mut scene, &mut input);
        render(
            &device,
            &queue,
            &mut renderer,
            &mut scene,
            &input,
            &settings,
            1,
        );
        assert!(
            renderer.test_dynamic_gi().test_traced_rays(&device, &queue) > 0,
            "{edit}"
        );
        let again = render_until_paused(gpu, &mut renderer, &mut scene, &input, &settings, 400);
        assert!(again.is_some(), "{edit}: never paused again");
    }
    // The probe hits' light list without the scene's lights, a diagnostic
    // setting.
    #[cfg(feature = "diagnostics")]
    {
        let mut without = settings;
        without.diagnostics.disable.local_lights = true;
        render(
            &device,
            &queue,
            &mut renderer,
            &mut scene,
            &input,
            &without,
            1,
        );
        assert!(
            renderer.test_dynamic_gi().test_traced_rays(&device, &queue) > 0,
            "the scene's lights left out"
        );
    }
}

// Under an open sky, with nothing about its probes, a volume's variability
// is nothing from the start; one too large for its first frames to start
// every probe still starts them all before it pauses.
#[test]
fn a_volume_does_not_pause_while_probes_have_yet_to_start() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    let environment = uniform_environment(&device, &queue, &mut scene, [0.5; 3]);
    // 4800 probes: 38 frames of 128.
    let volume = DynamicGiVolume {
        origin: Vec3::new(-19., -11., -19.),
        spacing: Vec3::splat(2.),
        probes: [20, 12, 20],
    };
    scene.set_dynamic_gi_volume(&device, Some(volume)).unwrap();
    let mut input = input(Vec3::new(0., 0., 18.));
    input.environment = Some(environment);
    let settings = settings(DynamicGiQuality::High);
    let mut renderer = Renderer::for_test(&device, &queue, SIZE, &settings);
    render(
        &device,
        &queue,
        &mut renderer,
        &mut scene,
        &input,
        &settings,
        60,
    );
    // Far from the camera, among the last probes to start.
    let far = [(Vec3::new(-18.3, -10.2, -18.1), Vec3::Y)];
    assert_eq!(irradiance(&device, &queue, &renderer, &far)[0][3], 1.);
}

// A closed box of single-sided faces turned inward, which glow and hold a
// shadowed light, over a floor, with every probe outside it and nothing
// else lit: the probes' rays meet the box's backs and take nothing from
// within, and the light's visibility rays from the floor meet them too, so
// no probe holds any light.
#[test]
fn a_probe_outside_an_inward_facing_box_takes_nothing_from_inside() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    let mut lamp = inward(test_support::cube());
    lamp.materials[0].base = [0., 0., 0., 1.];
    lamp.materials[0].metallic = 0.;
    lamp.materials[0].emissive = [0.5; 3];
    // The box spans ±1.
    add_static(
        &device,
        &queue,
        &mut scene,
        lamp,
        Mat4::from_scale(Vec3::splat(2.)),
    );
    add_cube(
        &device,
        &queue,
        &mut scene,
        Mat4::from_translation(Vec3::new(0., -1.6, 0.))
            * Mat4::from_scale(Vec3::new(20., 0.2, 20.)),
        Mobility::Static,
        |material| {
            material.base = [0.5, 0.5, 0.5, 1.];
            material.metallic = 0.;
        },
    );
    scene
        .add_light(
            &device,
            &queue,
            crate::Light {
                intensity: 200.,
                range: 20.,
                casts_shadow: true,
                ..Default::default()
            },
        )
        .unwrap();
    // Probes at -5, -2.5, 0, 2.5 and 5 along x and z, and at -1.2 and 1.3
    // along y, between the floor's top (-1.5) and the box and above it.
    scene
        .set_dynamic_gi_volume(
            &device,
            Some(DynamicGiVolume {
                origin: Vec3::new(-5., -1.2, -5.),
                spacing: Vec3::new(2.5, 2.5, 2.5),
                probes: [5, 2, 5],
            }),
        )
        .unwrap();
    let input = input(Vec3::new(0., 0., 4.));
    let settings = settings(DynamicGiQuality::High);
    let mut renderer = Renderer::for_test(&device, &queue, SIZE, &settings);
    render(
        &device,
        &queue,
        &mut renderer,
        &mut scene,
        &input,
        &settings,
        3,
    );
    // Outside the box, facing it.
    let queries = [
        (Vec3::new(2., 0.3, 0.4), Vec3::NEG_X),
        (Vec3::new(0.2, -1.15, 0.3), Vec3::Y),
        (Vec3::new(0.3, 1.25, -0.2), Vec3::NEG_Y),
        (Vec3::new(-0.4, 0.1, -2.2), Vec3::Z),
    ];
    for (query, answer) in queries
        .iter()
        .zip(irradiance(&device, &queue, &renderer, &queries))
    {
        assert_eq!(answer[3], 1., "{query:?}");
        assert!(
            answer[..3].iter().all(|&channel| channel < 1e-6),
            "{query:?}: {answer:?}"
        );
    }
}

// The volume's share is whole within its extent and fades to nothing over
// the one spacing past it, along each axis it passes.
#[test]
fn the_volumes_share_fades_over_the_spacing_past_its_extent() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    let environment = uniform_environment(&device, &queue, &mut scene, [0.5; 3]);
    scene.set_dynamic_gi_volume(&device, Some(VOLUME)).unwrap();
    let mut input = input(Vec3::new(0., 0., 8.));
    input.environment = Some(environment);
    let settings = settings(DynamicGiQuality::High);
    let mut renderer = Renderer::for_test(&device, &queue, SIZE, &settings);
    render(
        &device,
        &queue,
        &mut renderer,
        &mut scene,
        &input,
        &settings,
        1,
    );
    // The extent is -3..3 on each axis, the spacing 2.
    let points = [
        (Vec3::new(2.9, 0., 0.), 1.),
        (Vec3::new(4., 0., 0.), 0.5),
        (Vec3::new(0., -3.5, 0.), 0.75),
        (Vec3::new(4., 4., 0.), 0.25),
        (Vec3::new(0., 0., 5.5), 0.),
    ];
    let queries: Vec<_> = points.iter().map(|&(point, _)| (point, Vec3::Y)).collect();
    for ((point, share), answer) in points
        .iter()
        .zip(irradiance(&device, &queue, &renderer, &queries))
    {
        assert!(close(answer[3], *share, 1e-5), "{point}: {answer:?}");
    }
}

/// `surface_indirect_diffuse` of a surface at `position` facing +Y: moving
/// with an ambient cube of 0.25 or static and, where `lightmapped`, of a
/// lightmapped material, as the last frame's camera group 0 shades it.
fn indirect(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    renderer: &Renderer,
    surfaces: &[(Vec3, bool, bool)],
) -> Vec<[f32; 4]> {
    surfaces
        .iter()
        .flat_map(|&(position, moving, lightmapped)| {
            let expression =
                format!("indirect_of(queries[id.x].position.xyz,{moving},{lightmapped})");
            observe(
                device,
                queue,
                renderer,
                &[(position, Vec3::Y, Vec3::Y)],
                &expression,
            )
        })
        .collect()
}

// The one determination: a lightmapped receiver keeps its bake and takes
// nothing of the volume while baked lighting is on; a moving one takes the
// volume over its ambient cube where the volume lights it, and its cube
// beyond; a static receiver with no chart takes the volume whether or not
// baked lighting is on. A floor below the receivers gives the probes about
// them a surface, as a static receiver's own surface gives its probes.
#[test]
fn the_volume_lights_receivers_below_charts_and_above_ambient_cubes() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    let environment = uniform_environment(&device, &queue, &mut scene, [0.5; 3]);
    // Its top at y = -1.5, half a metre below the probes at y = -1.
    add_cube(
        &device,
        &queue,
        &mut scene,
        Mat4::from_translation(Vec3::new(0., -2., 0.)) * Mat4::from_scale(Vec3::new(20., 1., 20.)),
        Mobility::Static,
        |material| {
            material.base = [0.5, 0.5, 0.5, 1.];
            material.metallic = 0.;
        },
    );
    scene.set_dynamic_gi_volume(&device, Some(VOLUME)).unwrap();
    let settings = settings(DynamicGiQuality::High);
    let mut renderer = Renderer::for_test(&device, &queue, SIZE, &settings);
    let inside = Vec3::new(0.3, -0.7, 1.1);
    let beyond = Vec3::new(0., 0., 6.);
    for baked_lighting in [true, false] {
        let mut input = input(Vec3::new(0., 0., 8.));
        input.environment = Some(environment);
        input.baked_lighting = baked_lighting;
        render(
            &device,
            &queue,
            &mut renderer,
            &mut scene,
            &input,
            &settings,
            1,
        );
        let answers = indirect(
            &device,
            &queue,
            &renderer,
            &[
                (inside, false, true),
                (inside, true, false),
                (beyond, true, false),
                (inside, false, false),
            ],
        );
        let [charted, moving, moving_beyond, unbaked] = answers.try_into().unwrap();
        // a: the volume's share; rgb: the volume's irradiance where it has a
        // share, else the bake or cube. With baked lighting off nothing is
        // charted, and the volume lights the lightmapped receiver too.
        let charted_share = if baked_lighting { 0. } else { 1. };
        assert_eq!(charted[3], charted_share, "{baked_lighting}: {charted:?}");
        assert_eq!(moving[3], 1., "{baked_lighting}: {moving:?}");
        assert!(close(moving[0], 0.5, 0.005), "{moving:?}");
        assert_eq!(moving_beyond[3], 0., "{moving_beyond:?}");
        let cube = if baked_lighting { 0.25 } else { 0. };
        assert!(
            close(moving_beyond[0], cube, 1e-6),
            "{baked_lighting}: {moving_beyond:?}"
        );
        assert_eq!(unbaked[3], 1., "{baked_lighting}: {unbaked:?}");
    }
}

// Inside a closed room whose black walls emit, a white receiver takes its
// diffuse light from the volume in place of the frame's ambient: the
// hemisphere fill, which the walls hide, changes nothing of it; without the
// volume it takes the fill and nothing of the walls.
#[test]
fn a_receiver_the_volume_lights_takes_it_in_place_of_the_frames_ambient() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut pixels = Vec::new();
    for (quality, fill) in [
        (DynamicGiQuality::High, 0.),
        (DynamicGiQuality::High, 8.),
        (DynamicGiQuality::Off, 0.),
    ] {
        let mut scene = Scene::new(&device, &queue);
        add_cube(
            &device,
            &queue,
            &mut scene,
            Mat4::from_scale(Vec3::splat(10.)),
            Mobility::Static,
            |material| {
                material.base = [0., 0., 0., 1.];
                material.metallic = 0.;
                material.emissive = [0.4, 0.2, 0.1];
            },
        );
        add_cube(
            &device,
            &queue,
            &mut scene,
            Mat4::IDENTITY,
            Mobility::Moving,
            |material| {
                material.base = [1.; 4];
                material.metallic = 0.;
                material.roughness = 1.;
            },
        );
        scene.set_dynamic_gi_volume(&device, Some(VOLUME)).unwrap();
        let mut input = input(Vec3::new(0., 0., 3.));
        input.hemisphere_light = HemisphereLight {
            sky_color: [1.; 3],
            ground_color: [1.; 3],
            intensity: fill,
        };
        let settings = settings(quality);
        let mut renderer = Renderer::for_test(&device, &queue, SIZE, &settings);
        let output = crate::view::targets::target(&device, "receiver", SIZE, gbuffer::COLOR);
        for _ in 0..3 {
            let mut encoder = device.create_command_encoder(&Default::default());
            renderer.render(
                &device,
                &queue,
                &mut encoder,
                &mut scene,
                &input,
                &settings,
                &output,
                None,
            );
            queue.submit([encoder.finish()]);
            renderer.finish_frame(&mut scene);
        }
        // The receiver's face toward the camera fills the frame's centre.
        let bytes = test_support::read(&device, &queue, output.texture(), 8);
        let at = ((SIZE[1] / 2) * SIZE[0] + SIZE[0] / 2) as usize * 8;
        pixels.push(std::array::from_fn::<f32, 3, _>(|channel| {
            test_support::half(&bytes[at + channel * 2..])
        }));
    }
    let [lit, filled, without] = pixels.try_into().unwrap();
    assert!(lit[0] > 0.2 && lit[0] > lit[2] * 2., "{lit:?}");
    // The first frame's probe hits, about which no probe has yet been
    // blended, take the volume's zero, not the fill, so the probes hold
    // nothing of it; added to the receiver the fill would bring 8 / PI.
    for channel in 0..3 {
        assert!(
            close(filled[channel], lit[channel], lit[channel] * 1e-3),
            "the fill reached a receiver the volume lights: {lit:?}, {filled:?}"
        );
    }
    assert!(without[0] < 1e-3, "{without:?}");
}

/// A volume of 16 by 4 by 16 probes two metres apart: more than a frame
/// starts.
const LARGE: DynamicGiVolume = DynamicGiVolume {
    origin: Vec3::new(-15., -3., -15.),
    spacing: Vec3::splat(2.),
    probes: [16, 4, 16],
};
/// The camera's position, within `LARGE`.
const EYE: Vec3 = Vec3::new(0., 0., 14.);
/// Near the camera, and at the volume's far corner.
const NEAR: Vec3 = Vec3::new(0.3, 0.2, 12.9);
const FAR: Vec3 = Vec3::new(-14.1, 0.2, -14.3);

/// An open scene of uniform radiance holding `LARGE`, seen from `EYE`.
fn large_scene(device: &wgpu::Device, queue: &wgpu::Queue) -> (Scene, FrameInput) {
    let mut scene = Scene::new(device, queue);
    let environment = uniform_environment(device, queue, &mut scene, [0.5; 3]);
    scene.set_dynamic_gi_volume(device, Some(LARGE)).unwrap();
    let mut input = input(EYE);
    input.environment = Some(environment);
    (scene, input)
}

/// The volume's share at `NEAR` and at `FAR`, displaced by `offset`: 1
/// where a probe about the point has been blended, 0 where none has.
fn shares(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    renderer: &Renderer,
    offset: Vec3,
) -> [f32; 2] {
    let answers = irradiance(
        device,
        queue,
        renderer,
        &[(NEAR + offset, Vec3::Y), (FAR + offset, Vec3::Y)],
    );
    [answers[0][3], answers[1][3]]
}

/// The frames that start every probe of `LARGE` at High, with a margin.
fn ramp_frames() -> usize {
    let started = (super::RAMP_RAYS / crate::shading::dynamic_gi::MOST_RAYS) as usize;
    1024usize.div_ceil(started) + 2
}

// A restart starts no more probes in a frame than its budget of rays holds
// at the most rays, the nearest the camera first, where Wicked starts every
// probe in the first frame; a probe not yet started weighs nothing, so a
// receiver among such probes keeps its fallback, until later frames start
// them all.
#[test]
fn a_restart_starts_the_probes_nearest_the_camera_first() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let (mut scene, input) = large_scene(&device, &queue);
    let settings = settings(DynamicGiQuality::High);
    let mut renderer = Renderer::for_test(&device, &queue, SIZE, &settings);
    render(
        &device,
        &queue,
        &mut renderer,
        &mut scene,
        &input,
        &settings,
        1,
    );
    let most = crate::shading::dynamic_gi::MOST_RAYS;
    assert_eq!(
        renderer.test_dynamic_gi().test_traced_rays(&device, &queue),
        super::RAMP_RAYS / most * STARTING_RAYS,
        "the first frame's rays"
    );
    assert_eq!(shares(&device, &queue, &renderer, Vec3::ZERO), [1., 0.]);
    render(
        &device,
        &queue,
        &mut renderer,
        &mut scene,
        &input,
        &settings,
        ramp_frames(),
    );
    assert_eq!(shares(&device, &queue, &renderer, Vec3::ZERO), [1., 1.]);
}

// Once started, the probes continue through a camera cut and a resize,
// which reset the renderer's history but not theirs, and through a frame
// rendered for a scroll that would start them all and abandoned; another
// placement, another scene and a frame that does not run them start them
// afresh, the far probes not in the first frame.
#[test]
fn the_probes_restart_only_for_another_placement_or_scene_or_a_frame_without_them() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let settings = settings(DynamicGiQuality::High);
    let started = |renderer: &mut Renderer, scene: &mut Scene, input: &FrameInput| {
        render(
            &device,
            &queue,
            renderer,
            scene,
            input,
            &settings,
            ramp_frames(),
        );
        assert_eq!(shares(&device, &queue, renderer, Vec3::ZERO), [1., 1.]);
    };
    let restarted = |renderer: &Renderer| shares(&device, &queue, renderer, Vec3::ZERO)[1] == 0.;
    let (mut scene, input) = large_scene(&device, &queue);
    let mut renderer = Renderer::for_test(&device, &queue, SIZE, &settings);
    started(&mut renderer, &mut scene, &input);
    // A camera cut and a resize.
    let mut cut = input;
    cut.camera_cut = true;
    renderer.resize(&device, [SIZE[0] / 2, SIZE[1]], 1., &settings);
    render(
        &device,
        &queue,
        &mut renderer,
        &mut scene,
        &cut,
        &settings,
        1,
    );
    assert!(!restarted(&renderer), "a renderer reset");
    // A frame for the volume scrolled a whole lattice away, every probe of
    // which enters, abandoned, and the volume scrolled back.
    let away = DynamicGiVolume {
        origin: LARGE.origin + Vec3::X * LARGE.spacing.x * LARGE.probes[0] as f32,
        ..LARGE
    };
    scene.set_dynamic_gi_volume(&device, Some(away)).unwrap();
    let output = crate::view::targets::target(&device, "abandoned", SIZE, gbuffer::COLOR);
    let mut encoder = device.create_command_encoder(&Default::default());
    renderer.render(
        &device,
        &queue,
        &mut encoder,
        &mut scene,
        &input,
        &settings,
        &output,
        None,
    );
    drop(encoder);
    scene.set_dynamic_gi_volume(&device, Some(LARGE)).unwrap();
    render(
        &device,
        &queue,
        &mut renderer,
        &mut scene,
        &input,
        &settings,
        1,
    );
    assert!(!restarted(&renderer), "an abandoned frame");
    // Another placement: off the lattice by a quarter spacing.
    let moved = DynamicGiVolume {
        origin: LARGE.origin + Vec3::splat(0.5),
        ..LARGE
    };
    scene.set_dynamic_gi_volume(&device, Some(moved)).unwrap();
    render(
        &device,
        &queue,
        &mut renderer,
        &mut scene,
        &input,
        &settings,
        1,
    );
    assert!(restarted(&renderer), "another placement");
    started(&mut renderer, &mut scene, &input);
    // A frame that does not run them.
    let off = self::settings(DynamicGiQuality::Off);
    render(&device, &queue, &mut renderer, &mut scene, &input, &off, 1);
    render(
        &device,
        &queue,
        &mut renderer,
        &mut scene,
        &input,
        &settings,
        1,
    );
    assert!(restarted(&renderer), "a frame without them");
    // Another scene with the same placement.
    started(&mut renderer, &mut scene, &input);
    let (mut other, input) = large_scene(&device, &queue);
    render(
        &device,
        &queue,
        &mut renderer,
        &mut other,
        &input,
        &settings,
        1,
    );
    assert!(restarted(&renderer), "another scene");
}

// A placement is refused, keeping the installed one, unless it is a finite
// lattice of at least two probes along each axis, a positive spacing apart,
// that fits the device's largest texture.
#[test]
fn the_scene_refuses_a_placement_that_is_not_a_lattice_or_does_not_fit() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    scene.set_dynamic_gi_volume(&device, Some(VOLUME)).unwrap();
    let largest = device.limits().max_texture_dimension_2d;
    for (refused, invalid) in [
        (
            DynamicGiVolume {
                spacing: Vec3::new(1., 0., 1.),
                ..VOLUME
            },
            true,
        ),
        (
            DynamicGiVolume {
                spacing: Vec3::new(1., -1., 1.),
                ..VOLUME
            },
            true,
        ),
        (
            DynamicGiVolume {
                origin: Vec3::new(f32::NAN, 0., 0.),
                ..VOLUME
            },
            true,
        ),
        (
            DynamicGiVolume {
                spacing: Vec3::splat(f32::INFINITY),
                ..VOLUME
            },
            true,
        ),
        (
            DynamicGiVolume {
                probes: [4, 1, 4],
                ..VOLUME
            },
            true,
        ),
        // Each slab of probes is a row of 18-texel depth maps.
        (
            DynamicGiVolume {
                probes: [largest / 18 + 1, 2, 2],
                ..VOLUME
            },
            false,
        ),
    ] {
        let error = scene
            .set_dynamic_gi_volume(&device, Some(refused))
            .unwrap_err();
        assert_eq!(
            matches!(error, crate::SceneError::InvalidDynamicGiVolume),
            invalid,
            "{refused:?}: {error}"
        );
        assert!(
            invalid || matches!(error, crate::SceneError::DeviceLimit),
            "{error}"
        );
        assert_eq!(scene.dynamic_gi_volume(), Some(VOLUME));
    }
    scene.set_dynamic_gi_volume(&device, None).unwrap();
    assert_eq!(scene.dynamic_gi_volume(), None);
}

// In a closed room whose walls emit and reflect half the light reaching
// them, each frame's hits reflect the volume's last frame, damped by 0.95,
// and the walls take it,
// so the volume settles between the walls' emission and the bound
// emission / (1 - 0.95 / 2) of damped reflection without loss elsewhere;
// without the bounce it would hold the emission, and with Wicked df44c3d's
// further division by PI about 1.18 times it.
#[test]
fn the_bounce_carries_light_between_the_walls_damped() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    let emission = 0.2;
    add_cube(
        &device,
        &queue,
        &mut scene,
        Mat4::from_scale(Vec3::splat(6.)),
        Mobility::Static,
        |material| {
            material.base = [0.5, 0.5, 0.5, 1.];
            material.metallic = 0.;
            material.roughness = 1.;
            material.emissive = [emission; 3];
        },
    );
    // Probes half a metre beyond the walls and between them, off every
    // surface, so the volume covers the walls.
    scene
        .set_dynamic_gi_volume(
            &device,
            Some(DynamicGiVolume {
                origin: Vec3::splat(-3.5),
                spacing: Vec3::splat(7. / 3.),
                probes: [4, 4, 4],
            }),
        )
        .unwrap();
    let input = input(Vec3::new(0., 0., 4.));
    let settings = settings(DynamicGiQuality::High);
    let mut renderer = Renderer::for_test(&device, &queue, SIZE, &settings);
    render(
        &device,
        &queue,
        &mut renderer,
        &mut scene,
        &input,
        &settings,
        60,
    );
    let bound = emission / (1. - 0.95 * 0.5);
    // On each wall, facing into the room.
    let walls = [
        (Vec3::new(2.99, 0.3, -1.1), Vec3::NEG_X),
        (Vec3::new(-2.99, -0.7, 1.4), Vec3::X),
        (Vec3::new(0.4, 2.99, 0.9), Vec3::NEG_Y),
        (Vec3::new(-1.2, -2.99, -0.3), Vec3::Y),
        (Vec3::new(1.3, -0.2, 2.99), Vec3::NEG_Z),
        (Vec3::new(-0.6, 1.7, -2.99), Vec3::Z),
    ];
    for (query, answer) in walls.iter().zip(irradiance_seen(
        &device,
        &queue,
        &renderer,
        &along(&walls),
        Receiver::Static,
    )) {
        assert!(
            answer[0] > emission * 1.5 && answer[0] < bound,
            "{query:?}: {answer:?}"
        );
    }
}

/// A volume of 8 by 2 by 2 probes two metres apart, all of which a frame
/// starts.
const SCROLLED: DynamicGiVolume = DynamicGiVolume {
    origin: Vec3::new(-7., -1., -1.),
    spacing: Vec3::splat(2.),
    probes: [8, 2, 2],
};

// The rays a probe traces once its light has settled: the fewest, a bucket
// (DDGI_RAY_BUCKET_COUNT), and its fixed rays.
const SETTLED_RAYS: u32 = 4 + crate::shading::dynamic_gi::FIXED_RAYS_PER_FRAME;
// The rays a probe that starts afresh traces: the most at High, and its
// fixed rays.
const STARTING_RAYS: u32 =
    crate::shading::dynamic_gi::MOST_RAYS + crate::shading::dynamic_gi::FIXED_RAYS_PER_FRAME;

// Under an unchanging sky every probe's light settles, so each traces the
// fewest rays and the volume then pauses; a probe that starts afresh traces
// the most, as a restart's do. A scroll by whole spacings keeps the probes
// that stay, so only those of the planes that enter trace the most: one
// plane forward, the planes of two axes at once, and after a move of the
// render origin, in its new frame. An origin off the lattice is another
// placement, and every probe starts again.
#[test]
fn a_scroll_starts_only_the_probes_that_enter() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    let environment = uniform_environment(&device, &queue, &mut scene, [0.5; 3]);
    scene
        .set_dynamic_gi_volume(&device, Some(SCROLLED))
        .unwrap();
    let mut input = input(Vec3::new(0., 0., 6.));
    input.environment = Some(environment);
    let settings = settings(DynamicGiQuality::High);
    let mut renderer = Renderer::for_test(&device, &queue, SIZE, &settings);
    let most = STARTING_RAYS;
    // Installs the volume with its origin at `origin`, renders one frame,
    // and returns the rays it traced, then lets the light settle again.
    let mut scroll_to = |scene: &mut Scene, input: &FrameInput, origin: Vec3| {
        let volume = DynamicGiVolume { origin, ..SCROLLED };
        scene.set_dynamic_gi_volume(&device, Some(volume)).unwrap();
        render(&device, &queue, &mut renderer, scene, input, &settings, 1);
        let rays = renderer.test_dynamic_gi().test_traced_rays(&device, &queue);
        render(&device, &queue, &mut renderer, scene, input, &settings, 100);
        rays
    };
    let origin = SCROLLED.origin;
    assert_eq!(
        scroll_to(&mut scene, &input, origin),
        32 * most,
        "the start"
    );
    // The same placement again changes nothing: the settled volume has
    // paused.
    assert_eq!(
        scroll_to(&mut scene, &input, origin),
        0,
        "the settled light"
    );
    // One spacing forward along x, within the lattice's rounding: its last
    // plane of 2 by 2 enters.
    let forward = origin + Vec3::new(2. + 1e-5, 0., 0.);
    assert_eq!(
        scroll_to(&mut scene, &input, forward),
        28 * SETTLED_RAYS + 4 * most,
        "one plane"
    );
    // Two spacings back along x and one up along y: x's first two planes
    // and y's last enter, 8 and 16 probes sharing 4.
    let back = forward + Vec3::new(-4., 2., 0.);
    assert_eq!(
        scroll_to(&mut scene, &input, back),
        12 * SETTLED_RAYS + 20 * most,
        "two axes"
    );
    // A move of the render origin, then one spacing along z in its frame.
    let to = Vec3::new(1000.5, 0., -3000.25);
    scene.move_origin(&device, &queue, to).unwrap();
    input.camera.eye -= to;
    input.camera.view = Mat4::from_translation(-input.camera.eye);
    let moved = scene.dynamic_gi_volume().unwrap().origin;
    assert_eq!(
        scroll_to(&mut scene, &input, moved),
        32 * SETTLED_RAYS,
        "the move itself"
    );
    assert_eq!(
        scroll_to(&mut scene, &input, moved + Vec3::new(0., 0., 2.)),
        16 * SETTLED_RAYS + 16 * most,
        "a scroll after the move"
    );
    // A quarter spacing off the lattice.
    let off = moved + Vec3::new(0., 0., 2.5);
    assert_eq!(
        scroll_to(&mut scene, &input, off),
        32 * most,
        "another placement"
    );
}

// Above a floor that glows over positive x and is black over negative x,
// under a black sky, the volume's light falls off along x. Scrolled one
// spacing along x, each receiver within the probes that stay takes what it
// took before: the sample finds each probe where the scroll stored it, and
// the probes that stay go on tracing from where they lie.
#[test]
fn a_scroll_keeps_each_probe_where_it_lies() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    let environment = uniform_environment(&device, &queue, &mut scene, [0.; 3]);
    // Its top at y = -1, over x from 0 to 10.
    add_cube(
        &device,
        &queue,
        &mut scene,
        Mat4::from_translation(Vec3::new(5., -1.5, 0.)) * Mat4::from_scale(Vec3::new(10., 1., 10.)),
        Mobility::Static,
        |material| {
            material.base = [0., 0., 0., 1.];
            material.metallic = 0.;
            material.emissive = [1.; 3];
        },
    );
    add_cube(
        &device,
        &queue,
        &mut scene,
        Mat4::from_translation(Vec3::new(-5., -1.5, 0.))
            * Mat4::from_scale(Vec3::new(10., 1., 10.)),
        Mobility::Static,
        |material| {
            material.base = [0., 0., 0., 1.];
            material.metallic = 0.;
        },
    );
    let volume = DynamicGiVolume {
        origin: Vec3::new(-4., 0., -1.),
        spacing: Vec3::ONE,
        probes: [8, 2, 3],
    };
    scene.set_dynamic_gi_volume(&device, Some(volume)).unwrap();
    let mut input = input(Vec3::new(0., 0.5, 6.));
    input.environment = Some(environment);
    let settings = settings(DynamicGiQuality::High);
    let mut renderer = Renderer::for_test(&device, &queue, SIZE, &settings);
    render(
        &device,
        &queue,
        &mut renderer,
        &mut scene,
        &input,
        &settings,
        100,
    );
    // Facing the floor, seen from below, in the cells between the probes
    // at x from -3 to 3, which stay.
    let queries: Vec<_> = (-3..3)
        .map(|x| {
            (
                Vec3::new(x as f32 + 0.5, 0.5, 0.2),
                Vec3::NEG_Y,
                Vec3::NEG_Y,
            )
        })
        .collect();
    let before = irradiance_seen(&device, &queue, &renderer, &queries, Receiver::Static);
    // The light falls off along x, so a probe sampled a spacing from where
    // it lies would show.
    assert!(before[5][0] > before[0][0] * 1.5, "{before:?}");
    let scrolled = DynamicGiVolume {
        origin: volume.origin + Vec3::X,
        ..volume
    };
    scene
        .set_dynamic_gi_volume(&device, Some(scrolled))
        .unwrap();
    for frames in [1, 60] {
        render(
            &device,
            &queue,
            &mut renderer,
            &mut scene,
            &input,
            &settings,
            frames,
        );
        let after = irradiance_seen(&device, &queue, &renderer, &queries, Receiver::Static);
        for ((query, before), after) in queries.iter().zip(&before).zip(&after) {
            assert!(
                close(after[0], before[0], 0.01 + before[0] * 0.03) && after[3] == 1.,
                "{query:?} after {frames} frames: {before:?}, {after:?}"
            );
        }
    }
}

// A move of the render origin translates the volume with everything else
// and keeps its probes, as installing the placement it then holds does.
#[test]
fn a_render_origin_move_translates_the_volume_and_keeps_its_probes() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let settings = settings(DynamicGiQuality::High);
    let (mut scene, mut input) = large_scene(&device, &queue);
    let mut renderer = Renderer::for_test(&device, &queue, SIZE, &settings);
    render(
        &device,
        &queue,
        &mut renderer,
        &mut scene,
        &input,
        &settings,
        ramp_frames(),
    );
    let to = Vec3::new(4096.25, -64., -8191.5);
    scene.move_origin(&device, &queue, to).unwrap();
    let moved = scene.dynamic_gi_volume().unwrap();
    assert_eq!(moved.origin, LARGE.origin - to);
    scene.set_dynamic_gi_volume(&device, Some(moved)).unwrap();
    input.camera.eye = EYE - to;
    input.camera.view = Mat4::from_translation(-input.camera.eye);
    render(
        &device,
        &queue,
        &mut renderer,
        &mut scene,
        &input,
        &settings,
        1,
    );
    assert_eq!(shares(&device, &queue, &renderer, -to), [1., 1.]);
    // The probes light the points they lit, in the new frame.
    let answer = irradiance(
        &device,
        &queue,
        &renderer,
        &[(Vec3::new(1.3, 0.4, 12.1) - to, Vec3::Y)],
    );
    assert!(
        close(answer[0][0], 0.5, 0.005) && answer[0][3] == 1.,
        "{answer:?}"
    );
}

/// The sum of a capture's RGB from inside the emissive room of
/// `a_receiver_the_volume_lights_takes_it_in_place_of_the_frames_ambient`,
/// whose white static box the volume lights.
fn capture_sum(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    renderer: &mut Renderer,
    scene: &mut Scene,
    input: &FrameInput,
    settings: &Settings,
) -> f32 {
    let radiance = renderer
        .capture_specular_probe(
            device,
            queue,
            scene,
            input,
            settings,
            Vec3::new(0., 0., 3.),
            64,
        )
        .unwrap();
    let crate::SpecularProbeTexels::Rgba16Float(texels) = radiance.texels else {
        unreachable!("captures return RGBA16F")
    };
    texels
        .chunks_exact(4)
        .flat_map(|texel| texel[..3].to_vec())
        .map(|half| test_support::half(&half.to_le_bytes()))
        .sum()
}

// A probe capture between frames is lit by the probes of the last
// submitted frame, where that frame placed them: a scroll no frame has run,
// and a frame since for it, abandoned, which would have started every probe
// afresh, leave it as it was, and the volume lights the capture's static
// box.
#[test]
fn a_capture_takes_the_submitted_probes_not_an_abandoned_frames() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    add_cube(
        &device,
        &queue,
        &mut scene,
        Mat4::from_scale(Vec3::splat(10.)),
        Mobility::Static,
        |material| {
            material.base = [0., 0., 0., 1.];
            material.metallic = 0.;
            material.emissive = [0.4, 0.2, 0.1];
        },
    );
    add_cube(
        &device,
        &queue,
        &mut scene,
        Mat4::IDENTITY,
        Mobility::Static,
        |material| {
            material.base = [1.; 4];
            material.metallic = 0.;
            material.roughness = 1.;
        },
    );
    scene.set_dynamic_gi_volume(&device, Some(VOLUME)).unwrap();
    let input = input(Vec3::new(0., 0., 3.));
    let settings = settings(DynamicGiQuality::High);
    let mut renderer = Renderer::for_test(&device, &queue, SIZE, &settings);
    render(
        &device,
        &queue,
        &mut renderer,
        &mut scene,
        &input,
        &settings,
        3,
    );
    let submitted = capture_sum(
        &device,
        &queue,
        &mut renderer,
        &mut scene,
        &input,
        &settings,
    );
    let unlit = {
        let off = self::settings(DynamicGiQuality::Off);
        capture_sum(&device, &queue, &mut renderer, &mut scene, &input, &off)
    };
    assert!(submitted > unlit, "{submitted} {unlit}");
    // Scrolled a whole lattice away, every probe of which enters.
    let away = DynamicGiVolume {
        origin: VOLUME.origin + Vec3::X * VOLUME.spacing.x * VOLUME.probes[0] as f32,
        ..VOLUME
    };
    scene.set_dynamic_gi_volume(&device, Some(away)).unwrap();
    let output = crate::view::targets::target(&device, "abandoned", SIZE, gbuffer::COLOR);
    let mut encoder = device.create_command_encoder(&Default::default());
    renderer.render(
        &device,
        &queue,
        &mut encoder,
        &mut scene,
        &input,
        &settings,
        &output,
        None,
    );
    drop(encoder);
    let pending = capture_sum(
        &device,
        &queue,
        &mut renderer,
        &mut scene,
        &input,
        &settings,
    );
    assert_eq!(pending, submitted, "a scroll no frame has run");
    scene.set_dynamic_gi_volume(&device, Some(VOLUME)).unwrap();
    let after = capture_sum(
        &device,
        &queue,
        &mut renderer,
        &mut scene,
        &input,
        &settings,
    );
    assert_eq!(after, submitted, "scrolled back");
}

/// The volume's irradiance at `queries()` after three frames inside a closed
/// room ten metres across, its walls `test_support::cube`'s with
/// `material`'s changes, under an environment four times brighter than any
/// wall, with a point light at its centre that casts no shadow when `lamp`.
fn room_probes(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    material: impl FnOnce(&mut crate::asset::Material),
    lamp: bool,
) -> Vec<[f32; 4]> {
    let mut scene = Scene::new(device, queue);
    let environment = uniform_environment(device, queue, &mut scene, [4.; 3]);
    add_cube(
        device,
        queue,
        &mut scene,
        Mat4::from_scale(Vec3::splat(10.)),
        Mobility::Static,
        material,
    );
    if lamp {
        scene
            .add_light(
                device,
                queue,
                crate::Light {
                    position: Vec3::ZERO,
                    intensity: 50.,
                    range: 30.,
                    casts_shadow: false,
                    ..Default::default()
                },
            )
            .unwrap();
    }
    scene.set_dynamic_gi_volume(device, Some(VOLUME)).unwrap();
    let mut input = input(Vec3::new(0., 0., 4.));
    input.environment = Some(environment);
    let settings = settings(DynamicGiQuality::High);
    let mut renderer = Renderer::for_test(device, queue, SIZE, &settings);
    render(
        device,
        queue,
        &mut renderer,
        &mut scene,
        &input,
        &settings,
        3,
    );
    let answers = irradiance(device, queue, &renderer, &queries());
    assert!(answers.iter().all(|answer| answer[3] == 1.), "{answers:?}");
    answers
}

// A fixture that a scene light stands for keeps its own light out of the
// probes. A probe inside a closed room of walls that give off light sees that
// radiance in every direction, so the volume holds it as its irradiance / PI:
// a lit wall's emission, or an unlit wall's colour. With `emits_into_gi`
// false it holds none of it, and none of the brighter environment outside
// either, so the walls still block the probes' rays. Lit grey walls still
// reflect a lamp's light into the probes as walls that give off none do.
#[test]
fn a_material_that_does_not_emit_into_gi_gives_the_probes_none_of_its_light() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let emission = [0.4, 0.2, 0.1];
    let colour = [0.3, 0.15, 0.6];
    for emits_into_gi in [true, false] {
        let lit = room_probes(
            &device,
            &queue,
            |material| {
                material.base = [0., 0., 0., 1.];
                material.metallic = 0.;
                material.emissive = emission;
                material.emits_into_gi = emits_into_gi;
            },
            false,
        );
        let unlit = room_probes(
            &device,
            &queue,
            |material| {
                material.unlit = true;
                material.base = [colour[0], colour[1], colour[2], 1.];
                material.emits_into_gi = emits_into_gi;
            },
            false,
        );
        for (radiance, answers) in [(emission, lit), (colour, unlit)] {
            for (query, answer) in queries().iter().zip(answers) {
                for channel in 0..3 {
                    let expected = if emits_into_gi { radiance[channel] } else { 0. };
                    assert!(
                        close(answer[channel], expected, radiance[channel] * 0.01),
                        "emits_into_gi {emits_into_gi}, {radiance:?}, {query:?}: {answer:?}"
                    );
                }
            }
        }
    }
    let grey = |material: &mut crate::asset::Material| {
        material.base = [0.5, 0.5, 0.5, 1.];
        material.metallic = 0.;
        material.roughness = 1.;
    };
    let dark = room_probes(&device, &queue, grey, true);
    let kept_out = room_probes(
        &device,
        &queue,
        |material| {
            grey(material);
            material.emissive = emission;
            material.emits_into_gi = false;
        },
        true,
    );
    for ((query, expected), answer) in queries().iter().zip(dark).zip(kept_out) {
        assert!(
            expected[0] > 0.01,
            "{query:?}: the lamp's bounce {expected:?}"
        );
        for channel in 0..3 {
            assert!(
                close(answer[channel], expected[channel], expected[channel] * 0.01),
                "{query:?}: {answer:?} against {expected:?}"
            );
        }
    }
}
