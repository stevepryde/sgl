//! Observe actual depth, uploaded records, ray hits and frames across scene
//! content edits.
use crate::asset::Asset;
use crate::content::identity::Identity;
use crate::renderer::Renderer;
use crate::settings::{RenderPreset, Settings};
use crate::view::pipelines::GeometryPass;
use crate::*;
use glam::camera;
use glam::{Mat4, Vec3};

fn plane(z: f32, slope: [f32; 2], half: f32, reversed: bool) -> asset::CpuMesh {
    asset::CpuMesh {
        vertices: [(-half, -half), (half, -half), (half, half), (-half, half)]
            .map(|(x, y)| asset::Vertex {
                tangent: [0.0; 4],
                lightmap_bounds: [0., 0., 1., 1.],
                lightmap_uv: [0.; 2],
                position: [x, y, z + slope[0] * x + slope[1] * y],
                normal: Vec3::new(-slope[0], -slope[1], 1.).normalize().to_array(),
                uv: [0.; 2],
                color: [1.; 4],
            })
            .to_vec(),
        indices: if reversed {
            vec![0, 2, 1, 0, 3, 2]
        } else {
            vec![0, 1, 2, 0, 2, 3]
        },
        material: 0,
        deformation: Default::default(),
    }
}

/// A one-mesh asset of `mesh` with the fixture material.
fn asset_of(mesh: asset::CpuMesh) -> Asset {
    let mut asset = test_support::cube();
    asset.meshes = vec![mesh];
    asset
}

fn state(model: ModelId, pose: Mat4) -> InstanceState {
    InstanceState {
        model,
        pose,
        visible: true,
        capture_visible: true,
    }
}

fn mapped(device: &wgpu::Device, buffer: &wgpu::Buffer) -> Vec<f32> {
    let (send, receive) = std::sync::mpsc::channel();
    buffer.map_async(wgpu::MapMode::Read, .., move |result| {
        send.send(result).unwrap()
    });
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    receive.recv().unwrap().unwrap();
    let bytes = buffer.get_mapped_range(..);
    bytemuck::cast_slice(&bytes).to_vec()
}

/// Runs WGSL `body` (an `observe` entry point writing `result`, an array of
/// `count` vec4s) with `group1` at group 1 and returns what it wrote.
fn dispatch(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    library: String,
    body: &str,
    count: usize,
    binding: impl FnOnce(&wgpu::ComputePipeline) -> Option<wgpu::BindGroup>,
) -> Vec<[f32; 4]> {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("scene content observation"),
        source: wgpu::ShaderSource::Wgsl(format!("{library}\n{body}").into()),
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: None,
        layout: None,
        module: &shader,
        entry_point: Some("observe"),
        compilation_options: Default::default(),
        cache: None,
    });
    let size = (count * 16) as u64;
    let output = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let result = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: output.as_entire_binding(),
        }],
    });
    let group1 = binding(&pipeline);
    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_compute_pass(&Default::default());
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &result, &[]);
        if let Some(group1) = &group1 {
            pass.set_bind_group(1, group1, &[]);
        }
        pass.dispatch_workgroups(1, 1, 1);
    }
    encoder.copy_buffer_to_buffer(&output, 0, &readback, 0, size);
    queue.submit([encoder.finish()]);
    mapped(device, &readback)
        .chunks_exact(4)
        .map(|v| v.try_into().unwrap())
        .collect()
}

/// An instance's uploaded record as vertex shading reads it: the world
/// position of its model origin, the motion of that point (current minus
/// previous) and the +X entry of its ambient cube.
struct Record {
    position: [f32; 3],
    motion: [f32; 3],
    cube: [f32; 3],
}

fn uploaded_record(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    scene: &Scene,
    id: InstanceId,
) -> Record {
    // The record vertex shading reads at the instance's index, applying the
    // two uploaded poses to the same point. This is not a copy of
    // history-selection logic.
    let values = dispatch(
        device,
        queue,
        crate::shading::compose(&[&crate::shading::UNIFORMS]),
        &r#"
@group(0) @binding(0) var<storage,read_write> result:array<vec4<f32>,3>;
@group(1) @binding(0) var<storage,read> objects:array<Object>;
@compute @workgroup_size(1) fn observe() {
 let object=objects[INDEX];
 let p=vec4(0.,0.,0.,1.);
 result[0]=object.model*p;
 result[1]=object.model*p-object.previous_model*p;
 result[2]=object.baked_irradiance[0];
}
"#
        .replace("INDEX", &format!("{}u", id.index())),
        3,
        |pipeline| {
            Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &pipeline.get_bind_group_layout(1),
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: scene.object_records(),
                }],
            }))
        },
    );
    let xyz = |v: [f32; 4]| [v[0], v[1], v[2]];
    Record {
        position: xyz(values[0]),
        motion: xyz(values[1]),
        cube: xyz(values[2]),
    }
}

/// The scene's nearest hit of the ray from `origin` down -Z, after updating
/// its rays: [distance, the hit material's base red, its lightmap
/// eligibility, the instance's index], or zeros for a miss.
fn trace(device: &wgpu::Device, queue: &wgpu::Queue, scene: &mut Scene, origin: Vec3) -> [f32; 4] {
    let body = format!(
        r#"
@group(0) @binding(0) var<storage,read_write> result:array<vec4<f32>,1>;
@compute @workgroup_size(1) fn observe() {{
 let origin=vec3<f32>({},{},{});
 let direction=vec3(0.,0.,-1.);
 let hit=scene_decode_hit(scene_trace_nearest(SceneRay(vec4(origin,0.),vec4(direction,100.))),origin,direction);
 if hit.hit {{
  let material=scene_material(hit.material_word);
  result[0]=vec4(hit.distance,material.values.base.x,f32(material.baked),f32(hit.instance_id));
 }}
}}
"#,
        origin.x, origin.y, origin.z
    );
    ray_dispatch(device, queue, scene, &body, true)
}

