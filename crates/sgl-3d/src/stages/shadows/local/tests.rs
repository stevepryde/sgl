//! Local-light shadows observed at real boundaries: frames rendered,
//! submitted and finished through `Renderer::render`, the draws its encoder
//! issued into the atlas, and a light's visibility at receivers sampled
//! through the frame's own lit groups 0: the camera's, and ray hits', which
//! sample the static layers.
use crate::asset::{self, Vertex};
use crate::renderer::Renderer;
use crate::scene::buffer;
use crate::settings::{Antialiasing, Settings, ShadowQuality};
use crate::shading;
use crate::{
    AssetIds, Camera, FrameInput, InstanceId, InstanceState, Light, LightId, LightShape,
    LocalShadowStats, Mobility, ModelMesh, PreparedModel, Scene, test_support,
};
use glam::{Mat4, Vec3};

fn plane(center: Vec3, u: Vec3, v: Vec3) -> asset::CpuMesh {
    asset::CpuMesh {
        vertices: [(-1., -1.), (1., -1.), (1., 1.), (-1., 1.)]
            .map(|(x, y)| Vertex {
                tangent: [0.0; 4],
                lightmap_bounds: [0., 0., 1., 1.],
                lightmap_uv: [0.; 2],
                position: (center + u * x + v * y).to_array(),
                normal: u.cross(v).normalize().to_array(),
                uv: [0.; 2],
                color: [1.; 4],
            })
            .to_vec(),
        indices: vec![0, 1, 2, 0, 2, 3],
        material: 0,
        deformation: Default::default(),
    }
}

/// `meshes` as one double-sided asset.
fn asset(meshes: Vec<asset::CpuMesh>) -> asset::Asset {
    let mut asset = test_support::cube();
    asset.meshes = meshes;
    asset.materials[0].double_sided = true;
    asset
}

/// A square blocker of half size 0.25 at the origin, facing +Z.
fn blocker() -> asset::Asset {
    asset(vec![plane(Vec3::ZERO, Vec3::X * 0.25, Vec3::Y * 0.25)])
}

fn point(position: Vec3, range: f32) -> Light {
    Light {
        position,
        shape: LightShape::Point {
            radius: LightShape::DEFAULT_RADIUS,
        },
        color: [1.; 3],
        intensity: 8.,
        range,
        baked: false,
        specular: 1.,
        casts_shadow: true,
        ..Default::default()
    }
}

fn at(model: crate::ModelId, position: Vec3) -> InstanceState {
    InstanceState {
        model,
        pose: Mat4::from_translation(position),
        visible: false,
        capture_visible: true,
    }
}

/// An identity camera: an orthographic view of the unit cube around the
/// origin, which every light here reaches.
fn input() -> FrameInput {
    FrameInput::new(Camera {
        view: Mat4::IDENTITY,
        projection: Mat4::IDENTITY,
        eye: Vec3::ZERO,
    })
}

/// A renderer whose frames are rendered, submitted and finished as a game's
/// are, and an observer of a light's visibility through the last frame's
/// lit group 0.
struct Harness {
    device: wgpu::Device,
    queue: wgpu::Queue,
    renderer: Renderer,
    /// What its frames render with: the defaults, unless a test changes
    /// them.
    settings: Settings,
    output: wgpu::TextureView,
    /// Observers of the camera's view and of ray hits'.
    observers: [wgpu::ComputePipeline; 2],
}

/// Whose view an observation takes.
#[derive(Clone, Copy, PartialEq)]
enum Seen {
    /// The frame's camera, through its lit group 0 and the frame's atlas.
    Camera,
    /// World-space ray hits, through theirs and the static layers.
    RayHit,
}

