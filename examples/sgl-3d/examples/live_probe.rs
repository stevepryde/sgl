//! A live specular probe: a chrome ball rolls among coloured pillars and a
//! glowing cart, and a probe that follows it shows them in its surface as
//! they move. The scene owns the probe (`Scene::add_live_specular_probe`);
//! each frame the game moves it to the ball (`set_live_specular_probe`) and
//! asks the renderer for a step of its refresh
//! (`Renderer::refresh_specular_probe`) before it renders. The probe hides
//! the ball's visibility group (`LiveSpecularProbe::hidden_groups`), so the
//! ball is not in its own probe. Nothing is read back: the cube stays on
//! the GPU, and the probe shows its last whole cube until the next is
//! published.
//!
//! Run with `cargo run -p sgl-3d-examples --example live_probe -- target/live_probe.png`.
//! Optional `--frames N` sets how many frames run and `--pace whole`,
//! `--pace faces` or `--pace face` how a refresh is spread (`face`, one
//! face a frame, by default). `--measure` (use `--release`) prints, where
//! the device has timestamp queries, a frame's GPU time without a refresh
//! and under each pace: the refresh passes' time in the costliest frame of
//! a refresh and over a whole refresh, and the frames a refresh takes.
use sgl_3d::glam::{Mat4, Vec3, camera};
use sgl_3d::settings::Settings;
use sgl_3d::timing::{FrameTime, GpuTiming};
use sgl_3d::{
    Camera, DirectionalLight, DirectionalShadow, FrameInput, HemisphereLight, InstanceId,
    InstanceState, LiveSpecularProbe, LiveSpecularProbeId, Mobility, ProbeRefresh,
    ProbeRefreshPace, Renderer, Scene, SpecularProbeBox, View, ViewFrame, ViewOutput,
    asset::{Asset, CpuMesh, Material, Vertex},
};
use std::{error::Error, path::PathBuf};

const SIZE: [u32; 2] = [1280, 720];
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
/// The visibility group of the ball's material: frames show it, the
/// probe hides it.
const BALL_GROUP: u32 = 1;
const BALL_RADIUS: f32 = 0.7;
/// Each face of the probe's cube, in texels.
const FACE_SIZE: u32 = 128;
/// The frames `--measure` renders before those it counts, and those it
/// counts, for each pace.
const WARM_UP: u32 = 30;
const MEASURED: u32 = 210;

fn vertex(position: Vec3, normal: Vec3, tangent: Vec3) -> Vertex {
    Vertex {
        position: position.to_array(),
        normal: normal.to_array(),
        uv: [0.5; 2],
        color: [1.; 4],
        tangent: [tangent.x, tangent.y, tangent.z, 1.],
        lightmap_bounds: [0., 0., 1., 1.],
        lightmap_uv: [0.; 2],
    }
}

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
            mesh.vertices
                .push(vertex(center + (normal + u * s + v * t) * half, normal, u));
        }
        mesh.indices.extend([0, 1, 2, 0, 2, 3].map(|i| start + i));
    }
    mesh
}

/// A sphere of `radius` about its model's origin.
fn sphere(radius: f32) -> CpuMesh {
    const RINGS: u32 = 24;
    const SEGMENTS: u32 = 48;
    let mut mesh = CpuMesh {
        vertices: Vec::new(),
        indices: Vec::new(),
        material: 0,
        deformation: Default::default(),
    };
    for ring in 0..=RINGS {
        let polar = ring as f32 / RINGS as f32 * std::f32::consts::PI;
        for segment in 0..=SEGMENTS {
            let around = segment as f32 / SEGMENTS as f32 * std::f32::consts::TAU;
            let normal = Vec3::new(
                polar.sin() * around.cos(),
                polar.cos(),
                polar.sin() * around.sin(),
            );
            let tangent = Vec3::new(-around.sin(), 0., around.cos());
            mesh.vertices.push(vertex(normal * radius, normal, tangent));
        }
    }
    for ring in 0..RINGS {
        for segment in 0..SEGMENTS {
            let a = ring * (SEGMENTS + 1) + segment;
            let b = a + SEGMENTS + 1;
            mesh.indices.extend([a, a + 1, b, a + 1, b + 1, b]);
        }
    }
    mesh
}

