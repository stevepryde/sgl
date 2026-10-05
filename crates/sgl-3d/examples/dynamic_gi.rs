//! Dynamic diffuse GI in a lit interior: a room with red and green walls,
//! lit by the sun through a window and by a lamp under the ceiling, with two
//! boxes moving through it. A dynamic GI volume of probes fills the room
//! (`Scene::set_dynamic_gi_volume`), so the walls bounce coloured light onto
//! each other and onto the moving boxes, and the corners away from the
//! window stay dark; `--quality off` renders it with the frame's ambient
//! alone for comparison.
//!
//! Run with `cargo run -p sgl-3d --release --example dynamic_gi --
//! target/dynamic_gi.png`. Optional `--frames N` (120), `--quality
//! off|low|high` (`Settings::dynamic_gi`, High), `--probes X,Y,Z` (8,5,8)
//! spreading that many probes over the room, and `--timing`, which prints
//! the dynamic GI stage's GPU time in each frame, as its probes start and
//! settle, and each pass's median and 95th percentile over the second half
//! of the frames, where the device has timestamp queries; `--still` parks
//! the boxes, so the room's light settles and the volume pauses once it has,
//! and `--edit N` moves the lamp at frame N, which starts it again.
//! `--scroll` walks the camera along the room with a volume half as long
//! that follows it, installed each frame with its origin moved by whole
//! spacings, so it scrolls: the probes that stay keep their light, and those
//! that enter start afresh. Printed numbers are diagnostics, not image QA.
use sgl_3d::glam::camera;
use sgl_3d::{
    Camera, DirectionalLight, DirectionalShadow, DynamicGiVolume, FrameInput, HemisphereLight,
    InstanceState, Light, LightShape, Mobility, Renderer, Scene,
    asset::{Asset, CpuMesh, Material, Vertex},
    environment::{EnvironmentMap, PmremAtlas},
    glam::{Mat4, Vec3},
    settings::{DynamicGiQuality, Settings},
    timing::{FrameTime, GpuTiming},
};
use std::{error::Error, path::PathBuf};

struct Options {
    output: PathBuf,
    frames: u32,
    quality: DynamicGiQuality,
    probes: [u32; 3],
    timing: bool,
    still: bool,
    scroll: bool,
    edit: Option<u32>,
}

impl Options {
    fn parse() -> Result<Self, Box<dyn Error>> {
        let mut options = Self {
            output: "target/sgl-3d-dynamic-gi.png".into(),
            frames: 120,
            quality: DynamicGiQuality::High,
            probes: [8, 5, 8],
            timing: false,
            still: false,
            scroll: false,
            edit: None,
        };
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--frames" => {
                    options.frames = args.next().ok_or("--frames requires a count")?.parse()?
                }
                "--quality" => {
                    options.quality = match args.next().as_deref() {
                        Some("off") => DynamicGiQuality::Off,
                        Some("low") => DynamicGiQuality::Low,
                        Some("high") => DynamicGiQuality::High,
                        _ => return Err("--quality requires off, low or high".into()),
                    }
                }
                "--probes" => {
                    let counts = args.next().ok_or("--probes requires X,Y,Z")?;
                    let counts: Vec<u32> = counts
                        .split(',')
                        .map(str::parse)
                        .collect::<Result<_, _>>()?;
                    options.probes = counts
                        .try_into()
                        .map_err(|_| "--probes requires three counts")?;
                }
                "--timing" => options.timing = true,
                "--still" => options.still = true,
                "--scroll" => options.scroll = true,
                "--edit" => {
                    options.edit = Some(args.next().ok_or("--edit requires a frame")?.parse()?)
                }
                "--help" | "-h" => {
                    println!(
                        "dynamic_gi [output.png] [--frames N] [--quality off|low|high] [--probes X,Y,Z] [--timing] [--still] [--scroll] [--edit N]"
                    );
                    std::process::exit(0);
                }
                flag if flag.starts_with('-') => {
                    return Err(format!("unknown option {flag}").into());
                }
                _ => options.output = arg.into(),
            }
        }
        if options.frames == 0 {
            return Err("--frames must be at least one".into());
        }
        Ok(options)
    }
}

fn material(base: [f32; 3], roughness: f32) -> Material {
    Material {
        name: "room surface".into(),
        base: [base[0], base[1], base[2], 1.],
        metallic: 0.,
        roughness,
        ..Default::default() // glTF's default material
    }
}