impl Harness {
    fn new() -> Option<Self> {
        let (device, queue) = test_support::device()?;
        let renderer = Renderer::for_test(&device, &queue, [64, 64], &Settings::default());
        let output = device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("local shadow frames"),
                size: wgpu::Extent3d {
                    width: 64,
                    height: 64,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: shading::gbuffer::COLOR,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            })
            .create_view(&Default::default());
        // The production scene-light sample at receivers facing the light:
        // its visibility observes light-to-receiver occlusion, independently
        // of shading.
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("local shadow visibility observation"),
            source: wgpu::ShaderSource::Wgsl(
                format!(
                    r#"{}
override camera:bool=true;
// Each query is a receiver and the light's index, then the receiver's
// normal, or zero for one facing the light.
@group(1) @binding(0) var<storage,read> queries:array<vec4<f32>>;
@group(1) @binding(1) var<storage,read_write> output:array<f32>;
@compute @workgroup_size(32) fn observe(@builtin(global_invocation_id) id:vec3<u32>) {{
 if id.x<arrayLength(&output) {{
  let index=bitcast<u32>(queries[2u*id.x].w);
  let receiver=queries[2u*id.x].xyz;
  var normal=queries[2u*id.x+1u].xyz;
  if all(normal==vec3(0.)) {{
   normal=normalize(lights[index].position-receiver);
  }}
  output[id.x]=scene_light_sample(index,receiver,normal,normal,vec2(0.),select(SHADOW_RECEIVER_CAPTURE,SHADOW_RECEIVER_CAMERA,camera)).visibility;
 }}
}}
"#,
                    shading::compose(&[
                        &shading::BIND_LIT,
                        &shading::LIGHTS,
                        &shading::SHADOW_MASK_NONE,
                    ])
                )
                .into(),
            ),
        });
        let storage = |binding, read_only| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let queries = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[storage(0, true), storage(1, false)],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(renderer.test_lit_layout()), Some(&queries)],
            immediate_size: 0,
        });
        let observers = [1., 0.].map(|camera| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("local shadow observer"),
                layout: Some(&layout),
                module: &shader,
                entry_point: Some("observe"),
                compilation_options: wgpu::PipelineCompilationOptions {
                    constants: &[("camera", camera)],
                    ..Default::default()
                },
                cache: None,
            })
        });
        Some(Self {
            device,
            queue,
            renderer,
            settings: Settings::default(),
            output,
            observers,
        })
    }

    fn encode(&mut self, scene: &mut Scene, input: &FrameInput) -> wgpu::CommandEncoder {
        let mut encoder = self.device.create_command_encoder(&Default::default());
        self.renderer.render(
            &self.device,
            &self.queue,
            &mut encoder,
            scene,
            input,
            &self.settings,
            &self.output,
            None,
        );
        encoder
    }

    /// Renders, submits and finishes a frame, as a game does.
    fn frame(&mut self, scene: &mut Scene, input: &FrameInput) -> LocalShadowStats {
        let encoder = self.encode(scene, input);
        self.queue.submit([encoder.finish()]);
        self.renderer.finish_frame(scene);
        self.renderer.local_shadow_stats()
    }

    /// Light `light`'s visibility at `receivers` (positions and normals,
    /// zero for one facing the light) after the last frame, as `seen`.
    fn visibility(&self, light: LightId, receivers: &[(Vec3, Vec3)], seen: Seen) -> Vec<f32> {
        use crate::content::identity::Identity;
        let index = f32::from_bits(light.index() as u32);
        let queries: Vec<_> = receivers
            .iter()
            .flat_map(|(p, n)| [[p.x, p.y, p.z, index], [n.x, n.y, n.z, 0.]])
            .collect();
        let device = &self.device;
        let input = buffer(
            device,
            "receivers",
            bytemuck::cast_slice(&queries),
            wgpu::BufferUsages::STORAGE,
        );
        let bytes = (receivers.len() * 4) as u64;
        let output = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: bytes,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: bytes,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.observers[0].get_bind_group_layout(1),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: input.as_entire_binding(),
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
            let (observer, lit) = match seen {
                Seen::Camera => (&self.observers[0], self.renderer.test_camera_lit()),
                Seen::RayHit => (&self.observers[1], self.renderer.test_ray_hit_lit()),
            };
            pass.set_pipeline(observer);
            pass.set_bind_group(0, lit, &[]);
            pass.set_bind_group(1, &group, &[]);
            pass.dispatch_workgroups((receivers.len() as u32).div_ceil(32), 1, 1);
        }
        encoder.copy_buffer_to_buffer(&output, 0, &readback, 0, bytes);
        self.queue.submit([encoder.finish()]);
        let (tx, rx) = std::sync::mpsc::channel();
        readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| tx.send(result).unwrap());
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        rx.recv().unwrap().unwrap();
        let data = readback.slice(..).get_mapped_range();
        data.chunks_exact(4)
            .map(|bytes| f32::from_le_bytes(bytes.try_into().unwrap()))
            .collect()
    }

    /// Asserts light `light`'s visibility at each receiver, as the camera
    /// sees it.
    fn expect(&self, light: LightId, expected: &[(Vec3, f32)], label: &str) {
        self.expect_seen(light, expected, Seen::Camera, label);
    }

    /// Asserts light `light`'s visibility at each receiver, as `seen`.
    fn expect_seen(&self, light: LightId, expected: &[(Vec3, f32)], seen: Seen, label: &str) {
        let receivers: Vec<_> = expected.iter().map(|(p, _)| (*p, Vec3::ZERO)).collect();
        let actual = self.visibility(light, &receivers, seen);
        for ((point, target), actual) in expected.iter().zip(actual) {
            assert!(
                (actual - target).abs() < 0.01,
                "{label}, receiver {point:?}: visibility {actual}, expected {target}"
            );
        }
    }
}

// Plausible defects: a masked material's casters drawn whole (no discard)
// into the frame's faces or their static layers, or discarding other texels
// than the material cuts out. The oracle is geometric: a blocker between a
// point light and two receivers, its base map cut out over its left half;
// the segment from the light to the left receiver crosses the cut-out half,
// the right one's the opaque half. Both views of the shadow see it: the
// camera's (the frame's atlas) and ray hits' (the static layers).
#[test]
fn masked_casters_shadow_with_their_opaque_texels_only() {
    let Some(mut harness) = Harness::new() else {
        return;
    };
    let mut blocker = blocker();
    for (vertex, uv) in
        blocker.meshes[0]
            .vertices
            .iter_mut()
            .zip([[0., 1.], [1., 1.], [1., 0.], [0., 0.]])
    {
        vertex.uv = uv;
    }
    let (device, queue) = (&harness.device, &harness.queue);
    let mut scene = Scene::new(device, queue);
    let ids = scene
        .add_asset(device, queue, test_support::masked(blocker, 0.5))
        .unwrap();
    scene
        .add_instance(device, queue, at(ids.model, Vec3::ZERO), Mobility::Static)
        .unwrap();
    let light = scene
        .add_light(device, queue, point(Vec3::new(0., 0., 1.), 4.))
        .unwrap();
    harness.frame(&mut scene, &input());
    // The segments cross the blocker at x = -0.075 and +0.075 m, U = 0.35
    // and 0.65.
    let expected = [
        (Vec3::new(-0.15, 0., -1.), 1.),
        (Vec3::new(0.15, 0., -1.), 0.),
    ];
    harness.expect(light, &expected, "the camera's view");
    harness.expect_seen(light, &expected, Seen::RayHit, "ray hits");
}

/// A scene of `world` as a static instance, and the identities of a
/// double-sided blocker it may place.
fn scene(harness: &Harness, world: Vec<asset::CpuMesh>) -> (Scene, AssetIds) {
    let (device, queue) = (&harness.device, &harness.queue);
    let mut scene = Scene::new(device, queue);
    if !world.is_empty() {
        test_support::add_static(device, queue, &mut scene, asset(world));
    }
    let blocker = scene.add_asset(device, queue, blocker()).unwrap();
    (scene, blocker)
}

