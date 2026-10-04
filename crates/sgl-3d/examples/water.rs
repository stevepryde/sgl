//! Blended receivers of screen-space reflections: a lake whose surface, a
//! caller-generated grid whose normals the example animates each frame (a
//! sum of sines, replaced with `Scene::set_model`), receives reflections
//! over submerged rocks and a sunken block, between an opaque shoreline
//! rising out of the water and posts standing in it, under a bright panel
//! above the far shore. A second receiver sheet lies partly over the lake
//! and a glass pane that does not receive stands in front.
//!
//! `cargo run --release -p sgl-3d --example water [-- --frames N]`
//!
//! Renders each run below at 1920×1080 through the public `Scene` and
//! `Renderer` API and prints, per run, the median and 95th percentile GPU
//! time of the frame and of the pass groups receivers touch, over the frames
//! after a warm-up, with up to two frames in flight. `before` and `after`
//! are the same frames with the lake and the sheet unmarked and marked.
//! Every 30th frame and the last of each run are written to
//! `target/water-example/<run>/` for the owner to judge.
use sgl_3d::glam::{Quat, Vec3, camera};
use sgl_3d::{
    AlphaMode, Camera, DirectionalLight, DirectionalShadow, Exposure, FrameInput, InstanceState,
    MaterialId, Mobility, ModelId, ModelMesh, MotionBlurParameters, Renderer, Scene,
    asset::{Asset, CpuMesh, Material, Vertex},
    environment::{EnvironmentMap, PmremAtlas},
    settings::{
        Antialiasing, Fsr2Quality, MotionBlur, ReflectionMethod, ScreenSpaceReflections, Settings,
    },
    timing::{FrameTime, GpuTiming},
};
use std::collections::BTreeMap;
use std::error::Error;
use std::path::Path;

const SIZE: [u32; 2] = [1920, 1080];
/// The size the `resize` run switches to a third of the way through.
const RESIZED: [u32; 2] = [1280, 720];
const WARM_UP: usize = 10;
/// Grid cells along each side of the lake's surface.
const CELLS: usize = 128;
/// The lake's surface spans x in ±40 m and z from 2.5 m to -52 m, at y = 0.
const LAKE: [[f32; 2]; 2] = [[-40., 2.5], [40., -52.]];

/// One run: the settings it renders with and what it exercises.
struct Run {
    name: &'static str,
    settings: Settings,
    /// The lake and the sheet receive screen-space reflections.
    marked: bool,
    /// The camera orbits; otherwise it stands still.
    orbit: bool,
    /// The lake's normals move.
    waves: bool,
    /// A resize a third of the way through and a camera cut at two thirds.
    resize_and_cut: bool,
}

fn runs() -> Vec<Run> {
    let base = Settings {
        scene_resolution: sgl_3d::settings::SceneResolution::Full,
        atmosphere: false,
        antialiasing: Antialiasing::Taa,
        screen_space_reflections: ScreenSpaceReflections::Full,
        reflection_method: ReflectionMethod::Crystal,
        ..Settings::default()
    };
    let run = |name, settings: Settings| Run {
        name,
        settings,
        marked: true,
        orbit: true,
        waves: true,
        resize_and_cut: false,
    };
    let with = |change: fn(&mut Settings)| {
        let mut settings = base;
        change(&mut settings);
        settings
    };
    let blurred = with(|s| s.motion_blur = MotionBlur::Full);
    vec![
        Run {
            marked: false,
            ..run("before", blurred)
        },
        run("after", blurred),
        Run {
            orbit: false,
            ..run("stationary", base)
        },
        Run {
            waves: false,
            ..run("still-waves", base)
        },
        run(
            "crystal-half",
            with(|s| s.screen_space_reflections = ScreenSpaceReflections::Half),
        ),
        run(
            "velvet-full",
            with(|s| s.reflection_method = ReflectionMethod::Velvet),
        ),
        run(
            "velvet-half",
            with(|s| {
                s.reflection_method = ReflectionMethod::Velvet;
                s.screen_space_reflections = ScreenSpaceReflections::Half;
            }),
        ),
        run(
            "ssr-off",
            with(|s| s.screen_space_reflections = ScreenSpaceReflections::Off),
        ),
        run("smaa", with(|s| s.antialiasing = Antialiasing::Smaa)),
        run(
            "fsr2",
            with(|s| {
                s.antialiasing = Antialiasing::Fsr2;
                s.fsr2_quality = Fsr2Quality::Quality;
            }),
        ),
        Run {
            resize_and_cut: true,
            ..run("resize-and-cut", base)
        },
    ]
}

