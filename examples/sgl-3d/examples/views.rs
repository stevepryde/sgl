//! Two views of one scene in one frame: a main view and a rear-view inset,
//! each with its own camera, settings, output and history, listed in one
//! `Renderer::render` with the main view first, its primary. The inset is
//! copied into the main view's corner, as a game composes it, and the result
//! written to a PNG.
//!
//! Run with `cargo run -p sgl-3d-examples --example views -- target/views.png`.
//! Optional `--frames N` sets how many frames run.
use sgl_3d::glam::{Mat4, Vec3, camera};
use sgl_3d::settings::{RenderPreset, Settings};
use sgl_3d::{
    Camera, DirectionalLight, DirectionalShadow, FrameInput, HemisphereLight, InstanceState,
    Mobility, Renderer, Scene, View, ViewFrame,
    asset::{Asset, CpuMesh, Material, Vertex},
};
use std::{error::Error, path::PathBuf};

const SIZE: [u32; 2] = [640, 360];
const INSET: [u32; 2] = [192, 108];
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// An axis-aligned box at `center` of `size` in material `material`.
fn cuboid(center: Vec3, size: Vec3, material: usize) -> CpuMesh {
    let half = size * 0.5;
    let mut mesh = CpuMesh {
        vertices: Vec::new(),
        indices: Vec::new(),
        material,
        deformation: Default::default(),
    };
    for (normal, u, v) in [
        (Vec3::X, -Vec3::Z, Vec3::Y),
        (-Vec3::X, Vec3::Z, Vec3::Y),
        (Vec3::Y, Vec3::X, -Vec3::Z),
        (-Vec3::Y, Vec3::X, Vec3::Z),
        (Vec3::Z, Vec3::X, Vec3::Y),
        (-Vec3::Z, -Vec3::X, Vec3::Y),
    ] {
        let start = mesh.vertices.len() as u32;
        for [s, t] in [[-1., -1.], [1., -1.], [1., 1.], [-1., 1.]] {
            mesh.vertices.push(Vertex {
                position: (center + (normal + u * s + v * t) * half).to_array(),
                normal: normal.to_array(),
                uv: [(s + 1.) * 0.5, (t + 1.) * 0.5],
                color: [1.; 4],
                tangent: [u.x, u.y, u.z, 1.],
                lightmap_bounds: [0., 0., 1., 1.],
                lightmap_uv: [0.; 2],
            });
        }
        mesh.indices.extend([0, 1, 2, 0, 2, 3].map(|i| start + i));
    }
    mesh
}

fn material(base: [f32; 3], roughness: f32) -> Material {
    Material {
        name: "views surface".into(),
        base: [base[0], base[1], base[2], 1.],
        roughness,
        metallic: 0.,
        ..Default::default()
    }
}

/// A camera at `eye` looking at `target`, for an output of `size`.
fn camera(eye: Vec3, target: Vec3, size: [u32; 2]) -> Camera {
    Camera {
        eye,
        view: camera::rh::view::look_at_mat4(eye, target, Vec3::Y),
        projection: sgl_3d::perspective(55f32.to_radians(), size[0] as f32 / size[1] as f32, 0.1),
    }
}

fn texture(device: &wgpu::Device, size: [u32; 2], usage: wgpu::TextureUsages) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("views output"),
        size: wgpu::Extent3d {
            width: size[0],
            height: size[1],
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | usage,
        view_formats: &[],
    })
}

fn read_pixels(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
) -> Result<Vec<u8>, Box<dyn Error>> {
    let stride = (texture.width() * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
        * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("views readback"),
        size: u64::from(stride * texture.height()),
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
                rows_per_image: None,
            },
        },
        texture.size(),
    );
    queue.submit([encoder.finish()]);
    let slice = buffer.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    device.poll(wgpu::PollType::wait_indefinitely())?;
    let mapped = slice.get_mapped_range().unwrap();
    Ok(mapped
        .chunks_exact(stride as usize)
        .flat_map(|row| &row[..texture.width() as usize * 4])
        .copied()
        .collect())
}