// Plausible defects: face sign/axis errors between the face views, their
// atlas slots and the lookup, clip-depth mismatch, static range culling
// omissions, stale moving casters, accidentally hiding capture-visible
// casters, single-sided casters writing from the wrong side, or a cached
// face kept after its static instance, its model or its light changed.
// The oracle is geometric placement: a square lies between source and
// receiver or wholly outside that segment, and faces the source or turns
// away from it. No production sampling math predicts results.
#[test]
fn point_light_shadows_follow_current_opaque_blockers() {
    let Some(mut harness) = Harness::new() else {
        return;
    };
    let axes = [
        Vec3::X,
        Vec3::NEG_X,
        Vec3::Y,
        Vec3::NEG_Y,
        Vec3::Z,
        Vec3::NEG_Z,
    ];
    let perpendicular = [Vec3::Y, Vec3::Y, Vec3::Z, Vec3::Z, Vec3::X, Vec3::X];
    let (mut scene, moving) = scene(
        &harness,
        axes.into_iter()
            .zip(perpendicular)
            .map(|(axis, p)| plane(axis * 2., p * 0.6, axis.cross(p) * 0.6))
            .collect(),
    );
    let (device, queue) = (harness.device.clone(), harness.queue.clone());
    let input = input();
    let light = scene
        .add_light(&device, &queue, point(Vec3::ZERO, 9.))
        .unwrap();
    let mut expected = Vec::new();
    for (axis, p) in axes.into_iter().zip(perpendicular) {
        expected.extend([(axis * 4., 0.), (axis, 1.), (axis * 4. + p * 2., 1.)]);
    }
    harness.frame(&mut scene, &input);
    harness.expect(light, &expected, "six face blockers");
    // An instance hidden from the main camera must still occlude the light.
    let blocker = |x| at(moving.model, Vec3::new(x, 0., 2.));
    let instance = scene
        .add_instance(&device, &queue, blocker(1.), Mobility::Moving)
        .unwrap();
    harness.frame(&mut scene, &input);
    harness.expect(
        light,
        &[(Vec3::new(2., 0., 4.), 0.), (Vec3::new(-2., 0., 4.), 1.)],
        "hidden opaque instance",
    );
    scene.set_instance(&queue, instance, blocker(-1.)).unwrap();
    harness.frame(&mut scene, &input);
    harness.expect(
        light,
        &[(Vec3::new(2., 0., 4.), 1.), (Vec3::new(-2., 0., 4.), 0.)],
        "current moving instance",
    );
    // A single-sided blocker casts from its front faces, as the directional
    // light's do: turned away from the light it casts nothing, and facing it,
    // it shadows the receiver behind it.
    let material = moving.materials[0];
    let mut values = scene.material(material).unwrap();
    values.double_sided = false;
    scene.set_material(&queue, material, values).unwrap();
    for (turn, visibility, label) in [
        (0., 1., "single-sided blocker facing away"),
        (
            std::f32::consts::PI,
            0.,
            "single-sided blocker facing the light",
        ),
    ] {
        let pose = Mat4::from_translation(Vec3::new(-1., 0., 2.)) * Mat4::from_rotation_y(turn);
        scene
            .set_instance(
                &queue,
                instance,
                InstanceState {
                    pose,
                    ..blocker(-1.)
                },
            )
            .unwrap();
        harness.frame(&mut scene, &input);
        harness.expect(light, &[(Vec3::new(-2., 0., 4.), visibility)], label);
    }
    values.double_sided = true;
    scene.set_material(&queue, material, values).unwrap();
    scene
        .set_instance(
            &queue,
            instance,
            InstanceState {
                capture_visible: false,
                ..blocker(-1.)
            },
        )
        .unwrap();
    // A static blocker casts through its model's clusters, whose bounds
    // follow its pose and its model's replaced geometry, and its static
    // edits mark the faces it leaves and enters.
    let fixed = scene
        .add_instance(
            &device,
            &queue,
            at(moving.model, Vec3::new(1., 0., 2.)),
            Mobility::Static,
        )
        .unwrap();
    let ahead = [(Vec3::new(2., 0., 4.), 0.), (Vec3::new(4., 0., 2.), 1.)];
    let aside = [(Vec3::new(2., 0., 4.), 1.), (Vec3::new(4., 0., 2.), 0.)];
    harness.frame(&mut scene, &input);
    harness.expect(light, &ahead, "static instance");
    scene
        .set_instance(&queue, fixed, at(moving.model, Vec3::new(2., 0., 1.)))
        .unwrap();
    harness.frame(&mut scene, &input);
    harness.expect(light, &aside, "posed static instance");
    let shifted = plane(Vec3::new(-1., 0., 1.), Vec3::X * 0.25, Vec3::Y * 0.25);
    scene
        .set_model(
            &device,
            &queue,
            moving.model,
            PreparedModel::new(vec![ModelMesh {
                vertices: shifted.vertices,
                indices: shifted.indices,
                material: moving.materials[0],
                deformation: Default::default(),
            }])
            .unwrap(),
        )
        .unwrap();
    harness.frame(&mut scene, &input);
    harness.expect(light, &ahead, "static instance of replaced geometry");
    scene.remove_instance(fixed).unwrap();
    harness.frame(&mut scene, &input);
    harness.expect(light, &aside[..1], "removed static instance");
    // Moving the source requires all faces to use the new light origin.
    scene
        .set_light(&queue, light, point(Vec3::new(6., 0., 0.), 9.))
        .unwrap();
    harness.frame(&mut scene, &input);
    // Receivers the faces drawn from the old origin would shadow, or not.
    harness.expect(
        light,
        &[
            (Vec3::ZERO, 0.),
            (Vec3::new(5., 0., 0.), 1.),
            (Vec3::new(9., 0., 0.), 1.),
            (Vec3::new(6., 0., 4.), 1.),
        ],
        "moving source",
    );
}