fn material(name: &str, base: [f32; 4], roughness: f32) -> Material {
    Material {
        name: name.into(),
        base,
        metallic: 0.,
        roughness,
        ..Default::default()
    }
}

/// A box of `size` centred at `center`, turned by `rotation`.
fn cuboid(center: Vec3, size: Vec3, rotation: Quat, material: usize) -> CpuMesh {
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
            let local = (normal + u * s + v * t) * half;
            let tangent = rotation * u;
            mesh.vertices.push(Vertex {
                tangent: [tangent.x, tangent.y, tangent.z, 1.],
                lightmap_bounds: [0., 0., 1., 1.],
                lightmap_uv: [0.; 2],
                position: (center + rotation * local).to_array(),
                normal: (rotation * normal).to_array(),
                uv: [(s + 1.) * 0.5, (t + 1.) * 0.5],
                color: [1.; 4],
            });
        }
        mesh.indices.extend([0, 1, 2, 0, 2, 3].map(|i| start + i));
    }
    mesh
}

/// A quad with corners `origin`, `origin + u`, `origin + u + v` and
/// `origin + v`, facing `u × v`.
fn quad(origin: Vec3, u: Vec3, v: Vec3, material: usize) -> CpuMesh {
    let normal = u.cross(v).normalize().to_array();
    let tangent = u.normalize();
    CpuMesh {
        vertices: [[0., 0.], [1., 0.], [1., 1.], [0., 1.]]
            .map(|[s, t]| Vertex {
                tangent: [tangent.x, tangent.y, tangent.z, 1.],
                lightmap_bounds: [0., 0., 1., 1.],
                lightmap_uv: [0.; 2],
                position: (origin + u * s + v * t).to_array(),
                normal,
                uv: [s, t],
                color: [1.; 4],
            })
            .to_vec(),
        indices: vec![0, 1, 2, 0, 2, 3],
        material,
        deformation: Default::default(),
    }
}

/// The lake's surroundings, the second sheet and the glass pane; the lake
/// itself is its own model. Materials 6 and 7 receive when `marked`.
fn world(marked: bool) -> Asset {
    let receiver = AlphaMode::Blend {
        receives_screen_space_reflections: marked,
    };
    let tilt = |angle: f32| Quat::from_rotation_z(angle.to_radians());
    let turn = |angle: f32| Quat::from_rotation_y(angle.to_radians());
    let mut meshes = vec![
        // The lake bed, 2 m down, and the shores: far, near (where the
        // camera stands) and the two sloping banks.
        cuboid(
            Vec3::new(0., -2.1, -25.),
            Vec3::new(90., 0.2, 70.),
            Quat::IDENTITY,
            1,
        ),
        cuboid(
            Vec3::new(0., 0.25, -62.),
            Vec3::new(90., 4.5, 24.),
            Quat::IDENTITY,
            0,
        ),
        cuboid(
            Vec3::new(0., -0.3, 10.),
            Vec3::new(90., 1.4, 16.),
            Quat::IDENTITY,
            0,
        ),
        cuboid(
            Vec3::new(-34., -0.4, -25.),
            Vec3::new(24., 4., 70.),
            tilt(-12.),
            0,
        ),
        cuboid(
            Vec3::new(34., -0.4, -25.),
            Vec3::new(24., 4., 70.),
            tilt(12.),
            0,
        ),
        // Submerged rocks and a sunken block.
        cuboid(
            Vec3::new(-8., -1.7, -16.),
            Vec3::splat(1.6),
            turn(30.) * tilt(20.),
            2,
        ),
        cuboid(
            Vec3::new(-3., -1.9, -28.),
            Vec3::splat(2.2),
            turn(-15.) * tilt(-25.),
            2,
        ),
        cuboid(
            Vec3::new(12., -1.8, -36.),
            Vec3::splat(1.8),
            turn(50.) * tilt(10.),
            2,
        ),
        cuboid(
            Vec3::new(5., -1.4, -22.),
            Vec3::new(4., 1.2, 3.),
            turn(20.),
            3,
        ),
        // The bright panel above the far shore.
        cuboid(
            Vec3::new(0., 9., -56.),
            Vec3::new(24., 3., 0.3),
            Quat::IDENTITY,
            5,
        ),
        // A sheet 0.5 m over part of the lake, and a glass pane in front.
        quad(
            Vec3::new(8., 0.5, -16.),
            Vec3::X * 12.,
            Vec3::NEG_Z * 10.,
            6,
        ),
        quad(Vec3::new(-10., 0., -6.), Vec3::X * 6., Vec3::Y * 3., 7),
    ];
    // Posts standing in the water: a jetty's on the left, two on the right.
    for (x, z) in [
        (-6., -10.),
        (-6., -16.),
        (-6., -22.),
        (9., -12.),
        (14., -30.),
    ] {
        meshes.push(cuboid(
            Vec3::new(x, 0.25, z),
            Vec3::new(0.3, 4.5, 0.3),
            Quat::IDENTITY,
            4,
        ));
    }
    let sheet = Material {
        alpha: receiver,
        ..material("sheet", [0.05, 0.08, 0.1, 0.5], 0.08)
    };
    let glass = Material {
        double_sided: true,
        alpha: AlphaMode::Blend {
            receives_screen_space_reflections: false,
        },
        ..material("glass", [0.6, 0.8, 0.9, 0.25], 0.05)
    };
    Asset {
        meshes,
        materials: vec![
            material("shore", [0.35, 0.3, 0.22, 1.], 0.85),
            material("lake bed", [0.25, 0.22, 0.16, 1.], 0.9),
            material("rock", [0.3, 0.3, 0.32, 1.], 0.7),
            material("sunken block", [0.6, 0.15, 0.1, 1.], 0.5),
            material("post", [0.15, 0.1, 0.07, 1.], 0.6),
            Material {
                unlit: true,
                casts_directional_shadow: false,
                ..material("panel", [40., 30., 18., 1.], 1.)
            },
            sheet,
            glass,
        ],
        images: Vec::new(),
        rig: Default::default(),
    }
}

