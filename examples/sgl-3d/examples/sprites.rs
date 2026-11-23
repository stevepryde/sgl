//! Textured world-space sprites (`Scene::add_sprite_batch`): a field of
//! foliage cards (masked, upright along +Y), hit markers that keep their
//! size on screen (blended, an HDR tint so they glow), and an explosion
//! whose flipbook frame the game advances each frame from a 4 by 4 sheet
//! (`SpriteRegion::cell`), all under an orbiting camera with TAA. Each batch
//! is one instanced draw; the field never changes, so it is uploaded once.
//!
//! Run with `cargo run --release -p sgl-3d-examples --example sprites -- target/sprites.png`.
//! `--compare N` instead measures N sprites against game-built quads
//! through the model path (`support/sprite_cost.rs`), as
//! `bun scripts/tasks.ts measure-browser --sprites` does in the browser.
use sgl_3d::asset::{Image, Sampler, TextureFilter};
use sgl_3d::glam::{Mat4, Vec2, Vec3, camera};
use sgl_3d::settings::Settings;
use sgl_3d::sprite::{Sprite, SpriteBatch, SpriteBlend, SpriteFacing, SpriteRegion, SpriteSizing};
use sgl_3d::{
    Camera, DirectionalLight, FrameInput, HemisphereLight, InstanceState, Mobility, Renderer,
    Scene, View, ViewFrame, ViewOutput,
};
use std::error::Error;

#[path = "support/sprite_cost.rs"]
mod sprite_cost;

/// The foliage: a leaf cut out of its square, its stem at the bottom.
fn leaf() -> Image {
    Image::Rgba8(image::RgbaImage::from_fn(32, 32, |x, y| {
        let (u, v) = ((x as f32 + 0.5) / 16. - 1., (y as f32 + 0.5) / 16. - 1.);
        let inside = u.abs() < 0.7 * (1. - v * v).max(0.).sqrt();
        let shade = (200. - 60. * v) as u8;
        image::Rgba([shade / 3, shade, shade / 4, if inside { 255 } else { 0 }])
    }))
}

/// A hit marker: four ticks round an empty centre, pixel art.
fn marker() -> Image {
    Image::Rgba8(image::RgbaImage::from_fn(16, 16, |x, y| {
        let (dx, dy) = (
            x.abs_diff(8).min(x.abs_diff(7)),
            y.abs_diff(8).min(y.abs_diff(7)),
        );
        let tick = (dx == dy) && (3..7).contains(&dx);
        image::Rgba([255, 255, 255, if tick { 255 } else { 0 }])
    }))
}

/// A 4 by 4 flipbook of an explosion: a ring that grows and fades, one
/// frame a cell, row by row.
fn flipbook() -> Image {
    Image::Rgba8(image::RgbaImage::from_fn(256, 256, |x, y| {
        let cell = (y / 64) * 4 + x / 64;
        let t = cell as f32 / 15.;
        let (u, v) = (
            ((x % 64) as f32 + 0.5) / 32. - 1.,
            ((y % 64) as f32 + 0.5) / 32. - 1.,
        );
        let r = (u * u + v * v).sqrt();
        let ring = (1. - ((r - 0.2 - 0.6 * t) / 0.15).abs()).max(0.) * (1. - t);
        image::Rgba([255, (200. * (1. - t)) as u8 + 40, 40, (ring * 255.) as u8])
    }))
}

fn options() -> Result<(String, Option<usize>), Box<dyn Error>> {
    let mut output = "target/sgl-3d-sprites.png".to_string();
    let mut compare = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--compare" => compare = Some(args.next().ok_or("--compare requires N")?.parse()?),
            "--help" | "-h" => {
                println!("sprites [output.png] [--compare N]");
                std::process::exit(0);
            }
            flag if flag.starts_with('-') => return Err(format!("unknown option {flag}").into()),
            _ => output = arg,
        }
    }
    Ok((output, compare))
}

struct Native;

impl sprite_cost::Platform for Native {
    fn now(&self) -> f64 {
        use std::sync::OnceLock;
        static START: OnceLock<std::time::Instant> = OnceLock::new();
        START
            .get_or_init(std::time::Instant::now)
            .elapsed()
            .as_secs_f64()
            * 1000.
    }

    fn completed<'a>(
        &'a self,
        device: &'a wgpu::Device,
        _: &'a wgpu::Queue,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + 'a>> {
        let _ = device.poll(wgpu::PollType::wait_indefinitely());
        Box::pin(std::future::ready(()))
    }
}

