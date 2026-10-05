//! The two-level ray source of a real `Scene`: its instance BVHs and
//! slot-stable entries traced against the f64 world-space oracle over many
//! instances of both kinds, across edits, reused identities, replaced
//! geometry and frames that trace nothing or are abandoned.
use super::portable_tests::oracle_except;
use super::tests::{Pose, asset, triangle};
use crate::asset::Asset;
use crate::content::identity::{Identity, InstanceId, ModelId};
use crate::content::instance::{InstanceState, Mobility};
use crate::content::model::ModelMesh;
use crate::{PreparedModel, Scene};
use glam::{Mat4, Quat, Vec3};
use wgpu::util::DeviceExt;

/// Each ray's nearest hit of both kinds, its nearest moving hit except its
/// moving receiver, whether no static surface but its static receiver lies
/// in its interval with the end open, whether no surface lies in it closed,
/// and the decoded nearest hit's object flags plus one (zero for a miss).
const OBSERVER: &str = r#"
struct TestRay {
 ray:SceneRay,
 moving_receiver:vec4<u32>,
 static_receiver:vec4<u32>,
}
struct Observed {
 nearest:RawSceneHit,
 moving:RawSceneHit,
 visible:vec4<u32>,
}
@group(3) @binding(0) var<storage,read> test_rays:array<TestRay>;
@group(3) @binding(1) var<storage,read_write> observed:array<Observed>;
// A receiver's raster identity: its index plus one and its triangle's first
// index word, from an index plus one, a mesh and a triangle.
fn test_receiver(receiver:vec4<u32>)->vec2<u32> {
 if receiver.x==0u {
  return vec2(0u);
 }
 let mesh=scene_instances[receiver.x-1u].mesh_word+receiver.y*SCENE_MESH_WORDS;
 return vec2(receiver.x,scene_source[mesh+SCENE_MESH_INDICES]+receiver.z*3u);
}
@compute @workgroup_size(64) fn observe(@builtin(global_invocation_id) id:vec3<u32>) {
 if id.x>=arrayLength(&test_rays) {
  return;
 }
 let test=test_rays[id.x];
 let ray=test.ray;
 var result:Observed;
 result.nearest=scene_trace_nearest(ray,SCENE_SIDES_AS_RASTER);
 result.moving=scene_trace_moving_except_receiver(ray,test_receiver(test.moving_receiver));
 let hit=scene_decode_hit(result.nearest,ray.origin.xyz,ray.direction.xyz);
 result.visible=vec4(
  select(0u,1u,scene_static_segment_visible_except_receiver(ray,test_receiver(test.static_receiver))),
  select(0u,1u,scene_segment_visible(ray.origin.xyz,ray.direction.xyz,ray.origin.w,ray.direction.w,SCENE_SIDES_AS_RASTER)),
  select(0u,(hit.instance_flags&OBJECT_STATIC)+1u,hit.hit),
  0u);
 observed[id.x]=result;
}
"#;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct TestRay {
    /// Origin and interval start, direction and interval end.
    ray: [f32; 8],
    /// Index plus one, mesh and triangle, or zeros for none.
    moving_receiver: [u32; 4],
    static_receiver: [u32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct Observed {
    nearest: [u32; 8],
    moving: [u32; 8],
    visible: [u32; 4],
}

/// The observer's pipeline over the scene at group 1.
struct Observer {
    pipeline: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
}

impl Observer {
    fn new(device: &wgpu::Device, scene: &Scene) -> Self {
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
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ray observer"),
            entries: &[storage(0, true), storage(1, false)],
        });
        let source = format!(
            "{}\n{OBSERVER}",
            crate::shading::compose(&[&crate::shading::SCENE_RAYS_PORTABLE])
        );
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("ray observer"),
            source: wgpu::ShaderSource::Wgsl(source.into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("ray observer"),
            layout: Some(
                &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: None,
                    bind_group_layouts: &[None, Some(scene.scene_layout()), None, Some(&layout)],
                    immediate_size: 0,
                }),
            ),
            module: &module,
            entry_point: Some("observe"),
            compilation_options: Default::default(),
            cache: None,
        });
        Self { pipeline, layout }
    }

    /// Encodes the observation of `rays` into `output`.
    fn encode(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        scene: &Scene,
        rays: &[TestRay],
        output: &wgpu::Buffer,
    ) {
        let input = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: None,
            contents: bytemuck::cast_slice(rays),
            usage: wgpu::BufferUsages::STORAGE,
        });
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.layout,
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
        let mut pass = encoder.begin_compute_pass(&Default::default());
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(1, &scene.scene_group, &[]);
        pass.set_bind_group(3, &group, &[]);
        pass.dispatch_workgroups((rays.len() as u32).div_ceil(64), 1, 1);
    }

    /// A traced frame: updates the scene's rays, then observes `rays`.
    fn observe(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        scene: &mut Scene,
        rays: &[TestRay],
    ) -> Vec<Observed> {
        scene.update_rays(device, queue, 0, false);
        let size = (rays.len() * std::mem::size_of::<Observed>()) as u64;
        let output = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        self.encode(device, &mut encoder, scene, rays, &output);
        queue.submit([encoder.finish()]);
        bytemuck::cast_slice(&crate::test_support::read_words(device, queue, &output)).to_vec()
    }
}