/// The lake's surface at `seconds`: a flat grid whose normals follow a sum
/// of sines, and its indices.
fn lake(seconds: f32, material: MaterialId) -> ModelMesh {
    // Direction (x, z), wavelength (m), amplitude (m), speed (m/s).
    const WAVES: [([f32; 2], f32, f32, f32); 4] = [
        ([1., 0.3], 9., 0.06, 1.6),
        ([-0.4, 1.], 5., 0.035, 1.2),
        ([0.7, -0.7], 3., 0.02, 0.9),
        ([-1., -0.2], 13., 0.05, 2.),
    ];
    let [[x0, z0], [x1, z1]] = LAKE;
    let mut vertices = Vec::with_capacity((CELLS + 1) * (CELLS + 1));
    for j in 0..=CELLS {
        for i in 0..=CELLS {
            let (s, t) = (i as f32 / CELLS as f32, j as f32 / CELLS as f32);
            let (x, z) = (x0 + (x1 - x0) * s, z0 + (z1 - z0) * t);
            // The height's slope: d/dx and d/dz of A sin(k (d · p) - w t).
            let mut slope = [0f32; 2];
            for (direction, wavelength, amplitude, speed) in WAVES {
                let d = Vec3::new(direction[0], 0., direction[1]).normalize();
                let k = std::f32::consts::TAU / wavelength;
                let phase = k * (d.x * x + d.z * z) - k * speed * seconds;
                let rate = amplitude * k * phase.cos();
                slope[0] += rate * d.x;
                slope[1] += rate * d.z;
            }
            let normal = Vec3::new(-slope[0], 1., -slope[1]).normalize();
            vertices.push(Vertex {
                tangent: [1., 0., 0., 1.],
                lightmap_bounds: [0., 0., 1., 1.],
                lightmap_uv: [0.; 2],
                position: [x, 0., z],
                normal: normal.to_array(),
                uv: [s, t],
                color: [1.; 4],
            });
        }
    }
    let row = CELLS as u32 + 1;
    let indices = (0..CELLS as u32)
        .flat_map(|j| (0..CELLS as u32).map(move |i| j * row + i))
        .flat_map(|a| [a, a + 1, a + row, a + 1, a + row + 1, a + row])
        .collect();
    ModelMesh {
        vertices,
        indices,
        material,
        deformation: Default::default(),
    }
}

