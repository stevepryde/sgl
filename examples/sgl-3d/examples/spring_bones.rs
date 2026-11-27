//! Secondary motion: a body with a head, a ponytail and a cape, generated
//! here as a skinned GLB so that no asset file or licence is needed, walks,
//! stops and turns, and the ponytail and the cape swing by `spring_bones`
//! around a sphere on the head and a capsule on the torso. The cape is six
//! chains side by side, held together as one `Sheet`. Each frame the game
//! samples its pose (the rest pose here), steps the chains with the
//! instance's world matrix and its `dt`, then composes the joint matrices
//! and poses the instance as for any skinned model.
//!
//! Run with `cargo run -p sgl-3d-examples --example spring_bones -- target/spring_bones.png`.
//! Optional `--frames N` sets how many frames (at 60 Hz) it animates.
use sgl_3d::glam::camera;
use sgl_3d::{
    Camera, DirectionalLight, DirectionalShadow, FrameInput, HemisphereLight, InstanceState,
    Mobility, Renderer, Scene, View, ViewFrame, ViewOutput,
    animation::Pose,
    asset::{self, Asset, CpuMesh, Material, Vertex},
    deformation::{JointMatrices, Rig},
    glam::{Mat4, Quat, Vec3},
    settings::{RenderPreset, Settings},
    spring_bones::{Chain, Collider, ColliderShape, Sheet, SpringBones},
};
use std::error::Error;

const SEGMENTS: u32 = 16;
/// The neck joint, where the head turns, above the body's origin at its feet.
const NECK: Vec3 = Vec3::new(0., 1.5, 0.);
/// The head sphere's centre, in the head's space, and its radius.
const SKULL: Vec3 = Vec3::new(0., 0.12, 0.);
const SKULL_RADIUS: f32 = 0.14;
/// The torso: a capsule between these points, in the body's space.
const TORSO: [Vec3; 2] = [Vec3::new(0., 0.6, 0.), Vec3::new(0., 1.3, 0.)];
const TORSO_RADIUS: f32 = 0.14;
/// The ponytail's root at the back of the head, in the head's space; its
/// bones hang straight down, and the last runs `TIP` past the last node.
const HAIR_ROOT: Vec3 = Vec3::new(0., 0.08, -0.19);
const HAIR_NODES: u32 = 6;
const HAIR_BONE: f32 = 0.1;
const TIP: f32 = 0.1;
const HAIR_RADIUS: f32 = 0.025;
/// The cape: `CAPE_CHAINS` chains of `CAPE_NODES` nodes hanging from the
/// shoulders behind the torso, `CAPE_ROOT` (in the body's space) the first
/// chain's root and `CAPE_SPACING` apart along X, each node `CAPE_BONE`
/// below the last and the tip as far again. The cape hangs against the
/// torso's back: the line between its two middle chains, 0.16 m behind the
/// torso's axis, lies on the torso's surface grown by the cape's radius,
/// and every other point of the cape is further out, as a sheet's links
/// must clear its colliders.
const CAPE_CHAINS: u32 = 6;
const CAPE_NODES: u32 = 6;
const CAPE_ROOT: Vec3 = Vec3::new(-0.15, 1.36, -0.16);
const CAPE_SPACING: f32 = 0.06;
const CAPE_BONE: f32 = 0.1;
const CAPE_RADIUS: f32 = 0.02;
/// Node indices: the mesh, the body, the head, the hair, then the cape,
/// chain after chain.
const BODY: usize = 1;
const HEAD: usize = 2;
const HAIR: usize = 3;
const CAPE: usize = HAIR + HAIR_NODES as usize;

/// The cape's node `node` of chain `chain`.
fn cape_node(chain: u32, node: u32) -> usize {
    CAPE + (chain * CAPE_NODES + node) as usize
}

/// Where the cape's node `node` of chain `chain` lies at rest in the
/// model; `CAPE_NODES` for the tip past the last.
fn cape_rest(chain: u32, node: u32) -> Vec3 {
    CAPE_ROOT + Vec3::new(chain as f32 * CAPE_SPACING, -(node as f32) * CAPE_BONE, 0.)
}

