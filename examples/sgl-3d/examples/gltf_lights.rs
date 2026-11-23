//! A glTF's authored lights (`KHR_lights_punctual`) installed as a game
//! installs them: the asset loads them as data (`Asset::lights`), each
//! placement becomes an SGL3D light (`PunctualLight::place`), point and spot
//! lights go into the scene, and the game picks which directional lights
//! fill the frame's two slots. Here that is the first two in the file's
//! order; the rest are listed and left out. A point or spot light the file
//! leaves without a range (Blender's exporter writes none) is given the
//! range at which its light falls below `MIN_ILLUMINANCE_LUX`, so it does
//! not reach every pixel.
//!
//! Run with `cargo run -p sgl-3d-examples --example gltf_lights`, or name
//! a glTF and an output image:
//! `cargo run -p sgl-3d-examples --example gltf_lights -- scene.gltf target/lights.png`.
//! The default glTF, `fixtures/authored-lights.gltf`, holds a courtyard lit
//! by three directional lights, two lanterns placing one point light below
//! a turned and scaled parent, and a spot without a range.
use sgl_3d::asset::{self, PlacedLight, PunctualKind};
use sgl_3d::glam::{Mat4, Vec3, camera};
use sgl_3d::{
    AutoExposure, Camera, CompensationCurve, FrameInput, InstanceState, Mobility, Renderer, Scene,
    View, ViewFrame, settings::Settings,
};
use std::error::Error;
use std::path::PathBuf;

/// The game's threshold for bounding a light the file leaves unbounded:
/// the illuminance, in lux, below which this game's exposure shows nothing
/// of a light. A game picks it for its own brightness and exposure.
const MIN_ILLUMINANCE_LUX: f32 = 0.05;

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
    let loaded = asset::load(&gltf)?;
    // Where the game places the asset: its lights go where its meshes do.
    let model = Mat4::IDENTITY;
    let (low, high) = loaded
        .meshes
        .iter()
        .flat_map(|mesh| &mesh.vertices)
        .map(|vertex| Vec3::from(vertex.position))
        .fold((Vec3::MAX, Vec3::MIN), |(low, high), p| {
            (low.min(p), high.max(p))
        });
    let lights = loaded.lights.clone();

    let mut scene = Scene::new(&device, &queue);
    let ids = scene.add_asset(&device, &queue, loaded)?;
    scene.add_instance(
        &device,
        &queue,
        InstanceState {
            pose: model,
            ..InstanceState::new(ids.model)
        },
        Mobility::Static,
    )?;
    let mut suns = Vec::new();
    let mut local = 0;
    for placement in &lights.placements {
        let definition = &lights.definitions[placement.light];
        let unbounded = matches!(
            definition.kind,
            PunctualKind::Point { range: None } | PunctualKind::Spot { range: None, .. }
        );
        match definition.place(model * placement.transform) {
            PlacedLight::Local(mut light) => {
                if unbounded {
                    // The game's policy for a light the file leaves
                    // unbounded: it reaches as far as its inverse-square
                    // illuminance stays above MIN_ILLUMINANCE_LUX.
                    let peak = light.intensity * light.color.into_iter().fold(0., f32::max);
                    if peak <= 0. {
                        continue; // It lights nothing.
                    }
                    light.range = (peak / MIN_ILLUMINANCE_LUX).sqrt();
                    println!(
                        "{}: no range in the file, bounded at {:.1} m",
                        definition.name.as_deref().unwrap_or("unnamed"),
                        light.range
                    );
                }
                scene.add_light(&device, &queue, light)?;
                local += 1;
            }
            PlacedLight::Directional(sun) => suns.push((placement.name.clone(), sun)),
        }
    }

    let size = [960, 540];
    let center = (low + high) / 2.;
    let eye = center + Vec3::new(1., 0.8, 1.2).normalize() * (high - low).length();
    let mut frame = FrameInput::new(Camera {
        view: camera::rh::view::look_at_mat4(eye, center, Vec3::Y),
        projection: sgl_3d::perspective(50f32.to_radians(), size[0] as f32 / size[1] as f32, 0.1),
        eye,
    });
    // The game's choice of suns: the first two the file places.
    for (slot, (_, sun)) in frame.directional_lights.iter_mut().zip(&suns) {
        *slot = Some(*sun);
    }
    let name = |name: &Option<String>| name.clone().unwrap_or_else(|| "unnamed".into());
    let left_out: Vec<String> = suns.iter().skip(2).map(|(n, _)| name(n)).collect();
    // Metered: the average luminance becomes middle grey (2^-2.5).
    frame.exposure.automatic = Some(AutoExposure {
        compensation: CompensationCurve::new(&[[0., -2.5]])?,
        ..Default::default()
    });

    let output_texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("gltf_lights output"),
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
    // A few frames, so metering settles.
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
                output: &output_view,
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
    println!(
        "{}: {} light definitions, {} placements; {local} scene lights, suns {:?}, left out {left_out:?}; {}",
        gltf.display(),
        lights.definitions.len(),
        lights.placements.len(),
        suns.iter()
            .take(2)
            .map(|(n, _)| name(n))
            .collect::<Vec<_>>(),
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
        label: Some("gltf_lights readback"),
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
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/authored-lights.gltf"),
        PathBuf::from,
    );
    let output = args.next().map_or_else(
        || PathBuf::from("target/sgl-3d-gltf-lights.png"),
        PathBuf::from,
    );
    pollster::block_on(run(gltf, output))
}
