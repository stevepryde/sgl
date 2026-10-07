//! Screen-space reflections in SGL3D (Crystal, the default method): a static,
//! dimly lit ground strip with a smooth reflective surface (perceptual
//! roughness 0.1, below Crystal's 0.2 cutoff, so it traces), boxes of many
//! heights at many distances along it, thin bright emitter bars at distance
//! and a fast-translating camera (1 m per frame, 60 m/s at 60 Hz). Its rays
//! end near, far, on the bars or in the sky.
//!
//! `cargo run --release -p sgl-3d --example ssr [-- --frames N]`
//!
//! Renders the sequence at 960×540 through the public Scene and Renderer
//! API, with environment and probe specular only and with SSR on, both with
//! the High preset's TAA. The SSR frames are written to `target/ssr-example/`.
//!
//! Printed:
//! - GPU time per SGL3D pass group (`GpuTiming`; the SSR passes are named
//!   after DiligentFX's debug groups) and the frame total: mean, median and
//!   95th percentile over the measured frames with up to two frames in
//!   flight; the SSR frame cost is the difference of the two mean frame
//!   totals.
//! - Brightness jumps: for each frame, the mean over the ground band (rows
//!   62–80 % of the height, columns 30–70 % of the width: the strip ahead of
//!   the camera, below the horizon) of the display luminance
//!   0.2126 R + 0.7152 G + 0.0722 B of the tone-mapped 8-bit output (0–255);
//!   then the mean, 95th percentile and maximum over consecutive frames of
//!   the absolute change of that band mean. The first `WARM_UP` frames are
//!   excluded from timing and jump statistics.
use sgl_3d::glam::camera;
use sgl_3d::{
    Camera, DirectionalLight, Exposure, FrameInput, InstanceState, Mobility, Renderer, Scene,
    asset::{Asset, CpuMesh, Material, Vertex},
    environment::{EnvironmentMap, PmremAtlas},
    glam::Vec3,
    settings::{self, Settings},
    timing::{FrameTime, GpuTiming},
};
use std::collections::BTreeMap;
use std::error::Error;
use std::path::Path;

const SIZE: [u32; 2] = [960, 540];
const WARM_UP: usize = 8;
/// Metres per frame.
const SPEED: f32 = 1.;

fn material(base: [f32; 4], roughness: f32, unlit: bool) -> Material {
    Material {
        name: "ssr example".into(),
        casts_directional_shadow: false,
        base,
        metallic: 0.,
        roughness,
        double_sided: true,
        unlit,
        ..Default::default()
    }
}

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
                tangent: [u.x, u.y, u.z, 1.],
                lightmap_bounds: [0., 0., 1., 1.],
                lightmap_uv: [0.; 2],
                position: (center + (normal + u * s + v * t) * half).to_array(),
                normal: normal.to_array(),
                uv: [(s + 1.) * 0.5, (t + 1.) * 0.5],
                color: [1.; 4],
            });
        }
        mesh.indices.extend([0, 1, 2, 0, 2, 3].map(|i| start + i));
    }
    mesh
}

/// A reflective strip 16 m wide and 1 km long between rougher side planes;
/// warm and cool bars on posts every 25 m on both sides and an overhead bar
/// every 100 m; and boxes from 0.3 to 6 m high on the strip either side of
/// the camera's path and on the side planes, every few metres.
fn world() -> Asset {
    let mut meshes = vec![
        cuboid(Vec3::new(0., -0.05, -490.), Vec3::new(16., 0.1, 1000.), 0),
        cuboid(Vec3::new(-20., -0.06, -490.), Vec3::new(24., 0.1, 1000.), 1),
        cuboid(Vec3::new(20., -0.06, -490.), Vec3::new(24., 0.1, 1000.), 1),
    ];
    for i in 0..40 {
        let z = -30. - 25. * i as f32;
        for (x, warm) in [(-8.5, true), (8.5, false)] {
            meshes.push(cuboid(
                Vec3::new(x, 2., z),
                Vec3::new(0.2, 3.2, 0.2),
                if warm { 2 } else { 3 },
            ));
        }
        if i % 4 == 0 {
            meshes.push(cuboid(Vec3::new(0., 6., z), Vec3::new(17., 0.15, 0.15), 2));
        }
    }
    // A fixed xorshift sequence places the boxes, so every run is the same.
    let mut seed = 0x2545_f491_u32;
    let mut random = || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed as f32 / u32::MAX as f32
    };
    let mut z = -8.;
    while z > -980. {
        let side = if random() < 0.5 { -1. } else { 1. };
        // Clear of the camera's path at x = 0, on the strip or beyond it.
        let x = side * (2.5 + random() * 15.);
        let height = 0.3 + random() * random() * 5.7;
        let width = 0.4 + random() * 1.8;
        meshes.push(cuboid(
            Vec3::new(x, height * 0.5, z),
            Vec3::new(width, height, width),
            4,
        ));
        z -= 2. + random() * 6.;
    }
    Asset {
        meshes,
        materials: vec![
            material([0.12, 0.12, 0.13, 1.], 0.1, false),
            material([0.02, 0.025, 0.02, 1.], 0.9, false),
            material([120., 70., 25., 1.], 1., true),
            material([25., 70., 120., 1.], 1., true),
            material([0.3, 0.28, 0.25, 1.], 0.7, false),
        ],
        images: Vec::new(),
        rig: Default::default(),
        ignored: Vec::new(),
    }
}