async fn run() -> Result<(), Box<dyn Error>> {
    let (output_path, compare) = options()?;
    let adapter = wgpu::Instance::default()
        .request_adapter(&Default::default())
        .await?;
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            required_features: sgl_3d::graphics_device::features(&adapter)
                | (adapter.features() & wgpu::Features::TIMESTAMP_QUERY),
            required_limits: sgl_3d::graphics_device::limits(&adapter),
            ..Default::default()
        })
        .await?;
    if let Some(count) = compare {
        print!(
            "{}",
            sprite_cost::report(&Native, &device, &queue, count, 120).await?
        );
        return Ok(());
    }
    let mut scene = Scene::new(&device, &queue);
    let ground = scene.add_asset(&device, &queue, ground())?.model;
    scene.add_instance(
        &device,
        &queue,
        InstanceState {
            model: ground,
            pose: Mat4::IDENTITY,
            visible: true,
            capture_visible: true,
        },
        Mobility::Static,
    )?;
    // Foliage cards: masked, upright, set once.
    let leaf = scene.add_sprite_image(&device, &queue, &leaf())?;
    let foliage = scene.add_sprite_batch(&SpriteBatch {
        facing: SpriteFacing::Axis(Vec3::Y),
        blend: SpriteBlend::Masked { cutoff: 0.5 },
        ..SpriteBatch::new(leaf)
    })?;
    let cards: Vec<Sprite> = (0..400)
        .map(|i| {
            let (x, z) = ((i % 20) as f32 * 0.6 - 6., (i / 20) as f32 * 0.6 - 6.);
            Sprite {
                pivot: Vec2::new(0.5, 0.),
                ..Sprite::new(
                    Vec3::new(x + (i as f32 * 0.37).fract() * 0.3, 0., z),
                    Vec2::new(0.5, 0.7),
                )
            }
        })
        .collect();
    scene.set_sprites(&device, &queue, foliage, &cards)?;
    // Hit markers: crisp, their size kept on screen, glowing.
    let marker = scene.add_sprite_image(&device, &queue, &marker())?;
    let markers = scene.add_sprite_batch(&SpriteBatch {
        sampler: Sampler {
            mag_filter: TextureFilter::Nearest,
            min_filter: TextureFilter::Nearest,
            mipmap_filter: None,
            ..Sampler::default()
        },
        sizing: SpriteSizing::Fixed,
        ..SpriteBatch::new(marker)
    })?;
    // An explosion's flipbook, advanced by the game.
    let sheet = scene.add_sprite_image(&device, &queue, &flipbook())?;
    let explosion = scene.add_sprite_batch(&SpriteBatch::new(sheet))?;
    let size = [640, 360];
    let output = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("sprites output"),
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
        atmosphere: false,
        ..Settings::default()
    };
    let mut renderer = Renderer::new(&device);
    let mut view = View::new(
        &renderer,
        &device,
        &queue,
        output.format(),
        size,
        1.,
        &settings,
    )?;
    for frame in 0..24u32 {
        let angle = frame as f32 * 0.02;
        let eye = Vec3::new(angle.sin() * 8., 2.5, angle.cos() * 8.);
        let mut input = FrameInput::new(Camera {
            view: camera::rh::view::look_at_mat4(eye, Vec3::new(0., 0.8, 0.), Vec3::Y),
            projection: sgl_3d::perspective(1., size[0] as f32 / size[1] as f32, 0.1),
            eye,
        });
        input.camera_cut = frame == 0;
        input.elapsed_seconds = f64::from(frame) / 60.;
        input.directional_lights[0] = Some(DirectionalLight {
            direction: Vec3::new(0.3, -1., -0.4),
            illuminance: 3.,
            ..Default::default()
        });
        input.hemisphere_light = HemisphereLight {
            sky_color: [0.3, 0.4, 0.6],
            ground_color: [0.05, 0.04, 0.03],
            intensity: 0.4,
        };
        let marked = [Vec3::new(-1., 1.2, 1.), Vec3::new(1.5, 0.8, -0.5)].map(|p| Sprite {
            tint: [4., 1.5, 1.5, 1.],
            ..Sprite::new(p, Vec2::splat(0.08))
        });
        scene.set_sprites(&device, &queue, markers, &marked)?;
        let blast = Sprite {
            region: SpriteRegion::cell(4, 4, frame / 3),
            tint: [3., 3., 3., 1.],
            ..Sprite::new(Vec3::new(0., 1.2, 0.), Vec2::splat(2.))
        };
        scene.set_sprites(&device, &queue, explosion, &[blast])?;
        let mut encoder = device.create_command_encoder(&Default::default());
        renderer.render(
            &device,
            &queue,
            &mut encoder,
            &mut scene,
            &mut [ViewFrame {
                view: &mut view,
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
    if let Some(parent) = std::path::Path::new(&output_path)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    image::save_buffer(
        &output_path,
        &pixels,
        size[0],
        size[1],
        image::ColorType::Rgba8,
    )?;
    println!("{output_path}");
    Ok(())
}

fn ground() -> sgl_3d::asset::Asset {
    let corner = |x: f32, z: f32| sgl_3d::asset::Vertex {
        uv1: [0.0; 2],
        position: [x, 0., z],
        normal: [0., 1., 0.],
        uv: [0.; 2],
        color: [1.; 4],
        tangent: [0.; 4],
        lightmap_uv: [0.; 2],
        lightmap_bounds: [0., 0., 1., 1.],
    };
    sgl_3d::asset::Asset {
        meshes: vec![sgl_3d::asset::CpuMesh {
            vertices: vec![
                corner(-8., 8.),
                corner(8., 8.),
                corner(8., -8.),
                corner(-8., -8.),
            ],
            indices: vec![0, 1, 2, 0, 2, 3],
            material: 0,
            deformation: Default::default(),
        }],
        materials: vec![sgl_3d::asset::Material {
            base: [0.25, 0.2, 0.15, 1.],
            roughness: 0.9,
            ..Default::default()
        }],
        images: Vec::new(),
        rig: Default::default(),
        ignored: Vec::new(),
        lights: Default::default(),
        instanced: Default::default(),
    }
}

fn read_pixels(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
) -> Result<Vec<u8>, Box<dyn Error>> {
    let stride = (texture.width() * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
        * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("sprites image readback"),
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
    pollster::block_on(run())
}