/// A ray down -Z from `origin` over `[0, length]`, with no receivers.
fn down(origin: Vec3, length: f32) -> TestRay {
    TestRay {
        ray: [origin.x, origin.y, origin.z, 0., 0., 0., -1., length],
        moving_receiver: [0; 4],
        static_receiver: [0; 4],
    }
}

fn state(model: ModelId, pose: Mat4, capture_visible: bool) -> InstanceState {
    InstanceState {
        model,
        pose,
        visible: true,
        capture_visible,
    }
}

/// An instance the test placed, as the oracle sees it.
#[derive(Clone, Copy)]
struct Placed {
    id: InstanceId,
    /// Its model's asset, in the test's list.
    asset: usize,
    pose: Mat4,
    mobility: Mobility,
    capture_visible: bool,
}

impl Placed {
    fn pose(&self) -> Pose {
        Pose {
            model: self.asset,
            world: self.pose,
            id: self.id.index() as u32,
        }
    }
}

/// The oracle's hit as a raw hit's words: whether it hit, the index, mesh
/// and triangle.
fn words(hit: Option<(f64, u32, u32, u32)>) -> [u32; 4] {
    hit.map_or([0; 4], |(_, index, mesh, triangle)| {
        [1, index, mesh, triangle]
    })
}

/// Compares `observed` with the oracle over `placed`, for each ray its
/// nearest hit, nearest moving hit except its receiver, and static and
/// total visibility.
fn compare(
    round: &str,
    assets: &[Asset],
    placed: &[Placed],
    rays: &[TestRay],
    observed: &[Observed],
) -> usize {
    let shown = |kind: Option<Mobility>| -> Vec<Pose> {
        placed
            .iter()
            .filter(|p| p.capture_visible && kind.is_none_or(|kind| p.mobility == kind))
            .map(Placed::pose)
            .collect()
    };
    let (all, moving, statics) = (
        shown(None),
        shown(Some(Mobility::Moving)),
        shown(Some(Mobility::Static)),
    );
    let receiver = |r: [u32; 4]| (r[0] != 0).then(|| (r[0] - 1, r[1], r[2]));
    let mut hits = 0;
    for (index, (test, actual)) in rays.iter().zip(observed).enumerate() {
        let close = |hit: [u32; 8], expected: Option<(f64, u32, u32, u32)>, what: &str| {
            assert_eq!(
                hit[..4],
                words(expected),
                "{round}, ray {index}: {what} {hit:?}, expected {expected:?}"
            );
            if let Some((t, ..)) = expected {
                let actual = f64::from(f32::from_bits(hit[4]));
                assert!(
                    (actual - t).abs() <= 2e-4 * t.max(1.),
                    "{round}, ray {index}: {what} at {actual}, expected {t}"
                );
            }
        };
        let nearest = oracle_except(assets, &all, test.ray, false, None);
        hits += usize::from(nearest.is_some());
        close(actual.nearest, nearest, "nearest");
        close(
            actual.moving,
            oracle_except(
                assets,
                &moving,
                test.ray,
                false,
                receiver(test.moving_receiver),
            ),
            "moving",
        );
        let blocked = oracle_except(
            assets,
            &statics,
            test.ray,
            true,
            receiver(test.static_receiver),
        );
        assert_eq!(
            actual.visible[0] == 1,
            blocked.is_none(),
            "{round}, ray {index}: static visibility, blocker {blocked:?}"
        );
        assert_eq!(
            actual.visible[1] == 1,
            nearest.is_none(),
            "{round}, ray {index}: visibility, blocker {nearest:?}"
        );
        let flags = nearest.map_or(0, |(_, hit, ..)| {
            let p = placed.iter().find(|p| p.id.index() as u32 == hit).unwrap();
            1 + u32::from(p.mobility == Mobility::Static)
        });
        assert_eq!(actual.visible[2], flags, "{round}, ray {index}: hit flags");
    }
    eprintln!("{round}: {hits} hits of {} rays", rays.len());
    hits
}

