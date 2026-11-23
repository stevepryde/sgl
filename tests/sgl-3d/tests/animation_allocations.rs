//! A game that animates allocates nothing per frame once its buffers have
//! their size: sampling, blending, local and joint matrices, and posing an
//! instance. The oracle is the heap itself: this binary's global allocator
//! counts the allocations the measuring thread makes, and the requirement
//! is that a steady-state frame makes none, apart from those of the GPU
//! upload wgpu makes for SGL.
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use sgl_3d::animation::{Pose, Wrap};
use sgl_3d::deformation::{
    Channel, ChannelValues, Clip, Interpolation, Joint, JointMatrices, MorphWeight, Node, Rig,
    Transform,
};
use sgl_3d::glam::{Mat4, Quat, Vec3};
use wasm_bindgen_test::wasm_bindgen_test;

/// The system allocator, counting the allocations of a thread while it
/// measures.
struct Counting;

thread_local! {
    static MEASURING: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

fn count() {
    // `try_with`: a thread's locals may be gone while it exits.
    let _ = MEASURING.try_with(|measuring| {
        if measuring.get() {
            ALLOCATIONS.with(|allocations| allocations.set(allocations.get() + 1));
        }
    });
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count();
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        count();
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        count();
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// The allocations `work` makes on this thread.
fn allocations(work: impl FnOnce()) -> usize {
    ALLOCATIONS.with(|allocations| allocations.set(0));
    MEASURING.with(|measuring| measuring.set(true));
    work();
    MEASURING.with(|measuring| measuring.set(false));
    ALLOCATIONS.with(Cell::get)
}

/// Nodes in a humanoid's shape: a spine of six, two arms and two legs of
/// five, and fingers, 61 in all, built children before parents so composing
/// them walks ancestors that are not yet composed. Every node is a joint and
/// every node is animated; the head carries four morph weights.
const NODES: usize = 61;

fn rig() -> Rig {
    // Parents in top-down order: the spine, then limbs off spine nodes.
    let mut parents: Vec<Option<usize>> = vec![None];
    parents.extend((1..6).map(|i| Some(i - 1)));
    for root in [4, 4, 0, 0] {
        let first = parents.len();
        parents.push(Some(root));
        parents.extend((first + 1..first + 5).map(|i| Some(i - 1)));
    }
    while parents.len() < NODES {
        // Three-bone fingers off the arms' last nodes.
        let hand = [10, 15][parents.len() % 2];
        let first = parents.len();
        parents.push(Some(hand));
        parents.push(Some(first));
        parents.push(Some(first + 1));
    }
    parents.truncate(NODES);
    // Reversed, so children come before parents.
    let order = |node: usize| NODES - 1 - node;
    let mut nodes: Vec<Node> = (0..NODES)
        .map(|_| Node {
            name: None,
            parent: None,
            rest: Transform::IDENTITY,
        })
        .collect();
    for (node, parent) in parents.iter().enumerate() {
        nodes[order(node)] = Node {
            name: None,
            parent: parent.map(order),
            rest: Transform {
                translation: Vec3::new(0., 0.2, 0.01 * node as f32),
                ..Transform::IDENTITY
            },
        };
    }
    let channel = |node: usize, phase: f32| -> [Channel; 2] {
        let times = vec![0., 0.5, 1.];
        [
            Channel {
                node,
                interpolation: Interpolation::Linear,
                times: times.clone(),
                values: ChannelValues::Rotation(
                    [0., 0.4, 0.]
                        .map(|angle| Quat::from_rotation_z(angle + phase))
                        .to_vec(),
                ),
            },
            Channel {
                node,
                interpolation: Interpolation::CubicSpline,
                times,
                values: ChannelValues::Translation(vec![Vec3::new(0., 0.2, phase); 9]),
            },
        ]
    };
    let clip = |phase: f32| Clip {
        name: None,
        channels: (0..NODES)
            .flat_map(|node| channel(node, phase))
            .chain([Channel {
                node: order(5),
                interpolation: Interpolation::Linear,
                times: vec![0., 1.],
                values: ChannelValues::MorphWeights(vec![0., 0., 0., 0., 1., 0.5, 0.25, phase]),
            }])
            .collect(),
    };
    Rig {
        joints: (0..NODES)
            .map(|node| Joint {
                node,
                inverse_bind: Mat4::from_translation(Vec3::new(0., -0.1, 0.)),
            })
            .collect(),
        nodes,
        morph_weights: (0..4)
            .map(|_| MorphWeight {
                node: order(5),
                rest: 0.,
            })
            .collect(),
        clips: vec![clip(0.), clip(0.3)],
    }
}

/// One frame of the documented path for one instance: two clips sampled,
/// crossfaded and layered, then composed into joint matrices.
struct Animator {
    walk: Pose,
    run: Pose,
    reference: Pose,
    locals: Vec<Mat4>,
    joints: JointMatrices,
}

impl Animator {
    fn new(rig: &Rig) -> Self {
        Self {
            walk: Pose::rest(rig),
            run: Pose::rest(rig),
            reference: Pose::rest(rig),
            locals: Vec::new(),
            joints: JointMatrices::new(),
        }
    }

    fn frame(&mut self, rig: &Rig, time: f32) {
        self.walk.sample(rig, &rig.clips[0], time, Wrap::Loop);
        self.run.sample(rig, &rig.clips[1], time * 1.3, Wrap::Loop);
        self.reference.sample(rig, &rig.clips[1], 0., Wrap::Clamp);
        self.walk.blend(&self.run, 0.4);
        self.walk.add(&self.run, &self.reference, 0.2);
        self.walk.local_matrices(&mut self.locals);
        rig.joint_matrices(&self.locals, &mut self.joints)
            .expect("the pose is the rig's");
    }
}

// Plausible defects: composing joint matrices into a fresh vector, or
// fresh scratch for node transforms or ancestor walks, on every call;
// sampling or blending reallocating a pose's buffers.
#[wasm_bindgen_test(unsupported = test)]
fn a_steady_animation_frame_allocates_nothing() {
    let rig = rig();
    let mut animator = Animator::new(&rig);
    // The first frame sizes the buffers.
    animator.frame(&rig, 0.);
    assert_eq!(animator.joints.matrices().len(), NODES);
    let made = allocations(|| {
        for frame in 1..30 {
            animator.frame(&rig, frame as f32 / 30.);
        }
    });
    assert_eq!(made, 0, "allocations in 29 steady frames");
}

// Plausible defect: the allocation count missing allocations, so the test
// above passes whatever the path does. A vector's first push allocates.
#[wasm_bindgen_test(unsupported = test)]
fn the_count_sees_an_allocation() {
    let made = allocations(|| {
        std::hint::black_box(vec![std::hint::black_box(1u8)]);
    });
    assert_eq!(made, 1);
}

#[cfg(not(target_arch = "wasm32"))]
mod gpu {
    use super::*;
    use sgl_3d::asset::{Asset, CpuMesh, Material, Vertex};
    use sgl_3d::deformation::{Influence, MeshDeformation, MorphDelta, MorphTarget};
    use sgl_3d::{InstanceState, Mobility, Scene};

    /// A skinned, morphed strip of `NODES` vertices, one per joint.
    fn asset(rig: Rig) -> Asset {
        let vertex = |i: usize| Vertex {
            position: [i as f32 * 0.1, (i % 2) as f32, 0.],
            normal: [0., 0., 1.],
            uv: [0.; 2],
            tangent: [1., 0., 0., 1.],
            color: [1.; 4],
            lightmap_uv: [0.; 2],
            lightmap_bounds: [0., 0., 1., 1.],
        };
        Asset {
            meshes: vec![CpuMesh {
                vertices: (0..NODES).map(vertex).collect(),
                indices: (0..NODES as u32 - 2)
                    .flat_map(|i| [i, i + 1, i + 2])
                    .collect(),
                material: 0,
                deformation: MeshDeformation {
                    influences: (0..NODES as u32)
                        .map(|joint| Influence {
                            joints: [joint, 0, 0, 0],
                            weights: [1., 0., 0., 0.],
                        })
                        .collect(),
                    morph_targets: (0..4)
                        .map(|weight| MorphTarget {
                            weight,
                            deltas: vec![
                                MorphDelta {
                                    position: [0., 0., 0.1],
                                    ..Default::default()
                                };
                                NODES
                            ],
                        })
                        .collect(),
                },
            }],
            materials: vec![Material::default()],
            images: Vec::new(),
            rig,
            ignored: Vec::new(),
            lights: Default::default(),
            instanced: Vec::new(),
        }
    }

    // Plausible defects: posing an instance building its upload words or
    // its meshes' deformed bounds in fresh vectors every call. Posing
    // uploads the joint matrices and morph weights with one
    // `Queue::write_buffer`, which allocates its staging buffer in wgpu;
    // the oracle is as many writes of as many bytes made directly.
    #[test]
    fn posing_an_instance_every_frame_allocates_nothing() {
        let adapter =
            pollster::block_on(wgpu::Instance::default().request_adapter(&Default::default()));
        let Ok(adapter) = adapter else {
            assert!(
                !std::env::var("SGL_REQUIRE_GPU").is_ok_and(|v| !v.is_empty() && v != "0"),
                "GPU required"
            );
            eprintln!("skipping GPU test: no adapter");
            return;
        };
        let (device, queue) =
            pollster::block_on(adapter.request_device(&Default::default())).unwrap();
        let rig = rig();
        let mut scene = Scene::new(&device, &queue);
        let model = scene
            .add_asset(&device, &queue, asset(rig.clone()))
            .unwrap()
            .model;
        let instance = scene
            .add_instance(&device, &queue, InstanceState::new(model), Mobility::Moving)
            .unwrap();
        let mut animator = Animator::new(&rig);
        let mut frame = |time: f32| {
            animator.frame(&rig, time);
            scene
                .set_instance_deformation(
                    &queue,
                    instance,
                    animator.joints.matrices(),
                    &animator.walk.morph_weights,
                )
                .unwrap();
        };
        // Each frame is submitted, as a game's is, outside the count: wgpu
        // keeps its writes until a submission.
        let submit = || {
            queue.submit([]);
            device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        };
        frame(0.);
        submit();
        let mut made = 0;
        for step in 1..30 {
            made += allocations(|| frame(step as f32 / 30.));
            submit();
        }
        // Each joint's matrix and each morph weight, as 32-bit words.
        let upload = (NODES * 16 + 4) * 4;
        let target = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: upload as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let words = vec![0u8; upload];
        queue.write_buffer(&target, 0, &words);
        submit();
        let mut written = 0;
        for _ in 1..30 {
            written += allocations(|| queue.write_buffer(&target, 0, &words));
            submit();
        }
        assert_eq!(
            made, written,
            "allocations in 29 steady frames against 29 bare uploads"
        );
    }
}
