//! Outlining selected instances (`Scene::add_selection`,
//! `FrameInput::outline`): a game highlights the block its player targets,
//! orange where it is in sight and dimmer where a wall hides part of it,
//! and then a selection of every block, as a box-select would make.
//!
//! `cargo run --release -p sgl-3d-examples --example outline [-- --frames N] [--size WxH]`
//!
//! Renders the scene with no outline, with the targeted block outlined
//! (sparse) and with every block outlined (dense), prints the median GPU
//! time of the frame and of the `outline` passes for each, and writes the
//! last frame of each to `target/outline-example/`.
use sgl_3d::glam::{Mat4, Vec3, camera};
use sgl_3d::{
    Camera, DirectionalLight, DirectionalShadow, FrameInput, InstanceId, InstanceState, Mobility,
    Outline, Renderer, Scene, View, ViewFrame, ViewOutput,
    asset::{Asset, CpuMesh, Material, Vertex},
    settings::Settings,
    timing::GpuTiming,
};
use std::error::Error;
use std::path::Path;

const SIZE: [u32; 2] = [1920, 1080];
const WARM_UP: u64 = 10;
/// Blocks along each side of the grid, a metre apart.
const GRID: usize = 12;

/// A box `size` metres across, its foot centred at the origin.
fn block(size: Vec3, base: [f32; 4]) -> Asset {
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    for (axis, u, v) in [
        (Vec3::X, Vec3::Y, Vec3::Z),
        (Vec3::Y, Vec3::Z, Vec3::X),
        (Vec3::Z, Vec3::X, Vec3::Y),
    ] {
        for sign in [-1., 1.] {
            let start = vertices.len() as u32;
            for (x, y) in [(-1., -1.), (1., -1.), (1., 1.), (-1., 1.)] {
                let corner = axis * sign * 0.5 + u * x * 0.5 + v * y * sign * 0.5;
                vertices.push(Vertex {
                    position: (corner * size + Vec3::Y * size.y * 0.5).to_array(),
                    normal: (axis * sign).to_array(),
                    tangent: (u.extend(1.)).to_array(),
                    uv: [(x + 1.) * 0.5, (y + 1.) * 0.5],
                    color: [1.; 4],
                    lightmap_uv: [0.; 2],
                    lightmap_bounds: [0., 0., 1., 1.],
                });
            }
            indices.extend([0, 1, 2, 0, 2, 3].map(|index| start + index));
        }
    }
    Asset {
        meshes: vec![CpuMesh {
            vertices,
            indices,
            material: 0,
            deformation: Default::default(),
        }],
        materials: vec![Material {
            name: "block".into(),
            base,
            roughness: 0.7,
            ..Default::default()
        }],
        images: Vec::new(),
        rig: Default::default(),
        ignored: Vec::new(),
        lights: Default::default(),
        instanced: Vec::new(),
    }
}