// Plausible defects: an instance BVH whose posed bounds miss part of an
// instance (rotated, scaled or mirrored) or name the wrong entry; a kind
// traversed by the other kind's query; instances that are not capture
// visible, or whose model is empty, traversed; a removed instance, or a
// reused index's earlier instance, still hit; entries not rewritten when an
// instance is posed again or its model's geometry is replaced; the static
// BVH not rebuilt after a static edit whose frame traced nothing; receiver
// exclusion or the open end applied in one traversal and not another. The
// oracle is an f64 world-space plane and edge test over the placed
// instances of each kind.
#[test]
fn two_level_traversal_matches_brute_force_over_edited_instances() {
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    // Ten triangles, each in its own plane, so no two meet a ray at one
    // distance.
    let scattered = |offset: f32, back_every: usize| {
        (0..10)
            .map(|i| {
                triangle(
                    (i % 5) as f32 * 1.6 - 3.2 + offset,
                    -(i as f32) * 0.37 - offset,
                    i % back_every == 0,
                    0,
                )
            })
            .collect::<Vec<_>>()
    };
    let mut assets = vec![
        asset(scattered(0., 3), false),
        asset(scattered(0.4, 4), true),
        asset(Vec::new(), false),
        // Asset 1's replacement geometry.
        asset(scattered(-0.7, 2), true),
    ];
    let models: Vec<_> = assets[..3]
        .iter()
        .map(|a| scene.add_asset(&device, &queue, a.clone()).unwrap())
        .collect();
    // Fixed PRNG solely chooses physical cases; it does not derive expected hits.
    let mut seed = 0x51ed_270bu32;
    let mut random = move || {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        (seed >> 8) as f32 / 16_777_216.
    };
    let pose = |random: &mut dyn FnMut() -> f32| {
        let axis = Vec3::new(random() - 0.5, random() - 0.5, random() - 0.5).normalize_or(Vec3::Y);
        let mut scale = Vec3::new(0.6 + random(), 0.6 + random(), 0.6 + random());
        if random() < 0.25 {
            scale.x = -scale.x;
        }
        let at = Vec3::new(random(), random(), random()) * 36. - 18.;
        Mat4::from_scale_rotation_translation(
            scale,
            Quat::from_axis_angle(axis, random() * 6.3),
            at,
        )
    };
    let mut placed = Vec::new();
    let place = |scene: &mut Scene,
                 placed: &mut Vec<Placed>,
                 random: &mut dyn FnMut() -> f32,
                 asset: usize,
                 mobility: Mobility,
                 capture_visible: bool| {
        let pose = pose(random);
        let id = scene
            .add_instance(
                &device,
                &queue,
                state(models[asset].model, pose, capture_visible),
                mobility,
            )
            .unwrap();
        placed.push(Placed {
            id,
            asset,
            pose,
            mobility,
            capture_visible,
        });
    };
    for i in 0..210 {
        let mobility = if i % 3 == 0 {
            Mobility::Moving
        } else {
            Mobility::Static
        };
        let asset = if i % 37 == 0 { 2 } else { i % 2 };
        place(
            &mut scene,
            &mut placed,
            &mut random,
            asset,
            mobility,
            i % 23 != 0,
        );
    }
    let observer = Observer::new(&device, &scene);
    let mut rays: Vec<_> = (0..1024)
        .map(|_| {
            let origin = Vec3::new(random(), random(), random()) * 44. - 22.;
            let direction = Vec3::new(random(), random(), random()) - 0.5;
            let start = random() * 0.05;
            let end = 10. + random() * 60.;
            TestRay {
                ray: [
                    origin.x,
                    origin.y,
                    origin.z,
                    start,
                    direction.x,
                    direction.y,
                    direction.z,
                    end,
                ],
                moving_receiver: [0; 4],
                static_receiver: [0; 4],
            }
        })
        .collect();
    // Odd rays exclude the moving and static triangles they would meet
    // first, as a receiver's own triangle.
    let receivers = |assets: &[Asset], placed: &[Placed], rays: &mut [TestRay]| {
        let kind = |mobility| -> Vec<Pose> {
            placed
                .iter()
                .filter(|p| p.capture_visible && p.mobility == mobility)
                .map(Placed::pose)
                .collect()
        };
        let (moving, statics) = (kind(Mobility::Moving), kind(Mobility::Static));
        for test in rays.iter_mut().skip(1).step_by(2) {
            let first = |poses: &[Pose]| {
                oracle_except(assets, poses, test.ray, false, None)
                    .map_or([0; 4], |(_, index, mesh, triangle)| {
                        [index + 1, mesh, triangle, 0]
                    })
            };
            test.moving_receiver = first(&moving);
            test.static_receiver = first(&statics);
        }
    };
    receivers(&assets, &placed, &mut rays);
    let observed = observer.observe(&device, &queue, &mut scene, &rays);
    assert!(compare("first", &assets, &placed, &rays, &observed) > 100);

    // Remove some of each kind, pose the rest again, some on another model,
    // hide some static ones and add new instances at the freed indices.
    let mut kept = Vec::new();
    for (i, mut p) in placed.into_iter().enumerate() {
        if i % 4 == 1 {
            scene.remove_instance(p.id).unwrap();
            continue;
        }
        if p.mobility == Mobility::Moving || i % 11 == 0 {
            p.pose = pose(&mut random);
            if p.asset != 2 && i % 2 == 0 {
                p.asset = 1 - p.asset;
            }
            p.capture_visible = p.mobility == Mobility::Moving || i % 22 != 0;
            scene
                .set_instance(
                    &queue,
                    p.id,
                    state(models[p.asset].model, p.pose, p.capture_visible),
                )
                .unwrap();
        }
        kept.push(p);
    }
    let mut placed = kept;
    for i in 0..64 {
        let mobility = if i % 2 == 0 {
            Mobility::Moving
        } else {
            Mobility::Static
        };
        place(&mut scene, &mut placed, &mut random, i % 2, mobility, true);
    }
    receivers(&assets, &placed, &mut rays);
    let observed = observer.observe(&device, &queue, &mut scene, &rays);
    compare("edited", &assets, &placed, &rays, &observed);

    // Replace a model both kinds show and remove static instances in a frame
    // that traces nothing; the next traced frame shows both.
    let materials = models[1].materials.clone();
    let meshes = assets[3]
        .meshes
        .iter()
        .map(|mesh| ModelMesh {
            vertices: mesh.vertices.clone(),
            indices: mesh.indices.clone(),
            material: materials[mesh.material],
            deformation: Default::default(),
        })
        .collect();
    scene
        .set_model(
            &device,
            &queue,
            models[1].model,
            PreparedModel::new(meshes).unwrap(),
        )
        .unwrap();
    assets[1] = assets[3].clone();
    let mut kept = Vec::new();
    for (i, p) in placed.into_iter().enumerate() {
        if p.mobility == Mobility::Static && i % 5 == 0 {
            scene.remove_instance(p.id).unwrap();
        } else {
            kept.push(p);
        }
    }
    let placed = kept;
    scene.finish_frame();
    receivers(&assets, &placed, &mut rays);
    let observed = observer.observe(&device, &queue, &mut scene, &rays);
    compare("replaced", &assets, &placed, &rays, &observed);
    // A traced frame with no edits shows the same.
    scene.finish_frame();
    let observed = observer.observe(&device, &queue, &mut scene, &rays);
    compare("unchanged", &assets, &placed, &rays, &observed);
}