fn cuboid(min: Vec3, max: Vec3, material: usize) -> CpuMesh {
    let center = (min + max) * 0.5;
    let half = (max - min) * 0.5;
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

/// The room's inside spans `HALF` metres either side of the origin along x
/// and z, and `HEIGHT` metres up from the floor.
const HALF: f32 = 4.;
const HEIGHT: f32 = 4.;
const WALL: f32 = 0.2;

/// The room: a grey floor and ceiling, a white back and front wall, a green
/// wall on the right and a red one on the left with a window in it.
fn room() -> Asset {
    let (h, t, top) = (HALF, WALL, HEIGHT);
    let window = (1.2, 3.0, 1.2); // its bottom and top, and half its width
    Asset {
        meshes: vec![
            cuboid(
                Vec3::new(-h - t, -t, -h - t),
                Vec3::new(h + t, 0., h + t),
                0,
            ),
            cuboid(
                Vec3::new(-h - t, top, -h - t),
                Vec3::new(h + t, top + t, h + t),
                0,
            ),
            cuboid(Vec3::new(-h, 0., -h - t), Vec3::new(h, top, -h), 1),
            cuboid(Vec3::new(-h, 0., h), Vec3::new(h, top, h + t), 1),
            cuboid(Vec3::new(h, 0., -h - t), Vec3::new(h + t, top, h + t), 2),
            // The red wall, around its window.
            cuboid(
                Vec3::new(-h - t, 0., -h - t),
                Vec3::new(-h, window.0, h + t),
                3,
            ),
            cuboid(
                Vec3::new(-h - t, window.1, -h - t),
                Vec3::new(-h, top, h + t),
                3,
            ),
            cuboid(
                Vec3::new(-h - t, window.0, -h - t),
                Vec3::new(-h, window.1, -window.2),
                3,
            ),
            cuboid(
                Vec3::new(-h - t, window.0, window.2),
                Vec3::new(-h, window.1, h + t),
                3,
            ),
        ],
        materials: vec![
            material([0.6, 0.6, 0.6], 0.8),
            material([0.75, 0.75, 0.75], 0.9),
            material([0.12, 0.55, 0.15], 0.9),
            material([0.7, 0.1, 0.08], 0.9),
        ],
        images: Vec::new(),
        rig: Default::default(),
    }
}

fn moving_box(base: [f32; 3]) -> Asset {
    Asset {
        meshes: vec![cuboid(Vec3::splat(-0.4), Vec3::splat(0.4), 0)],
        materials: vec![material(base, 0.5)],
        images: Vec::new(),
        rig: Default::default(),
    }
}

/// A sky of even radiance, seen through the window.
fn sky() -> EnvironmentMap {
    // A constant radiance field is unchanged by convolution at every
    // roughness: 16px cube faces in a 336x64 cube-UV atlas, RGBA16F texels
    // of (0.25, 0.375, 0.5, 1).
    EnvironmentMap {
        panorama: image::RgbaImage::from_pixel(4, 2, image::Rgba([64, 96, 128, 255])),
        filtered: PmremAtlas {
            width: 336,
            height: 64,
            rgba16: [0x3400u16, 0x3600, 0x3800, 0x3c00]
                .into_iter()
                .flat_map(u16::to_le_bytes)
                .cycle()
                .take(336 * 64 * 8)
                .collect(),
        },
    }
}

/// The median and 95th percentile of `values`.
fn median_p95(values: &mut [f64]) -> (f64, f64) {
    values.sort_by(f64::total_cmp);
    let at = |share: f64| values[((values.len() - 1) as f64 * share).round() as usize];
    (at(0.5), at(0.95))
}

/// The dynamic GI stage's passes in `frame`, in milliseconds.
fn dynamic_gi_ms(frame: &FrameTime) -> Vec<(&'static str, f64)> {
    frame
        .passes
        .iter()
        .filter(|pass| pass.name.starts_with("dynamic GI"))
        .map(|pass| (pass.name, pass.ms))
        .collect()
}

async fn run(options: Options) -> Result<(), Box<dyn Error>> {
    let adapter = wgpu::Instance::default()
        .request_adapter(&Default::default())
        .await?;
    let timestamps = options.timing && adapter.features().contains(wgpu::Features::TIMESTAMP_QUERY);
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            required_features: sgl_3d::graphics_device::features(&adapter)
                | if timestamps {
                    wgpu::Features::TIMESTAMP_QUERY
                } else {
                    wgpu::Features::empty()
                },
            required_limits: sgl_3d::graphics_device::limits(&adapter),
            ..Default::default()
        })
        .await?;
    println!(
        "GPU: {} ({:?})",
        adapter.get_info().name,
        adapter.get_info().backend
    );
    let size = [480, 320];
    let mut scene = Scene::new(&device, &queue);
    let room = scene.add_asset(&device, &queue, room())?.model;
    scene.add_instance(&device, &queue, InstanceState::new(room), Mobility::Static)?;
    let boxes = [[0.8, 0.8, 0.78], [0.15, 0.3, 0.8]].map(|base| {
        let model = scene
            .add_asset(&device, &queue, moving_box(base))
            .map(|ids| ids.model);
        model.and_then(|model| {
            let state = InstanceState::new(model);
            Ok((
                state,
                scene.add_instance(&device, &queue, state, Mobility::Moving)?,
            ))
        })
    });
    let mut boxes = boxes.into_iter().collect::<Result<Vec<_>, _>>()?;
    // A lamp under the ceiling at the back, casting the boxes' shadows.
    let lamp = Light {
        position: Vec3::new(1.8, 3.4, -2.6),
        shape: LightShape::Point,
        color: [1., 0.75, 0.45],
        intensity: 25.,
        range: 9.,
        casts_shadow: true,
        ..Default::default()
    };
    let lamp_id = scene.add_light(&device, &queue, lamp)?;
    // The outer layer of probes lies inside the walls, floor and ceiling,
    // halfway through each slab, so the volume covers every surface in the
    // room with no probe on one (a probe on a surface sees both its sides
    // at once). A probe inside a slab sees only the slab's backs, so the
    // receivers in the room weigh it as occluded.
    let counts = Vec3::from_array(options.probes.map(|n| n as f32));
    let room = Vec3::new(2. * HALF, HEIGHT, 2. * HALF);
    let spacing = (room + WALL) / (counts - 1.);
    let lattice = Vec3::new(-HALF, 0., -HALF) - WALL * 0.5;
    // With `--scroll`, half the probes along x, on the same lattice, centred
    // on the camera by whole spacings: the game keeps its volume on one
    // lattice as it moves it, and the scene scrolls it.
    let mut probes = options.probes;
    if options.scroll {
        probes[0] = probes[0] / 2 + 1;
    }
    let volume_at = |eye: Vec3| {
        let mut origin = lattice;
        if options.scroll {
            let first = (eye.x - lattice.x) / spacing.x - (probes[0] - 1) as f32 * 0.5;
            origin.x += first.round() * spacing.x;
        }
        DynamicGiVolume {
            origin,
            spacing,
            probes,
        }
    };
    let eye_at = |frame_index: u32| {
        let walk = if options.scroll {
            2.4 * (frame_index as f32 / 60. * 0.5).sin()
        } else {
            0.
        };
        Vec3::new(0.6 + walk, 1.7, 3.6)
    };
    scene.set_dynamic_gi_volume(&device, Some(volume_at(eye_at(0))))?;
    let environment = scene.add_environment(&device, &queue, &sky())?;
    let output = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("dynamic GI output"),
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
        dynamic_gi: options.quality,
        ..Settings::default()
    };
    let mut renderer = Renderer::new(&device, &queue, output.format(), size, 1., &settings)?;
    let projection = sgl_3d::perspective(65f32.to_radians(), size[0] as f32 / size[1] as f32, 0.1);
    let camera_at = |eye: Vec3| Camera {
        view: camera::rh::view::look_at_mat4(eye, eye + Vec3::new(-1., -0.4, -7.6), Vec3::Y),
        projection,
        eye,
    };
    let mut frame = FrameInput::new(camera_at(eye_at(0)));
    frame.environment = Some(environment);
    // The sun shines in through the window, onto the floor and the green
    // wall.
    frame.directional_lights[0] = Some(DirectionalLight {
        direction: Vec3::new(1., -0.55, -0.25),
        color: [1., 0.9, 0.75],
        illuminance: 12.,
        shadow: Some(DirectionalShadow {
            distance: 20.,
            cascades: 2,
        }),
        ..Default::default()
    });
    frame.hemisphere_light = HemisphereLight {
        sky_color: [0.3, 0.4, 0.55],
        ground_color: [0.1, 0.08, 0.06],
        intensity: 0.3,
    };
    frame.exposure.stops = 1.;
    let mut timing = timestamps
        .then(|| GpuTiming::new(&device, &queue))
        .flatten();
    let mut times = Vec::new();
    for frame_index in 0..options.frames {
        let phase = if options.still {
            0.
        } else {
            frame_index as f32 / 60. * 0.8
        };
        for (index, (state, instance)) in boxes.iter_mut().enumerate() {
            let angle = phase + index as f32 * std::f32::consts::PI;
            state.pose = Mat4::from_translation(Vec3::new(
                1.6 * angle.cos(),
                0.4 + 0.5 * index as f32,
                -1. + 1.6 * angle.sin(),
            )) * Mat4::from_rotation_y(angle);
            scene.set_instance(&queue, *instance, *state)?;
        }
        if options.scroll {
            let eye = eye_at(frame_index);
            frame.camera = camera_at(eye);
            scene.set_dynamic_gi_volume(&device, Some(volume_at(eye)))?;
        }
        if options.edit == Some(frame_index) {
            let moved = Light {
                position: Vec3::new(-1.6, 3.4, -2.6),
                ..lamp
            };
            scene.set_light(&queue, lamp_id, moved)?;
        }
        frame.elapsed_seconds = f64::from(frame_index) / 60.;
        frame.camera_cut = frame_index == 0;
        if let Some(timing) = &mut timing {
            times.extend(timing.begin_frame(&device, &queue));
        }
        let mut encoder = device.create_command_encoder(&Default::default());
        renderer.render(
            &device,
            &queue,
            &mut encoder,
            &mut scene,
            &frame,
            &settings,
            &output_view,
            timing.as_ref(),
        );
        queue.submit([encoder.finish()]);
        renderer.finish_frame(&mut scene);
        if let Some(timing) = &mut timing {
            timing.submitted(&queue);
            // Each frame's timings in turn, for the ramp's first frames.
            device.poll(wgpu::PollType::wait_indefinitely())?;
        }
    }
    if let Some(timing) = &mut timing {
        for _ in 0..8 {
            device.poll(wgpu::PollType::wait_indefinitely())?;
            times.extend(timing.begin_frame(&device, &queue));
        }
        report(&times);
    }
    let pixels = read_pixels(&device, &queue, &output)?;
    if let Some(parent) = options
        .output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    image::save_buffer(
        &options.output,
        &pixels,
        size[0],
        size[1],
        image::ColorType::Rgba8,
    )?;
    println!(
        "Rendered {} frames, {}x{}, dynamic GI {:?} with {}x{}x{} probes; {}",
        options.frames,
        size[0],
        size[1],
        options.quality,
        probes[0],
        probes[1],
        probes[2],
        options.output.display()
    );
    Ok(())
}