/// Whether the segment from `origin` down -Z to `length` metres meets none
/// of the scene's surfaces (`scene_segment_visible`, the any-hit traversal),
/// after updating its rays.
fn segment_visible(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    scene: &mut Scene,
    origin: Vec3,
    length: f32,
) -> bool {
    let body = format!(
        r#"
@group(0) @binding(0) var<storage,read_write> result:array<vec4<f32>,1>;
@compute @workgroup_size(1) fn observe() {{
 let visible=scene_segment_visible(vec3<f32>({},{},{}),vec3(0.,0.,-1.),0.,{length:?});
 result[0]=vec4(select(0.,1.,visible));
}}
"#,
        origin.x, origin.y, origin.z
    );
    ray_dispatch(device, queue, scene, &body, false)[0] == 1.
}

/// `body`'s first result, run over the scene's ray buffers after updating
/// them for a traced frame, and over its object records when it `decodes`
/// a hit.
fn ray_dispatch(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    scene: &mut Scene,
    body: &str,
    decodes: bool,
) -> [f32; 4] {
    scene.update_rays(device, queue, 0);
    let scene = &*scene;
    dispatch(
        device,
        queue,
        crate::shading::compose(&[&crate::shading::SCENE_RAYS_PORTABLE]),
        body,
        1,
        |pipeline| {
            // Group 1 as the scene binds it, with an auto layout, which
            // holds the object records only where the body reads them.
            let entries = [
                (
                    crate::shading::bind::group1::OBJECTS,
                    scene.object_records(),
                ),
                (
                    crate::shading::bind::group1::SCENE_SOURCE,
                    scene.rays.source().as_entire_binding(),
                ),
                (
                    crate::shading::bind::group1::SCENE_INSTANCES,
                    scene.ray_instances.buffer().as_entire_binding(),
                ),
            ]
            .into_iter()
            .skip(usize::from(!decodes))
            .map(|(binding, resource)| wgpu::BindGroupEntry { binding, resource })
            .collect::<Vec<_>>();
            Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &pipeline.get_bind_group_layout(1),
                entries: &entries,
            }))
        },
    )[0]
}

// Plausible defects: rays hit a masked material's cut-out texels (no
// any-hit test, or one reading the base alpha at another texel, or ignoring
// the cutoff), miss its opaque texels, or hit blended surfaces, which write
// no depth. The oracle is geometric: rays down -Z cross a plane whose base
// map is cut out over its left half (U < 0.5), or a blended plane, in front
// of an opaque floor 4 m away; each traversal, nearest and any-hit, meets
// the masked plane only on its opaque half.
#[test]
fn rays_pass_through_cut_out_texels_and_blended_surfaces() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    let mut floor = asset_of(plane(-4., [0.; 2], 8., false));
    floor.materials[0].base = [0.25, 0., 0., 1.];
    let mut masked = plane(-1., [0.; 2], 1., false);
    for (vertex, uv) in masked
        .vertices
        .iter_mut()
        .zip([[0., 1.], [1., 1.], [1., 0.], [0., 0.]])
    {
        vertex.uv = uv;
    }
    let mut masked = test_support::masked(asset_of(masked), 0.5);
    masked.materials[0].base = [0.5, 0., 0., 1.];
    let mut blended = asset_of(plane(-2., [0.; 2], 1., false));
    blended.materials[0].base = [0.75, 0., 0., 0.5];
    blended.materials[0].alpha = AlphaMode::Blend {
        receives_screen_space_reflections: false,
    };
    for (asset, x) in [(floor, 0.), (masked, 0.), (blended, 4.)] {
        let model = scene.add_asset(&device, &queue, asset).unwrap().model;
        let at = state(model, Mat4::from_translation(Vec3::X * x));
        scene
            .add_instance(&device, &queue, at, Mobility::Static)
            .unwrap();
    }
    // Through the masked plane's cut-out half (U = 0.25), its opaque half
    // (U = 0.75), and the blended plane.
    for (x, distance, red) in [(-0.5, 4., 0.25), (0.5, 1., 0.5), (4., 4., 0.25)] {
        let origin = Vec3::X * x;
        let hit = trace(&device, &queue, &mut scene, origin);
        assert!(
            (hit[0] - distance).abs() < 1e-5 && hit[1] == red,
            "the nearest hit from x = {x} is {hit:?}, expected {distance} m on base red {red}"
        );
        assert_eq!(
            segment_visible(&device, &queue, &mut scene, origin, 3.),
            distance > 3.,
            "the any-hit visibility of 3 m from x = {x}"
        );
    }
}

/// A directional light along the casters' shadow view, which
/// `Renderer::set_test_cascade` sets.
fn caster_light() -> DirectionalLight {
    DirectionalLight {
        direction: Vec3::NEG_Z,
        color: [1.; 3],
        illuminance: 1.,
        shadow: None,
        ..Default::default()
    }
}