// Plausible defects: a reused index's entry, leaf or object record still
// that of the instance removed before it (its kind, pose or model), or an
// entry naming a model's words after its geometry was replaced and other
// content took them. The oracle is the triangles' planes: a ray down -Z
// from the origin meets the plane at z = -d at distance d.
#[test]
fn reused_identities_never_hit_earlier_geometry() {
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    let model = |scene: &mut Scene, x: f32, z: f32| {
        scene
            .add_asset(&device, &queue, asset(vec![triangle(x, z, false, 0)], true))
            .unwrap()
    };
    let near = model(&mut scene, 0., -2.);
    let far = model(&mut scene, 10., 0.);
    let observer = Observer::new(&device, &scene);
    let rays = [down(Vec3::ZERO, 10.), down(Vec3::new(10., 0., 1.), 10.)];
    let first = scene
        .add_instance(
            &device,
            &queue,
            state(near.model, Mat4::IDENTITY, true),
            Mobility::Static,
        )
        .unwrap();
    let observed = observer.observe(&device, &queue, &mut scene, &rays);
    assert_eq!(observed[0].nearest[..2], [1, first.index() as u32]);
    assert_eq!(f32::from_bits(observed[0].nearest[4]), 2.);
    // The removed static instance's index is reused by a moving one of the
    // other model, 5 m down at the origin, in a frame that traces nothing.
    scene.remove_instance(first).unwrap();
    let second = scene
        .add_instance(
            &device,
            &queue,
            state(
                far.model,
                Mat4::from_translation(Vec3::new(-10., 0., -5.)),
                true,
            ),
            Mobility::Moving,
        )
        .unwrap();
    assert_eq!(second.index(), first.index(), "the index is reused");
    scene.finish_frame();
    let observed = observer.observe(&device, &queue, &mut scene, &rays);
    let hit = |observed: &Observed| {
        (
            observed.nearest[0],
            observed.nearest[1],
            f32::from_bits(observed.nearest[4]),
            observed.visible[2],
        )
    };
    // Moving: flags 0, plus one.
    assert_eq!(hit(&observed[0]), (1, second.index() as u32, 5., 1));
    assert_eq!(observed[0].moving, observed[0].nearest);
    assert_eq!(observed[0].visible[0], 1, "no static instance remains");
    assert_eq!(hit(&observed[1]).0, 0, "the model untransformed is not hit");
    // Its model's geometry, 1 m nearer, replaces the old, whose words a new
    // model may take.
    let materials = far.materials.clone();
    let replaced = triangle(10., 1., false, 0);
    scene
        .set_model(
            &device,
            &queue,
            far.model,
            PreparedModel::new(vec![ModelMesh {
                vertices: replaced.vertices,
                indices: replaced.indices,
                material: materials[0],
                deformation: Default::default(),
            }])
            .unwrap(),
        )
        .unwrap();
    let other = model(&mut scene, 0., -3.);
    scene
        .add_instance(
            &device,
            &queue,
            state(other.model, Mat4::from_translation(Vec3::X * 40.), true),
            Mobility::Static,
        )
        .unwrap();
    let observed = observer.observe(&device, &queue, &mut scene, &rays);
    assert_eq!(hit(&observed[0]), (1, second.index() as u32, 4., 1));
    assert_eq!(hit(&observed[1]).0, 0);
}

