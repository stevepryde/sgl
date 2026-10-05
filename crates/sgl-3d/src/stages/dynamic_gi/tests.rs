//! The dynamic GI volume in real frames, observed through the one sample
//! every view shades with (`dynamic_gi_irradiance`) and the frame's pixels:
//! an open volume holds a uniform environment's radiance and the hemisphere
//! fill's irradiance, and a closed room's emitters' with their bounces,
//! damped, while light outside the room reaches no probe inside but through
//! its shadow opacity or as a light that casts none, and the backs of
//! single-sided walls keep the sky out of a room seen from within and an
//! inside-out box's light in; its share fades over the spacing past its
//! extent; the
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

/// `dynamic_gi_irradiance` at each (position, normal) of `queries` with
/// the last frame's camera group 0: irradiance / PI in rgb, the volume's
/// share in a.
fn irradiance(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    renderer: &Renderer,
    queries: &[(Vec3, Vec3)],
) -> Vec<[f32; 4]> {
    observe(
        device,
        queue,
        renderer,
        queries,
        "dynamic_gi_irradiance(queries[id.x].position.xyz,normalize(queries[id.x].normal.xyz))",
    )
}

/// `expression` at each (position, normal) of `queries`, as `irradiance`
/// observes the sample.
fn observe(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    renderer: &Renderer,
    queries: &[(Vec3, Vec3)],
    expression: &str,
) -> Vec<[f32; 4]> {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("dynamic GI observation"),
        source: wgpu::ShaderSource::Wgsl(
            format!(
                r#"{library}
{INDIRECT}
struct Query {{ position:vec4<f32>, normal:vec4<f32>, answer:vec4<f32> }}
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
        .flat_map(|(position, normal)| {
            [
                position.extend(1.).to_array(),
                normal.extend(0.).to_array(),
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
        .chunks_exact(12)
        .map(|query| std::array::from_fn(|channel| f32::from_bits(query[8 + channel])))
        .collect()
}

/// The fixtures' surfaces for `indirect`.
const INDIRECT: &str = r#"
fn indirect_of(position:vec3<f32>,moving:bool,lightmapped:bool)->vec4<f32> {
 var s:Surface;
 s.position=position;
 s.normal=vec3(0.,1.,0.);
 s.moving=moving;
 s.baked=lightmapped;
 s.front=true;
 for (var face=0u;face<6u;face++) {
  s.baked_irradiance[face]=vec4(.25);
 }
 let indirect=surface_indirect_diffuse(s,s.normal);
 return select(vec4(indirect.baked,0.),indirect.volume,indirect.volume.a>0.);
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

// A probe inside a closed room of black walls that emit `emission` sees that
// radiance in every direction and nothing of the brighter environment
// outside: the volume holds it as its irradiance / PI.
#[test]
fn a_volume_inside_an_emissive_room_holds_its_radiance() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    let emission = [0.4, 0.2, 0.1];
    let environment = uniform_environment(&device, &queue, &mut scene, [4.; 3]);
    let mut room = test_support::cube();
    room.materials[0].base = [0., 0., 0., 1.];
    room.materials[0].metallic = 0.;
    room.materials[0].emissive = emission;
    let ids = scene.add_asset(&device, &queue, room).unwrap();
    scene
        .add_instance(
            &device,
            &queue,
            InstanceState {
                model: ids.model,
                pose: Mat4::from_scale(Vec3::splat(10.)),
                visible: true,
                capture_visible: true,
            },
            Mobility::Static,
        )
        .unwrap();
    scene.set_dynamic_gi_volume(&device, Some(VOLUME)).unwrap();
    let mut input = input(Vec3::new(0., 0., 4.));
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
    for (query, answer) in queries()
        .iter()
        .zip(irradiance(&device, &queue, &renderer, &queries()))
    {
        assert_eq!(answer[3], 1., "{query:?}: the volume's share");
        for channel in 0..3 {
            assert!(
                close(answer[channel], emission[channel], emission[channel] * 0.01),
                "{query:?}: {answer:?}"
            );
        }
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
// room's far side as unoccluded and bring the sky in. The walls reflect
// nothing, so no bounce from them carries light either way.
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
    // Inside, near a wall and facing along it, so the probes beyond the
    // wall lie in front of the receiver; one facing the wall would weigh
    // the probes inside as behind it and every probe at Wicked's floor.
    let queries = [
        (Vec3::new(2.7, 0.3, 0.1), Vec3::Y),
        (Vec3::new(-2.6, 0.2, 1.1), Vec3::Z),
        (Vec3::new(0.3, 2.7, -0.4), Vec3::X),
        (Vec3::new(0.4, -2.7, 0.9), Vec3::NEG_Z),
    ];
    for (query, answer) in queries
        .iter()
        .zip(irradiance(&device, &queue, &renderer, &queries))
    {
        assert_eq!(answer[3], 1., "{query:?}");
        assert!(
            answer[..3].iter().all(|&channel| channel < 0.01),
            "{query:?}: {answer:?}"
        );
    }
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
            observe(device, queue, renderer, &[(position, Vec3::Y)], &expression)
        })
        .collect()
}

// The one determination: a lightmapped receiver keeps its bake and takes
// nothing of the volume while baked lighting is on; a moving one takes the
// volume over its ambient cube where the volume lights it, and its cube
// beyond; a static receiver with no chart takes the volume whether or not
// baked lighting is on.
#[test]
fn the_volume_lights_receivers_below_charts_and_above_ambient_cubes() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    let environment = uniform_environment(&device, &queue, &mut scene, [0.5; 3]);
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
    // blended, take the fill as their fallback, and the probes keep a
    // trace of it; added to the receiver it would bring 8 / PI.
    for channel in 0..3 {
        assert!(
            close(filled[channel], lit[channel], lit[channel] * 0.01),
            "the fill reached a receiver the volume lights: {lit:?}, {filled:?}"
        );
    }
    assert!(without[0] < 1e-3, "{without:?}");
}

/// A volume of 16 by 4 by 16 probes two metres apart.
const LARGE: DynamicGiVolume = DynamicGiVolume {
    origin: Vec3::new(-15., -3., -15.),
    spacing: Vec3::splat(2.),
    probes: [16, 4, 16],
};
/// The camera's position, within `LARGE`.
const EYE: Vec3 = Vec3::new(0., 0., 14.);

/// An open scene of uniform radiance holding `LARGE`, seen from `EYE`.
fn large_scene(device: &wgpu::Device, queue: &wgpu::Queue) -> (Scene, FrameInput) {
    let mut scene = Scene::new(device, queue);
    let environment = uniform_environment(device, queue, &mut scene, [0.5; 3]);
    scene.set_dynamic_gi_volume(device, Some(LARGE)).unwrap();
    let mut input = input(EYE);
    input.environment = Some(environment);
    (scene, input)
}

/// Whether the last frame started the probes afresh: every probe of
/// `LARGE` not yet blended traces the most rays, as Wicked's first frame
/// does, where a probe that has been blended traces as its inconsistency
/// asks, which an unchanging environment keeps below the most.
fn restarted(device: &wgpu::Device, queue: &wgpu::Queue, renderer: &Renderer) -> bool {
    let most = crate::shading::dynamic_gi::MOST_RAYS;
    let rays = renderer.test_dynamic_gi().test_traced_rays(device, queue);
    assert!(rays <= 1024 * most, "{rays}");
    rays == 1024 * most
}

// Once started, the probes continue through a camera cut and a resize,
// which reset the renderer's history but not theirs, and through a frame
// rendered for another placement and abandoned; another placement, another
// scene and a frame that does not run them start them afresh.
#[test]
fn the_probes_restart_only_for_another_placement_or_scene_or_a_frame_without_them() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let settings = settings(DynamicGiQuality::High);
    let (mut scene, input) = large_scene(&device, &queue);
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
    assert!(restarted(&device, &queue, &renderer), "the first frame");
    render(
        &device,
        &queue,
        &mut renderer,
        &mut scene,
        &input,
        &settings,
        1,
    );
    assert!(!restarted(&device, &queue, &renderer), "the next frame");
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
    assert!(!restarted(&device, &queue, &renderer), "a renderer reset");
    // A frame for another placement, abandoned.
    let moved = DynamicGiVolume {
        origin: LARGE.origin + Vec3::splat(0.5),
        ..LARGE
    };
    scene.set_dynamic_gi_volume(&device, Some(moved)).unwrap();
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
    assert!(!restarted(&device, &queue, &renderer), "an abandoned frame");
    // Another placement.
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
    assert!(restarted(&device, &queue, &renderer), "another placement");
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
    assert!(
        restarted(&device, &queue, &renderer),
        "a frame without them"
    );
    // Another scene with the same placement.
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
    assert!(restarted(&device, &queue, &renderer), "another scene");
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
// them, at the volume's extent, each frame's hits reflect the volume's last
// frame, damped by 0.95,
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
    scene.set_dynamic_gi_volume(&device, Some(VOLUME)).unwrap();
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
    for (query, answer) in queries()
        .iter()
        .zip(irradiance(&device, &queue, &renderer, &queries()))
    {
        assert!(
            answer[0] > emission * 1.5 && answer[0] < bound,
            "{query:?}: {answer:?}"
        );
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
        1,
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
    assert!(!restarted(&device, &queue, &renderer));
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
// submitted frame: a frame since for another placement, abandoned, leaves
// it as it was, and the volume lights the capture's static box.
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
    let moved = DynamicGiVolume {
        origin: VOLUME.origin + Vec3::splat(0.5),
        ..VOLUME
    };
    scene.set_dynamic_gi_volume(&device, Some(moved)).unwrap();
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
    scene.set_dynamic_gi_volume(&device, Some(VOLUME)).unwrap();
    let after = capture_sum(
        &device,
        &queue,
        &mut renderer,
        &mut scene,
        &input,
        &settings,
    );
    assert_eq!(after, submitted);
}
