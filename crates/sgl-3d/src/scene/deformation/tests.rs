//! Deformation on a real GPU: the deform stage's vertices against the CPU
//! morph and skin blends that define them (glTF 2.0's skinning and morph
//! target equations), and what a deforming instance draws: its motion, its
//! culling and its shadow.
use super::DeformedMesh;
use crate::asset::{Asset, CpuMesh, Vertex};
use crate::content::identity::Identity;
use crate::deformation::{Influence, MeshDeformation, MorphDelta, MorphTarget};
use crate::renderer::Renderer;
use crate::settings::{self, Settings};
use crate::shading::gbuffer;
use crate::stages::deform::Deform;
use crate::test_support;
use crate::{Camera, FrameInput, InstanceId, InstanceState, Mobility, ModelId, Scene, SceneError};
use glam::{Mat3, Mat4, Quat, Vec3, Vec4};

/// A quad facing +Z at depth `z` around (`x`, 0), `half` metres to each
/// side, with varied normals and tangents of both handedness.
fn quad(x: f32, z: f32, half: f32, deformation: MeshDeformation) -> CpuMesh {
    let corners = [(-1., -1.), (1., -1.), (1., 1.), (-1., 1.)];
    CpuMesh {
        vertices: corners
            .iter()
            .enumerate()
            .map(|(i, &(u, v))| Vertex {
                position: [x + u * half, v * half, z],
                normal: Vec3::new(0.3 * u, 0.2 * v, 1.).normalize().to_array(),
                uv: [(u + 1.) / 2., (1. - v) / 2.],
                color: [1.; 4],
                lightmap_uv: [0.; 2],
                lightmap_bounds: [0., 0., 1., 1.],
                tangent: [1., 0., -0.3 * u, if i == 2 { -1. } else { 1. }],
            })
            .collect(),
        indices: vec![0, 1, 2, 0, 2, 3],
        material: 0,
        deformation,
    }
}

/// An asset of `meshes` with the fixture material.
fn asset(meshes: Vec<CpuMesh>) -> Asset {
    let mut asset = test_support::cube();
    asset.meshes = meshes;
    asset
}

fn moving(
    scene: &mut Scene,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    model: ModelId,
) -> InstanceId {
    let state = InstanceState {
        model,
        pose: Mat4::IDENTITY,
        visible: true,
        capture_visible: true,
    };
    scene
        .add_instance(device, queue, state, Mobility::Moving)
        .unwrap()
}

/// Each vertex of `instance` as the deform stage writes it for the next
/// frame: position, normal and tangent, in its model's vertex order.
fn deformed(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    scene: &mut Scene,
    instance: InstanceId,
    vertices: u32,
) -> Vec<(Vec3, Vec3, Vec4)> {
    scene.prepare_frame(device, queue, Vec3::ZERO);
    let mut encoder = device.create_command_encoder(&Default::default());
    Deform::new(device).dispatch(device, queue, &mut encoder, scene, None);
    queue.submit([encoder.finish()]);
    let words = crate::test_support::read_words(device, queue, scene.rays.source());
    let f = |at: u32| f32::from_bits(words[at as usize]);
    let v3 = |at: u32| Vec3::new(f(at), f(at + 1), f(at + 2));
    let [positions, _, normals] = scene
        .instances
        .slots
        .at(instance.index())
        .unwrap()
        .deformation
        .as_ref()
        .unwrap()
        .record;
    (0..vertices)
        .map(|v| {
            let frame = normals + v * 7;
            (
                v3(positions + v * 3),
                v3(frame),
                v3(frame + 3).extend(f(frame + 6)),
            )
        })
        .collect()
}

fn close(a: Vec4, b: Vec4) -> bool {
    (a - b).abs().max_element() <= 2e-5 * b.abs().max_element().max(1.)
}