/// One ring of a surface around the Y axis: its centre, radius, the Y part
/// of its normal, and the joints (indices into the skin) and weights of its
/// vertices.
struct Ring {
    center: Vec3,
    radius: f32,
    normal_y: f32,
    joints: [u16; 2],
    weights: [f32; 2],
}

#[derive(Default)]
struct Surface {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    joints: Vec<[u16; 4]>,
    weights: Vec<[f32; 4]>,
    indices: Vec<u32>,
}

impl Surface {
    /// A tube through `rings`, in order.
    fn tube(&mut self, rings: &[Ring]) {
        let first = self.positions.len() as u32;
        for ring in rings {
            let across = (1. - ring.normal_y * ring.normal_y).max(0.).sqrt();
            for segment in 0..SEGMENTS {
                let angle = segment as f32 / SEGMENTS as f32 * std::f32::consts::TAU;
                let out = Vec3::new(angle.cos(), 0., angle.sin());
                self.positions
                    .push((ring.center + out * ring.radius).to_array());
                self.normals
                    .push((out * across + Vec3::Y * ring.normal_y).to_array());
                self.joints.push([ring.joints[0], ring.joints[1], 0, 0]);
                self.weights
                    .push([ring.weights[0], ring.weights[1], 0., 0.]);
            }
        }
        for ring in 0..rings.len() as u32 - 1 {
            for segment in 0..SEGMENTS {
                let a = first + ring * SEGMENTS + segment;
                let b = first + ring * SEGMENTS + (segment + 1) % SEGMENTS;
                self.indices
                    .extend([a, a + SEGMENTS, b, b, a + SEGMENTS, b + SEGMENTS]);
            }
        }
    }
}

/// A sphere's rings, bottom to top, all on one joint.
fn sphere(center: Vec3, radius: f32, joint: u16) -> Vec<Ring> {
    (0..=12)
        .map(|ring| {
            let polar = std::f32::consts::PI * (1. - ring as f32 / 12.);
            Ring {
                center: center + Vec3::Y * radius * polar.cos(),
                radius: radius * polar.sin(),
                normal_y: polar.cos(),
                joints: [joint, 0],
                weights: [1., 0.],
            }
        })
        .collect()
}