/// A light at the origin reaching 9 m with a static blocker at +X, and a
/// moving blocker far outside its range.
fn cached_scene(harness: &mut Harness) -> (Scene, LightId, InstanceId, crate::ModelId) {
    let (mut scene, moving) = scene(
        harness,
        vec![plane(Vec3::X * 2., Vec3::Y * 0.6, Vec3::Z * 0.6)],
    );
    let (device, queue) = (&harness.device, &harness.queue);
    let light = scene
        .add_light(device, queue, point(Vec3::ZERO, 9.))
        .unwrap();
    let far = scene
        .add_instance(
            device,
            queue,
            at(moving.model, Vec3::new(0., 0., 50.)),
            Mobility::Moving,
        )
        .unwrap();
    (scene, light, far, moving.model)
}

const BEHIND_STATIC: Vec3 = Vec3::new(4., 0., 0.);
const BEHIND_MOVING: Vec3 = Vec3::new(0., 0., 4.);

// Plausible defects: a cache that never hits (every face redrawn each frame,
// the cost the atlas exists to avoid), one that hits when a caster entered or
// left a light's range or moved within it (a stale shadow), or one that
// reuses a static layer for a face whose moving casters changed without
// redrawing them over it. The oracles are the draws the production encoder
// issued into the atlas, and the light's visibility behind each blocker.
#[test]
fn a_frame_in_which_nothing_moved_in_range_draws_no_shadows() {
    let Some(mut harness) = Harness::new() else {
        return;
    };
    let (mut scene, light, far, model) = cached_scene(&mut harness);
    let queue = harness.queue.clone();
    let input = input();
    let first = harness.frame(&mut scene, &input);
    assert!(
        first.layers_drawn > 0 && first.draws > 0,
        "first frame: {first:?}"
    );
    // The far instance moves, but nowhere near the light.
    for z in [51., 52.] {
        scene
            .set_instance(&queue, far, at(model, Vec3::new(0., 0., z)))
            .unwrap();
        let stats = harness.frame(&mut scene, &input);
        assert_eq!(stats.draws, 0, "out of range: {stats:?}");
        assert_eq!(stats.shadowed, 1, "out of range: {stats:?}");
    }
    harness.expect(
        light,
        &[(BEHIND_STATIC, 0.), (BEHIND_MOVING, 1.)],
        "cached static layer",
    );
    // Moving it into range shadows the point behind it, over the cached
    // static layer.
    scene
        .set_instance(&queue, far, at(model, Vec3::new(0., 0., 2.)))
        .unwrap();
    let entered = harness.frame(&mut scene, &input);
    assert!(
        entered.draws > 0 && entered.layers_drawn == 0,
        "entered range: {entered:?}"
    );
    harness.expect(
        light,
        &[(BEHIND_STATIC, 0.), (BEHIND_MOVING, 0.)],
        "moved into range",
    );
    // Staying put draws nothing and keeps its shadow.
    let still = harness.frame(&mut scene, &input);
    assert_eq!(still.draws, 0, "still in range: {still:?}");
    harness.expect(light, &[(BEHIND_MOVING, 0.)], "still in range");
    // Leaving the range redraws the face it left.
    scene
        .set_instance(&queue, far, at(model, Vec3::new(0., 0., 50.)))
        .unwrap();
    let left = harness.frame(&mut scene, &input);
    assert!(left.draws > 0, "left range: {left:?}");
    harness.expect(
        light,
        &[(BEHIND_STATIC, 0.), (BEHIND_MOVING, 1.)],
        "left range",
    );
}

// Plausible defects: a cache that commits what a frame drew before the frame
// is submitted and finished (S3D-4), so a dropped encoder, or a submitted
// frame the game never finished, leaves the cache believing the atlas holds
// what it does not. The oracle is the light's visibility behind the moving
// blocker, which the cache must redraw to its current place.
#[test]
fn unfinished_frames_leave_nothing_reusable() {
    let Some(mut harness) = Harness::new() else {
        return;
    };
    let (mut scene, light, far, model) = cached_scene(&mut harness);
    let queue = harness.queue.clone();
    let input = input();
    harness.frame(&mut scene, &input);
    // A frame with the blocker in range, dropped unsubmitted.
    scene
        .set_instance(&queue, far, at(model, Vec3::new(0., 0., 2.)))
        .unwrap();
    drop(harness.encode(&mut scene, &input));
    let redrawn = harness.frame(&mut scene, &input);
    assert!(redrawn.draws > 0, "after a dropped frame: {redrawn:?}");
    harness.expect(light, &[(BEHIND_MOVING, 0.)], "after a dropped frame");
    // A frame with the blocker out of range, submitted but not finished,
    // then the blocker back where the last finished frame drew it.
    scene
        .set_instance(&queue, far, at(model, Vec3::new(0., 0., 50.)))
        .unwrap();
    let encoder = harness.encode(&mut scene, &input);
    harness.queue.submit([encoder.finish()]);
    scene
        .set_instance(&queue, far, at(model, Vec3::new(0., 0., 2.)))
        .unwrap();
    harness.frame(&mut scene, &input);
    harness.expect(
        light,
        &[(BEHIND_MOVING, 0.)],
        "after a submitted, unfinished frame",
    );
}