/// Prints the dynamic GI stage's GPU time in each frame, then the median
/// and 95th percentile of every pass over the second half of the frames,
/// once the probes have started.
fn report(times: &[FrameTime]) {
    let totals: Vec<String> = times
        .iter()
        .map(|frame| {
            let total: f64 = dynamic_gi_ms(frame).iter().map(|(_, ms)| ms).sum();
            format!("{total:.2}")
        })
        .collect();
    println!("dynamic GI per frame (ms): {}", totals.join(" "));
    let rest = &times[times.len() / 2..];
    let mut names: Vec<&str> = Vec::new();
    for pass in rest.iter().flat_map(|frame| &frame.passes) {
        if !names.contains(&pass.name) {
            names.push(pass.name);
        }
    }
    for name in names {
        let mut values: Vec<f64> = rest
            .iter()
            .flat_map(|frame| frame.passes.iter().filter(|pass| pass.name == name))
            .map(|pass| pass.ms)
            .collect();
        let (median, p95) = median_p95(&mut values);
        println!("{name:>32}: median {median:.3} ms, p95 {p95:.3} ms");
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
        label: Some("dynamic GI image readback"),
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
    let mapped = buffer.slice(..).get_mapped_range();
    Ok(mapped
        .chunks_exact(stride as usize)
        .flat_map(|row| row[..texture.width() as usize * 4].iter().copied())
        .collect())
}

fn main() -> Result<(), Box<dyn Error>> {
    pollster::block_on(run(Options::parse()?))
}