/// The character as a GLB: a torso and a head, each rigid on its bone, a
/// ponytail skinned along its chain, each ring weighted between the two
/// hair bones nearest it, and a double-sided cape, a grid with a vertex on
/// each cape node (and tip) rigid on that node.
fn character() -> Vec<u8> {
    let mut surface = Surface::default();
    // Joints: 0 the body, 1 the head, 2.. the hair, in node order.
    let [bottom, top] = TORSO;
    let torso: Vec<Ring> = sphere(bottom, TORSO_RADIUS, 0)
        .into_iter()
        .take(7)
        .chain(sphere(top, TORSO_RADIUS, 0).into_iter().skip(6))
        .collect();
    surface.tube(&torso);
    surface.tube(&sphere(NECK + SKULL, SKULL_RADIUS, 1));
    let root = NECK + HAIR_ROOT;
    let length = (HAIR_NODES - 1) as f32 * HAIR_BONE + TIP;
    let hair: Vec<Ring> = (0..=24)
        .map(|ring| {
            let down = ring as f32 / 24. * length;
            let bone = (down / HAIR_BONE).min((HAIR_NODES - 1) as f32);
            let lower = (bone.floor() as u16).min(HAIR_NODES as u16 - 2);
            let blend = (bone - f32::from(lower)).min(1.);
            Ring {
                center: root - Vec3::Y * down,
                radius: HAIR_RADIUS * (1. - 0.7 * down / length),
                normal_y: 0.,
                joints: [2 + lower, 3 + lower],
                weights: [1. - blend, blend],
            }
        })
        .collect();
    surface.tube(&hair);
    // The cape's vertices, chain after chain, root to tip.
    let first = surface.positions.len() as u32;
    let at = |chain: u32, node: u32| first + chain * (CAPE_NODES + 1) + node;
    for chain in 0..CAPE_CHAINS {
        for node in 0..=CAPE_NODES {
            surface.positions.push(cape_rest(chain, node).to_array());
            surface.normals.push([0., 0., -1.]);
            let joint = (cape_node(chain, node.min(CAPE_NODES - 1)) - BODY) as u16;
            surface.joints.push([joint, 0, 0, 0]);
            surface.weights.push([1., 0., 0., 0.]);
        }
    }
    let mut cape_indices = Vec::new();
    for chain in 0..CAPE_CHAINS - 1 {
        for node in 0..CAPE_NODES {
            let (a, b) = (at(chain, node), at(chain + 1, node));
            let (c, d) = (at(chain, node + 1), at(chain + 1, node + 1));
            cape_indices.extend([a, b, c, b, d, c]);
        }
    }

    // Each joint's rest position in the model; its inverse bind matrix
    // undoes it, since no rest node is rotated.
    let mut rest = vec![Vec3::ZERO, NECK];
    for node in 0..HAIR_NODES {
        rest.push(root - Vec3::Y * node as f32 * HAIR_BONE);
    }
    for chain in 0..CAPE_CHAINS {
        for node in 0..CAPE_NODES {
            rest.push(cape_rest(chain, node));
        }
    }
    let inverse_binds: Vec<[f32; 16]> = rest
        .iter()
        .map(|&at| Mat4::from_translation(-at).to_cols_array())
        .collect();

    let mut bin: Vec<u8> = Vec::new();
    let mut views = Vec::new();
    let mut accessors = Vec::new();
    let mut add = |bytes: &[u8], component: u32, kind: &str, count: usize| {
        views.push(
            serde_json::json!({"buffer": 0, "byteOffset": bin.len(), "byteLength": bytes.len()}),
        );
        bin.extend_from_slice(bytes);
        accessors.push(serde_json::json!({
            "bufferView": views.len() - 1, "componentType": component, "count": count, "type": kind
        }));
        accessors.len() - 1
    };
    let count = surface.positions.len();
    let position = add(
        bytemuck::cast_slice(&surface.positions),
        5126,
        "VEC3",
        count,
    );
    let normal = add(bytemuck::cast_slice(&surface.normals), 5126, "VEC3", count);
    let joint = add(bytemuck::cast_slice(&surface.joints), 5123, "VEC4", count);
    let weight = add(bytemuck::cast_slice(&surface.weights), 5126, "VEC4", count);
    let index = add(
        bytemuck::cast_slice(&surface.indices),
        5125,
        "SCALAR",
        surface.indices.len(),
    );
    let cape_index = add(
        bytemuck::cast_slice(&cape_indices),
        5125,
        "SCALAR",
        cape_indices.len(),
    );
    let inverse_bind = add(
        bytemuck::cast_slice(&inverse_binds),
        5126,
        "MAT4",
        inverse_binds.len(),
    );
    let mut nodes = vec![
        serde_json::json!({"name": "character", "mesh": 0, "skin": 0}),
        serde_json::json!({
            "name": "body",
            "children": std::iter::once(HEAD)
                .chain((0..CAPE_CHAINS).map(|chain| cape_node(chain, 0)))
                .collect::<Vec<_>>()
        }),
        serde_json::json!({"name": "head", "translation": NECK.to_array(), "children": [HAIR]}),
    ];
    for node in 0..HAIR_NODES {
        let mut hair = serde_json::json!({"name": format!("hair {node}")});
        hair["translation"] = serde_json::json!(if node == 0 {
            HAIR_ROOT.to_array()
        } else {
            [0., -HAIR_BONE, 0.]
        });
        if node + 1 < HAIR_NODES {
            hair["children"] = serde_json::json!([HAIR as u32 + node + 1]);
        }
        nodes.push(hair);
    }
    for chain in 0..CAPE_CHAINS {
        for node in 0..CAPE_NODES {
            let mut cape = serde_json::json!({"name": format!("cape {chain} {node}")});
            cape["translation"] = serde_json::json!(if node == 0 {
                cape_rest(chain, 0).to_array()
            } else {
                [0., -CAPE_BONE, 0.]
            });
            if node + 1 < CAPE_NODES {
                cape["children"] = serde_json::json!([cape_node(chain, node + 1)]);
            }
            nodes.push(cape);
        }
    }
    let joints: Vec<usize> = (BODY..nodes.len()).collect();
    let (min, max) = surface
        .positions
        .iter()
        .fold((Vec3::INFINITY, Vec3::NEG_INFINITY), |(min, max), p| {
            (min.min(Vec3::from(*p)), max.max(Vec3::from(*p)))
        });
    accessors[position]["min"] = serde_json::json!(min.to_array());
    accessors[position]["max"] = serde_json::json!(max.to_array());
    let document = serde_json::json!({
        "asset": {"version": "2.0"},
        "buffers": [{"byteLength": bin.len()}],
        "bufferViews": views,
        "accessors": accessors,
        "materials": [
            {"pbrMetallicRoughness": {"baseColorFactor": [0.75, 0.55, 0.35, 1.], "metallicFactor": 0., "roughnessFactor": 0.6}},
            {"pbrMetallicRoughness": {"baseColorFactor": [0.5, 0.08, 0.1, 1.], "metallicFactor": 0., "roughnessFactor": 0.8}, "doubleSided": true}
        ],
        "meshes": [{"primitives": [
            {
                "attributes": {"POSITION": position, "NORMAL": normal, "JOINTS_0": joint, "WEIGHTS_0": weight},
                "indices": index, "material": 0
            },
            {
                "attributes": {"POSITION": position, "NORMAL": normal, "JOINTS_0": joint, "WEIGHTS_0": weight},
                "indices": cape_index, "material": 1
            }
        ]}],
        "skins": [{"joints": joints, "inverseBindMatrices": inverse_bind}],
        "nodes": nodes,
        "scenes": [{"nodes": [0, BODY]}], "scene": 0
    });
    glb(&document, bin)
}