/// A skinned and morphed mesh, then a morphed mesh after its vertices, with
/// out-of-order weight indices and unnormalized weights, and joints with
/// nonuniform scale (and one more than the influences name).
fn fixture() -> (Vec<CpuMesh>, [Mat4; 5]) {
    let displacement = |seed: f32| {
        (0..4)
            .map(|v| {
                let s = seed + v as f32;
                MorphDelta {
                    position: [0.1 * s, -0.05 * s, 0.02 * s * s],
                    normal: [0.03 * s, 0.01, -0.02 * s],
                    tangent: [0., 0.04 * s, 0.01],
                }
            })
            .collect::<Vec<_>>()
    };
    let targets = vec![
        MorphTarget {
            weight: 1,
            deltas: displacement(1.),
        },
        MorphTarget {
            weight: 0,
            deltas: displacement(-2.),
        },
    ];
    let influences = vec![
        Influence {
            joints: [0, 0, 0, 0],
            weights: [1., 0., 0., 0.],
        },
        Influence {
            joints: [0, 1, 0, 0],
            weights: [0.25, 0.75, 0., 0.],
        },
        // Unnormalized: the scene normalizes.
        Influence {
            joints: [1, 2, 3, 0],
            weights: [2., 1., 1., 0.],
        },
        Influence {
            joints: [3, 2, 1, 0],
            weights: [0.1, 0.2, 0.3, 0.4],
        },
    ];
    let skinned = MeshDeformation {
        influences,
        morph_targets: targets.clone(),
    };
    let morphed = MeshDeformation {
        influences: Vec::new(),
        morph_targets: targets,
    };
    let joints = [
        Mat4::from_scale_rotation_translation(
            Vec3::ONE,
            Quat::from_rotation_y(0.3),
            Vec3::new(0.5, 0., 0.),
        ),
        Mat4::from_scale_rotation_translation(
            Vec3::new(2., 0.5, 1.),
            Quat::from_rotation_x(0.7),
            Vec3::new(0., 1., -1.),
        ),
        Mat4::from_scale_rotation_translation(
            Vec3::splat(1.5),
            Quat::from_rotation_z(-0.4),
            Vec3::Z * 0.3,
        ),
        Mat4::from_translation(Vec3::Y * -0.2),
        // Beyond those the influences name: ignored.
        Mat4::ZERO,
    ];
    (
        vec![quad(0., -2., 0.5, skinned), quad(1., -3., 0.25, morphed)],
        joints,
    )
}

/// `mesh`'s vertices under `joints` and `weights` by glTF 2.0's definition:
/// each displaced by its targets at their weights, then transformed by the
/// weighted sum of its joint matrices (normals by its inverse transpose).
fn cpu_blend(mesh: &CpuMesh, joints: &[Mat4], weights: &[f32]) -> Vec<(Vec3, Vec3, Vec4)> {
    let deformation = &mesh.deformation;
    mesh.vertices
        .iter()
        .enumerate()
        .map(|(v, vertex)| {
            let mut position = Vec3::from_array(vertex.position);
            let mut normal = Vec3::from_array(vertex.normal);
            let tangent = Vec4::from_array(vertex.tangent);
            let mut direction = tangent.truncate();
            for target in &deformation.morph_targets {
                let weight = weights[target.weight as usize];
                let delta = target.deltas[v];
                position += weight * Vec3::from_array(delta.position);
                normal += weight * Vec3::from_array(delta.normal);
                direction += weight * Vec3::from_array(delta.tangent);
            }
            let Some(influence) = deformation.influences.get(v) else {
                return (position, normal, direction.extend(tangent.w));
            };
            let sum: f32 = influence.weights.iter().sum();
            let matrix = influence
                .joints
                .iter()
                .zip(influence.weights)
                .fold(Mat4::ZERO, |m, (&joint, weight)| {
                    m + joints[joint as usize] * (weight / sum)
                });
            let linear = Mat3::from_mat4(matrix);
            (
                matrix.transform_point3(position),
                (linear.inverse().transpose() * normal).normalize(),
                (linear * direction).normalize().extend(tangent.w),
            )
        })
        .collect()
}