fn place(
    (device, queue): (&wgpu::Device, &wgpu::Queue),
    scene: &mut Scene,
    model: sgl_3d::ModelId,
    pose: Mat4,
) -> Result<InstanceId, Box<dyn Error>> {
    Ok(scene.add_instance(
        device,
        queue,
        InstanceState {
            pose,
            ..InstanceState::new(model)
        },
        Mobility::Static,
    )?)
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut frames = 60u64;
    let mut size = SIZE;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--frames" => frames = args.next().ok_or("--frames requires a count")?.parse()?,
            "--size" => {
                let given = args.next().ok_or("--size requires WxH")?;
                let (width, height) = given.split_once('x').ok_or("--size requires WxH")?;
                size = [width.parse()?, height.parse()?];
            }
            other => return Err(format!("unknown argument {other}").into()),
        }
    }
    if frames <= WARM_UP {
        return Err(format!("--frames must be above {WARM_UP}").into());
    }
    let adapter =
        pollster::block_on(wgpu::Instance::default().request_adapter(&Default::default()))?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_features: (adapter.features() & wgpu::Features::TIMESTAMP_QUERY)
            | sgl_3d::graphics_device::features(&adapter),
        required_limits: sgl_3d::graphics_device::limits(&adapter),
        ..Default::default()
    }))?;
    let gpu = (&device, &queue);
    let mut scene = Scene::new(&device, &queue);
    let ground = scene
        .add_asset(
            &device,
            &queue,
            block(Vec3::new(40., 0.1, 40.), [0.4, 0.38, 0.33, 1.]),
        )?
        .model;
    place(
        gpu,
        &mut scene,
        ground,
        Mat4::from_translation(Vec3::NEG_Y * 0.1),
    )?;
    let stone = scene
        .add_asset(
            &device,
            &queue,
            block(Vec3::splat(0.8), [0.6, 0.62, 0.65, 1.]),
        )?
        .model;
    let mut blocks = Vec::new();
    for index in 0..GRID * GRID {
        let at = Vec3::new(
            (index % GRID) as f32 - (GRID as f32 - 1.) * 0.5,
            0.,
            -((index / GRID) as f32) - 2.,
        );
        blocks.push(place(gpu, &mut scene, stone, Mat4::from_translation(at))?);
    }
    // A wall in front of the targeted block's left half.
    let wall = scene
        .add_asset(
            &device,
            &queue,
            block(Vec3::new(1.2, 2., 0.2), [0.5, 0.3, 0.25, 1.]),
        )?
        .model;
    let target = blocks[2 * GRID + GRID / 2];
    let target_at = scene.instance(target)?.pose.w_axis.truncate();
    place(
        gpu,
        &mut scene,
        wall,
        Mat4::from_translation(target_at + Vec3::new(-0.7, 0., 1.2)),
    )?;
    // The game's selections: what the crosshair targets, and a box-select.
    let targeted = scene.add_selection(&[target])?;
    let everything = scene.add_selection(&blocks)?;
    let outline = |selection| Outline {
        width: 3.,
        visible_color: [1., 0.6, 0.1, 1.],
        hidden_color: [1., 0.6, 0.1, 0.35],
        ..Outline::new(selection)
    };
    let eye = Vec3::new(0., 3.5, 4.);
    let camera = Camera {
        eye,
        view: camera::rh::view::look_at_mat4(eye, target_at, Vec3::Y),
        projection: sgl_3d::perspective(0.9, size[0] as f32 / size[1] as f32, 0.1),
    };
    let settings = Settings::default();
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/outline-example");
    std::fs::create_dir_all(&directory)?;
    println!(
        "{frames} frames at {}x{}; GPU time median",
        size[0], size[1]
    );
    for (name, selected) in [
        ("none", None),
        ("sparse", Some(outline(targeted))),
        ("dense", Some(outline(everything))),
    ] {
        let output = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("outline example output"),
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
        });
        let output_view = output.create_view(&Default::default());
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
        let mut timing = GpuTiming::new(&device, &queue);
        let (mut totals, mut outline_times, mut passes) = (Vec::new(), Vec::new(), 0);
        let mut collect = |frame: sgl_3d::timing::FrameTime| {
            if frame.frame > WARM_UP {
                totals.push(frame.total_ms);
                passes = frame.passes.iter().map(|pass| pass.passes).sum();
                let outline = frame.passes.iter().find(|pass| pass.name == "outline");
                outline_times.push(outline.map_or(0., |pass| pass.ms));
            }
        };
        for index in 0..frames {
            let mut input = FrameInput::new(camera);
            input.camera_cut = index == 0;
            input.elapsed_seconds = index as f64 / 60.;
            input.directional_lights[0] = Some(DirectionalLight {
                direction: Vec3::new(-0.4, -1., -0.6),
                illuminance: 4.,
                shadow: Some(DirectionalShadow {
                    distance: 30.,
                    cascades: 3,
                }),
                ..Default::default()
            });
            input.outline = selected;
            if let Some(timing) = &mut timing {
                timing.begin_frame(&device, &queue).for_each(&mut collect);
            }
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
                    timing: timing.as_ref(),
                }],
            );
            queue.submit([encoder.finish()]);
            if let Some(timing) = &mut timing {
                timing.submitted(&queue);
            }
            device.poll(wgpu::PollType::wait_indefinitely())?;
            renderer.finish_frame(&mut scene);
        }
        if let Some(timing) = &mut timing {
            for _ in 0..3 {
                device.poll(wgpu::PollType::wait_indefinitely())?;
                timing.begin_frame(&device, &queue).for_each(&mut collect);
            }
        }
        let median = |times: &mut Vec<f64>| {
            times.sort_by(f64::total_cmp);
            times.get(times.len() / 2).copied().unwrap_or(f64::NAN)
        };
        println!(
            "{name:<7} frame {:.3} ms, outline {:.3} ms, {passes} timed passes",
            median(&mut totals),
            median(&mut outline_times)
        );
        save(
            &device,
            &queue,
            &output,
            &directory.join(format!("{name}.png")),
        )?;
    }
    println!("Frames: {}", directory.canonicalize()?.display());
    Ok(())
}

/// Writes `texture`'s RGBA8 texels to `path` as a PNG.
fn save(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    path: &Path,
) -> Result<(), Box<dyn Error>> {
    let row = texture.width() * 4;
    let stride = row.next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("outline example readback"),
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
        .get_mapped_range(..)?
        .chunks_exact(stride as usize)
        .flat_map(|line| line[..row as usize].to_vec())
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