// Plausible defect: the frame's ray uploads (entries, instance BVHs, their
// roots) recorded into the frame's encoder, or the static BVH marked built
// by a frame that was then abandoned, so the next submitted frame traces
// the scene before the edit. The oracle is the planes' distances.
#[test]
fn a_frame_after_an_abandoned_one_traces_the_edited_scene() {
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    let near = scene
        .add_asset(
            &device,
            &queue,
            asset(vec![triangle(0., -2., false, 0)], true),
        )
        .unwrap()
        .model;
    let far = scene
        .add_asset(
            &device,
            &queue,
            asset(vec![triangle(0., -6., false, 0)], true),
        )
        .unwrap()
        .model;
    let observer = Observer::new(&device, &scene);
    let rays = [down(Vec3::ZERO, 10.)];
    let first = scene
        .add_instance(
            &device,
            &queue,
            state(near, Mat4::IDENTITY, true),
            Mobility::Static,
        )
        .unwrap();
    let observed = observer.observe(&device, &queue, &mut scene, &rays);
    assert_eq!(f32::from_bits(observed[0].nearest[4]), 2.);
    scene.remove_instance(first).unwrap();
    scene
        .add_instance(
            &device,
            &queue,
            state(far, Mat4::IDENTITY, true),
            Mobility::Static,
        )
        .unwrap();
    // The abandoned frame: its rays updated and a trace encoded, never
    // submitted, and the frame never finished.
    scene.update_rays(&device, &queue, 0, false);
    let output = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: std::mem::size_of::<Observed>() as u64,
        usage: wgpu::BufferUsages::STORAGE,
        mapped_at_creation: false,
    });
    let mut abandoned = device.create_command_encoder(&Default::default());
    observer.encode(&device, &mut abandoned, &scene, &rays, &output);
    drop(abandoned);
    let observed = observer.observe(&device, &queue, &mut scene, &rays);
    assert_eq!(observed[0].nearest[0], 1);
    assert_eq!(f32::from_bits(observed[0].nearest[4]), 6.);
}