#[test]
fn directional_casters_respect_enabled_groups_and_explicit_policy() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    pollster::block_on(async {
        let depth = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("authored plane depth"),
            size: wgpu::Extent3d {
                width: 64,
                height: 64,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let depth_view = depth.create_view(&Default::default());
        let mut renderer = Renderer::for_test(
            &device,
            &queue,
            [64, 64],
            &Settings {
                preset: RenderPreset::Low,
                ..Settings::default()
            },
        );
        for mobility in [Mobility::Static, Mobility::Moving] {
            for (group, mask, casts, expected_caster) in [
                (3, 3, true, true),
                (3, 1, true, false),
                (3, 0, true, false),
                (0, 0, true, true),
                (3, 3, false, false),
            ] {
                let mut scene = Scene::new(&device, &queue);
                let mut asset = asset_of(plane(0.5, [0.15, 0.2], 0.75, false));
                asset.materials[0].visibility_group = group;
                asset.materials[0].casts_directional_shadow = casts;
                let model = scene.add_asset(&device, &queue, asset).unwrap().model;
                // Hidden from the main camera: capture visibility alone
                // makes a caster.
                let hidden = InstanceState {
                    visible: false,
                    ..state(model, Mat4::IDENTITY)
                };
                scene
                    .add_instance(&device, &queue, hidden, mobility)
                    .unwrap();
                let mut frame = FrameInput::new(Camera {
                    // The caster is outside this main camera, but inside the
                    // independent light frustum whose exact plane depth is observed.
                    view: Mat4::from_translation(Vec3::X * 100.),
                    projection: crate::perspective(1., 1., 0.1),
                    eye: Vec3::ZERO,
                });
                frame.directional_lights[0] = Some(caster_light());
                frame.visibility_mask = mask;
                let prepared = renderer.prepare_test_frame(
                    &device,
                    &queue,
                    &mut scene,
                    &frame,
                    &Settings::default(),
                );
                renderer.set_test_cascade((&device, &queue), &scene, &prepared, Mat4::IDENTITY);
                let mut encoder = device.create_command_encoder(&Default::default());
                renderer.encode_test_shadows(
                    &device,
                    &queue,
                    &mut encoder,
                    &scene,
                    &prepared,
                    Some(&depth_view),
                );
                queue.submit([encoder.finish()]);
                let bytes = test_support::read(&device, &queue, &depth, 4);
                let observed: &[f32] = bytemuck::cast_slice(&bytes);
                for y in 12..52 {
                    for x in 12..52 {
                        let px = 2. * (x as f32 + 0.5) / 64. - 1.;
                        let py = 1. - 2. * (y as f32 + 0.5) / 64.;
                        let expected = if expected_caster {
                            0.5 + 0.15 * px + 0.2 * py
                        } else {
                            0.
                        };
                        assert!(
                            (observed[y * 64 + x] - expected).abs() < 0.000002,
                            "{mobility:?} group={group} mask={mask} casts={casts}: wrong depth at {x},{y}"
                        );
                    }
                }
            }
        }
    });
}

