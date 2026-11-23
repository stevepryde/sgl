//! A glTF's instanced nodes (`EXT_mesh_gpu_instancing`) installed as a game
//! installs them: the asset loads each instanced mesh once with its
//! placements (`Asset::instanced`), `Scene::add_asset` prepares each as a
//! model (`AssetIds::instanced`), and the game adds a static instance per
//! placement, at the asset's pose times the placement's transform. Draw
//! lists batch them, and they are culled, shadowed and traced as any
//! instances. It prints the load's time and retained geometry, the time
//! `add_asset` took to prepare and place it, and the camera's draws and
//! triangles.
//!
//! Run with `cargo run --release -p sgl-3d-examples --example gltf_instancing`, or
//! name a glTF and an output image:
//! `cargo run --release -p sgl-3d-examples --example gltf_instancing -- forest.glb target/forest.png`.
//! The default glTF, the `InstancedProps` test fixture, places one prop
//! nine times under two instanced nodes, three of them mirrored, beside an
//! ordinary ground. Load the same scene exported as ordinary nodes
//! (`glTF-Expanded/`) to compare.
use sgl_3d::asset::{self, Asset, Vertex};
use sgl_3d::glam::{Mat4, Vec3, camera};
use sgl_3d::{
    AutoExposure, Camera, CompensationCurve, DirectionalLight, DirectionalShadow, FrameInput,
    InstanceState, Mobility, Renderer, Scene, View, ViewFrame, settings::Settings,
};
use std::error::Error;
use std::path::PathBuf;
use std::time::Instant;

/// The bytes an asset's geometry retains: its meshes' and its instanced
/// meshes' vertices and indices.
fn geometry_bytes(asset: &Asset) -> usize {
    asset
        .meshes
        .iter()
        .chain(
            asset
                .instanced
                .iter()
                .flat_map(|instanced| &instanced.meshes),
        )
        .map(|mesh| mesh.vertices.len() * size_of::<Vertex>() + mesh.indices.len() * 4)
        .sum()
}