fn glb(document: &serde_json::Value, mut bin: Vec<u8>) -> Vec<u8> {
    let mut json = serde_json::to_vec(document).expect("a document serializes");
    while !json.len().is_multiple_of(4) {
        json.push(b' ');
    }
    while !bin.len().is_multiple_of(4) {
        bin.push(0);
    }
    let mut bytes = Vec::new();
    for word in [
        0x4654_6c67u32,
        2,
        (28 + json.len() + bin.len()) as u32,
        json.len() as u32,
        0x4e4f_534a,
    ] {
        bytes.extend(word.to_le_bytes());
    }
    bytes.extend(json);
    bytes.extend((bin.len() as u32).to_le_bytes());
    bytes.extend(0x004e_4942u32.to_le_bytes());
    bytes.extend(bin);
    bytes
}

/// The ponytail's chain, the cape's chains as one open sheet, and the head
/// and torso colliders, sized from the same body measurements the mesh is
/// built from.
fn spring_bones(rig: &Rig) -> SpringBones {
    let ponytail = Chain {
        tip: TIP,
        stiffness: 0.6,
        gravity: 0.6,
        radius: HAIR_RADIUS,
        ..Chain::from_root(rig, HAIR)
    };
    let chains: Vec<Chain> = std::iter::once(ponytail)
        .chain((0..CAPE_CHAINS).map(|chain| Chain {
            tip: CAPE_BONE,
            stiffness: 0.4,
            drag: 0.5,
            gravity: 0.8,
            radius: CAPE_RADIUS,
            ..Chain::from_root(rig, cape_node(chain, 0))
        }))
        .collect();
    let cape = Sheet {
        chains: (1..chains.len()).collect(),
        closed: false,
    };
    let colliders = [
        Collider {
            node: HEAD,
            shape: ColliderShape::Sphere {
                center: SKULL,
                radius: SKULL_RADIUS,
            },
        },
        Collider {
            node: BODY,
            shape: ColliderShape::Capsule {
                head: TORSO[0],
                tail: TORSO[1],
                radius: TORSO_RADIUS,
            },
        },
    ];
    SpringBones::new(rig, &chains, &[cape], &colliders).expect("the chains fit the rig")
}

/// Where the body stands at `t` seconds: still, then a walk along +Z at
/// 1.5 m/s that stops dead, then a quarter turn and back.
fn world_at(t: f32) -> Mat4 {
    let walk = (t - 0.3).clamp(0., 1.2) * 1.5;
    let turn = ((t - 2.).clamp(0., 0.5) - (t - 3.).clamp(0., 0.5)) * std::f32::consts::PI;
    Mat4::from_rotation_translation(Quat::from_rotation_y(turn), Vec3::Z * walk)
}