// The directional caster pipeline must cull as the camera does, so only a
// single-sided plane's front faces cast (Bevy's shadow pipelines keep the
// material's cull mode), and store the authored plane depth without raster
// bias: a sloped plane exposes slope bias; reversed winding and single or
// double sides expose culling errors, for a static and a moving instance,
// with the device's unclipped depth and with its emulation in the shader.
#[test]
fn directional_casters_cast_front_faces_at_unbiased_depth() {
    for (path, without) in [
        ("unclipped depth", wgpu::Features::empty()),
        (
            "emulated unclipped depth",
            wgpu::Features::DEPTH_CLIP_CONTROL,
        ),
    ] {
        let Some((device, queue)) = test_support::device_without(without) else {
            return;
        };
        let native = device
            .features()
            .contains(wgpu::Features::DEPTH_CLIP_CONTROL);
        if native != without.is_empty() {
            eprintln!("skipping {path}: the adapter has no DEPTH_CLIP_CONTROL");
            continue;
        }
        const SIZE: u32 = 64;
        let depth = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("analytic directional caster depth"),
            size: wgpu::Extent3d {
                width: SIZE,
                height: SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let depth_view = depth.create_view(&Default::default());
        let mut renderer = Renderer::for_test(&device, &queue, [SIZE; 2], &Settings::default());
        let mut frame = FrameInput::new(Camera {
            view: Mat4::from_translation(Vec3::X * 100.),
            projection: crate::perspective(1., 1., 0.1),
            eye: Vec3::ZERO,
        });
        frame.directional_lights[0] = Some(caster_light());
        for mobility in [Mobility::Static, Mobility::Moving] {
            for (label, double_sided, reversed, should_cast) in [
                ("single facing light", false, false, true),
                ("single facing away", false, true, false),
                ("double facing light", true, false, true),
                ("double facing away", true, true, true),
            ] {
                let mut asset = asset_of(plane(0.5, [0.15, 0.2], 0.75, reversed));
                asset.materials[0].double_sided = double_sided;
                let mut scene = Scene::new(&device, &queue);
                let model = scene.add_asset(&device, &queue, asset).unwrap().model;
                scene
                    .add_instance(&device, &queue, state(model, Mat4::IDENTITY), mobility)
                    .unwrap();
                let prepared = renderer.prepare_test_frame(
                    &device,
                    &queue,
                    &mut scene,
                    &frame,
                    &Settings::default(),
                );
                renderer.set_test_cascade((&device, &queue), &scene, &prepared, Mat4::IDENTITY);
                let mut encoder = device.create_command_encoder(&Default::default());
                renderer.encode_test_shadows(
                    &device,
                    &queue,
                    &mut encoder,
                    &scene,
                    &prepared,
                    Some(&depth_view),
                );
                queue.submit([encoder.finish()]);
                let bytes = test_support::read(&device, &queue, &depth, 4);
                let observed: &[f32] = bytemuck::cast_slice(&bytes);
                // Clear of the edges: raster coverage, stored depth and the
                // instance's draw bindings.
                for y in 12..52 {
                    for x in 12..52 {
                        let px = 2. * (x as f32 + 0.5) / SIZE as f32 - 1.;
                        let py = 1. - 2. * (y as f32 + 0.5) / SIZE as f32;
                        let expected = if should_cast {
                            0.5 + 0.15 * px + 0.2 * py
                        } else {
                            0.
                        };
                        let depth = observed[(y * SIZE + x) as usize];
                        assert!(
                            depth.is_finite() && (depth - expected).abs() < 0.000002,
                            "{path} {mobility:?} {label} pixel({x},{y}): depth {depth}, expected {expected}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn instance_motion_uses_last_submitted_model_after_abandoned_updates() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    pollster::block_on(async {
        let mut scene = Scene::new(&device, &queue);
        let [a, b] = [(); 2].map(|()| {
            scene
                .add_asset(&device, &queue, test_support::cube())
                .unwrap()
                .model
        });
        let at = |model, x: f32| state(model, Mat4::from_translation(Vec3::X * x));
        let id = scene
            .add_instance(&device, &queue, at(a, 1.), Mobility::Moving)
            .unwrap();
        queue.submit([]);
        scene.finish_frame();
        scene.set_instance(&queue, id, at(b, 3.)).unwrap();
        // This model-B frame is abandoned; a second update must still compare to A.
        scene.set_instance(&queue, id, at(b, 5.)).unwrap();
        assert_eq!(
            uploaded_record(&device, &queue, &scene, id).motion,
            [0., 0., 0.],
            "model replacement must leave no predecessor motion, even after an abandoned matching update"
        );
        // Returning to A without submitting B must recover A's last rendered pose.
        scene.set_instance(&queue, id, at(a, 7.)).unwrap();
        assert_eq!(
            uploaded_record(&device, &queue, &scene, id).motion,
            [6., 0., 0.]
        );
        scene.finish_frame();
        scene.set_instance(&queue, id, at(a, 9.)).unwrap();
        assert_eq!(
            uploaded_record(&device, &queue, &scene, id).motion,
            [2., 0., 0.]
        );
    });
}

// Plausible defects: a static instance's record carries its previous pose;
// a moving one's keeps a stale previous pose when it is not posed again after
// a submitted frame, or measures motion across a frame that did not show it.
// The oracle is the vertex shader's own record: current minus previous pose
// applied to the model origin.
#[test]
fn static_instances_write_no_motion_and_moving_ones_measure_from_the_last_submitted_frame() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    let model = scene
        .add_asset(&device, &queue, test_support::cube())
        .unwrap()
        .model;
    let at = |x: f32| state(model, Mat4::from_translation(Vec3::X * x));
    let fixed = scene
        .add_instance(&device, &queue, at(1.), Mobility::Static)
        .unwrap();
    let moving = scene
        .add_instance(&device, &queue, at(1.), Mobility::Moving)
        .unwrap();
    scene.finish_frame();
    for id in [fixed, moving] {
        scene.set_instance(&queue, id, at(4.)).unwrap();
    }
    let motion = |scene: &Scene, id| uploaded_record(&device, &queue, scene, id).motion;
    assert_eq!(motion(&scene, fixed), [0.; 3], "a static instance moved");
    // A static instance takes baked light from charts, never a cube.
    assert!(matches!(
        scene.set_instance_baked_irradiance(
            &queue,
            fixed,
            static_lighting::AmbientCube {
                irradiance: [[1.; 3]; 6]
            }
        ),
        Err(SceneError::StaticInstance)
    ));
    assert_eq!(
        uploaded_record(&device, &queue, &scene, fixed).cube,
        [0.; 3]
    );
    assert_eq!(motion(&scene, moving), [3., 0., 0.]);
    // Submitted, then not posed again: the next frame shows it still.
    scene.finish_frame();
    scene.prepare_frame(&device, &queue, Vec3::ZERO);
    assert_eq!(
        motion(&scene, moving),
        [0.; 3],
        "a moving instance kept motion from before the last submitted frame"
    );
    // Hidden in the last submitted frame: no motion from that frame's pose.
    scene
        .set_instance(
            &queue,
            moving,
            InstanceState {
                visible: false,
                ..at(6.)
            },
        )
        .unwrap();
    scene.finish_frame();
    scene.set_instance(&queue, moving, at(8.)).unwrap();
    assert_eq!(motion(&scene, moving), [0.; 3]);
}

// Plausible defects: an operation accepts an identity whose content was
// removed, or one another scene issued at the same index, and writes over the
// content that index holds now. The oracles are the refusal and the uploaded
// record of the content that holds the index.
#[test]
fn ended_and_foreign_identities_are_refused_and_change_nothing() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    let mut other = Scene::new(&device, &queue);
    let ids = scene
        .add_asset(&device, &queue, test_support::cube())
        .unwrap();
    let foreign = other
        .add_asset(&device, &queue, test_support::cube())
        .unwrap();
    let at = |model, x: f32| state(model, Mat4::from_translation(Vec3::X * x));
    let ended = scene
        .add_instance(&device, &queue, at(ids.model, 1.), Mobility::Moving)
        .unwrap();
    scene.remove_instance(ended).unwrap();
    let current = scene
        .add_instance(&device, &queue, at(ids.model, 2.), Mobility::Moving)
        .unwrap();
    let foreign_instance = other
        .add_instance(&device, &queue, at(foreign.model, 3.), Mobility::Moving)
        .unwrap();
    for id in [ended, foreign_instance] {
        assert!(matches!(
            scene.set_instance(&queue, id, at(ids.model, 9.)),
            Err(SceneError::UnknownInstance)
        ));
        assert!(matches!(
            scene.set_instance_baked_irradiance(
                &queue,
                id,
                static_lighting::AmbientCube {
                    irradiance: [[5.; 3]; 6]
                }
            ),
            Err(SceneError::UnknownInstance)
        ));
        assert!(matches!(
            scene.remove_instance(id),
            Err(SceneError::UnknownInstance)
        ));
    }
    let record = uploaded_record(&device, &queue, &scene, current);
    assert_eq!(record.position, [2., 0., 0.]);
    assert_eq!(record.cube, [0.; 3]);
    assert_eq!(
        scene.instance(current).unwrap().pose,
        at(ids.model, 2.).pose
    );
    // Another scene's model and material at the same indices.
    assert!(matches!(
        scene.add_instance(&device, &queue, at(foreign.model, 0.), Mobility::Static),
        Err(SceneError::UnknownModel)
    ));
    let values = scene.material(ids.materials[0]).unwrap();
    assert!(matches!(
        scene.set_material(&queue, foreign.materials[0], values),
        Err(SceneError::UnknownMaterial)
    ));
    assert!(matches!(
        scene.remove_model(foreign.model),
        Err(SceneError::UnknownModel)
    ));
}

// Plausible defects: content added at a removed index shows the removed
// content's geometry, ray record, lightmap eligibility, motion or ambient
// cube. The oracles: a ray's hit distance and material record, and the
// uploaded object record.
#[test]
fn reused_indices_show_no_earlier_geometry_record_or_motion() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    let mut near = asset_of(plane(-2., [0.; 2], 1., false));
    near.materials[0].base = [0.25, 0., 0., 1.];
    near.materials[0].double_sided = true;
    let near = scene.add_asset(&device, &queue, near).unwrap();
    scene
        .set_lightmap(
            &device,
            &queue,
            &static_lighting::Lightmap {
                size: [1, 1],
                uv_scale_offset: [1., 1., 0., 0.],
                irradiance: vec![[1.; 3]],
                directionality: vec![],
            },
            &near.materials,
        )
        .unwrap();
    let at = |model, x: f32| state(model, Mat4::from_translation(Vec3::X * x));
    let first = scene
        .add_instance(&device, &queue, at(near.model, 0.), Mobility::Moving)
        .unwrap();
    scene
        .set_instance_baked_irradiance(
            &queue,
            first,
            static_lighting::AmbientCube {
                irradiance: [[3.; 3]; 6],
            },
        )
        .unwrap();
    scene.finish_frame();
    assert_eq!(
        trace(&device, &queue, &mut scene, Vec3::ZERO),
        [2., 0.25, 1., 0.]
    );
    scene.remove_instance(first).unwrap();
    scene.remove_model(near.model).unwrap();
    scene.remove_material(near.materials[0]).unwrap();
    let mut far = asset_of(plane(-5., [0.; 2], 1., false));
    far.materials[0].base = [0.75, 0., 0., 1.];
    far.materials[0].double_sided = true;
    let far = scene.add_asset(&device, &queue, far).unwrap();
    assert_eq!(far.model.index(), near.model.index());
    assert_eq!(far.materials[0].index(), near.materials[0].index());
    let second = scene
        .add_instance(&device, &queue, at(far.model, 4.), Mobility::Moving)
        .unwrap();
    assert_eq!(second.index(), first.index());
    assert_ne!(second, first);
    let record = uploaded_record(&device, &queue, &scene, second);
    assert_eq!(
        record.motion, [0.; 3],
        "the new instance moved from the old one's pose"
    );
    assert_eq!(
        record.cube, [0.; 3],
        "the new instance took the old one's ambient cube"
    );
    assert_eq!(
        trace(&device, &queue, &mut scene, Vec3::X * 4.),
        [5., 0.75, 0., 0.],
        "the reused index hit the old geometry or material record"
    );
}

// Plausible defects: a masked material's cutoff outside glTF's bound on
// `alphaCutoff` (finite, at least zero) accepted when it is added or edited,
// so that a NaN cutoff silently cuts out nothing, or a refused edit applied
// anyway. The oracles are that bound and the refusal's contract: nothing
// changes.
#[test]
fn alpha_cutoffs_outside_gltfs_bound_are_refused() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    for cutoff in [f32::NAN, -0.1, f32::INFINITY] {
        let mut asset = test_support::cube();
        asset.materials[0].alpha = AlphaMode::Mask { cutoff };
        assert!(
            matches!(
                scene.add_asset(&device, &queue, asset),
                Err(SceneError::InvalidAlphaCutoff)
            ),
            "added cutoff {cutoff}"
        );
    }
    let masked = test_support::masked(test_support::cube(), 0.5);
    let material = scene.add_asset(&device, &queue, masked).unwrap().materials[0];
    let before = scene.material(material).unwrap();
    for cutoff in [f32::NAN, -0.1, f32::INFINITY] {
        let mut values = before;
        values.alpha = AlphaMode::Mask { cutoff };
        assert!(
            matches!(
                scene.set_material(&queue, material, values),
                Err(SceneError::InvalidAlphaCutoff)
            ),
            "edited cutoff {cutoff}"
        );
        assert_eq!(scene.material(material).unwrap(), before);
    }
    let mut values = before;
    values.alpha = AlphaMode::Mask { cutoff: 0. };
    scene.set_material(&queue, material, values).unwrap();
}

// Plausible defects: content another content uses is removed or replaced
// under it, or replacing a model keeps levels of detail that name it. The
// oracles are the refusals the spec requires, and the removals they allow once
// the uses end.
#[test]
fn content_in_use_is_neither_removed_nor_replaced() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    let base = scene
        .add_asset(&device, &queue, test_support::cube())
        .unwrap();
    let alternative = scene
        .add_asset(&device, &queue, test_support::cube())
        .unwrap();
    let instance = scene
        .add_instance(
            &device,
            &queue,
            state(base.model, Mat4::IDENTITY),
            Mobility::Static,
        )
        .unwrap();
    let geometry = || {
        vec![ModelMesh {
            vertices: test_support::cube().meshes[0].vertices.clone(),
            indices: test_support::cube().meshes[0].indices.clone(),
            material: base.materials[0],
            deformation: Default::default(),
        }]
    };
    assert!(matches!(
        scene.remove_material(base.materials[0]),
        Err(SceneError::MaterialInUse)
    ));
    assert!(matches!(
        scene.remove_model(base.model),
        Err(SceneError::ModelInUse)
    ));
    scene
        .set_mesh_lods(
            base.model,
            0,
            vec![lod::MeshLod {
                model: alternative.model,
                mesh: 0,
                max_error: 0.,
            }],
        )
        .unwrap();
    assert!(matches!(
        scene.remove_model(alternative.model),
        Err(SceneError::ModelInUse)
    ));
    // A model draws all its meshes, so none is another's level of detail.
    assert!(matches!(
        scene.set_mesh_lods(
            alternative.model,
            0,
            vec![lod::MeshLod {
                model: alternative.model,
                mesh: 0,
                max_error: 0.,
            }],
        ),
        Err(SceneError::LodInSameModel)
    ));
    assert!(matches!(
        scene.set_model(&device, &queue, alternative.model, geometry()),
        Err(SceneError::ModelInUse)
    ));
    // Replacing the base clears its levels of detail.
    scene
        .set_model(&device, &queue, base.model, geometry())
        .unwrap();
    scene.remove_model(alternative.model).unwrap();
    scene.remove_material(alternative.materials[0]).unwrap();
    scene.remove_instance(instance).unwrap();
    scene.remove_model(base.model).unwrap();
    scene.remove_material(base.materials[0]).unwrap();
}