async fn run(gltf: PathBuf, output: PathBuf) -> Result<(), Box<dyn Error>> {
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
    let started = Instant::now();
    let loaded = asset::load(&gltf)?;
    let import_ms = started.elapsed().as_secs_f64() * 1e3;
    let geometry = geometry_bytes(&loaded);
    // Where the game places the asset: its instances go where its meshes do.
    let model = Mat4::IDENTITY;
    // The game's placements, taken before the asset moves into the scene:
    // here every instance of every instanced node.
    let placements: Vec<Vec<Mat4>> = loaded
        .instanced
        .iter()
        .map(|instanced| {
            instanced
                .nodes
                .iter()
                .flat_map(|node| node.transforms.iter().map(|&t| model * t))
                .collect()
        })
        .collect();
    let (low, high) = loaded
        .meshes
        .iter()
        .map(|mesh| (mesh, Mat4::IDENTITY))
        .chain(
            loaded
                .instanced
                .iter()
                .zip(&placements)
                .flat_map(|(instanced, poses)| {
                    poses
                        .iter()
                        .flat_map(|&pose| instanced.meshes.iter().map(move |mesh| (mesh, pose)))
                }),
        )
        .flat_map(|(mesh, pose)| {
            mesh.vertices
                .iter()
                .map(move |vertex| pose.transform_point3(Vec3::from(vertex.position)))
        })
        .fold((Vec3::MAX, Vec3::MIN), |(low, high), p| {
            (low.min(p), high.max(p))
        });

    let mut scene = Scene::new(&device, &queue);
    let started = Instant::now();
    let ids = scene.add_asset(&device, &queue, loaded)?;
    let add_ms = started.elapsed().as_secs_f64() * 1e3;
    let mut instances = 0;
    let mut add = |id, pose| -> Result<(), Box<dyn Error>> {
        let state = InstanceState {
            pose,
            ..InstanceState::new(id)
        };
        scene.add_instance(&device, &queue, state, Mobility::Static)?;
        instances += 1;
        Ok(())
    };
    add(ids.model, model)?;
    for (&id, poses) in ids.instanced.iter().zip(&placements) {
        for &pose in poses {
            add(id, pose)?;
        }
    }

    let size = [960, 540];
    let center = (low + high) / 2.;
    let eye = center + Vec3::new(0.6, 0.9, 1.0).normalize() * (high - low).length() * 0.8;
    let mut frame = FrameInput::new(Camera {
        view: camera::rh::view::look_at_mat4(eye, center, Vec3::Y),
        projection: sgl_3d::perspective(50f32.to_radians(), size[0] as f32 / size[1] as f32, 0.1),
        eye,
    });
    frame.directional_lights[0] = Some(DirectionalLight {
        direction: Vec3::new(-0.4, -1., -0.3).normalize(),
        illuminance: 3.,
        shadow: Some(DirectionalShadow {
            distance: (high - low).length(),
            cascades: 2,
        }),
        ..Default::default()
    });
    // Metered: the average luminance becomes middle grey (2^-2.5).
    frame.exposure.automatic = Some(AutoExposure {
        compensation: CompensationCurve::new(&[[0., -2.5]])?,
        ..Default::default()
    });

    let output_texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("gltf_instancing output"),
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
    let output_view = output_texture.create_view(&Default::default());
    let settings = Settings::default();
    let mut renderer = Renderer::new(&device);
    let mut view = View::new(
        &renderer,
        &device,
        &queue,
        output_texture.format(),
        size,
        1.,
        &settings,
    )?;
    // A few frames, so metering settles and the statistics read back.
    for index in 0..8 {
        frame.elapsed_seconds = f64::from(index) / 60.;
        frame.camera_cut = index == 0;
        let mut encoder = device.create_command_encoder(&Default::default());
        renderer.render(
            &device,
            &queue,
            &mut encoder,
            &mut scene,
            &mut [ViewFrame {
                view: &mut view,
                input: &frame,
                settings: &settings,
                output: sgl_3d::ViewOutput::Texture(&output_view),
                timing: None,
            }],
        );
        queue.submit([encoder.finish()]);
        renderer.finish_frame(&mut scene);
    }
    let pixels = read_pixels(&device, &queue, &output_texture)?;
    if let Some(parent) = output.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    image::save_buffer(&output, &pixels, size[0], size[1], image::ColorType::Rgba8)?;
    let draws = view.diagnostic_draws();
    let triangles = view.geometry_stats(&device).unwrap_or_default().total().1;
    println!(
        "{}: loaded in {import_ms:.1} ms, {geometry} bytes of geometry, added in {add_ms:.1} ms; {} instanced meshes, {instances} instances; camera draws {}, cascade draws {:?}, camera triangles {triangles}; {}",
        gltf.display(),
        ids.instanced.len(),
        draws.camera,
        draws.cascades,
        output.display()
    );
    Ok(())
}

fn read_pixels(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
) -> Result<Vec<u8>, Box<dyn Error>> {
    let stride = (texture.width() * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
        * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("gltf_instancing readback"),
        size: u64::from(stride) * u64::from(texture.height()),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(stride),
                rows_per_image: Some(texture.height()),
            },
        },
        texture.size(),
    );
    queue.submit([encoder.finish()]);
    let (send, receive) = std::sync::mpsc::channel();
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            let _ = send.send(result);
        });
    device.poll(wgpu::PollType::wait_indefinitely())?;
    receive.recv()??;
    let mapped = buffer.slice(..).get_mapped_range().unwrap();
    Ok(mapped
        .chunks_exact(stride as usize)
        .flat_map(|row| row[..texture.width() as usize * 4].iter().copied())
        .collect())
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args_os().skip(1);
    let gltf = args.next().map_or_else(
        || {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(
                "../../crates/sgl-3d/tests/fixtures/gltf/InstancedProps/glTF/InstancedProps.gltf",
            )
        },
        PathBuf::from,
    );
    let output = args.next().map_or_else(
        || PathBuf::from("target/sgl-3d-gltf-instancing.png"),
        PathBuf::from,
    );
    pollster::block_on(run(gltf, output))
}