// Plausible defects: the deform stage reads a joint, weight, influence or
// morph displacement from the wrong word (another vertex's, another mesh's,
// the wrong target's weight), multiplies matrices in the wrong order or
// layout, skips weight normalization, skins before morphing, transforms
// normals by the matrix instead of its inverse transpose, or writes one
// mesh's vertices over another's. The oracle is glTF 2.0's definition
// evaluated on the CPU from the same inputs (`cpu_blend`).
#[test]
fn deformed_vertices_match_the_cpu_morph_and_skin_blends() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let (meshes, joints) = fixture();
    let mut scene = Scene::new(&device, &queue);
    let model = scene
        .add_asset(&device, &queue, asset(meshes.clone()))
        .unwrap()
        .model;
    let instance = moving(&mut scene, &device, &queue, model);
    let weights = [0.5, -0.25];
    scene
        .set_instance_deformation(&queue, instance, &joints, &weights)
        .unwrap();
    let observed = deformed(&device, &queue, &mut scene, instance, 8);
    for (mesh_index, mesh) in meshes.iter().enumerate() {
        for (v, expected) in cpu_blend(mesh, &joints, &weights).into_iter().enumerate() {
            let (p, n, t) = observed[mesh_index * 4 + v];
            assert!(
                close(p.extend(0.), expected.0.extend(0.))
                    && close(n.extend(0.), expected.1.extend(0.))
                    && close(t, expected.2),
                "mesh {mesh_index} vertex {v}: deformed {:?}, expected {expected:?}",
                (p, n, t)
            );
        }
    }
    // Fewer joints or weights than the model takes are refused, and change
    // nothing.
    for (joints, weights) in [(&joints[..3], &weights[..]), (&joints[..], &weights[..1])] {
        assert!(matches!(
            scene.set_instance_deformation(&queue, instance, joints, weights),
            Err(SceneError::DeformationMismatch)
        ));
    }
    assert_eq!(deformed(&device, &queue, &mut scene, instance, 8), observed);
}

// Plausible defects: the skinned bounds leave out a joint's vertices, use a
// joint's matrix for another joint's box, or grow by morph displacements
// without the sign of a negative weight, so culling drops a visible
// deformed mesh. The oracle is the CPU blend (`cpu_blend`): every deformed
// vertex lies within its mesh's bounds.
#[test]
fn skinned_and_morphed_bounds_hold_every_deformed_vertex() {
    let (meshes, joints) = fixture();
    let swung =
        joints.map(|joint| Mat4::from_rotation_z(1.2) * joint * Mat4::from_rotation_x(-0.9));
    for joints in [joints, swung] {
        for weights in [[0.5, -0.25], [-1.5, 2.], [0., 0.]] {
            for mesh in &meshes {
                let [low, high] =
                    DeformedMesh::new(&mesh.vertices, &mesh.deformation).bounds(&joints, &weights);
                let slack = 1e-5 * low.abs().max(high.abs()).max_element().max(1.);
                for (position, _, _) in cpu_blend(mesh, &joints, &weights) {
                    assert!(
                        position.cmpge(low - slack).all() && position.cmple(high + slack).all(),
                        "{position} outside {low}..{high} at weights {weights:?}"
                    );
                }
            }
        }
    }
}