// Plausible defects: growing the object buffer loses existing records, or
// growing the ray source loses or moves existing geometry, or group 1 keeps
// binding the replaced buffers. The oracles: the first instance's source
// identity and depth in a rendered frame, and its ray hit, before and after
// enough content to grow every buffer several times.
#[test]
fn content_survives_buffer_growth() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    const SIZE: [u32; 2] = [16, 16];
    let mut scene = Scene::new(&device, &queue);
    let mut quad = asset_of(plane(-2., [0.; 2], 4., false));
    quad.materials[0].double_sided = true;
    let first = scene.add_asset(&device, &queue, quad.clone()).unwrap();
    let instance = scene
        .add_instance(
            &device,
            &queue,
            state(first.model, Mat4::IDENTITY),
            Mobility::Static,
        )
        .unwrap();
    // A model without meshes is content too: it draws and traces nothing.
    let empty = scene.add_asset(&device, &queue, asset::empty()).unwrap();
    scene
        .add_instance(
            &device,
            &queue,
            state(empty.model, Mat4::IDENTITY),
            Mobility::Moving,
        )
        .unwrap();
    let settings = Settings {
        antialiasing: settings::Antialiasing::Off,
        ..Settings::default()
    };
    let mut renderer = Renderer::for_test(&device, &queue, SIZE, &settings);
    let input = FrameInput::new(Camera {
        view: Mat4::IDENTITY,
        projection: crate::perspective(1., 1., 0.1),
        eye: Vec3::ZERO,
    });
    let mut observe = |scene: &mut Scene| {
        let mut encoder = device.create_command_encoder(&Default::default());
        let output = crate::view::targets::target(
            &device,
            "growth output",
            SIZE,
            crate::shading::gbuffer::COLOR,
        );
        renderer.render(
            &device,
            &queue,
            &mut encoder,
            scene,
            &input,
            &settings,
            &output,
            None,
        );
        queue.submit([encoder.finish()]);
        renderer.finish_frame(scene);
        let targets = renderer.targets();
        (
            test_support::read(&device, &queue, targets.source_id.texture(), 8),
            test_support::read(&device, &queue, targets.depth.texture(), 4),
            trace(&device, &queue, scene, Vec3::ZERO),
        )
    };
    let before = observe(&mut scene);
    let owner = u32::from_le_bytes(before.0[..4].try_into().unwrap());
    assert_eq!(owner, diagnostics::source_id(instance));
    assert_eq!(before.2, [2., 0.78, 0., 0.]);
    // Hidden content: more records, textures, materials and geometry.
    let mut textured = quad.clone();
    textured.images = vec![crate::asset::Image::Rgba8(image::RgbaImage::from_pixel(
        64,
        64,
        image::Rgba([9; 4]),
    ))];
    textured.materials[0].base_texture = Some(0);
    for _ in 0..40 {
        let ids = scene.add_asset(&device, &queue, textured.clone()).unwrap();
        for _ in 0..8 {
            scene
                .add_instance(
                    &device,
                    &queue,
                    InstanceState {
                        visible: false,
                        capture_visible: false,
                        ..state(ids.model, Mat4::from_translation(Vec3::Z * 50.))
                    },
                    Mobility::Moving,
                )
                .unwrap();
        }
    }
    assert!(
        before == observe(&mut scene),
        "content changed after growth"
    );
}