fn material(base: [f32; 3], roughness: f32) -> Material {
    Material {
        name: "live probe example surface".into(),
        base: [base[0], base[1], base[2], 1.],
        roughness,
        metallic: 0.,
        ..Default::default()
    }
}

fn texture(device: &wgpu::Device) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("live probe example output"),
        size: wgpu::Extent3d {
            width: SIZE[0],
            height: SIZE[1],
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
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
        label: Some("live probe example readback"),
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

/// The example's scene: a ground, a ring of coloured pillars, a glowing
/// cart that circles outside them and the chrome ball. Returns the cart
/// and the ball.
fn build(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    scene: &mut Scene,
) -> Result<[(InstanceId, InstanceState); 2], Box<dyn Error>> {
    let mut add = |meshes: Vec<CpuMesh>, materials: Vec<Material>, mobility| {
        let asset = Asset {
            meshes,
            materials,
            images: Vec::new(),
            rig: Default::default(),
            ignored: Vec::new(),
            lights: Default::default(),
            instanced: Vec::new(),
        };
        let ids = scene.add_asset(device, queue, asset)?;
        let state = InstanceState::new(ids.model);
        let instance = scene.add_instance(device, queue, state, mobility)?;
        Ok::<_, Box<dyn Error>>((instance, state))
    };
    add(
        vec![cuboid(Vec3::new(0., -0.1, 0.), Vec3::new(40., 0.2, 40.), 0)],
        vec![material([0.35, 0.37, 0.4], 0.8)],
        Mobility::Static,
    )?;
    add(
        (0..8)
            .map(|i| {
                let angle = i as f32 * std::f32::consts::TAU / 8.;
                cuboid(
                    Vec3::new(6. * angle.cos(), 1.5, 6. * angle.sin()),
                    Vec3::new(0.8, 3., 0.8),
                    i % 3,
                )
            })
            .collect(),
        vec![
            material([0.8, 0.2, 0.15], 0.5),
            material([0.15, 0.6, 0.25], 0.5),
            material([0.2, 0.3, 0.85], 0.5),
        ],
        Mobility::Static,
    )?;
    let cart = add(
        vec![cuboid(Vec3::new(0., 0.5, 0.), Vec3::new(0.9, 1., 1.6), 0)],
        vec![Material {
            emissive: [4., 2.4, 0.4],
            ..material([0.9, 0.7, 0.2], 0.4)
        }],
        Mobility::Moving,
    )?;
    // The ball's material is in a visibility group of its own, which the
    // probe hides.
    let ball = add(
        vec![sphere(BALL_RADIUS)],
        vec![Material {
            metallic: 1.,
            visibility_group: BALL_GROUP,
            ..material([0.95, 0.95, 0.95], 0.05)
        }],
        Mobility::Moving,
    )?;
    Ok([cart, ball])
}

/// A probe about `center`: the ball's surroundings, seen as distant.
fn probe(center: Vec3) -> LiveSpecularProbe {
    LiveSpecularProbe {
        center,
        world_to_local: Mat4::from_translation(-center),
        influence: SpecularProbeBox {
            min: Vec3::splat(-2. * BALL_RADIUS),
            max: Vec3::splat(2. * BALL_RADIUS),
        },
        blend: Vec3::splat(0.5 * BALL_RADIUS),
        proxy: None,
        face_size: FACE_SIZE,
        // The ball is not in its own probe, and still shadows the ground
        // the probe shows.
        hidden_groups: BALL_GROUP,
    }
}

/// Everything a frame needs.
struct Stage {
    device: wgpu::Device,
    queue: wgpu::Queue,
    scene: Scene,
    renderer: Renderer,
    view: View,
    settings: Settings,
    output: wgpu::Texture,
    cart: (InstanceId, InstanceState),
    ball: (InstanceId, InstanceState),
    probe: LiveSpecularProbeId,
    timing: Option<GpuTiming>,
    frame: u32,
}

impl Stage {
    /// Moves the scene on, advances the probe's refresh by `pace` (none
    /// leaves it alone), then renders, submits and finishes one frame.
    /// Returns whether the frame published a cube and the GPU timings that
    /// completed meanwhile.
    fn frame(
        &mut self,
        pace: Option<ProbeRefreshPace>,
    ) -> Result<(bool, Vec<FrameTime>), Box<dyn Error>> {
        let frame = self.frame;
        self.frame += 1;
        let time = frame as f32 / 60.;
        // The cart circles outside the pillars; the ball rolls a figure of
        // eight between them.
        let around = time * 0.6;
        self.cart.1.pose =
            Mat4::from_translation(Vec3::new(9. * around.cos(), 0., 9. * around.sin()))
                * Mat4::from_rotation_y(-around);
        let center = Vec3::new(
            3.5 * (time * 0.5).sin(),
            BALL_RADIUS,
            2.5 * (time * 1.).sin(),
        );
        self.ball.1.pose = Mat4::from_translation(center);
        for (instance, state) in [self.cart, self.ball] {
            self.scene.set_instance(&self.queue, instance, state)?;
        }
        let eye = Vec3::new(0., 1.8, -4.6);
        let mut input = FrameInput::new(Camera {
            eye,
            view: camera::rh::view::look_at_mat4(eye, center, Vec3::Y),
            projection: sgl_3d::perspective(
                50f32.to_radians(),
                SIZE[0] as f32 / SIZE[1] as f32,
                0.1,
            ),
        });
        input.directional_lights[0] = Some(DirectionalLight {
            direction: Vec3::new(0.4, -1., 0.3),
            color: [1., 0.92, 0.8],
            illuminance: 3.,
            shadow: Some(DirectionalShadow {
                distance: 30.,
                cascades: 2,
            }),
            ..Default::default()
        });
        input.hemisphere_light = HemisphereLight {
            sky_color: [0.3, 0.4, 0.6],
            ground_color: [0.1, 0.08, 0.06],
            intensity: 0.4,
        };
        input.elapsed_seconds = f64::from(frame) / 60.;
        input.frame_time_ms = 1000. / 60.;
        // The frame shows the ball's group.
        input.visibility_mask = BALL_GROUP;
        let timings = match &mut self.timing {
            Some(timing) => timing.begin_frame(&self.device, &self.queue).collect(),
            None => Vec::new(),
        };
        self.view.resize(&self.device, SIZE, 1., &self.settings);
        let mut published = false;
        if let Some(pace) = pace {
            // The probe follows the ball: its influence now, and its
            // capture point from its next refresh.
            self.scene
                .set_live_specular_probe(&self.queue, self.probe, probe(center))?;
            // Between frames: after the last `finish_frame`, before
            // `render`, with the frame's own input.
            published = self.renderer.refresh_specular_probe(
                &self.device,
                &self.queue,
                &mut self.scene,
                &mut self.view,
                &input,
                &self.settings,
                self.timing.as_ref(),
                self.probe,
                pace,
            )? == ProbeRefresh::Published;
        }
        let target = self.output.create_view(&Default::default());
        let mut encoder = self.device.create_command_encoder(&Default::default());
        self.renderer.render(
            &self.device,
            &self.queue,
            &mut encoder,
            &mut self.scene,
            &mut [ViewFrame {
                view: &mut self.view,
                input: &input,
                settings: &self.settings,
                output: ViewOutput::Texture(&target),
                timing: self.timing.as_ref(),
            }],
        );
        let submission = self.queue.submit([encoder.finish()]);
        if let Some(timing) = &mut self.timing {
            timing.submitted(&self.queue);
        }
        self.renderer.finish_frame(&mut self.scene);
        if self.timing.is_some() {
            // One frame on the GPU at a time, so a frame's time is its own.
            self.device.poll(wgpu::PollType::Wait {
                submission_index: Some(submission),
                timeout: None,
            })?;
        }
        Ok((published, timings))
    }
}

/// The refresh's timing groups: its directional cascades, its faces and its
/// filter.
const GROUPS: [&str; 3] = [
    "probe refresh shadows",
    "probe refresh faces",
    "probe refresh filter",
];

/// One measured frame: the GPU time of all its timed passes, of each of
/// the refresh's groups, and how many passes it timed.
struct Measured {
    frame: u32,
    passes_ms: f64,
    refresh_ms: [f64; 3],
    passes: u32,
}

impl Measured {
    fn new(timed: &FrameTime) -> Self {
        Self {
            // `GpuTiming` counts frames from 1, one per `begin_frame`.
            frame: timed.frame as u32 - 1,
            passes_ms: timed.passes.iter().map(|group| group.ms).sum(),
            refresh_ms: GROUPS.map(|name| {
                timed
                    .passes
                    .iter()
                    .filter(|group| group.name == name)
                    .map(|group| group.ms)
                    .sum()
            }),
            passes: timed.passes.iter().map(|group| group.passes).sum(),
        }
    }
}

fn median(mut values: Vec<f64>) -> f64 {
    values.sort_by(f64::total_cmp);
    values.get(values.len() / 2).copied().unwrap_or(0.)
}

/// Renders frames without a refresh and under each pace, and prints their
/// GPU times: a frame's is the sum of its timed passes, the refresh's
/// among them, so time the GPU waits between the refresh's submission and
/// the frame's is not counted.
fn measure(stage: &mut Stage) -> Result<(), Box<dyn Error>> {
    if stage.timing.is_none() {
        println!("no GPU timestamps: the device has no timestamp queries");
        return Ok(());
    }
    let cube = u64::from(FACE_SIZE).pow(2) * 64;
    println!(
        "view {}x{}, probe faces {FACE_SIZE}x{FACE_SIZE}: its cube {:.2} MiB; {MEASURED} frames \
         a pace after {WARM_UP}; medians, ms of GPU passes",
        SIZE[0],
        SIZE[1],
        cube as f64 / 1048576.,
    );
    println!(
        "{:<19}{:>6}{:>9}{:>9}{:>10}{:>9}{:>9}{:>9}{:>9}{:>8}",
        "pace",
        "calls",
        "frame",
        "peak",
        "refresh",
        "shadows",
        "faces",
        "filter",
        "call max",
        "passes"
    );
    for (name, pace) in [
        ("no refresh", None),
        ("whole", Some(ProbeRefreshPace::Whole)),
        ("faces then filter", Some(ProbeRefreshPace::FacesThenFilter)),
        ("face per call", Some(ProbeRefreshPace::FacePerCall)),
    ] {
        let mut frames: Vec<Measured> = Vec::new();
        // The measured frames that published.
        let mut publications = Vec::new();
        let first = stage.frame + WARM_UP;
        let last = first + MEASURED;
        for _ in 0..WARM_UP + MEASURED + 8 {
            let rendered = stage.frame;
            let (published, timings) = stage.frame(pace)?;
            if published && (first..last).contains(&rendered) {
                publications.push(rendered);
            }
            frames.extend(
                timings
                    .iter()
                    .map(Measured::new)
                    .filter(|frame| (first..last).contains(&frame.frame)),
            );
        }
        if frames.is_empty() {
            println!("{name:<19} no completed timings");
            continue;
        }
        // Each whole refresh among the measured frames: its calls, each
        // group's time over them, and its costliest call's.
        let mut refreshes: Vec<(usize, [f64; 3], f64)> = Vec::new();
        for pair in publications.windows(2) {
            let calls: Vec<&Measured> = frames
                .iter()
                .filter(|frame| frame.frame > pair[0] && frame.frame <= pair[1])
                .collect();
            if calls.len() as u32 == pair[1] - pair[0] {
                refreshes.push((
                    calls.len(),
                    std::array::from_fn(|group| {
                        calls.iter().map(|call| call.refresh_ms[group]).sum()
                    }),
                    calls
                        .iter()
                        .map(|call| call.refresh_ms.iter().sum())
                        .fold(0., f64::max),
                ));
            }
        }
        let group = |index: usize| median(refreshes.iter().map(|r| r.1[index]).collect());
        // A frame's passes, and those of the frames with a refresh's
        // costliest call: the peak a frame pays.
        let frame = median(frames.iter().map(|frame| frame.passes_ms).collect());
        let call_max = median(refreshes.iter().map(|r| r.2).collect());
        let peak = median(
            frames
                .iter()
                .filter(|frame| frame.refresh_ms.iter().sum::<f64>() >= call_max * 0.9)
                .map(|frame| frame.passes_ms)
                .collect(),
        );
        println!(
            "{name:<19}{:>6}{:>9.3}{:>9.3}{:>10.3}{:>9.3}{:>9.3}{:>9.3}{:>9.3}{:>8}",
            refreshes.first().map_or(0, |refresh| refresh.0),
            frame,
            peak,
            group(0) + group(1) + group(2),
            group(0),
            group(1),
            group(2),
            call_max,
            frames.iter().map(|frame| frame.passes).max().unwrap_or(0),
        );
    }
    Ok(())
}

async fn run(
    output: PathBuf,
    frames: u32,
    pace: ProbeRefreshPace,
    measuring: bool,
) -> Result<(), Box<dyn Error>> {
    let adapter = wgpu::Instance::default()
        .request_adapter(&Default::default())
        .await?;
    let timestamps = if measuring {
        adapter.features() & wgpu::Features::TIMESTAMP_QUERY
    } else {
        wgpu::Features::empty()
    };
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            required_features: sgl_3d::graphics_device::features(&adapter) | timestamps,
            required_limits: sgl_3d::graphics_device::limits(&adapter),
            ..Default::default()
        })
        .await?;
    let mut scene = Scene::new(&device, &queue);
    let [cart, ball] = build(&device, &queue, &mut scene)?;
    // The probe, at the ball: it weighs nothing until its first refresh is
    // published.
    let probe = scene.add_live_specular_probe(&device, &queue, probe(Vec3::Y * BALL_RADIUS))?;
    let renderer = Renderer::new(&device);
    let settings = Settings::default();
    let view = View::new(&renderer, &device, &queue, FORMAT, SIZE, 1., &settings)?;
    let mut stage = Stage {
        timing: GpuTiming::new(&device, &queue),
        output: texture(&device),
        device,
        queue,
        scene,
        renderer,
        view,
        settings,
        cart,
        ball,
        probe,
        frame: 0,
    };
    if measuring {
        return measure(&mut stage);
    }
    let mut publications = 0;
    for _ in 0..frames {
        publications += u32::from(stage.frame(Some(pace))?.0);
    }
    let pixels = read_pixels(&stage.device, &stage.queue, &stage.output)?;
    if let Some(parent) = output.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    image::save_buffer(&output, &pixels, SIZE[0], SIZE[1], image::ColorType::Rgba8)?;
    println!(
        "Rendered {frames} frames; the probe published {publications} cubes ({pace:?}); {}",
        output.display()
    );
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut output: PathBuf = "target/sgl-3d-live-probe.png".into();
    let mut frames = 120;
    let mut pace = ProbeRefreshPace::default();
    let mut measuring = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--frames" => frames = args.next().ok_or("--frames requires a count")?.parse()?,
            "--pace" => {
                pace = match args.next().as_deref() {
                    Some("whole") => ProbeRefreshPace::Whole,
                    Some("faces") => ProbeRefreshPace::FacesThenFilter,
                    Some("face") => ProbeRefreshPace::FacePerCall,
                    _ => return Err("--pace takes whole, faces or face".into()),
                }
            }
            "--measure" => measuring = true,
            flag if flag.starts_with('-') => return Err(format!("unknown option {flag}").into()),
            _ => output = arg.into(),
        }
    }
    pollster::block_on(run(output, frames.max(1), pace, measuring))
}