// Plausible defects: a deforming instance is accepted as static, swaps its
// model, or takes part in levels of detail, whose cached or alternative
// geometry would not show its deformation.
#[test]
fn deforming_models_take_only_moving_instances_that_keep_their_model() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    let skin = MeshDeformation {
        influences: vec![
            Influence {
                joints: [0; 4],
                weights: [1., 0., 0., 0.],
            };
            4
        ],
        morph_targets: Vec::new(),
    };
    let deforming = scene
        .add_asset(&device, &queue, asset(vec![quad(0., -2., 1., skin)]))
        .unwrap()
        .model;
    let rigid = scene
        .add_asset(
            &device,
            &queue,
            asset(vec![quad(0., -2., 1., MeshDeformation::default())]),
        )
        .unwrap()
        .model;
    let state = |model| InstanceState {
        model,
        pose: Mat4::IDENTITY,
        visible: true,
        capture_visible: true,
    };
    assert!(matches!(
        scene.add_instance(&device, &queue, state(deforming), Mobility::Static),
        Err(SceneError::DeformingModel)
    ));
    let instance = moving(&mut scene, &device, &queue, deforming);
    let other = moving(&mut scene, &device, &queue, rigid);
    for (id, model) in [(instance, rigid), (other, deforming)] {
        assert!(matches!(
            scene.set_instance(&queue, id, state(model)),
            Err(SceneError::DeformingModel)
        ));
    }
    assert!(matches!(
        scene.set_instance_deformation(&queue, other, &[Mat4::IDENTITY], &[]),
        Err(SceneError::DeformationMismatch)
    ));
    let lod = crate::lod::MeshLod {
        model: rigid,
        mesh: 0,
        max_error: 0.1,
    };
    assert!(matches!(
        scene.set_mesh_lods(deforming, 0, vec![lod]),
        Err(SceneError::DeformingModel)
    ));
}

const SIZE: [u32; 2] = [32, 32];

/// A renderer without antialiasing's jitter or post effects, and a camera
/// at the origin looking down -Z.
fn camera_frames(device: &wgpu::Device, queue: &wgpu::Queue) -> (Renderer, Settings, FrameInput) {
    let settings = Settings {
        antialiasing: settings::Antialiasing::Off,
        bloom: settings::Bloom::Off,
        atmosphere: false,
        ..Settings::default()
    };
    let renderer = Renderer::for_test(device, queue, SIZE, &settings);
    let input = FrameInput::new(Camera {
        view: Mat4::IDENTITY,
        projection: crate::perspective(1., 1., 0.1),
        eye: Vec3::ZERO,
    });
    (renderer, settings, input)
}

// Plausible defects: the camera passes draw the model's bind pose, or the
// deformed position for the previous one, so a moving joint writes no or
// the wrong motion; a frame after it, posed the same or not posed at all,
// keeps a stale slot, so a joint that stopped still writes motion or the
// plane is drawn elsewhere; or culling tests the bind pose's bounds, which
// here lie outside the view, and draws nothing. The oracle is the
// joint's displacement projected by the camera: a plane facing the camera
// translated within it moves every covered pixel by the same screen offset.
#[test]
fn a_moving_joint_writes_its_screen_displacement_as_motion() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let (mut renderer, settings, input) = camera_frames(&device, &queue);
    let mut scene = Scene::new(&device, &queue);
    // Bound to one joint, at bind ten metres to the right, out of view.
    let skin = MeshDeformation {
        influences: vec![
            Influence {
                joints: [0; 4],
                weights: [1., 0., 0., 0.],
            };
            4
        ],
        morph_targets: Vec::new(),
    };
    let model = scene
        .add_asset(&device, &queue, asset(vec![quad(10., -4., 1., skin)]))
        .unwrap()
        .model;
    let instance = moving(&mut scene, &device, &queue, model);
    let output = crate::view::targets::target(&device, "skinned frames", SIZE, gbuffer::COLOR);
    let step = Vec3::new(0.2, 0.1, 0.);
    let joint = |at: Vec3| Mat4::from_translation(Vec3::X * -10. + at);
    // The motion and depth at the centre of a frame that poses the joint at
    // `at`, or leaves it as the last frame posed it.
    let mut frame = |scene: &mut Scene, at: Option<Vec3>| {
        if let Some(at) = at {
            scene
                .set_instance_deformation(&queue, instance, &[joint(at)], &[])
                .unwrap();
        }
        let mut encoder = device.create_command_encoder(&Default::default());
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
        let motion = test_support::read(&device, &queue, targets.motion.texture(), 4);
        let depth = test_support::read(&device, &queue, targets.depth.texture(), 4);
        let texel = ((SIZE[1] / 2) * SIZE[0] + SIZE[0] / 2) as usize * 4;
        (
            [
                test_support::half(&motion[texel..texel + 2]),
                test_support::half(&motion[texel + 2..texel + 4]),
            ],
            f32::from_le_bytes(depth[texel..texel + 4].try_into().unwrap()),
        )
    };
    frame(&mut scene, Some(Vec3::ZERO));
    let (observed, posed_depth) = frame(&mut scene, Some(step));
    let uv = |p: Vec3| {
        let clip = input.camera.projection * input.camera.view * p.extend(1.);
        [clip.x / clip.w * 0.5 + 0.5, clip.y / clip.w * -0.5 + 0.5]
    };
    let (now, then) = (
        uv(Vec3::new(0., 0., -4.)),
        uv(Vec3::new(0., 0., -4.) - step),
    );
    let expected = [now[0] - then[0], now[1] - then[1]];
    for axis in 0..2 {
        assert!(
            (observed[axis] - expected[axis]).abs() <= 2e-3 * expected[axis].abs().max(0.01),
            "motion {observed:?}, expected {expected:?}"
        );
    }
    assert!(posed_depth > 0., "the posed plane was not drawn");
    // Not posed at all, while the other slot holds the earlier pose, then
    // posed again where it was: no motion, and the plane is still drawn
    // where it was posed, not at its bind pose.
    for at in [None, Some(step)] {
        assert_eq!(
            frame(&mut scene, at),
            ([0.; 2], posed_depth),
            "a joint that stayed ({at:?}) wrote motion or moved"
        );
    }
}

