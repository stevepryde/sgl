//! An underwater colour shift as a game's HDR effect (`Scene::add_hdr_effect`,
//! `FrameInput::hdr_effect`): light from each surface is absorbed along the
//! view ray, blue-green water keeping its blue and green farthest, and the
//! water's own colour scatters in as it goes, so far pillars fade into the
//! water while near ones keep their colour. The effect reads the pixel's
//! linear HDR colour and its distance along the view ray, before depth of
//! field, motion blur, bloom and the tone map.
//!
//! `cargo run --release -p sgl-3d-examples --example hdr_effect [-- --frames N] [--size WxH]`
//!
//! Renders the scene with the effect and without it, prints the effect's
//! linear colour at the centre of each white pillar against the colour the
//! water's absorption and scattering give at that pillar's distance,
//! computed here from the scene, and the median GPU time of the frame and
//! of the effect's pass, and writes the last frame of each run to
//! `target/hdr-effect-example/`.
use sgl_3d::diagnostics::DiagnosticTarget;
use sgl_3d::glam::{Mat4, Vec3, camera};
use sgl_3d::{
    Backdrop, Camera, Exposure, FrameInput, InstanceState, Mobility, Renderer, Scene, ShaderSource,
    View, ViewFrame, ViewOutput,
    asset::{Asset, CpuMesh, Material, Vertex},
    settings::Settings,
    timing::GpuTiming,
};
use std::error::Error;
use std::path::Path;

const SIZE: [u32; 2] = [1280, 720];
const WARM_UP: u64 = 10;
/// Metres along the camera's axis to each pillar.
const PILLARS: [f32; 5] = [3., 6., 12., 24., 48.];
/// The pillars' radiance, unlit: a known HDR colour.
const WHITE: [f32; 3] = [1.; 3];
/// The camera's height above the sea floor and its vertical field of view.
const EYE: Vec3 = Vec3::new(0., 1., 0.);
const FOV_Y: f32 = 0.9;

/// The game's effect. `absorption` is the water's Beer-Lambert coefficient
/// per metre a channel; `scatter` the radiance its in-scattered light
/// reaches far away. A surface `d` metres along the ray keeps
/// `exp(-absorption d)` of its light, and the water adds the rest of
/// `scatter`; the sky, at `SCENE_DEPTH_FAR`, is the water's colour.
const UNDERWATER: &str = r#"
struct EffectParams {
 absorption:vec3<f32>,
 unused:f32,
 scatter:vec3<f32>,
 unused_too:f32,
}
fn hdr_effect(color:vec3<f32>,ctx:EffectContext,params:EffectParams)->vec3<f32> {
 let transmittance=exp(-params.absorption*ctx.distance);
 return color*transmittance+params.scatter*(1.-transmittance);
}
"#;

/// `EffectParams`, mirrored; checked against the layout naga gave it.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct UnderwaterParams {
    absorption: [f32; 3],
    unused: f32,
    scatter: [f32; 3],
    unused_too: f32,
}

const WATER: UnderwaterParams = UnderwaterParams {
    absorption: [0.12, 0.045, 0.03],
    unused: 0.,
    scatter: [0.02, 0.09, 0.12],
    unused_too: 0.,
};