// Plausible defects: a change of shadow quality that leaves lit group 0
// binding the old atlas, keeps placements made in the old atlas's layout
// (slots past the new atlas's edge, or records naming the old places), or
// keeps static layers or frame faces the old atlas held as if the new one
// held them, so a shadow vanishes, stays where it was or falls elsewhere.
// The oracle is geometric placement, as above, on the first frame at each
// quality: the moving blocker comes and goes with each change, so an atlas
// kept from the last quality shows where it was.
#[test]
fn shadows_follow_a_change_of_shadow_quality() {
    let Some(mut harness) = Harness::new() else {
        return;
    };
    let (mut scene, light, far, model) = cached_scene(&mut harness);
    let queue = harness.queue.clone();
    let input = input();
    harness.frame(&mut scene, &input);
    for (quality, z) in [
        (ShadowQuality::Low, 2.),
        (ShadowQuality::High, 50.),
        (ShadowQuality::Low, 50.),
        (ShadowQuality::High, 2.),
    ] {
        harness.settings.shadow_quality = quality;
        scene
            .set_instance(&queue, far, at(model, Vec3::new(0., 0., z)))
            .unwrap();
        let stats = harness.frame(&mut scene, &input);
        let label = format!("{quality:?}, moving blocker at z = {z}");
        assert_eq!(stats.shadowed, 1, "{label}: {stats:?}");
        let behind_moving = if z < 9. { 0. } else { 1. };
        harness.expect(
            light,
            &[
                (BEHIND_STATIC, 0.),
                (BEHIND_MOVING, behind_moving),
                (-BEHIND_STATIC, 1.),
            ],
            &label,
        );
    }
}

// Plausible defects: the Low quality's camera shadows taking Castaño's
// kernel, as High's do, rather than Godot's hard filter. The oracle is the
// filters' footprints: one hardware 2×2 comparison blends the four texels
// about a point, so the light rises from none to full across one texel; the
// 9-tap kernel spans five. Low's texels are twice High's, so across the
// static blocker's shadow edge Low's camera penumbra is narrower than
// High's, where the kernel's would be twice as wide.
#[test]
fn the_low_quality_takes_its_camera_shadows_with_one_hardware_tap() {
    let Some(mut harness) = Harness::new() else {
        return;
    };
    // Castaño's kernel at High, where TAA would take the spiral.
    harness.settings.antialiasing = Antialiasing::Off;
    let (mut scene, light, _, _) = cached_scene(&mut harness);
    let input = input();
    // The blocker's edge at z = 0.6, 2 m from the light, falls at z = 1.2 on
    // receivers 4 m from it.
    let receivers: Vec<_> = (0..=120)
        .map(|step| (Vec3::new(4., 0., 0.9 + step as f32 * 0.005), Vec3::ZERO))
        .collect();
    let mut penumbra = |quality| {
        harness.settings.shadow_quality = quality;
        harness.frame(&mut scene, &input);
        let visibility = harness.visibility(light, &receivers, Seen::Camera);
        assert!(
            visibility[0] < 0.01 && visibility[120] > 0.99,
            "{quality:?}: the receivers do not cross the shadow's edge: {visibility:?}"
        );
        visibility
            .iter()
            .filter(|v| (0.01..0.99).contains(*v))
            .count()
    };
    let high = penumbra(ShadowQuality::High);
    let low = penumbra(ShadowQuality::Low);
    assert!(
        low > 0 && low < high,
        "the penumbra spans {low} receivers at Low and {high} at High"
    );
}

// Plausible defects: static edits that mark nothing stale (a static instance
// added or removed in a light's range leaves its old shadow), or that mark
// every face stale wherever they are, beyond the light's range or in its
// range but outside a face. The oracles are the light's visibility behind
// the added instance, and the static layers and draws the encoder issued.
#[test]
fn static_edits_restale_only_the_faces_they_reach() {
    let Some(mut harness) = Harness::new() else {
        return;
    };
    let (mut scene, light, _, model) = cached_scene(&mut harness);
    let (device, queue) = (harness.device.clone(), harness.queue.clone());
    let input = input();
    harness.frame(&mut scene, &input);
    let away = scene
        .add_instance(
            &device,
            &queue,
            at(model, Vec3::new(30., 0., 0.)),
            Mobility::Static,
        )
        .unwrap();
    let stats = harness.frame(&mut scene, &input);
    assert_eq!(stats.draws, 0, "static edit out of range: {stats:?}");
    scene.remove_instance(away).unwrap();
    // In range, and only in the -Z face's view.
    let behind = scene
        .add_instance(
            &device,
            &queue,
            at(model, Vec3::new(0., 0., -4.)),
            Mobility::Static,
        )
        .unwrap();
    let stats = harness.frame(&mut scene, &input);
    assert_eq!(stats.layers_drawn, 1, "static edit in one face: {stats:?}");
    scene.remove_instance(behind).unwrap();
    harness.frame(&mut scene, &input);
    let near = scene
        .add_instance(
            &device,
            &queue,
            at(model, Vec3::new(0., 0., 2.)),
            Mobility::Static,
        )
        .unwrap();
    let stats = harness.frame(&mut scene, &input);
    assert!(stats.layers_drawn > 0, "static edit in range: {stats:?}");
    harness.expect(
        light,
        &[(BEHIND_MOVING, 0.), (BEHIND_STATIC, 0.)],
        "static instance added in range",
    );
    scene.remove_instance(near).unwrap();
    harness.frame(&mut scene, &input);
    harness.expect(
        light,
        &[(BEHIND_MOVING, 1.), (BEHIND_STATIC, 0.)],
        "static instance removed in range",
    );
}