// Plausible defects: the renderer keeps group 0 built for the first frame's
// environment, or binds a removed environment's textures. The oracle is the
// sky each frame draws from its environment's panorama: red, then blue,
// then the black stand-in.
#[test]
fn each_frame_draws_its_environment_and_an_ended_one_is_black() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    const SIZE: [u32; 2] = [8, 8];
    let mut scene = Scene::new(&device, &queue);
    let red = scene
        .add_environment(
            &device,
            &queue,
            &test_support::environment([255, 0, 0, 255], &[0]),
        )
        .unwrap();
    let blue = scene
        .add_environment(
            &device,
            &queue,
            &test_support::environment([0, 0, 255, 255], &[0]),
        )
        .unwrap();
    let settings = Settings::default();
    let mut renderer = Renderer::for_test(&device, &queue, SIZE, &settings);
    let mut input = FrameInput::new(Camera {
        view: Mat4::IDENTITY,
        projection: crate::perspective(1., 1., 0.1),
        eye: Vec3::ZERO,
    });
    let mut sky = |scene: &mut Scene, environment| {
        input.environment = environment;
        let frame = renderer.prepare_test_frame(&device, &queue, scene, &input, &settings);
        let mut encoder = device.create_command_encoder(&Default::default());
        renderer.encode_test_opaque(&device, &queue, &mut encoder, scene, &mut { frame }, false);
        queue.submit([encoder.finish()]);
        let pixels = test_support::read(&device, &queue, renderer.targets().color.texture(), 8);
        [0, 2, 4].map(|channel| test_support::half(&pixels[channel..]))
    };
    let [r, _, b] = sky(&mut scene, Some(red));
    assert!(r > 0.5 && b == 0., "red sky: {r} {b}");
    let [r, _, b] = sky(&mut scene, Some(blue));
    assert!(r == 0. && b > 0.5, "blue sky: {r} {b}");
    scene.remove_environment(blue).unwrap();
    assert_eq!(sky(&mut scene, Some(blue)), [0.; 3]);
    assert_eq!(sky(&mut scene, None), [0.; 3]);
}