// Plausible defect: every entry set while no frame traces (world-space
// reflections off) queues another upload, so the scene retains memory
// without bound and uploads it all when tracing starts. Retained resources
// follow content: at most one pending entry per instance, and the frame
// that traces next shows each instance's last pose.
#[test]
fn entries_set_over_untraced_frames_stay_one_per_instance() {
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    let model = scene
        .add_asset(
            &device,
            &queue,
            asset(vec![triangle(0., 0., false, 0)], true),
        )
        .unwrap()
        .model;
    let at = |instance: usize, depth: f32| {
        Mat4::from_translation(Vec3::new(instance as f32 * 4., 0., -depth))
    };
    let instances: Vec<_> = (0..32)
        .map(|instance| {
            scene
                .add_instance(
                    &device,
                    &queue,
                    state(model, at(instance, 1.), true),
                    Mobility::Moving,
                )
                .unwrap()
        })
        .collect();
    for frame in 0..200 {
        for (instance, &id) in instances.iter().enumerate() {
            let depth = 1. + (frame % 7) as f32 + instance as f32 * 0.01;
            scene
                .set_instance(&queue, id, state(model, at(instance, depth), true))
                .unwrap();
        }
        scene.finish_frame();
    }
    assert!(
        scene.ray_instances.pending() <= instances.len(),
        "{} entries pending for {} instances",
        scene.ray_instances.pending(),
        instances.len()
    );
    let observer = Observer::new(&device, &scene);
    let rays: Vec<_> = (0..instances.len())
        .map(|instance| down(Vec3::new(instance as f32 * 4., 0., 0.), 20.))
        .collect();
    let observed = observer.observe(&device, &queue, &mut scene, &rays);
    for (instance, observed) in observed.iter().enumerate() {
        let depth = 1. + (199 % 7) as f32 + instance as f32 * 0.01;
        assert_eq!(observed.nearest[0], 1, "instance {instance}");
        assert!(
            (f32::from_bits(observed.nearest[4]) - depth).abs() < 1e-5,
            "instance {instance} at {}, expected {depth}",
            f32::from_bits(observed.nearest[4])
        );
    }
    assert_eq!(
        scene.ray_instances.pending(),
        0,
        "a traced frame uploads them"
    );
}

// Plausible defects: a move of the render origin that leaves instance
// entries, the static instance BVH (which no static edit rebuilds) or the
// moving one in the old render frame. The oracle is the same rays expressed
// in the new frame: they meet the same instances, triangles and distances.
#[test]
fn rays_meet_the_same_geometry_after_a_render_origin_move() {
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    let model = scene
        .add_asset(
            &device,
            &queue,
            asset(vec![triangle(0., 0., false, 0)], true),
        )
        .unwrap()
        .model;
    let mut instances = Vec::new();
    for (index, mobility) in [Mobility::Static, Mobility::Moving, Mobility::Static]
        .into_iter()
        .enumerate()
    {
        let pose = Mat4::from_translation(Vec3::new(index as f32 * 4. + 2000., 0., -3.));
        let id = scene
            .add_instance(&device, &queue, state(model, pose, true), mobility)
            .unwrap();
        instances.push(id.index() as u32);
    }
    let observer = Observer::new(&device, &scene);
    let rays = |origin: Vec3| -> Vec<TestRay> {
        (0..3)
            .map(|index| down(Vec3::new(index as f32 * 4. + 2000., 0., 0.) - origin, 10.))
            .collect()
    };
    let before = observer.observe(&device, &queue, &mut scene, &rays(Vec3::ZERO));
    let by = Vec3::new(1024.75, 0., -512.);
    scene.move_origin(&device, &queue, by).unwrap();
    scene.finish_frame();
    let after = observer.observe(&device, &queue, &mut scene, &rays(by));
    for (index, (before, after)) in before.iter().zip(&after).enumerate() {
        assert_eq!(
            before.nearest[..4],
            [1, instances[index], 0, 0],
            "ray {index}"
        );
        assert_eq!(after.nearest[..4], before.nearest[..4], "ray {index}");
        assert_eq!(
            f32::from_bits(after.nearest[4]),
            f32::from_bits(before.nearest[4]),
            "ray {index}"
        );
    }
}