fn ground() -> Asset {
    let mut ground = asset::empty();
    ground.materials.push(Material {
        name: "ground".into(),
        base: [0.35, 0.37, 0.4, 1.],
        metallic: 0.,
        roughness: 0.8,
        ..Default::default()
    });
    ground.meshes.push(CpuMesh {
        vertices: [(-6., -6.), (6., -6.), (6., 6.), (-6., 6.)]
            .map(|(x, z)| Vertex {
                uv1: [0.0; 2],
                position: [x, 0., z],
                normal: [0., 1., 0.],
                uv: [0.; 2],
                color: [1.; 4],
                lightmap_uv: [0.; 2],
                lightmap_bounds: [0., 0., 1., 1.],
                tangent: [0.; 4],
            })
            .to_vec(),
        indices: vec![0, 2, 1, 0, 3, 2],
        material: 0,
        deformation: Default::default(),
    });
    ground
}

async fn run(output_path: std::path::PathBuf, frames: u32) -> Result<(), Box<dyn Error>> {
    let adapter = wgpu::Instance::default()
        .request_adapter(&Default::default())
        .await?;
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            required_features: sgl_3d::graphics_device::features(&adapter),
            required_limits: sgl_3d::graphics_device::limits(&adapter),
            ..Default::default()
        })
        .await?;
    let character = asset::load_slice(&character())?;
    let rig = character.rig.clone();
    let mut scene = Scene::new(&device, &queue);
    let ground = scene.add_asset(&device, &queue, ground())?.model;
    scene.add_instance(
        &device,
        &queue,
        InstanceState::new(ground),
        Mobility::Static,
    )?;
    let model = scene.add_asset(&device, &queue, character)?.model;
    let instance =
        scene.add_instance(&device, &queue, InstanceState::new(model), Mobility::Moving)?;
    let size = [320, 240];
    let output = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("spring bones output"),
        size: wgpu::Extent3d {
            width: size[0],
            height: size[1],
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let output_view = output.create_view(&Default::default());
    let settings = Settings {
        preset: RenderPreset::Low,
        atmosphere: false,
        ..Settings::default()
    };
    let mut renderer = Renderer::new(&device);
    let mut scene_view = View::new(
        &renderer,
        &device,
        &queue,
        output.format(),
        size,
        1.,
        &settings,
    )?;
    let mut input = FrameInput::new(Camera {
        view: Mat4::IDENTITY,
        projection: sgl_3d::perspective(40f32.to_radians(), size[0] as f32 / size[1] as f32, 0.1),
        eye: Vec3::ZERO,
    });
    input.directional_lights[0] = Some(DirectionalLight {
        direction: Vec3::new(-0.5, -1., 0.6),
        color: [1., 0.95, 0.9],
        illuminance: 2.,
        shadow: Some(DirectionalShadow {
            distance: 10.,
            cascades: 2,
        }),
        ..Default::default()
    });
    input.hemisphere_light = HemisphereLight {
        sky_color: [0.2, 0.3, 0.5],
        ground_color: [0.05, 0.04, 0.03],
        intensity: 0.4,
    };
    // Kept between frames: stepping, posing and composing allocate nothing.
    let mut springs = spring_bones(&rig);
    let rest = Pose::rest(&rig);
    let mut pose = rest.clone();
    let mut locals = Vec::new();
    let mut joints = JointMatrices::new();
    let dt = 1. / 60.;
    for frame in 0..frames {
        let seconds = frame as f32 * dt;
        let world = world_at(seconds);
        // The animation's pose first (the rest pose here), then the step.
        // Back to the rest pose in the same buffers (`Vec::clone_from`
        // reuses them; `Pose`'s derived `clone_from` would not).
        pose.nodes.clone_from(&rest.nodes);
        pose.morph_weights.clone_from(&rest.morph_weights);
        springs.step(&rig, &mut pose, world, Vec3::ZERO, dt);
        pose.local_matrices(&mut locals);
        rig.joint_matrices(&locals, &mut joints)?;
        scene.set_instance(
            &queue,
            instance,
            InstanceState {
                pose: world,
                ..InstanceState::new(model)
            },
        )?;
        scene.set_instance_deformation(&queue, instance, joints.matrices(), &[])?;
        // A camera behind the body and to its side, following it.
        let target = world.transform_point3(Vec3::Y * 1.2);
        let eye = target + Vec3::new(1.6, 0.5, -2.2);
        input.camera.view = camera::rh::view::look_at_mat4(eye, target, Vec3::Y);
        input.camera.eye = eye;
        input.elapsed_seconds = f64::from(seconds);
        input.camera_cut = frame == 0;
        scene_view.resize(&device, size, 1., &settings);
        let mut encoder = device.create_command_encoder(&Default::default());
        renderer.render(
            &device,
            &queue,
            &mut encoder,
            &mut scene,
            &mut [ViewFrame {
                view: &mut scene_view,
                input: &input,
                settings: &settings,
                output: ViewOutput::Texture(&output_view),
                timing: None,
            }],
        );
        queue.submit([encoder.finish()]);
        renderer.finish_frame(&mut scene);
    }
    let pixels = read_pixels(&device, &queue, &output)?;
    if let Some(parent) = output_path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    image::save_buffer(
        &output_path,
        &pixels,
        size[0],
        size[1],
        image::ColorType::Rgba8,
    )?;
    println!("Rendered {frames} frames; {}", output_path.display());
    Ok(())
}