#[test]
fn lod_fused_source_preserves_pixels_and_uses_alternative_identity() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    pollster::block_on(async {
        let mut world = test_support::cube();
        let repeated = world.meshes[0].indices.clone();
        world.meshes[0].indices.extend(repeated);
        let mut scene = Scene::new(&device, &queue);
        let (world, _) = test_support::add_static(&device, &queue, &mut scene, world);
        let alternative = scene
            .add_asset(&device, &queue, test_support::cube())
            .unwrap();
        let mut renderer = Renderer::for_test(
            &device,
            &queue,
            [64, 64],
            &Settings {
                preset: RenderPreset::Low,
                ..Settings::default()
            },
        );
        assert!(renderer.test_fused_supported());
        // Alternative geometry must not bring along its own material. A bright
        // red alternative exposes an accidental binding change in the pixel oracle.
        let mut red = scene.material(alternative.materials[0]).unwrap();
        red.base = [1., 0., 0., 1.];
        red.emission = [10., 0., 0.];
        scene
            .set_material(&queue, alternative.materials[0], red)
            .unwrap();
        // The fused pass's attachments, then depth.
        let formats = [
            shading::gbuffer::NORMAL,
            shading::gbuffer::MATERIAL,
            shading::gbuffer::MOTION,
            shading::gbuffer::F0,
            shading::gbuffer::COLOR,
            shading::gbuffer::AMBIENT,
            shading::gbuffer::SOURCE_ID,
            shading::gbuffer::ANISOTROPY,
            shading::gbuffer::DEPTH,
        ];
        let textures: Vec<_> = formats
            .into_iter()
            .map(|format| {
                device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("LOD fused evidence"),
                    size: wgpu::Extent3d {
                        width: 64,
                        height: 64,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                    view_formats: &[],
                })
            })
            .collect();
        let views: Vec<_> = textures
            .iter()
            .map(|t| t.create_view(&Default::default()))
            .collect();
        let input = FrameInput::new(Camera {
            view: camera::rh::view::look_at_mat4(Vec3::new(0., 0., 5.), Vec3::ZERO, Vec3::Y),
            projection: crate::perspective(1., 1., 0.1),
            eye: Vec3::new(0., 0., 5.),
        });
        let mut evidence = Vec::new();
        for coarse in [false, true] {
            if coarse {
                scene
                    .set_mesh_lods(
                        world.model,
                        0,
                        vec![crate::lod::MeshLod {
                            model: alternative.model,
                            mesh: 0,
                            max_error: 0.,
                        }],
                    )
                    .unwrap();
            }
            renderer.prepare_test_frame(&device, &queue, &mut scene, &input, &Settings::default());
            let mut encoder = device.create_command_encoder(&Default::default());
            let attachments: Vec<_> = views[..8]
                .iter()
                .map(|view| {
                    Some(wgpu::RenderPassColorAttachment {
                        view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: wgpu::StoreOp::Store,
                        },
                    })
                })
                .collect();
            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: None,
                    color_attachments: &attachments,
                    depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                        view: &views[8],
                        depth_ops: Some(wgpu::Operations {
                            load: wgpu::LoadOp::Clear(0.),
                            store: wgpu::StoreOp::Store,
                        }),
                        stencil_ops: None,
                    }),
                    ..Default::default()
                });
                renderer.draw_test_camera(&scene, &mut pass, GeometryPass::Fused);
            }
            queue.submit([encoder.finish()]);
            evidence.push(
                textures
                    .iter()
                    .map(|texture| {
                        test_support::read(
                            &device,
                            &queue,
                            texture,
                            texture.format().block_copy_size(None).unwrap(),
                        )
                    })
                    .collect::<Vec<_>>(),
            );
        }
        for attachment in [0, 1, 2, 3, 4, 5, 7, 8] {
            assert_eq!(
                evidence[0][attachment], evidence[1][attachment],
                "LOD changed fused pixels attachment {attachment}"
            );
        }
        let source = &evidence[1][6];
        let detailed_source = &evidence[0][6];
        let mut covered = 0;
        for (alternate, detailed) in source.chunks_exact(8).zip(detailed_source.chunks_exact(8)) {
            let owner = u32::from_le_bytes(alternate[..4].try_into().unwrap());
            if owner != 0 {
                covered += 1;
                assert_eq!(&alternate[..4], &detailed[..4], "LOD changed world owner");
                assert_ne!(
                    &alternate[4..],
                    &detailed[4..],
                    "LOD source falsely claims original triangle"
                );
            }
        }
        assert!(
            covered > 100,
            "unit cube at 5m must cover enough pixels to observe source identity"
        );
    });
}

fn uploaded_material(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    buffer: &wgpu::Buffer,
) -> Vec<f32> {
    // Read the actual uniform bound by material shading, so an early GPU write
    // before validation cannot be hidden by restoring only CPU state.
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("observe retained material after rejected edit"),
        source: wgpu::ShaderSource::Wgsl(
            (crate::shading::compose(&[&crate::shading::MATERIAL])
                + r#"
@group(0) @binding(0) var<uniform> material:Material;
@group(0) @binding(1) var<storage,read_write> observed:array<vec4<f32>,2>;
@compute @workgroup_size(1) fn observe() {
 observed[0]=material.base;
 observed[1]=vec4(material.anisotropy_strength,material.anisotropy_rotation,0.,0.);
}
"#)
            .into(),
        ),
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: None,
        layout: None,
        module: &shader,
        entry_point: Some("observe"),
        compilation_options: Default::default(),
        cache: None,
    });
    let output = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: 32,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: 32,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: output.as_entire_binding(),
            },
        ],
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_compute_pass(&Default::default());
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &group, &[]);
        pass.dispatch_workgroups(1, 1, 1);
    }
    encoder.copy_buffer_to_buffer(&output, 0, &readback, 0, 32);
    queue.submit([encoder.finish()]);
    mapped(device, &readback)
}