/// A uniform dim sky of radiance 1/32.
fn environment() -> EnvironmentMap {
    EnvironmentMap {
        panorama: image::RgbaImage::from_pixel(4, 2, image::Rgba([8, 8, 12, 255])),
        filtered: PmremAtlas {
            width: 336,
            height: 64,
            rgba16: [0x2800u16, 0x2800, 0x2c00, 0x3c00]
                .into_iter()
                .flat_map(u16::to_le_bytes)
                .cycle()
                .take(336 * 64 * 8)
                .collect(),
        },
    }
}

fn read_pixels(device: &wgpu::Device, queue: &wgpu::Queue, texture: &wgpu::Texture) -> Vec<u8> {
    let stride = (texture.width() * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("ssr example readback"),
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
                rows_per_image: Some(texture.height()),
            },
        },
        texture.size(),
    );
    queue.submit([encoder.finish()]);
    buffer.map_async(wgpu::MapMode::Read, .., |r| r.unwrap());
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    let mapped = buffer.get_mapped_range(..).unwrap();
    mapped
        .chunks_exact(stride as usize)
        .flat_map(|row| row[..texture.width() as usize * 4].iter().copied())
        .collect()
}

fn ground_band_luminance(pixels: &[u8]) -> f64 {
    let [width, height] = SIZE.map(|x| x as usize);
    let (rows, columns) = (
        height * 62 / 100..height * 80 / 100,
        width * 30 / 100..width * 70 / 100,
    );
    let count = rows.len() * columns.len();
    let mut sum = 0.;
    for y in rows {
        for x in columns.clone() {
            let p = &pixels[(y * width + x) * 4..];
            sum += 0.2126 * f64::from(p[0]) + 0.7152 * f64::from(p[1]) + 0.0722 * f64::from(p[2]);
        }
    }
    sum / count as f64
}

#[derive(Default)]
struct Run {
    band: Vec<f64>,
    /// Each group's time in each measured frame (0 where it did not run).
    groups: BTreeMap<&'static str, Vec<f64>>,
    totals: Vec<f64>,
}

impl Run {
    fn add(&mut self, frame: FrameTime) {
        if frame.frame as usize > WARM_UP {
            let measured = self.totals.len();
            self.totals.push(frame.total_ms);
            for pass in frame.passes {
                let times = self.groups.entry(pass.name).or_default();
                times.resize(measured + 1, 0.);
                times[measured] += pass.ms;
            }
        }
    }

