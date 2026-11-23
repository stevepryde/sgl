//! Many instances of a few models: props of three procedural models placed
//! on a grid over the ground, a quarter of them spinning, lit by a sun with
//! a cascaded shadow and a point light that shadows them. SGL3D draws the
//! instances of each model's meshes together (instanced draws); the example
//! prints the camera's draws and the CPU time recording each frame took.
//!
//! Run with `cargo run --release -p sgl-3d-examples --example instances -- target/instances.png`.
//! Optional `--count N` sets how many props (1000) and `--frames N` how many
//! frames it renders (62); the timing skips the first two. `--churn N`
//! removes N still props every other frame after the first two and places
//! N in their place, as a game despawns and spawns: under TAA each change
//! marks only where the prop stood and stands (the history register's
//! M-23). `--high` renders at the High preset (TAA), `--size W H` at
//! another size (320 240), and `--passes` prints the median GPU time of
//! each pass group over the timed frames, where the device has timestamp
//! queries: with `--churn`, apart for the frames that changed props and
//! those that did not, which one run times alike.
use sgl_3d::glam::camera;
use sgl_3d::{
    Camera, DirectionalLight, DirectionalShadow, FrameInput, HemisphereLight, InstanceState, Light,
    LightShape, Mobility, Renderer, Scene, View, ViewFrame, ViewOutput,
    asset::{Asset, CpuMesh, Material, Vertex},
    glam::{Mat4, Vec3},
    settings::{RenderPreset, Settings},
};
use std::{error::Error, path::PathBuf};

struct Options {
    output: PathBuf,
    count: usize,
    frames: u32,
    churn: usize,
    high: bool,
    size: [u32; 2],
    passes: bool,
}

impl Options {
    fn parse() -> Result<Self, Box<dyn Error>> {
        let mut options = Self {
            output: "target/sgl-3d-instances.png".into(),
            count: 1000,
            frames: 62,
            churn: 0,
            high: false,
            size: [320, 240],
            passes: false,
        };
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--count" => options.count = args.next().ok_or("--count requires N")?.parse()?,
                "--frames" => options.frames = args.next().ok_or("--frames requires N")?.parse()?,
                "--churn" => options.churn = args.next().ok_or("--churn requires N")?.parse()?,
                "--high" => options.high = true,
                "--passes" => options.passes = true,
                "--size" => {
                    for side in &mut options.size {
                        *side = args.next().ok_or("--size requires W H")?.parse()?;
                    }
                }
                "--help" | "-h" => {
                    println!(
                        "instances [output.png] [--count N] [--frames N] [--churn N] [--high] [--size W H] [--passes]"
                    );
                    std::process::exit(0);
                }
                flag if flag.starts_with('-') => {
                    return Err(format!("unknown option {flag}").into());
                }
                _ => options.output = arg.into(),
            }
        }
        if options.frames < 3 {
            return Err("--frames must be at least three".into());
        }
        Ok(options)
    }
}