fn read_pixels(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
) -> Result<Vec<u8>, Box<dyn Error>> {
    let size = texture.size();
    let row = (size.width * 4).div_ceil(256) * 256;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("spring bones readback"),
        size: u64::from(row * size.height),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row),
                rows_per_image: Some(size.height),
            },
        },
        size,
    );
    queue.submit([encoder.finish()]);
    let (sender, receiver) = std::sync::mpsc::channel();
    buffer.map_async(wgpu::MapMode::Read, .., move |result| {
        let _ = sender.send(result);
    });
    device.poll(wgpu::PollType::wait_indefinitely())?;
    receiver.recv()??;
    let mapped = buffer.get_mapped_range(..).unwrap();
    Ok(mapped
        .chunks(row as usize)
        .flat_map(|line| &line[..(size.width * 4) as usize])
        .copied()
        .collect())
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut output = std::path::PathBuf::from("target/sgl-3d-spring-bones.png");
    let mut frames = 100;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--frames" => frames = args.next().ok_or("--frames requires a count")?.parse()?,
            path => output = path.into(),
        }
    }
    pollster::block_on(run(output, frames))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Plausible defects: the colliders not following the body or the head,
    // placed in the wrong space, or sized differently from the mesh; the
    // ponytail chain built wrongly from the generated rig.
    // Oracle: the body's geometry. Each hair tail (every hair node below the
    // root, and the tip past the last) stays at least the hair's radius
    // outside the head's sphere and the torso's capsule, less 0.1 mm of f32
    // rounding, over the whole run, at the run's 60 Hz.
    #[test]
    fn no_tail_enters_a_collider() {
        let character = asset::load_slice(&character()).expect("the generated character loads");
        let rig = &character.rig;
        let mut springs = spring_bones(rig);
        let mut locals = Vec::new();
        let mut joints = JointMatrices::new();
        let mut closest = f32::INFINITY;
        for frame in 0..300 {
            let world = world_at(frame as f32 / 60.);
            let mut pose = Pose::rest(rig);
            springs.step(rig, &mut pose, world, Vec3::ZERO, 1. / 60.);
            pose.local_matrices(&mut locals);
            rig.joint_matrices(&locals, &mut joints).unwrap();
            // Joints follow the nodes from the body on; each joint matrix
            // times its rest position is its node's place in the model.
            let place = |node: usize, at: Vec3| {
                world.transform_point3(joints.matrices()[node - BODY].transform_point3(at))
            };
            let rest = |index: u32| NECK + HAIR_ROOT - Vec3::Y * index as f32 * HAIR_BONE;
            let mut tails: Vec<Vec3> = (1..HAIR_NODES)
                .map(|index| place(HAIR + index as usize, rest(index)))
                .collect();
            tails.push(place(
                HAIR + HAIR_NODES as usize - 1,
                rest(HAIR_NODES - 1) - Vec3::Y * TIP,
            ));
            let skull = place(HEAD, NECK + SKULL);
            let [bottom, top] = TORSO.map(|point| place(BODY, point));
            for (index, tail) in tails.iter().enumerate() {
                let along = ((*tail - bottom).dot(top - bottom) / (top - bottom).length_squared())
                    .clamp(0., 1.);
                for (name, away, reach) in [
                    ("head", tail.distance(skull), SKULL_RADIUS),
                    (
                        "torso",
                        tail.distance(bottom + (top - bottom) * along),
                        TORSO_RADIUS,
                    ),
                ] {
                    closest = closest.min(away - reach - HAIR_RADIUS);
                    assert!(
                        away >= reach + HAIR_RADIUS - 1e-4,
                        "frame {frame}: hair tail {index} is {away} from the {name}'s collider"
                    );
                }
            }
        }
        // The stop swings the ponytail into the body: it must have touched.
        assert!(
            closest < 1e-3,
            "the ponytail never reached a collider: {closest}"
        );
    }

    // Plausible defects: the colliders placed or sized differently from the
    // mesh for the cape, or the cape's chains built wrongly from the
    // generated rig.
    // Oracle: the body's and the cape's rest geometry. Over the walk at
    // 60 Hz, each cape tail (every cape node below a root, and each tip)
    // stays at least the cape's radius outside the torso's capsule and the
    // head's sphere, less 0.1 mm, while the cape (a tail, or a link between
    // two chains' tails, which hold it off between them) lies within 1 mm
    // of the torso's surface grown by its radius.
    #[test]
    fn the_cape_stays_outside_the_body() {
        let character = asset::load_slice(&character()).expect("the generated character loads");
        let rig = &character.rig;
        let mut springs = spring_bones(rig);
        let mut locals = Vec::new();
        let mut joints = JointMatrices::new();
        let mut closest = f32::INFINITY;
        for frame in 0..300 {
            let world = world_at(frame as f32 / 60.);
            let mut pose = Pose::rest(rig);
            springs.step(rig, &mut pose, world, Vec3::ZERO, 1. / 60.);
            pose.local_matrices(&mut locals);
            rig.joint_matrices(&locals, &mut joints).unwrap();
            // Each joint matrix times its node's rest position is its place.
            let place = |node: usize, at: Vec3| {
                world.transform_point3(joints.matrices()[node - BODY].transform_point3(at))
            };
            let tails: Vec<Vec<Vec3>> = (0..CAPE_CHAINS)
                .map(|chain| {
                    (1..=CAPE_NODES)
                        .map(|node| {
                            place(
                                cape_node(chain, node.min(CAPE_NODES - 1)),
                                cape_rest(chain, node),
                            )
                        })
                        .collect()
                })
                .collect();
            let [bottom, top] = TORSO.map(|point| place(BODY, point));
            // How far a point is from the torso's segment.
            let from_torso = |point: Vec3| {
                let along = ((point - bottom).dot(top - bottom) / (top - bottom).length_squared())
                    .clamp(0., 1.);
                point.distance(bottom + (top - bottom) * along)
            };
            for pair in tails.windows(2) {
                for (&a, &b) in pair[0].iter().zip(&pair[1]) {
                    // The nearest point of the link to the torso's segment,
                    // found by sampling it finely enough for the guard.
                    for step in 0..=100 {
                        let point = a.lerp(b, step as f32 / 100.);
                        closest = closest.min(from_torso(point) - TORSO_RADIUS - CAPE_RADIUS);
                    }
                }
            }
            let skull = place(HEAD, NECK + SKULL);
            for tail in tails.iter().flatten() {
                let torso = from_torso(*tail);
                closest = closest.min(torso - TORSO_RADIUS - CAPE_RADIUS);
                for (name, away, reach) in [
                    ("head", tail.distance(skull), SKULL_RADIUS),
                    ("torso", torso, TORSO_RADIUS),
                ] {
                    assert!(
                        away >= reach + CAPE_RADIUS - 1e-4,
                        "frame {frame}: a cape tail is {away} from the {name}'s collider"
                    );
                }
            }
        }
        assert!(
            closest < 1e-3,
            "the cape came no nearer than {closest} to the torso"
        );
    }
}