    /// The mean, median and 95th percentile of `times` over the measured
    /// frames.
    fn statistics(&self, times: &[f64]) -> (f64, f64, f64) {
        let mut sorted = times.to_vec();
        sorted.resize(self.totals.len(), 0.);
        sorted.sort_by(f64::total_cmp);
        let at = |q: f64| sorted[((sorted.len() as f64 * q).ceil() as usize).saturating_sub(1)];
        (
            sorted.iter().sum::<f64>() / sorted.len() as f64,
            at(0.5),
            at(0.95),
        )
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    /// Pass-group timings with frames in flight, as a game renders.
    Timing,
    /// Each frame read back for brightness statistics and, with SSR, saved.
    Images,
}

fn run(
    frames: usize,
    ssr: Option<settings::ScreenSpaceReflections>,
    mode: Mode,
    directory: &Path,
) -> Result<Run, Box<dyn Error>> {
    let adapter =
        pollster::block_on(wgpu::Instance::default().request_adapter(&Default::default()))?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_features: (adapter.features() & wgpu::Features::TIMESTAMP_QUERY)
            | sgl_3d::graphics_device::features(&adapter),
        required_limits: sgl_3d::graphics_device::limits(&adapter),
        ..Default::default()
    }))?;
    let mut scene = Scene::new(&device, &queue);
    let ground = scene.add_asset(&device, &queue, world())?.model;
    scene.add_instance(
        &device,
        &queue,
        InstanceState::new(ground),
        Mobility::Static,
    )?;
    let environment = scene.add_environment(&device, &queue, &environment())?;
    let output = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("ssr example output"),
        size: wgpu::Extent3d {
            width: SIZE[0],
            height: SIZE[1],
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
        scene_resolution: settings::SceneResolution::Full,
        atmosphere: false,
        screen_space_reflections: ssr.unwrap_or_default(),
        ..Settings::default()
    };
    let mut renderer = Renderer::new(&device, &queue, output.format(), SIZE, 1., &settings)?;
    let mut timing = GpuTiming::new(&device, &queue).filter(|_| mode == Mode::Timing);
    let projection = sgl_3d::perspective(55f32.to_radians(), SIZE[0] as f32 / SIZE[1] as f32, 0.1);
    let mut result = Run::default();
    let mut in_flight = None;
    for index in 0..frames {
        let eye = Vec3::new(0., 1.2, -(index as f32) * SPEED);
        let mut input = FrameInput::new(Camera {
            eye,
            view: camera::rh::view::look_at_mat4(eye, eye + Vec3::new(0., -0.08, -1.), Vec3::Y),
            projection,
        });
        input.camera_cut = index == 0;
        // A fixed exposure keeps the measured luminance comparable between
        // runs and builds.
        input.exposure = Exposure {
            stops: 0.,
            automatic: None,
        };
        input.environment = Some(environment);
        input.directional_lights[0] = Some(DirectionalLight {
            direction: Vec3::new(0.3, -1., -0.4),
            color: [0.5, 0.55, 0.7],
            illuminance: 3.,
            shadow: None,
            ..Default::default()
        });
        if let Some(timing) = &mut timing {
            for frame in timing.begin_frame(&device, &queue) {
                result.add(frame);
            }
        }
        let mut encoder = device.create_command_encoder(&Default::default());
        renderer.render(
            &device,
            &queue,
            &mut encoder,
            &mut scene,
            &input,
            &settings,
            &output_view,
            timing.as_ref(),
        );
        let submission = queue.submit([encoder.finish()]);
        if let Some(timing) = &mut timing {
            timing.submitted(&queue);
        }
        // At most two frames in flight, so that every frame's query set is free.
        if let Some(earlier) = in_flight.replace(submission) {
            let _ = device.poll(wgpu::PollType::Wait {
                submission_index: Some(earlier),
                timeout: None,
            });
        }
        renderer.finish_frame(&mut scene);
        match mode {
            Mode::Timing => {}
            Mode::Images => {
                let pixels = read_pixels(&device, &queue, &output);
                result.band.push(ground_band_luminance(&pixels));
                if ssr.is_some() {
                    image::save_buffer(
                        directory.join(format!("frame-{index:03}.png")),
                        &pixels,
                        SIZE[0],
                        SIZE[1],
                        image::ColorType::Rgba8,
                    )?;
                }
            }
        }
    }
    if let Some(timing) = &mut timing {
        for _ in 0..3 {
            let _ = device.poll(wgpu::PollType::wait_indefinitely());
            for frame in timing.begin_frame(&device, &queue) {
                result.add(frame);
            }
        }
    }
    Ok(result)
}

fn report_timing(label: &str, run: &Run) {
    println!(
        "\n{label} (mean / median / p95 over {} frames)",
        run.totals.len()
    );
    let (mean, median, p95) = run.statistics(&run.totals);
    println!("  GPU frame total              {mean:.3} / {median:.3} / {p95:.3} ms");
    for (name, times) in &run.groups {
        let (mean, median, p95) = run.statistics(times);
        println!("  {name:<28} {mean:.3} / {median:.3} / {p95:.3} ms");
    }
}

fn report_jumps(label: &str, run: &Run) {
    let mut jumps: Vec<f64> = run.band[WARM_UP..]
        .windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .collect();
    jumps.sort_by(f64::total_cmp);
    let mean = jumps.iter().sum::<f64>() / jumps.len() as f64;
    let p95 = jumps[((jumps.len() as f64 * 0.95).ceil() as usize).saturating_sub(1)];
    let band = run.band[WARM_UP..].iter().sum::<f64>() / (run.band.len() - WARM_UP) as f64;
    println!(
        "  {label:<36} band mean {band:6.2}; jump mean {mean:.2} / p95 {p95:.2} / max {:.2}",
        jumps.last().unwrap()
    );
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut frames = 120;
    let settings = settings::ScreenSpaceReflections::Full;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--frames" => frames = args.next().ok_or("--frames requires a count")?.parse()?,
            other => return Err(format!("unknown argument {other}").into()),
        }
    }
    if frames < WARM_UP + 3 {
        return Err(format!("--frames must be at least {}", WARM_UP + 3).into());
    }
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/ssr-example");
    std::fs::create_dir_all(&directory)?;
    println!(
        "{frames} frames at {}x{}, {SPEED} m per frame",
        SIZE[0], SIZE[1]
    );
    let baseline = run(frames, None, Mode::Timing, &directory)?;
    let with = run(frames, Some(settings), Mode::Timing, &directory)?;
    if !baseline.totals.is_empty() && !with.totals.is_empty() {
        report_timing("Environment and probe specular only", &baseline);
        report_timing("With SSR", &with);
        println!(
            "\nSSR frame cost: {:.3} ms",
            with.statistics(&with.totals).0 - baseline.statistics(&baseline.totals).0
        );
    } else {
        println!("No GPU timestamps on this device");
    }
    println!("\nGround-band brightness jumps (8-bit display units):");
    report_jumps(
        "Environment and probe specular only",
        &run(frames, None, Mode::Images, &directory)?,
    );
    report_jumps(
        "With SSR",
        &run(frames, Some(settings), Mode::Images, &directory)?,
    );
    println!("Frames: {}", directory.canonicalize()?.display());
    Ok(())
}
