//! A skinned and morphed glTF: a tentacle of four bones with a bulge morph
//! target and a swaying clip, generated here as a GLB so that no asset file
//! or licence is needed, loaded with `asset::load_slice`, and animated by
//! the game's side of the contract: sample the clip, pose the rig's nodes,
//! and give each instance its joint matrices and morph weights every frame.
//! A sun's cascades and a point light shadow it.
//!
//! Run with `cargo run -p sgl-3d --example skinned -- target/skinned.png`.
//! Optional `--frames N` sets how many frames it animates.
use sgl_3d::glam::camera;
use sgl_3d::{
    Camera, DirectionalLight, DirectionalShadow, FrameInput, HemisphereLight, InstanceState, Light,
    LightShape, Mobility, Renderer, Scene,
    asset::{self, Asset, CpuMesh, Material, Vertex},
    deformation::{Channel, ChannelValues, Clip, Interpolation, Rig},
    glam::{Mat4, Quat, Vec3},
    settings::{RenderPreset, Settings},
};
use std::error::Error;

const RINGS: u32 = 24;
const SEGMENTS: u32 = 16;
const BONES: u32 = 4;
const HEIGHT: f32 = 2.;

/// The tentacle as a GLB: a tube along +Y skinned to a chain of bones, each
/// vertex weighted between the two bones nearest its height; a morph target
/// that swells its middle; and a clip that bends every bone and swells it.
fn tentacle() -> Vec<u8> {
    let mut positions = Vec::new();
    let mut normals = Vec::new();
    let mut joints: Vec<[u16; 4]> = Vec::new();
    let mut weights = Vec::new();
    let mut bulge = Vec::new();
    for ring in 0..=RINGS {
        let height = ring as f32 / RINGS as f32;
        let bone = height * (BONES - 1) as f32;
        let lower = (bone.floor() as u16).min(BONES as u16 - 2);
        let blend = bone - f32::from(lower);
        let radius = 0.25 * (1. - 0.6 * height);
        for segment in 0..SEGMENTS {
            let angle = segment as f32 / SEGMENTS as f32 * std::f32::consts::TAU;
            let normal = Vec3::new(angle.cos(), 0., angle.sin());
            positions.push((normal * radius + Vec3::Y * height * HEIGHT).to_array());
            normals.push(normal.to_array());
            joints.push([lower, lower + 1, 0, 0]);
            weights.push([1. - blend, blend, 0., 0.]);
            let swell = (height * std::f32::consts::PI).sin().powi(2);
            bulge.push((normal * 0.12 * swell).to_array());
        }
    }
    let mut indices: Vec<u32> = Vec::new();
    for ring in 0..RINGS {
        for segment in 0..SEGMENTS {
            let a = ring * SEGMENTS + segment;
            let b = ring * SEGMENTS + (segment + 1) % SEGMENTS;
            indices.extend([a, a + SEGMENTS, b, b, a + SEGMENTS, b + SEGMENTS]);
        }
    }
    // Each bone's inverse bind matrix: the inverse of its rest transform.
    let inverse_binds: Vec<[f32; 16]> = (0..BONES)
        .map(|bone| {
            Mat4::from_translation(Vec3::Y * -(bone as f32) * HEIGHT / (BONES - 1) as f32)
                .to_cols_array()
        })
        .collect();
    let times = [0f32, 1., 2.];
    let bend = |angle: f32| Quat::from_rotation_z(angle).to_array();
    let rotations = [bend(0.), bend(0.35), bend(0.)];
    let swells = [0f32, 1., 0.];

    let mut bin: Vec<u8> = Vec::new();
    let mut views = Vec::new();
    let mut accessors = Vec::new();
    let mut add = |bytes: &[u8],
                   component: u32,
                   kind: &str,
                   count: usize,
                   bounds: Option<[Vec3; 2]>| {
        views.push(
            serde_json::json!({"buffer": 0, "byteOffset": bin.len(), "byteLength": bytes.len()}),
        );
        bin.extend_from_slice(bytes);
        let mut accessor = serde_json::json!({
            "bufferView": views.len() - 1, "componentType": component, "count": count, "type": kind
        });
        if let Some([min, max]) = bounds {
            accessor["min"] = serde_json::json!(min.to_array());
            accessor["max"] = serde_json::json!(max.to_array());
        }
        accessors.push(accessor);
        accessors.len() - 1
    };
    let bounds = |points: &[[f32; 3]]| {
        let min = points
            .iter()
            .fold(Vec3::INFINITY, |m, p| m.min(Vec3::from(*p)));
        let max = points
            .iter()
            .fold(Vec3::NEG_INFINITY, |m, p| m.max(Vec3::from(*p)));
        Some([min, max])
    };
    let count = positions.len();
    let position = add(
        bytemuck::cast_slice(&positions),
        5126,
        "VEC3",
        count,
        bounds(&positions),
    );
    let normal = add(bytemuck::cast_slice(&normals), 5126, "VEC3", count, None);
    let joint = add(bytemuck::cast_slice(&joints), 5123, "VEC4", count, None);
    let weight = add(bytemuck::cast_slice(&weights), 5126, "VEC4", count, None);
    let swell = add(
        bytemuck::cast_slice(&bulge),
        5126,
        "VEC3",
        count,
        bounds(&bulge),
    );
    let index = add(
        bytemuck::cast_slice(&indices),
        5125,
        "SCALAR",
        indices.len(),
        None,
    );
    let inverse_bind = add(
        bytemuck::cast_slice(&inverse_binds),
        5126,
        "MAT4",
        inverse_binds.len(),
        None,
    );
    let time = add(
        bytemuck::cast_slice(&times),
        5126,
        "SCALAR",
        times.len(),
        None,
    );
    let rotation = add(
        bytemuck::cast_slice(&rotations),
        5126,
        "VEC4",
        rotations.len(),
        None,
    );
    let swelling = add(
        bytemuck::cast_slice(&swells),
        5126,
        "SCALAR",
        swells.len(),
        None,
    );
    // Node 0 holds the mesh; nodes 1.. are the bone chain.
    let mut nodes = vec![serde_json::json!({"name": "tentacle", "mesh": 0, "skin": 0})];
    for bone in 0..BONES {
        let mut node = serde_json::json!({"name": format!("bone {bone}")});
        if bone > 0 {
            node["translation"] = serde_json::json!([0., HEIGHT / (BONES - 1) as f32, 0.]);
        }
        if bone + 1 < BONES {
            node["children"] = serde_json::json!([bone + 2]);
        }
        nodes.push(node);
    }
    let bones: Vec<u32> = (1..=BONES).collect();
    let mut channels: Vec<_> = (0..BONES - 1)
        .map(|bone| serde_json::json!({"sampler": 0, "target": {"node": bone + 1, "path": "rotation"}}))
        .collect();
    channels.push(serde_json::json!({"sampler": 1, "target": {"node": 0, "path": "weights"}}));
    let document = serde_json::json!({
        "asset": {"version": "2.0"},
        "buffers": [{"byteLength": bin.len()}],
        "bufferViews": views,
        "accessors": accessors,
        "materials": [{"pbrMetallicRoughness": {"baseColorFactor": [0.8, 0.3, 0.45, 1.], "metallicFactor": 0., "roughnessFactor": 0.45}}],
        "meshes": [{"primitives": [{
            "attributes": {"POSITION": position, "NORMAL": normal, "JOINTS_0": joint, "WEIGHTS_0": weight},
            "indices": index, "material": 0, "targets": [{"POSITION": swell}]
        }], "weights": [0.]}],
        "skins": [{"joints": bones, "inverseBindMatrices": inverse_bind}],
        "nodes": nodes,
        "animations": [{"name": "sway",
            "samplers": [{"input": time, "output": rotation}, {"input": time, "output": swelling}],
            "channels": channels}],
        "scenes": [{"nodes": [0, 1]}], "scene": 0
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

/// Keyframe `key` of a channel with `width` values per keyframe, and its
/// tangents for a cubic spline (glTF's in-tangent, value, out-tangent).
fn keyframe<T: Copy>(
    values: &[T],
    interpolation: Interpolation,
    key: usize,
    width: usize,
    item: usize,
) -> [T; 3] {
    let at = |k: usize| values[k * width + item];
    match interpolation {
        Interpolation::CubicSpline => [at(key * 3), at(key * 3 + 1), at(key * 3 + 2)],
        _ => [at(key); 3],
    }
}

/// A channel's value at `time`, as glTF interpolates it: `mix` blends two
/// values linearly (spherically for rotations).
fn sample<T>(
    channel: &Channel,
    values: &[T],
    width: usize,
    item: usize,
    time: f32,
    mix: impl Fn(T, T, f32) -> T,
) -> T
where
    T: Copy + std::ops::Add<Output = T> + std::ops::Mul<f32, Output = T>,
{
    let times = &channel.times;
    let key = times
        .partition_point(|&t| t <= time)
        .saturating_sub(1)
        .min(times.len() - 1);
    let [_, value, out] = keyframe(values, channel.interpolation, key, width, item);
    if key + 1 == times.len() || time <= times[0] {
        return value;
    }
    let [in_next, next, _] = keyframe(values, channel.interpolation, key + 1, width, item);
    let span = times[key + 1] - times[key];
    let t = (time - times[key]) / span;
    match channel.interpolation {
        Interpolation::Step => value,
        Interpolation::Linear => mix(value, next, t),
        Interpolation::CubicSpline => {
            let (t2, t3) = (t * t, t * t * t);
            value * (2. * t3 - 3. * t2 + 1.)
                + out * (span * (t3 - 2. * t2 + t))
                + next * (-2. * t3 + 3. * t2)
                + in_next * (span * (t3 - t2))
        }
    }
}

/// The rig's node transforms and morph weights at `time` of `clip`: what a
/// game's animation system produces, here one clip without blending.
fn pose(rig: &Rig, clip: &Clip, time: f32) -> (Vec<Mat4>, Vec<f32>) {
    let mut nodes: Vec<_> = rig
        .nodes
        .iter()
        .map(|node| (node.translation, node.rotation, node.scale))
        .collect();
    let mut weights: Vec<f32> = rig.morph_weights.iter().map(|weight| weight.rest).collect();
    for channel in &clip.channels {
        let node = &mut nodes[channel.node];
        match &channel.values {
            ChannelValues::Translation(values) => {
                node.0 = sample(channel, values, 1, 0, time, Vec3::lerp)
            }
            ChannelValues::Rotation(values) => {
                node.1 = sample(channel, values, 1, 0, time, Quat::slerp).normalize()
            }
            ChannelValues::Scale(values) => {
                node.2 = sample(channel, values, 1, 0, time, Vec3::lerp)
            }
            ChannelValues::MorphWeights(values) => {
                let first = rig
                    .morph_weights
                    .iter()
                    .position(|w| w.node == channel.node);
                let targets = rig
                    .morph_weights
                    .iter()
                    .filter(|w| w.node == channel.node)
                    .count();
                for target in 0..targets {
                    weights[first.unwrap() + target] =
                        sample(channel, values, targets, target, time, |a, b, t| {
                            a + (b - a) * t
                        });
                }
            }
        }
    }
    let locals = nodes
        .into_iter()
        .map(|(translation, rotation, scale)| {
            Mat4::from_scale_rotation_translation(scale, rotation, translation)
        })
        .collect();
    (locals, weights)
}

fn ground() -> Asset {
    let mut ground = asset::empty();
    ground.materials.push(Material {
        name: "ground".into(),
        visibility_group: 0,
        casts_directional_shadow: true,
        base: [0.35, 0.37, 0.4, 1.],
        emissive: [0.; 3],
        metallic: 0.,
        roughness: 0.8,
        clearcoat: 0.,
        coat_roughness: 0.,
        anisotropy_strength: 0.,
        anisotropy_rotation: 0.,
        anisotropy_texture: None,
        base_texture: None,
        mr_texture: None,
        emissive_texture: None,
        normal_texture: None,
        normal_scale: 1.,
        bump_texture: None,
        bump_scale: 0.,
        wrap: [gltf::texture::WrappingMode::Repeat; 2],
        double_sided: false,
        unlit: false,
        alpha: sgl_3d::AlphaMode::Opaque,
    });
    ground.meshes.push(CpuMesh {
        vertices: [(-6., -6.), (6., -6.), (6., 6.), (-6., 6.)]
            .map(|(x, z)| Vertex {
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
    let tentacle = asset::load_slice(&tentacle())?;
    let rig = tentacle.rig.clone();
    let mut scene = Scene::new(&device, &queue);
    let ground = scene.add_asset(&device, &queue, ground())?.model;
    let placed = |model, pose| InstanceState {
        model,
        pose,
        visible: true,
        capture_visible: true,
    };
    scene.add_instance(
        &device,
        &queue,
        placed(ground, Mat4::IDENTITY),
        Mobility::Static,
    )?;
    // A deforming model's instances move: the game poses them every frame.
    let model = scene.add_asset(&device, &queue, tentacle)?.model;
    let instances = [-1.5f32, 0., 1.5]
        .iter()
        .map(|&x| {
            scene.add_instance(
                &device,
                &queue,
                placed(model, Mat4::from_translation(Vec3::X * x)),
                Mobility::Moving,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    scene.add_light(
        &device,
        &queue,
        Light {
            position: Vec3::new(0.5, 2.6, 1.5),
            shape: LightShape::Point,
            color: [1., 0.85, 0.7],
            intensity: 12.,
            range: 8.,
            baked: false,
            specular: 1.,
            casts_shadow: true,
        },
    )?;
    let size = [320, 240];
    let output = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("skinned output"),
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
    let mut renderer = Renderer::new(&device, &queue, output.format(), size, 1., &settings)?;
    let eye = Vec3::new(0., 1.8, 5.);
    let mut input = FrameInput::new(Camera {
        view: camera::rh::view::look_at_mat4(eye, Vec3::new(0., 1., 0.), Vec3::Y),
        projection: sgl_3d::perspective(50f32.to_radians(), size[0] as f32 / size[1] as f32, 0.1),
        eye,
    });
    input.directional_lights[0] = Some(DirectionalLight {
        direction: Vec3::new(0.5, -1., -0.4),
        color: [1., 0.95, 0.9],
        illuminance: 2.,
        shadow: Some(DirectionalShadow {
            distance: 15.,
            cascades: 2,
            first_split: 6.,
        }),
    });
    input.hemisphere_light = HemisphereLight {
        sky_color: [0.2, 0.3, 0.5],
        ground_color: [0.05, 0.04, 0.03],
        intensity: 0.3,
    };
    let clip = &rig.clips[0];
    for frame in 0..frames {
        let seconds = frame as f32 / 30.;
        for (offset, &instance) in instances.iter().enumerate() {
            let (locals, weights) = pose(&rig, clip, (seconds + offset as f32 * 0.4) % 2.);
            scene.set_instance_deformation(
                &queue,
                instance,
                &rig.joint_matrices(&locals),
                &weights,
            )?;
        }
        input.elapsed_seconds = seconds;
        input.camera_cut = frame == 0;
        renderer.resize(&device, size, 1., &settings);
        let mut encoder = device.create_command_encoder(&Default::default());
        renderer.render(
            &device,
            &queue,
            &mut encoder,
            &mut scene,
            &input,
            &settings,
            &output_view,
            None,
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
        label: Some("skinned readback"),
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
    let mapped = buffer.get_mapped_range(..);
    Ok(mapped
        .chunks(row as usize)
        .flat_map(|line| &line[..(size.width * 4) as usize])
        .copied()
        .collect())
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut output = std::path::PathBuf::from("target/sgl-3d-skinned.png");
    let mut frames = 45;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--frames" => frames = args.next().ok_or("--frames requires a count")?.parse()?,
            path => output = path.into(),
        }
    }
    pollster::block_on(run(output, frames))
}