// Plausible defects: a frame's static edits merged into fewer boxes than
// there are edits, so a box that joins edits on either side of a light
// restales faces that none of them reaches; Godot b130438 dirties only the
// lights an edited instance pairs with. The oracle is each edit's place:
// seventeen static instances added in one frame, every one beyond the
// light's range, the last 30 m to one side of it and the one before 30 m to
// the other (the pair a 16-box merge joined across the light), leave every
// face untouched.
#[test]
fn edits_beyond_a_light_leave_its_faces_however_many_there_are() {
    let Some(mut harness) = Harness::new() else {
        return;
    };
    let (mut scene, _, _, model) = cached_scene(&mut harness);
    let (device, queue) = (harness.device.clone(), harness.queue.clone());
    let input = input();
    harness.frame(&mut scene, &input);
    let ring = (0..15).map(|step| {
        let angle = (step as f32 * 24.).to_radians();
        Vec3::new(angle.cos(), 0., angle.sin()) * 100.
    });
    for position in ring.chain([Vec3::X * 30., Vec3::NEG_X * 30.]) {
        scene
            .add_instance(&device, &queue, at(model, position), Mobility::Static)
            .unwrap();
    }
    let stats = harness.frame(&mut scene, &input);
    assert_eq!(
        (stats.layers_drawn, stats.draws),
        (0, 0),
        "edits beyond the light's range: {stats:?}"
    );
}

// Plausible defects: a spot's face projected differently when drawn and when
// sampled, or a spot wider than one face missing the faces its cone reaches.
// The oracle is geometric placement within each cone.
#[test]
fn spot_lights_shadow_within_their_cones() {
    let Some(mut harness) = Harness::new() else {
        return;
    };
    let (device, queue) = (harness.device.clone(), harness.queue.clone());
    let side = Vec3::new(0.866, -0.5, 0.);
    let (mut scene, _) = scene(
        &harness,
        vec![
            plane(Vec3::X * 2., Vec3::Y * 0.3, Vec3::Z * 0.3),
            plane(Vec3::new(0.75, -2., 0.), Vec3::X * 0.3, Vec3::Z * 0.3),
            plane(side * 2., Vec3::Z * 0.3, side.cross(Vec3::Z) * 0.3),
        ],
    );
    let spot = |direction, outer_angle| Light {
        shape: LightShape::Spot {
            direction,
            inner_angle: 0.,
            outer_angle,
            radius: LightShape::DEFAULT_RADIUS,
        },
        ..point(Vec3::ZERO, 9.)
    };
    let narrow = scene
        .add_light(&device, &queue, spot(Vec3::X, 0.4))
        .unwrap();
    let input = input();
    harness.frame(&mut scene, &input);
    // Receivers just inside and just outside the blocker's shadow, which
    // spans 0.6 m either side of the axis 4 m away, across the face.
    harness.expect(
        narrow,
        &[
            (Vec3::X * 4., 0.),
            (Vec3::X, 1.),
            (Vec3::new(4., 0.5, 0.), 0.),
            (Vec3::new(4., -0.5, 0.), 0.),
            (Vec3::new(4., 0., 0.5), 0.),
            (Vec3::new(4., 0., -0.5), 0.),
            (Vec3::new(4., 0.7, 0.), 1.),
            (Vec3::new(4., -0.7, 0.), 1.),
            (Vec3::new(4., 0., 0.7), 1.),
            (Vec3::new(4., 0., -0.7), 1.),
        ],
        "narrow spot",
    );
    scene
        .set_light(&queue, narrow, spot(Vec3::NEG_Y, 1.2))
        .unwrap();
    harness.frame(&mut scene, &input);
    harness.expect(
        narrow,
        &[
            (Vec3::new(1.5, -4., 0.), 0.),
            (Vec3::new(-1.5, -4., 0.), 1.),
            (side * 4., 0.),
            (side * 4. + Vec3::Z, 1.),
        ],
        "wide spot",
    );
}

// Plausible defects: a rectangle that draws the cube face behind it, which
// nothing it lights samples, misses a face its half-space reaches, or casts
// from anywhere but its centre. The oracles are the faces the production
// encoder drew into the static atlas, five for a face looking down an axis,
// and geometric placement of blockers between its centre and receivers.
#[test]
fn rect_lights_shadow_their_half_space_from_their_centres() {
    let Some(mut harness) = Harness::new() else {
        return;
    };
    let (device, queue) = (harness.device.clone(), harness.queue.clone());
    let (mut scene, _) = scene(
        &harness,
        vec![
            plane(Vec3::new(0., -2., 0.), Vec3::X * 0.3, Vec3::Z * 0.3),
            plane(Vec3::new(2., -0.5, 0.), Vec3::Y * 0.3, Vec3::Z * 0.3),
        ],
    );
    let rect = scene
        .add_light(
            &device,
            &queue,
            Light {
                shape: LightShape::Rect {
                    direction: Vec3::NEG_Y,
                    width_axis: Vec3::X,
                    width: 0.4,
                    height: 0.2,
                },
                ..point(Vec3::ZERO, 9.)
            },
        )
        .unwrap();
    let stats = harness.frame(&mut scene, &input());
    assert_eq!(stats.layers_drawn, 5, "a downward rectangle: {stats:?}");
    harness.expect(
        rect,
        &[
            (Vec3::new(0., -4., 0.), 0.),
            (Vec3::new(1.5, -4., 0.), 1.),
            (Vec3::new(4., -1., 0.), 0.),
            (Vec3::new(4., -1., 1.5), 1.),
        ],
        "rectangle",
    );
}