// Plausible defects: shadow casters draw the model's bind-pose positions, or
// read the deformed positions at the wrong offset of the scene source. The
// oracle is the depth an orthographic cascade stores for the deformed plane.
#[test]
fn deformed_casters_cast_at_their_deformed_depth() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    const DEPTH: u32 = 16;
    let depth = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("deformed caster depth"),
        size: wgpu::Extent3d {
            width: DEPTH,
            height: DEPTH,
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
    let mut renderer = Renderer::for_test(&device, &queue, [DEPTH; 2], &Settings::default());
    let mut frame = FrameInput::new(Camera {
        view: Mat4::from_translation(Vec3::X * 100.),
        projection: crate::perspective(1., 1., 0.1),
        eye: Vec3::ZERO,
    });
    frame.directional_lights[0] = Some(crate::DirectionalLight {
        direction: Vec3::NEG_Z,
        color: [1.; 3],
        illuminance: 1.,
        shadow: None,
    });
    let mut scene = Scene::new(&device, &queue);
    // A rigid mesh before the deforming one, so its positions start past
    // the model's first vertex.
    let skin = MeshDeformation {
        influences: vec![
            Influence {
                joints: [0; 4],
                weights: [1., 0., 0., 0.],
            };
            4
        ],
        morph_targets: Vec::new(),
    };
    let meshes = vec![
        quad(5., 0.9, 0.1, MeshDeformation::default()),
        quad(0., 0.9, 2., skin),
    ];
    let model = scene
        .add_asset(&device, &queue, asset(meshes))
        .unwrap()
        .model;
    let instance = moving(&mut scene, &device, &queue, model);
    // Bind depth 0.9 in the cascade's clip space; posed to 0.4.
    scene
        .set_instance_deformation(
            &queue,
            instance,
            &[Mat4::from_translation(Vec3::Z * -0.5)],
            &[],
        )
        .unwrap();
    let prepared =
        renderer.prepare_test_frame(&device, &queue, &mut scene, &frame, &Settings::default());
    renderer.set_test_cascade((&device, &queue), &scene, &prepared, Mat4::IDENTITY);
    let mut encoder = device.create_command_encoder(&Default::default());
    Deform::new(&device).dispatch(&device, &queue, &mut encoder, &scene, None);
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
    for (i, &depth) in observed.iter().enumerate() {
        assert!(
            (depth - 0.4).abs() < 1e-6,
            "texel {i}: depth {depth}, expected the deformed plane's 0.4"
        );
    }
}