/// A card `width` by `height` metres facing +Z, its foot centred at the
/// origin, or a floor `width` by `height` across x and z when `floor`.
fn card(width: f32, height: f32, floor: bool, base: [f32; 4]) -> Asset {
    let corner = |u: f32, v: f32| {
        let position = if floor {
            [(u - 0.5) * width, 0., -v * height]
        } else {
            [(u - 0.5) * width, v * height, 0.]
        };
        Vertex {
            position,
            normal: if floor { [0., 1., 0.] } else { [0., 0., 1.] },
            tangent: [1., 0., 0., 1.],
            uv: [u, 1. - v],
            color: [1.; 4],
            lightmap_uv: [0.; 2],
            lightmap_bounds: [0., 0., 1., 1.],
        }
    };
    Asset {
        meshes: vec![CpuMesh {
            vertices: vec![
                corner(0., 0.),
                corner(1., 0.),
                corner(1., 1.),
                corner(0., 1.),
            ],
            indices: vec![0, 1, 2, 0, 2, 3],
            material: 0,
            deformation: Default::default(),
        }],
        materials: vec![Material {
            name: "unlit".into(),
            base,
            unlit: true,
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
    asset: Asset,
    pose: Mat4,
) -> Result<(), Box<dyn Error>> {
    let ids = scene.add_asset(device, queue, asset)?;
    scene.add_instance(
        device,
        queue,
        InstanceState {
            pose,
            ..InstanceState::new(ids.model)
        },
        Mobility::Static,
    )?;
    Ok(())
}

/// Each pillar's x, so all of them show side by side.
fn pillar_x(index: usize) -> f32 {
    (index as f32 - 2.) * 0.18 * PILLARS[index]
}

fn camera_of(size: [u32; 2]) -> Camera {
    Camera {
        eye: EYE,
        view: camera::rh::view::look_at_mat4(EYE, EYE - Vec3::Z, Vec3::Y),
        projection: sgl_3d::perspective(FOV_Y, size[0] as f32 / size[1] as f32, 0.1),
    }
}

/// The pixel at the centre of pillar `index`, 1 m above the floor (the
/// camera's height), and the metres along its view ray to it.
fn pillar_pixel(index: usize, size: [u32; 2]) -> ([u32; 2], f32) {
    let depth = PILLARS[index];
    let x = pillar_x(index);
    let tan = (FOV_Y * 0.5).tan();
    let aspect = size[0] as f32 / size[1] as f32;
    let ndc = x / (depth * tan * aspect);
    let pixel = [((ndc + 1.) * 0.5 * size[0] as f32) as u32, size[1] / 2];
    (pixel, (x * x + depth * depth).sqrt())
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
    place(
        gpu,
        &mut scene,
        card(200., 200., true, [0.35, 0.3, 0.22, 1.]),
        Mat4::from_translation(Vec3::new(0., 0., 2.)),
    )?;
    for (index, &depth) in PILLARS.iter().enumerate() {
        let [r, g, b] = WHITE;
        place(
            gpu,
            &mut scene,
            card(0.12 * depth, 2.5, false, [r, g, b, 1.]),
            Mat4::from_translation(Vec3::new(pillar_x(index), 0., -depth)),
        )?;
    }
    let effect = scene.add_hdr_effect(ShaderSource {
        wgsl: UNDERWATER.into(),
        label: "underwater".into(),
    })?;
    // A mirror that drifted from the WGSL would write its members at the
    // wrong offsets.
    let layout = scene.hdr_effect_parameters_layout(effect)?;
    let offsets: Vec<_> = layout.fields.iter().map(|f| f.offset as usize).collect();
    if offsets
        != [
            std::mem::offset_of!(UnderwaterParams, absorption),
            std::mem::offset_of!(UnderwaterParams, unused),
            std::mem::offset_of!(UnderwaterParams, scatter),
            std::mem::offset_of!(UnderwaterParams, unused_too),
        ]
        || layout.size as usize != size_of::<UnderwaterParams>()
    {
        return Err(format!("UnderwaterParams does not mirror EffectParams: {layout:?}").into());
    }
    let underwater = scene.hdr_effect(effect, bytemuck::bytes_of(&WATER))?;
    let settings = Settings::default();
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/hdr-effect-example");
    std::fs::create_dir_all(&directory)?;
    println!(
        "{frames} frames at {}x{}; GPU time median",
        size[0], size[1]
    );
    for (name, applied) in [("underwater", Some(underwater)), ("none", None)] {
        let output = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("hdr effect example output"),
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
        let (mut totals, mut effect_times, mut passes) = (Vec::new(), Vec::new(), 0);
        let mut collect = |frame: sgl_3d::timing::FrameTime| {
            if frame.frame > WARM_UP {
                totals.push(frame.total_ms);
                passes = frame.passes.iter().map(|pass| pass.passes).sum();
                let effect = frame.passes.iter().find(|pass| pass.name == "HDR effect");
                effect_times.push(effect.map_or(0., |pass| pass.ms));
            }
        };
        for index in 0..frames {
            let mut input = FrameInput::new(camera_of(size));
            input.camera_cut = index == 0;
            input.elapsed_seconds = index as f64 / 60.;
            input.backdrop = Backdrop::Color([0.; 3]);
            input.exposure = Exposure {
                stops: 0.,
                automatic: None,
            };
            input.hdr_effect = applied;
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
            "{name:<10} frame {:.3} ms, HDR effect {:.3} ms, {passes} timed passes",
            median(&mut totals),
            median(&mut effect_times)
        );
        if applied.is_some() {
            let texture = view
                .diagnostic_target(DiagnosticTarget::HdrEffect)
                .ok_or("the effect did not run")?
                .texture();
            let linear = read_rgba16f(&device, &queue, texture)?;
            for index in 0..PILLARS.len() {
                let ([x, y], distance) = pillar_pixel(index, size);
                let measured = linear[(y * size[0] + x) as usize];
                let expected: [f32; 3] = std::array::from_fn(|c| {
                    let kept = (-WATER.absorption[c] * distance).exp();
                    WHITE[c] * kept + WATER.scatter[c] * (1. - kept)
                });
                println!(
                    "  pillar {distance:5.1} m away: {:.4} {:.4} {:.4}, from its distance {:.4} {:.4} {:.4}",
                    measured[0], measured[1], measured[2], expected[0], expected[1], expected[2]
                );
            }
        }
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

/// `texture`'s RGBA16F texels, decoded.
fn read_rgba16f(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
) -> Result<Vec<[f32; 4]>, Box<dyn Error>> {
    let bytes = read(device, queue, texture, 8)?;
    let half = |bits: u16| {
        let sign = if bits & 0x8000 != 0 { -1. } else { 1. };
        let exponent = i32::from((bits >> 10) & 0x1f);
        let mantissa = f32::from(bits & 0x3ff);
        sign * match exponent {
            0 => mantissa * 2f32.powi(-24),
            31 => f32::INFINITY,
            _ => (1. + mantissa / 1024.) * 2f32.powi(exponent - 15),
        }
    };
    Ok(bytes
        .chunks_exact(8)
        .map(|texel| {
            std::array::from_fn(|c| half(u16::from_le_bytes([texel[2 * c], texel[2 * c + 1]])))
        })
        .collect())
}

fn save(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    path: &Path,
) -> Result<(), Box<dyn Error>> {
    let pixels = read(device, queue, texture, 4)?;
    image::save_buffer(
        path,
        &pixels,
        texture.width(),
        texture.height(),
        image::ColorType::Rgba8,
    )?;
    Ok(())
}

/// `texture`'s texels of `bytes` each, row after row.
fn read(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    bytes: u32,
) -> Result<Vec<u8>, Box<dyn Error>> {
    let stride = (texture.width() * bytes).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("hdr effect example readback"),
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
    let row = (texture.width() * bytes) as usize;
    let pixels = buffer
        .get_mapped_range(..)?
        .chunks_exact(stride as usize)
        .flat_map(|line| line[..row].to_vec())
        .collect();
    Ok(pixels)
}