// Plausible defects: lights beyond the atlas's room dropped from lighting
// rather than lit unshadowed, the atlas filled in index or arbitrary order
// rather than by screen coverage, or the count not reported. The oracles are
// each light's visibility behind its own blocker and the lights' distances
// from the camera.
#[test]
fn lights_beyond_the_atlas_are_lit_unshadowed_and_counted() {
    let Some(mut harness) = Harness::new() else {
        return;
    };
    let (device, queue) = (harness.device.clone(), harness.queue.clone());
    // Point lights 2 m apart down the camera's view, each with a blocker
    // half a metre to its side; the nearest are added last.
    let count = 240;
    let position = |i: usize| Vec3::new(0., 0., -5. - 2. * (count - 1 - i) as f32);
    let (mut scene, _) = scene(
        &harness,
        (0..count)
            .map(|i| plane(position(i) + Vec3::X * 0.5, Vec3::Y * 0.2, Vec3::Z * 0.2))
            .collect(),
    );
    let lights: Vec<_> = (0..count)
        .map(|i| {
            scene
                .add_light(&device, &queue, point(position(i), 1.5))
                .unwrap()
        })
        .collect();
    let input = FrameInput::new(Camera {
        view: Mat4::IDENTITY,
        projection: crate::perspective(1., 1., 0.1),
        eye: Vec3::ZERO,
    });
    let stats = harness.frame(&mut scene, &input);
    assert_eq!(stats.shadowed + stats.unshadowed, count, "{stats:?}");
    assert!(stats.unshadowed > 0, "{stats:?}");
    let behind = |i: usize| position(i) + Vec3::X;
    harness.expect(
        lights[count - 1],
        &[(behind(count - 1), 0.)],
        "the nearest light",
    );
    harness.expect(lights[0], &[(behind(0), 1.)], "the farthest light");
}

// Probe captures and ray hits show static content: they sample the static
// layers, which hold a face's static casters and none of its moving ones.
// Plausible defects: ray hits bound to the frame's atlas (moving casters
// shadow the static world they reflect), static layers that miss their
// static casters, or a light that moved this frame, whose layers are stale,
// sampled anyway. The oracle is geometric placement behind each blocker.
#[test]
fn ray_hits_sample_the_static_layers() {
    let Some(mut harness) = Harness::new() else {
        return;
    };
    let (mut scene, light, far, model) = cached_scene(&mut harness);
    let queue = harness.queue.clone();
    let input = input();
    scene
        .set_instance(&queue, far, at(model, Vec3::new(0., 0., 2.)))
        .unwrap();
    harness.frame(&mut scene, &input);
    harness.expect(light, &[(BEHIND_STATIC, 0.), (BEHIND_MOVING, 0.)], "camera");
    harness.expect_seen(
        light,
        &[(BEHIND_STATIC, 0.), (BEHIND_MOVING, 1.)],
        Seen::RayHit,
        "ray hits",
    );
    // A light that moved this frame drew every caster into its faces, and
    // its static layers hold nothing yet.
    scene
        .set_light(&queue, light, point(Vec3::new(0., 0.1, 0.), 9.))
        .unwrap();
    harness.frame(&mut scene, &input);
    harness.expect(light, &[(BEHIND_STATIC, 0.)], "camera of a moved light");
    harness.expect_seen(
        light,
        &[(BEHIND_STATIC, 1.)],
        Seen::RayHit,
        "ray hits of a moved light",
    );
}

// Every receiver on a lit surface must see the whole light: the receiver
// offset, not a caster-side bias, keeps a surface from shadowing itself
// (acne), at grazing angles too, and each kernel tap stays inside its face,
// so a receiver whose kernel reaches past a face's edge reads no other
// face's or light's texels. Plausible defects: no or too little offset, an
// offset along the wrong axis or scaled by the wrong texel size, or an
// unclamped kernel at a cube face's edge. The oracle: nothing lies between
// the light and the surface it lights, so every receiver on it sees all of
// it.
#[test]
fn lit_surfaces_do_not_shadow_themselves() {
    let Some(mut harness) = Harness::new() else {
        return;
    };
    let (device, queue) = (harness.device.clone(), harness.queue.clone());
    // A floor a metre below the lights, and a caster near the point light
    // at the edge of its -X face that meets its +X face in the atlas.
    let (mut scene, _) = scene(
        &harness,
        vec![
            plane(Vec3::NEG_Y, Vec3::X * 8., Vec3::Z * 8.),
            plane(Vec3::new(-1., -0.3, -1.), Vec3::Z * 0.2, Vec3::Y * 0.3),
        ],
    );
    let lights = [
        point(Vec3::ZERO, 9.),
        Light {
            shape: LightShape::Spot {
                direction: Vec3::new(1., -1., 0.).normalize(),
                inner_angle: 0.,
                outer_angle: 0.7,
                radius: LightShape::DEFAULT_RADIUS,
            },
            position: Vec3::new(0., 0., 30.),
            ..point(Vec3::ZERO, 9.)
        },
    ]
    .map(|light| scene.add_light(&device, &queue, light).unwrap());
    let input = input();
    harness.frame(&mut scene, &input);
    // Receivers on the floor from straight below to 74° from its normal,
    // and either side of the edges between the point light's cube faces at
    // 45°. Closer to grazing, a normal offset leaves some acne in every
    // engine, where the cosine already dims the light.
    let floor = [0., 0.4, 0.98, 1., 1.02, 2., 3.5];
    let mut point_receivers: Vec<_> = floor
        .iter()
        .flat_map(|&x| [Vec3::new(x, -1., 0.), Vec3::new(x, -1., x * 0.7)])
        .map(|p| (p, Vec3::Y))
        .collect();
    // Above the floor on the +X face, under two texels from its edge in the
    // atlas, beside the -X face's slot where the near caster lies: a kernel
    // that crossed the edge would read the caster's depth.
    point_receivers.push((Vec3::new(3., -0.5, -2.98), Vec3::ZERO));
    let spot_receivers: Vec<_> = [0.5, 0.8, 1., 1.3, 1.6]
        .into_iter()
        .map(|x| (Vec3::new(x, -1., 30.), Vec3::Y))
        .collect();
    for (light, receivers, label) in [
        (lights[0], point_receivers, "point light"),
        (lights[1], spot_receivers, "spot light"),
    ] {
        let visibility = harness.visibility(light, &receivers, Seen::Camera);
        for ((receiver, _), visibility) in receivers.iter().zip(visibility) {
            assert!(
                visibility > 0.99,
                "{label}: the receiver at {receiver:?} is shadowed: {visibility}"
            );
        }
    }
}