fn material(base: [f32; 4], emissive: [f32; 3], metallic: f32, roughness: f32) -> Material {
    Material {
        name: "procedural surface".into(),
        base,
        emissive,
        metallic,
        roughness,
        coat_roughness: 0.3,
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
                uv1: [0.0; 2],
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

fn asset(meshes: Vec<CpuMesh>, materials: Vec<Material>) -> Asset {
    Asset {
        meshes,
        materials,
        images: Vec::new(),
        rig: Default::default(),
        ignored: Vec::new(),
        lights: Default::default(),
        instanced: Vec::new(),
    }
}

/// Three prop models: a cube, a post with a glowing cap (two meshes and
/// materials) and a tile sharing the cube's material values.
fn props() -> [Asset; 3] {
    let stone = material([0.45, 0.42, 0.38, 1.], [0.; 3], 0., 0.7);
    [
        asset(
            vec![cuboid(Vec3::new(0., 0.5, 0.), Vec3::ONE, 0)],
            vec![stone.clone()],
        ),
        asset(
            vec![
                cuboid(Vec3::new(0., 0.8, 0.), Vec3::new(0.2, 1.6, 0.2), 0),
                cuboid(Vec3::new(0., 1.7, 0.), Vec3::splat(0.4), 1),
            ],
            vec![
                material([0.1, 0.1, 0.12, 1.], [0.; 3], 0.9, 0.3),
                material([0.9, 0.7, 0.4, 1.], [1.5, 1., 0.4], 0., 0.5),
            ],
        ),
        asset(
            vec![cuboid(Vec3::new(0., 0.05, 0.), Vec3::new(1., 0.1, 1.), 0)],
            vec![stone],
        ),
    ]
}

/// The pose of prop `index` of `count` on a grid over the 10 m × 8 m
/// ground, turned by `turn` radians.
fn prop_pose(index: usize, count: usize, turn: f32) -> Mat4 {
    let columns = (count as f32 * 10. / 8.).sqrt().ceil().max(1.) as usize;
    let spacing = 10. / columns as f32;
    let (column, row) = (index % columns, index / columns);
    let position = Vec3::new(
        -5. + (column as f32 + 0.5) * spacing,
        -0.05,
        -4. + (row as f32 + 0.5) * spacing,
    );
    Mat4::from_translation(position)
        * Mat4::from_rotation_y(turn + index as f32)
        * Mat4::from_scale(Vec3::splat(spacing * 0.4))
}

fn read_pixels(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
) -> Result<Vec<u8>, Box<dyn Error>> {
    let stride = (texture.width() * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
        * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("instances image readback"),
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

async fn run(options: Options) -> Result<(), Box<dyn Error>> {
    let adapter = wgpu::Instance::default()
        .request_adapter(&Default::default())
        .await?;
    let timestamps = if options.passes {
        adapter.features() & wgpu::Features::TIMESTAMP_QUERY
    } else {
        wgpu::Features::empty()
    };
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            required_features: (adapter.features() & wgpu::Features::SHADER_F16)
                | sgl_3d::graphics_device::features(&adapter)
                | timestamps,
            required_limits: sgl_3d::graphics_device::limits(&adapter),
            ..Default::default()
        })
        .await?;
    let mut scene = Scene::new(&device, &queue);
    let ground = asset(
        vec![cuboid(Vec3::new(0., -0.25, 0.), Vec3::new(10., 0.4, 8.), 0)],
        vec![material([0.12, 0.16, 0.2, 1.], [0.; 3], 0.1, 0.4)],
    );
    let ground = scene.add_asset(&device, &queue, ground)?.model;
    scene.add_instance(
        &device,
        &queue,
        InstanceState::new(ground),
        Mobility::Static,
    )?;
    // The same few models placed many times; every fourth spins, and moving
    // instances are posed every frame.
    let models = props()
        .into_iter()
        .map(|asset| Ok(scene.add_asset(&device, &queue, asset)?.model))
        .collect::<Result<Vec<_>, Box<dyn Error>>>()?;
    let mut spinning = Vec::new();
    // The still props, oldest first, which `--churn` replaces.
    let mut still = std::collections::VecDeque::new();
    for index in 0..options.count {
        let state = InstanceState {
            pose: prop_pose(index, options.count, 0.),
            ..InstanceState::new(models[index % models.len()])
        };
        if index % 4 == 3 {
            let id = scene.add_instance(&device, &queue, state, Mobility::Moving)?;
            spinning.push((id, index, state));
        } else {
            let id = scene.add_instance(&device, &queue, state, Mobility::Static)?;
            still.push_back((id, state));
        }
    }
    scene.add_light(
        &device,
        &queue,
        Light {
            position: Vec3::new(0.5, 2.5, 0.5),
            shape: LightShape::Point {
                radius: LightShape::DEFAULT_RADIUS,
            },
            color: [1., 0.8, 0.6],
            intensity: 30.,
            range: 6.,
            baked: false,
            specular: 1.,
            casts_shadow: true,
            ..Default::default()
        },
    )?;
    let size = options.size;
    let output = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("instances output"),
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
        preset: if options.high {
            RenderPreset::High
        } else {
            RenderPreset::Low
        },
        atmosphere: false,
        ..Settings::default()
    };
    let mut timing = sgl_3d::timing::GpuTiming::new(&device, &queue);
    // Whether each frame changed props, and each pass group's GPU times
    // over the timed frames that did (1) and did not (0), in first-use
    // order.
    let mut churned = Vec::new();
    let mut groups: [Vec<(&'static str, Vec<f64>)>; 2] = Default::default();
    let mut totals: [Vec<f64>; 2] = Default::default();
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
    let eye = Vec3::new(6., 4., 7.);
    let mut frame = FrameInput::new(Camera {
        view: camera::rh::view::look_at_mat4(eye, Vec3::new(0., 0.7, 0.), Vec3::Y),
        projection: sgl_3d::perspective(50f32.to_radians(), size[0] as f32 / size[1] as f32, 0.1),
        eye,
    });
    frame.directional_lights[0] = Some(DirectionalLight {
        direction: Vec3::new(0.4, -1., -0.5),
        color: [1., 0.85, 0.7],
        illuminance: 3.,
        shadow: Some(DirectionalShadow {
            distance: 20.,
            cascades: 2,
        }),
        ..Default::default()
    });
    frame.hemisphere_light = HemisphereLight {
        sky_color: [0.2, 0.3, 0.5],
        ground_color: [0.05, 0.03, 0.02],
        intensity: 0.2,
    };
    // CPU time recording each frame: `Renderer::render` and finishing the
    // encoder, over every frame and apart for those that changed no prop (0)
    // and those that did (1).
    let mut recording = Vec::new();
    let mut recording_by_change: [Vec<f64>; 2] = Default::default();
    for frame_index in 0..options.frames {
        let phase = frame_index as f32 / options.frames as f32;
        for &(id, index, mut state) in &spinning {
            state.pose = prop_pose(index, options.count, phase);
            scene.set_instance(&queue, id, state)?;
        }
        let churn = frame_index >= 2 && frame_index % 2 == 0;
        churned.push(churn && options.churn > 0);
        if churn {
            for _ in 0..options.churn.min(still.len()) {
                let (id, state) = still.pop_front().expect("a still prop");
                scene.remove_instance(id)?;
                let id = scene.add_instance(&device, &queue, state, Mobility::Static)?;
                still.push_back((id, state));
            }
        }
        if let Some(timing) = &mut timing {
            for timed in timing.begin_frame(&device, &queue) {
                // `GpuTiming` counts frames from 1; skip the first two.
                if timed.frame <= 2 {
                    continue;
                }
                let changed = usize::from(churned[timed.frame as usize - 1]);
                totals[changed].push(timed.total_ms);
                let groups = &mut groups[changed];
                for group in &timed.passes {
                    match groups.iter_mut().find(|(name, _)| *name == group.name) {
                        Some((_, times)) => times.push(group.ms),
                        None => groups.push((group.name, vec![group.ms])),
                    }
                }
            }
        }
        frame.elapsed_seconds = frame_index as f64 / 60.;
        frame.camera_cut = frame_index == 0;
        scene_view.resize(&device, size, 1., &settings);
        let mut encoder = device.create_command_encoder(&Default::default());
        let started = std::time::Instant::now();
        renderer.render(
            &device,
            &queue,
            &mut encoder,
            &mut scene,
            &mut [ViewFrame {
                view: &mut scene_view,
                input: &frame,
                settings: &settings,
                output: ViewOutput::Texture(&output_view),
                timing: timing.as_ref(),
            }],
        );
        let commands = encoder.finish();
        if frame_index >= 2 {
            let ms = started.elapsed().as_secs_f64() * 1000.;
            recording.push(ms);
            recording_by_change[usize::from(churned[frame_index as usize])].push(ms);
        }
        let submission = queue.submit([commands]);
        if let Some(timing) = &mut timing {
            timing.submitted(&queue);
            // One frame on the GPU at a time, so a frame's time is its own.
            device.poll(wgpu::PollType::Wait {
                submission_index: Some(submission),
                timeout: None,
            })?;
        }
        renderer.finish_frame(&mut scene);
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
    // The last frame's statistics, read back once it completed.
    let _ = device.poll(wgpu::PollType::wait_indefinitely());
    let stats = scene_view.geometry_stats(&device).unwrap_or_default();
    recording.sort_by(f64::total_cmp);
    println!(
        "{} props: camera sections static {} / moving {}, triangles {}; recording CPU median {:.3} ms over {} frames; {}",
        options.count,
        stats.static_instances.0,
        stats.moving_instances.0,
        stats.total().1,
        recording[recording.len() / 2],
        recording.len(),
        options.output.display()
    );
    if options.churn > 0 {
        let [unchanged, changed] = recording_by_change.map(|mut times| {
            times.sort_by(f64::total_cmp);
            times[times.len() / 2]
        });
        println!(
            "recording CPU median {changed:.3} ms in frames that removed and placed {} props, {unchanged:.3} ms in those that changed none",
            options.churn
        );
    }
    if options.passes {
        if timing.is_none() {
            println!("no GPU timestamps: the device has no timestamp queries");
        }
        let median = |times: &mut Vec<f64>| {
            times.sort_by(f64::total_cmp);
            times[times.len() / 2]
        };
        for (changed, (totals, groups)) in totals.iter_mut().zip(&mut groups).enumerate() {
            if totals.is_empty() {
                continue;
            }
            println!(
                "GPU frame median {:.3} ms over {} frames {}",
                median(totals),
                totals.len(),
                if changed == 1 {
                    format!("that removed and placed {} props", options.churn)
                } else {
                    "that changed no prop".into()
                }
            );
            for (name, times) in groups {
                println!("    {name:<30}{:>10.3}", median(times));
            }
        }
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    pollster::block_on(run(Options::parse()?))
}