async fn run(output: PathBuf, frames: u32) -> Result<(), Box<dyn Error>> {
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
    let mut scene = Scene::new(&device, &queue);
    let ground = Asset {
        meshes: vec![cuboid(Vec3::new(0., -0.1, 0.), Vec3::new(20., 0.2, 20.), 0)],
        materials: vec![material([0.35, 0.37, 0.4], 0.8)],
        images: Vec::new(),
        rig: Default::default(),
        ignored: Vec::new(),
    };
    let ground = scene.add_asset(&device, &queue, ground)?.model;
    scene.add_instance(
        &device,
        &queue,
        InstanceState::new(ground),
        Mobility::Static,
    )?;
    let pillars = Asset {
        meshes: (0..6)
            .map(|i| {
                let angle = i as f32 * std::f32::consts::TAU / 6.;
                cuboid(
                    Vec3::new(4. * angle.cos(), 1., 4. * angle.sin()),
                    Vec3::new(0.6, 2., 0.6),
                    i % 2,
                )
            })
            .collect(),
        materials: vec![
            material([0.8, 0.3, 0.2], 0.5),
            material([0.2, 0.5, 0.8], 0.4),
        ],
        images: Vec::new(),
        rig: Default::default(),
        ignored: Vec::new(),
    };
    let pillars = scene.add_asset(&device, &queue, pillars)?.model;
    scene.add_instance(
        &device,
        &queue,
        InstanceState::new(pillars),
        Mobility::Static,
    )?;
    let cart = Asset {
        meshes: vec![cuboid(Vec3::new(0., 0.4, 0.), Vec3::new(0.8, 0.8, 1.4), 0)],
        materials: vec![material([0.9, 0.8, 0.2], 0.3)],
        images: Vec::new(),
        rig: Default::default(),
        ignored: Vec::new(),
    };
    let cart = scene.add_asset(&device, &queue, cart)?.model;
    let mut cart_state = InstanceState::new(cart);
    let cart = scene.add_instance(&device, &queue, cart_state, Mobility::Moving)?;

    // One renderer for everything the views share; one view per image.
    let mut renderer = Renderer::new(&device);
    let main_settings = Settings::default();
    // The inset is small and cheap: the Low preset, its own choice.
    let inset_settings = Settings {
        preset: RenderPreset::Low,
        ..Settings::default()
    };
    let mut main_view = View::new(&renderer, &device, &queue, FORMAT, SIZE, 1., &main_settings)?;
    let mut inset_view = View::new(
        &renderer,
        &device,
        &queue,
        FORMAT,
        INSET,
        1.,
        &inset_settings,
    )?;
    let main_output = texture(
        &device,
        SIZE,
        wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::COPY_DST,
    );
    let inset_output = texture(&device, INSET, wgpu::TextureUsages::COPY_SRC);
    let [main_target, inset_target] =
        [&main_output, &inset_output].map(|t| t.create_view(&Default::default()));
    let sun = DirectionalLight {
        direction: Vec3::new(0.4, -1., -0.3),
        color: [1., 0.92, 0.8],
        illuminance: 3.,
        shadow: Some(DirectionalShadow {
            distance: 20.,
            cascades: 2,
        }),
        ..Default::default()
    };
    let light = |input: &mut FrameInput| {
        input.directional_lights[0] = Some(sun);
        input.hemisphere_light = HemisphereLight {
            sky_color: [0.3, 0.4, 0.6],
            ground_color: [0.1, 0.08, 0.06],
            intensity: 0.4,
        };
    };
    for frame in 0..frames {
        // Edit the scene, then render, submit and finish, as with one view.
        let along = frame as f32 * 0.05 - 2.;
        cart_state.pose = Mat4::from_translation(Vec3::new(0., 0., along));
        scene.set_instance(&queue, cart, cart_state)?;
        let ahead = Vec3::new(0., 0.4, along);
        let elapsed_seconds = f64::from(frame) / 60.;
        // The main view chases the cart; the inset looks back from it.
        let mut main = FrameInput::new(camera(ahead + Vec3::new(0., 2., -4.), ahead, SIZE));
        let mut inset = FrameInput::new(camera(
            ahead + Vec3::new(0., 1., 0.8),
            ahead + Vec3::new(0., 0.5, 6.),
            INSET,
        ));
        for input in [&mut main, &mut inset] {
            light(input);
            input.elapsed_seconds = elapsed_seconds;
            input.frame_time_ms = 1000. / 60.;
        }
        // Cheap when nothing changed.
        main_view.resize(&device, SIZE, 1., &main_settings);
        inset_view.resize(&device, INSET, 1., &inset_settings);
        let mut encoder = device.create_command_encoder(&Default::default());
        renderer.render(
            &device,
            &queue,
            &mut encoder,
            &mut scene,
            &mut [
                ViewFrame {
                    view: &mut main_view,
                    input: &main,
                    settings: &main_settings,
                    output: &main_target,
                    timing: None,
                },
                ViewFrame {
                    view: &mut inset_view,
                    input: &inset,
                    settings: &inset_settings,
                    output: &inset_target,
                    timing: None,
                },
            ],
        );
        // The game composes the inset over the main view's top right.
        encoder.copy_texture_to_texture(
            inset_output.as_image_copy(),
            wgpu::TexelCopyTextureInfo {
                origin: wgpu::Origin3d {
                    x: SIZE[0] - INSET[0] - 16,
                    y: 16,
                    z: 0,
                },
                ..main_output.as_image_copy()
            },
            inset_output.size(),
        );
        queue.submit([encoder.finish()]);
        renderer.finish_frame(&mut scene);
    }
    let pixels = read_pixels(&device, &queue, &main_output)?;
    if let Some(parent) = output.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    image::save_buffer(&output, &pixels, SIZE[0], SIZE[1], image::ColorType::Rgba8)?;
    println!(
        "Rendered {frames} frames of a main view and an inset; {}",
        output.display()
    );
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut output: PathBuf = "target/sgl-3d-views.png".into();
    let mut frames = 30;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--frames" => frames = args.next().ok_or("--frames requires a count")?.parse()?,
            flag if flag.starts_with('-') => return Err(format!("unknown option {flag}").into()),
            _ => output = arg.into(),
        }
    }
    pollster::block_on(run(output, frames.max(1)))
}