#[test]
fn anisotropy_material_edits_are_transactional() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    pollster::block_on(async {
        let mut world = asset_of(plane(0.5, [0.; 2], 1., false));
        for vertex in &mut world.meshes[0].vertices {
            vertex.tangent = [1., 0., 0., 1.];
        }
        let mut scene = Scene::new(&device, &queue);
        let (world, _) = test_support::add_static(&device, &queue, &mut scene, world);
        let (untangented, _) =
            test_support::add_static(&device, &queue, &mut scene, test_support::cube());
        let alternative = scene
            .add_asset(&device, &queue, test_support::cube())
            .unwrap();
        let material = world.materials[0];
        // Even at initial zero strength, an alternative missing the retained
        // base frame would break a later valid anisotropy activation.
        assert!(matches!(
            scene.set_mesh_lods(
                world.model,
                0,
                vec![lod::MeshLod {
                    model: alternative.model,
                    mesh: 0,
                    max_error: 0.,
                }]
            ),
            Err(SceneError::MissingAnisotropyTangents)
        ));
        assert!(
            scene.models.get(world.model).unwrap().meshes[0]
                .lods
                .is_empty()
        );
        let mut anisotropic = scene.material(material).unwrap();
        anisotropic.anisotropy_strength = 0.7;
        anisotropic.anisotropy_rotation = -1.2;
        scene.set_material(&queue, material, anisotropic).unwrap();
        let uniform = |scene: &Scene, id| {
            uploaded_material(&device, &queue, scene.drawn_material(id).uniform_buffer())
        };
        let before_gpu = uniform(&scene, material);
        for invalid in [[-0.1, 0.], [1.1, 0.], [f32::NAN, 0.], [0.7, f32::INFINITY]] {
            let mut values = anisotropic;
            values.base = [0.; 4];
            [values.anisotropy_strength, values.anisotropy_rotation] = invalid;
            assert!(matches!(
                scene.set_material(&queue, material, values),
                Err(SceneError::InvalidAnisotropy)
            ));
            assert!(scene.material(material).unwrap() == anisotropic);
            assert_eq!(uniform(&scene, material), before_gpu);
        }
        let other = untangented.materials[0];
        let before_gpu = uniform(&scene, other);
        let mut values = scene.material(other).unwrap();
        values.base = [0.; 4];
        values.anisotropy_strength = 0.7;
        assert!(matches!(
            scene.set_material(&queue, other, values),
            Err(SceneError::MissingAnisotropyTangents)
        ));
        assert_eq!(uniform(&scene, other), before_gpu);
    });
}

// Defects: `Settings::anisotropic_filtering` never reaches the material
// samplers, or a change leaves materials sampling as they did. Anisotropic
// filtering keeps a texture's detail where a surface recedes at a grazing
// angle, which trilinear filtering blurs toward the texture's mean: a fine
// checkerboard on a floor keeps more contrast between neighbouring pixels
// with 16 samples than with none. Turning it off again restores the frame.
#[test]
fn anisotropic_filtering_keeps_a_receding_textures_detail() {
    use crate::settings::AnisotropicFiltering;
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    const SIZE: [u32; 2] = [64, 64];
    // An unlit floor 40 m deep, a 64×64 checkerboard of single texels
    // repeating every 2 m.
    let mut floor = asset_of(asset::CpuMesh {
        vertices: [(-20., 0.), (20., 0.), (20., -40.), (-20., -40.)]
            .map(|(x, z)| asset::Vertex {
                tangent: [0.; 4],
                lightmap_bounds: [0., 0., 1., 1.],
                lightmap_uv: [0.; 2],
                position: [x, 0., z],
                normal: [0., 1., 0.],
                uv: [x / 2., z / 2.],
                color: [1.; 4],
            })
            .to_vec(),
        indices: vec![0, 1, 2, 0, 2, 3],
        material: 0,
        deformation: Default::default(),
    });
    floor.images = vec![asset::Image::Rgba8(image::RgbaImage::from_fn(
        64,
        64,
        |x, y| image::Rgba([if (x + y) % 2 == 0 { 255 } else { 0 }; 4]),
    ))];
    floor.materials[0].base = [1.; 4];
    floor.materials[0].base_texture = Some(0);
    floor.materials[0].unlit = true;
    let mut scene = Scene::new(&device, &queue);
    test_support::add_static(&device, &queue, &mut scene, floor);
    let mut settings = Settings {
        antialiasing: settings::Antialiasing::Off,
        ..Settings::default()
    };
    let mut renderer = Renderer::for_test(&device, &queue, SIZE, &settings);
    let eye = Vec3::new(0., 1., 0.);
    let input = FrameInput::new(Camera {
        eye,
        view: camera::rh::view::look_at_mat4(eye, Vec3::new(0., 0., -10.), Vec3::Y),
        projection: crate::perspective(1., 1., 0.1),
    });
    let mut frame = |filtering| {
        settings.anisotropic_filtering = filtering;
        let output = crate::view::targets::target(
            &device,
            "anisotropy output",
            SIZE,
            crate::shading::gbuffer::COLOR,
        );
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
        test_support::read(&device, &queue, renderer.targets().color.texture(), 8)
    };
    // The summed difference in red between horizontally neighbouring pixels
    // below the horizon, where the floor recedes.
    let contrast = |pixels: &[u8]| -> f64 {
        let red = |x: u32, y: u32| {
            f64::from(test_support::half(
                &pixels[((y * SIZE[0] + x) * 8) as usize..],
            ))
        };
        (SIZE[1] / 2 + 1..SIZE[1])
            .flat_map(|y| (1..SIZE[0]).map(move |x| (x, y)))
            .map(|(x, y)| (red(x, y) - red(x - 1, y)).abs())
            .sum()
    };
    let off = frame(AnisotropicFiltering::Off);
    let sixteen = frame(AnisotropicFiltering::X16);
    let off_again = frame(AnisotropicFiltering::Off);
    let (blurred, sharp) = (contrast(&off), contrast(&sixteen));
    eprintln!("neighbour contrast: off {blurred}, 16x {sharp}");
    assert!(
        sharp > blurred,
        "16x keeps no more detail ({sharp}) than trilinear ({blurred})"
    );
    assert!(
        off_again == off,
        "turning anisotropy off again changed the frame"
    );
}