/// A sky of radiance (0.5, 0.65, 0.9) and a matching backdrop.
fn sky() -> EnvironmentMap {
    let radiance = [0x3800u16, 0x3933, 0x3b33, 0x3c00];
    EnvironmentMap {
        panorama: image::RgbaImage::from_pixel(4, 2, image::Rgba([150, 175, 220, 255])),
        filtered: PmremAtlas {
            width: 336,
            height: 64,
            rgba16: radiance
                .into_iter()
                .flat_map(u16::to_le_bytes)
                .cycle()
                .take(336 * 64 * 8)
                .collect(),
        },
    }
}

fn output(device: &wgpu::Device, size: [u32; 2]) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("water example output"),
        size: wgpu::Extent3d {
            width: size[0],
            height: size[1],
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}

fn save(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    path: &Path,
) -> Result<(), Box<dyn Error>> {
    let stride = (texture.width() * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("water example readback"),
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
    device.poll(wgpu::PollType::wait_indefinitely())?;
    let pixels: Vec<u8> = buffer
        .get_mapped_range(..)
        .chunks_exact(stride as usize)
        .flat_map(|row| row[..texture.width() as usize * 4].to_vec())
        .collect();
    image::save_buffer(
        path,
        &pixels,
        texture.width(),
        texture.height(),
        image::ColorType::Rgba8,
    )?;
    Ok(())
}

/// The camera of frame `index` of a run at `size`.
fn camera(index: usize, orbit: bool, size: [u32; 2]) -> Camera {
    let center = Vec3::new(0., 0.5, -25.);
    let eye = if orbit {
        let angle = 0.5 * (index as f32 / 120. * std::f32::consts::TAU).sin();
        center + Vec3::new(33. * angle.sin(), 3.5, 33. * angle.cos())
    } else {
        Vec3::new(0., 3., 8.)
    };
    Camera {
        eye,
        view: camera::rh::view::look_at_mat4(eye, center, Vec3::Y),
        projection: sgl_3d::perspective(50f32.to_radians(), size[0] as f32 / size[1] as f32, 0.1),
    }
}

/// Each pass group's time in each measured frame.
#[derive(Default)]
struct Times {
    groups: BTreeMap<&'static str, Vec<f64>>,
    totals: Vec<f64>,
}

impl Times {
    fn add(&mut self, frame: FrameTime) {
        if frame.frame as usize <= WARM_UP {
            return;
        }
        let measured = self.totals.len();
        self.totals.push(frame.total_ms);
        for pass in frame.passes {
            // Crystal's, Velvet's and DiligentFX's passes, each as one group.
            let name = ["Godot SSR", "SSR", "DiligentFX", "world reflection"]
                .into_iter()
                .find(|prefix| pass.name.starts_with(prefix))
                .unwrap_or(pass.name);
            let times = self.groups.entry(name).or_default();
            times.resize(measured + 1, 0.);
            times[measured] += pass.ms;
        }
    }

    fn report(&self, name: &str) {
        let quantiles = |times: &[f64]| {
            let mut sorted = times.to_vec();
            sorted.resize(self.totals.len(), 0.);
            sorted.sort_by(f64::total_cmp);
            let at = |q: f64| sorted[((sorted.len() as f64 * q).ceil() as usize).saturating_sub(1)];
            (at(0.5), at(0.95))
        };
        if self.totals.is_empty() {
            println!("{name:<16} no GPU timestamps");
            return;
        }
        let (median, p95) = quantiles(&self.totals);
        println!(
            "{name:<16} frame {median:7.3} / {p95:7.3} ms ({} frames)",
            self.totals.len()
        );
        for group in [
            "receivers",
            "DiligentFX",
            "SSR",
            "Godot SSR",
            "world reflection",
            "reflection composition",
            "blended",
            "TAA",
            "FSR2",
            "motion blur",
        ] {
            if let Some(times) = self.groups.get(group) {
                let (median, p95) = quantiles(times);
                println!("  {group:<26} {median:7.3} / {p95:7.3} ms");
            }
        }
    }
}

fn render(
    run: &Run,
    frames: usize,
    (device, queue): (&wgpu::Device, &wgpu::Queue),
    directory: &Path,
) -> Result<Times, Box<dyn Error>> {
    let directory = directory.join(run.name);
    std::fs::create_dir_all(&directory)?;
    let mut scene = Scene::new(device, queue);
    let world = scene.add_asset(device, queue, world(run.marked))?;
    scene.add_instance(
        device,
        queue,
        InstanceState::new(world.model),
        Mobility::Static,
    )?;
    let water = scene.add_materials(
        device,
        queue,
        &[Material {
            alpha: AlphaMode::Blend {
                receives_screen_space_reflections: run.marked,
            },
            casts_directional_shadow: false,
            ..material("water", [0.02, 0.05, 0.06, 0.6], 0.04)
        }],
        &[],
    )?[0];
    let lake_model: ModelId = scene.add_model(device, queue, vec![lake(0., water)])?;
    // Moving: replacing its geometry each frame is no static edit.
    scene.add_instance(
        device,
        queue,
        InstanceState::new(lake_model),
        Mobility::Moving,
    )?;
    let environment = scene.add_environment(device, queue, &sky())?;
    let mut size = SIZE;
    let mut texture = output(device, size);
    let mut renderer = Renderer::new(device, queue, texture.format(), size, 1., &run.settings)?;
    let mut timing = GpuTiming::new(device, queue);
    let mut times = Times::default();
    let mut in_flight = None;
    for index in 0..frames {
        if run.resize_and_cut && index == frames / 3 {
            size = RESIZED;
            texture = output(device, size);
        }
        renderer.resize(device, size, 1., &run.settings);
        let seconds = if run.waves { index as f32 / 60. } else { 0. };
        if run.waves {
            scene.set_model(device, queue, lake_model, vec![lake(seconds, water)])?;
        }
        let mut input = FrameInput::new(camera(index, run.orbit, size));
        input.camera_cut = index == 0 || (run.resize_and_cut && index == 2 * frames / 3);
        input.elapsed_seconds = seconds;
        input.environment = Some(environment);
        // A fixed exposure keeps runs comparable.
        input.exposure = Exposure {
            stops: 0.,
            automatic: None,
        };
        input.motion_blur = MotionBlurParameters { shutter_angle: 0.5 };
        input.directional_lights[0] = Some(DirectionalLight {
            direction: Vec3::new(-0.3, -1., -0.5),
            color: [1., 0.92, 0.8],
            illuminance: 3.,
            shadow: Some(DirectionalShadow::DEFAULT),
            ..Default::default()
        });
        if let Some(timing) = &mut timing {
            for frame in timing.begin_frame(device, queue) {
                times.add(frame);
            }
        }
        let view = texture.create_view(&Default::default());
        let mut encoder = device.create_command_encoder(&Default::default());
        renderer.render(
            device,
            queue,
            &mut encoder,
            &mut scene,
            &input,
            &run.settings,
            &view,
            timing.as_ref(),
        );
        let submission = queue.submit([encoder.finish()]);
        if let Some(timing) = &mut timing {
            timing.submitted(queue);
        }
        if let Some(earlier) = in_flight.replace(submission) {
            let _ = device.poll(wgpu::PollType::Wait {
                submission_index: Some(earlier),
                timeout: None,
            });
        }
        renderer.finish_frame(&mut scene);
        if index % 30 == 29 || index + 1 == frames {
            save(
                device,
                queue,
                &texture,
                &directory.join(format!("frame-{index:03}.png")),
            )?;
        }
    }
    if let Some(timing) = &mut timing {
        for _ in 0..3 {
            let _ = device.poll(wgpu::PollType::wait_indefinitely());
            for frame in timing.begin_frame(device, queue) {
                times.add(frame);
            }
        }
    }
    if let Some(error) = renderer.fsr2_error() {
        println!("{}: FSR2 does not run here ({error}); TAA ran", run.name);
    }
    Ok(times)
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut frames = 120;
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
    let adapter =
        pollster::block_on(wgpu::Instance::default().request_adapter(&Default::default()))?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_features: (adapter.features() & wgpu::Features::TIMESTAMP_QUERY)
            | sgl_3d::graphics_device::features(&adapter)
            | sgl_3d::graphics_device::fsr2_features(&adapter),
        required_limits: sgl_3d::graphics_device::limits(&adapter),
        ..Default::default()
    }))?;
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/water-example");
    println!(
        "{frames} frames per run at {}x{}; GPU time median / p95",
        SIZE[0], SIZE[1]
    );
    for run in runs() {
        render(&run, frames, (&device, &queue), &directory)?.report(run.name);
    }
    println!("Frames: {}", directory.canonicalize()?.display());
    Ok(())
}