// Plausible defects: a move of the render origin that leaves the atlas's
// slots and lights recorded in the old render frame, so every light looks
// moved and redraws every caster, or translates them the wrong way, or again
// in a frame after an abandoned one. The oracles are the draws the
// production encoder issued, none for a scene that moved only with its
// origin, and the light's visibility behind each blocker, in the new frame,
// through the frame's atlas and the static layers ray hits sample.
#[test]
fn a_render_origin_move_keeps_every_shadow_in_place_without_drawing() {
    let Some(mut harness) = Harness::new() else {
        return;
    };
    let (mut scene, light, far, model) = cached_scene(&mut harness);
    let (device, queue) = (harness.device.clone(), harness.queue.clone());
    scene
        .set_instance(&queue, far, at(model, Vec3::new(0., 0., 2.)))
        .unwrap();
    harness.frame(&mut scene, &input());
    let still = harness.frame(&mut scene, &input());
    assert_eq!(still.draws, 0, "before the move: {still:?}");
    let by = Vec3::new(1000.5, -3.25, 2048.);
    scene.move_origin(&device, &queue, by).unwrap();
    // The game expresses its camera, and its moving instance's pose, in the
    // new frame.
    let mut moved = input();
    moved.camera.view = Mat4::from_translation(by);
    moved.camera.eye = -by;
    scene
        .set_instance(&queue, far, at(model, Vec3::new(0., 0., 2.) - by))
        .unwrap();
    drop(harness.encode(&mut scene, &moved));
    let after = harness.frame(&mut scene, &moved);
    assert_eq!(after.draws, 0, "after the move: {after:?}");
    assert_eq!(after.shadowed, 1, "after the move: {after:?}");
    harness.expect(
        light,
        &[(BEHIND_STATIC - by, 0.), (BEHIND_MOVING - by, 0.)],
        "after the move",
    );
    harness.expect_seen(
        light,
        &[(BEHIND_STATIC - by, 0.), (Vec3::new(-4., 0., 0.) - by, 1.)],
        Seen::RayHit,
        "static layers after the move",
    );
}

// Plausible defects: a light's shadow record written only when the frame
// that changed it finishes, or kept as the last finished frame left it, so a
// record a dropped frame wrote (its writes land with the next submission)
// stays in the buffer when the next frame's record matches the finished
// one; a record not rewritten when its light loses or regains its shadow
// or its static layers; or a buffer grown for more lights whose copy is the
// old buffer's, so the grown buffer's zeros leave placed lights unshadowed. The oracles are the light's visibility behind the
// static blocker through the frame's atlas and through the static layers
// ray hits sample, which need the record's placement and its `layers`.
#[test]
fn shadow_records_follow_changes_and_dropped_frames() {
    let Some(mut harness) = Harness::new() else {
        return;
    };
    let (mut scene, light, _, _) = cached_scene(&mut harness);
    let queue = harness.queue.clone();
    let input = input();
    let lit = |casts_shadow, position| Light {
        casts_shadow,
        ..point(position, 9.)
    };
    let expect = |harness: &Harness, shadowed: bool, label: &str| {
        let behind = if shadowed { 0. } else { 1. };
        harness.expect(light, &[(BEHIND_STATIC, behind)], label);
        harness.expect_seen(light, &[(BEHIND_STATIC, behind)], Seen::RayHit, label);
    };
    harness.frame(&mut scene, &input);
    expect(&harness, true, "shadowed");
    // Its shadow turned off, and back on.
    scene
        .set_light(&queue, light, lit(false, Vec3::ZERO))
        .unwrap();
    harness.frame(&mut scene, &input);
    expect(&harness, false, "shadow off");
    scene
        .set_light(&queue, light, lit(true, Vec3::ZERO))
        .unwrap();
    harness.frame(&mut scene, &input);
    expect(&harness, true, "shadow back on");
    // A dropped frame with the shadow off, then a frame with it on, whose
    // record is the last finished frame's.
    scene
        .set_light(&queue, light, lit(false, Vec3::ZERO))
        .unwrap();
    drop(harness.encode(&mut scene, &input));
    scene
        .set_light(&queue, light, lit(true, Vec3::ZERO))
        .unwrap();
    harness.frame(&mut scene, &input);
    expect(&harness, true, "after a dropped frame without the shadow");
    // A dropped frame with the light moved, which leaves its static layers
    // stale, then a frame with it back, whose layers are redrawn.
    scene
        .set_light(&queue, light, lit(true, Vec3::Y * 0.5))
        .unwrap();
    drop(harness.encode(&mut scene, &input));
    scene
        .set_light(&queue, light, lit(true, Vec3::ZERO))
        .unwrap();
    harness.frame(&mut scene, &input);
    expect(&harness, true, "after a dropped frame with the light moved");
    // The same, the moved frame submitted but never finished.
    scene
        .set_light(&queue, light, lit(true, Vec3::Y * 0.5))
        .unwrap();
    let encoder = harness.encode(&mut scene, &input);
    harness.queue.submit([encoder.finish()]);
    scene
        .set_light(&queue, light, lit(true, Vec3::ZERO))
        .unwrap();
    harness.frame(&mut scene, &input);
    expect(
        &harness,
        true,
        "after an unfinished frame with the light moved",
    );
    // Lights added until the scene holds more than the records written so
    // far: the records move to a larger buffer, which must hold the shadowed
    // light's record too. The added lights cast no shadow, far away.
    let device = harness.device.clone();
    let capacity = scene.lights.capacity();
    while scene.lights.capacity() <= capacity {
        scene
            .add_light(&device, &queue, lit(false, Vec3::new(0., 100., 0.)))
            .unwrap();
    }
    harness.frame(&mut scene, &input);
    expect(
        &harness,
        true,
        "after the scene's lights outgrew the records",
    );
}
