//! Programmable surfaces (`Scene::add_shader`) beyond water: vegetation
//! bent by wind and tinted glass whose thickness varies across it, each the
//! game's own WGSL (`support/wind.wgsl`, `support/glass.wgsl`), which SGL3D
//! does not ship.
//!
//! `cargo run --release -p sgl-3d --example shaders [-- --frames N] [--size WxH]`
//!
//! A meadow of grass blades and a few trees, static instances whose masked
//! leaves the wind shader bends by each vertex's height weight (its shader
//! data) at a gust each instance offsets by its own data, under a sun with
//! cascaded shadows and a spot light whose shadow face catches the grass:
//! the G-buffer, the cascades and the local-light face displace alike, and
//! TAA reprojects the swaying leaves by their motion. In front stands a
//! glass pane that transmits a wall and the meadow behind it, 30 cm thick
//! at its foot and 2 cm at its top, its tint deepening with its thickness.
//! The spot light's shadow of the static meadow is its static layer, which
//! holds the leaves where they were bent when it was drawn. It prints
//! the median and 95th percentile GPU time of the passes the shaders touch
//! and writes frames to `target/shaders-example/` for the owner to judge.
use sgl_3d::glam::{Mat4, Vec3, camera};
use sgl_3d::{
    AlphaMode, Camera, DirectionalLight, DirectionalShadow, FrameInput, HemisphereLight,
    InstanceState, Light, LightShape, MaterialId, MaterialShader, Mobility, ModelMesh,
    PreparedModel, Renderer, Scene, ShaderSource,
    asset::{Asset, CpuMesh, Image, Material, Vertex},
    settings::{Antialiasing, Settings},
    timing::GpuTiming,
};
use std::collections::BTreeMap;
use std::error::Error;
use std::path::Path;

const WARM_UP: usize = 8;
/// A blade of grass's height and width, and the blades in the meadow.
const BLADE: [f32; 2] = [0.6, 0.12];
const BLADES: usize = 2500;
/// The wind's direction, the bend at a tip and the gusts per hour.
const WIND: [f32; 4] = [0.8, 0.6, 0.18, 1800.];
/// A tree canopy's bend at its top, more than a blade's for its height.
const CANOPY_WIND: [f32; 4] = [0.8, 0.6, 0.35, 900.];

/// `wind.wgsl`'s and `glass.wgsl`'s `ShaderParams`: a vec4 each.
type Params = [f32; 4];

fn vertex(position: Vec3, normal: Vec3, uv: [f32; 2]) -> Vertex {
    Vertex {
        tangent: [0.; 4],
        lightmap_bounds: [0., 0., 1., 1.],
        lightmap_uv: [0.; 2],
        position: position.to_array(),
        normal: normal.to_array(),
        uv,
        color: [1.; 4],
    }
}

/// Two quads crossed about the vertical through the origin, `size` wide
/// and tall, from height `base`: a blade's or a canopy's cards, with each
/// vertex's height weight (0 at the base, 1 at the top) as its shader
/// data's y.
fn cards(material: MaterialId, [height, width]: [f32; 2], base: f32) -> (ModelMesh, Vec<[f32; 4]>) {
    let mut vertices = Vec::new();
    let mut data = Vec::new();
    let mut indices = Vec::new();
    for across in [Vec3::X, Vec3::Z] {
        let normal = across.cross(Vec3::Y);
        let start = vertices.len() as u32;
        for (s, t) in [(0., 0.), (1., 0.), (1., 1.), (0., 1.)] {
            let position = across * (s - 0.5) * width + Vec3::Y * (base + t * height);
            vertices.push(vertex(position, normal, [s, 1. - t]));
            data.push([0., t, 0., 0.]);
        }
        indices.extend([0, 1, 2, 0, 2, 3].map(|index| start + index));
    }
    let mesh = ModelMesh {
        vertices,
        indices,
        material,
        deformation: Default::default(),
    };
    (mesh, data)
}

/// A box of `size` centred at `center`.
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
            mesh.vertices.push(vertex(
                center + (normal + u * s + v * t) * half,
                normal,
                [(s + 1.) * 0.5, (t + 1.) * 0.5],
            ));
        }
        mesh.indices.extend([0, 1, 2, 0, 2, 3].map(|i| start + i));
    }
    mesh
}

/// A leaf card's base colour and alpha: a tapering blade, opaque within it
/// and cut out beyond.
fn leaf(color: [u8; 3]) -> Image {
    Image::Rgba8(image::RgbaImage::from_fn(64, 64, |x, y| {
        let (u, v) = ((x as f32 + 0.5) / 64., (y as f32 + 0.5) / 64.);
        let inside = (u - 0.5).abs() < 0.45 * v.powf(0.7);
        image::Rgba([color[0], color[1], color[2], if inside { 255 } else { 0 }])
    }))
}

/// A material drawn through `shader`, moving its vertices at most `bound`.
fn shaded(material: Material, shader: sgl_3d::ShaderId, bound: f32) -> Material {
    Material {
        shader: Some(MaterialShader {
            shader,
            displacement_bound: bound,
        }),
        ..material
    }
}

/// A shader of the example's `wgsl`, and its `ShaderParams` checked to be
/// the vec4 `Params` mirrors.
fn add_shader(
    scene: &mut Scene,
    wgsl: &str,
    label: &str,
) -> Result<sgl_3d::ShaderId, Box<dyn Error>> {
    let shader = scene.add_shader(ShaderSource {
        wgsl: wgsl.into(),
        label: label.into(),
    })?;
    let layout = scene.shader_parameters_layout(shader)?;
    if layout.size as usize != size_of::<Params>() || layout.fields.len() != 1 {
        return Err(format!("{label}: ShaderParams is not one vec4: {layout:?}").into());
    }
    Ok(shader)
}

/// A pseudo-random number in 0..1 for `index`.
fn random(index: usize, salt: u32) -> f32 {
    let mut x = (index as u32).wrapping_mul(0x9e37_79b9) ^ salt.wrapping_mul(0x85eb_ca6b);
    x ^= x >> 15;
    x = x.wrapping_mul(0x2c1b_3c6d);
    x ^= x >> 12;
    (x & 0xffff) as f32 / 65536.
}

/// Places `model` as a static instance at `position`, its gust offset by
/// `phase`.
fn plant(
    (device, queue): (&wgpu::Device, &wgpu::Queue),
    scene: &mut Scene,
    model: sgl_3d::ModelId,
    pose: Mat4,
    phase: f32,
) -> Result<(), Box<dyn Error>> {
    let state = InstanceState {
        pose,
        ..InstanceState::new(model)
    };
    let instance = scene.add_instance(device, queue, state, Mobility::Static)?;
    scene.set_instance_shader_data(queue, instance, [phase, 0., 0., 0.])?;
    Ok(())
}

fn build(device: &wgpu::Device, queue: &wgpu::Queue) -> Result<Scene, Box<dyn Error>> {
    let gpu = (device, queue);
    let mut scene = Scene::new(device, queue);
    let wind = add_shader(&mut scene, include_str!("support/wind.wgsl"), "wind")?;
    let glass = add_shader(
        &mut scene,
        include_str!("support/glass.wgsl"),
        "tinted glass",
    )?;
    // The ground, tree trunks and a backdrop wall behind the glass.
    let plain = |name: &str, base: [f32; 4], roughness: f32| Material {
        name: name.into(),
        base,
        metallic: 0.,
        roughness,
        ..Default::default()
    };
    let mut world = vec![cuboid(Vec3::new(0., -0.1, 0.), Vec3::new(30., 0.2, 30.), 0)];
    let trees = [
        Vec3::new(-4., 0., -6.),
        Vec3::new(3., 0., -8.),
        Vec3::new(6., 0., -3.),
    ];
    for &tree in &trees {
        world.push(cuboid(tree + Vec3::Y * 1., Vec3::new(0.3, 2., 0.3), 1));
    }
    world.push(cuboid(Vec3::new(1.5, 0.75, 0.), Vec3::new(3., 1.5, 0.2), 2));
    let world = scene.add_asset(
        device,
        queue,
        Asset {
            meshes: world,
            materials: vec![
                plain("ground", [0.25, 0.22, 0.15, 1.], 0.9),
                plain("bark", [0.25, 0.17, 0.1, 1.], 0.8),
                plain("wall", [0.7, 0.3, 0.25, 1.], 0.6),
            ],
            images: Vec::new(),
            rig: Default::default(),
            ignored: Vec::new(),
        },
    )?;
    scene.add_instance(
        device,
        queue,
        InstanceState::new(world.model),
        Mobility::Static,
    )?;
    // Masked leaves through the wind shader, and the glass through its own.
    let leaf_material = |name: &str, image| Material {
        name: name.into(),
        base_texture: Some(image),
        alpha: AlphaMode::Mask { cutoff: 0.5 },
        double_sided: true,
        metallic: 0.,
        roughness: 0.7,
        ..Default::default()
    };
    let materials = scene.add_materials(
        device,
        queue,
        &[
            shaded(leaf_material("grass", 0), wind, WIND[2]),
            shaded(leaf_material("canopy", 1), wind, CANOPY_WIND[2]),
            shaded(
                Material {
                    name: "tinted glass".into(),
                    base: [1.; 4],
                    metallic: 0.,
                    roughness: 0.05,
                    ior: 1.5,
                    transmission: 1.,
                    // The shader replaces this constant slab with each
                    // fragment's thickness.
                    thickness: 0.1,
                    double_sided: true,
                    casts_directional_shadow: false,
                    ..Default::default()
                },
                glass,
                0.,
            ),
        ],
        &[leaf([70, 140, 50]), leaf([40, 110, 45])],
    )?;
    let [grass, canopy, pane] = [materials[0], materials[1], materials[2]];
    scene.set_shader_parameters(queue, grass, bytemuck::bytes_of(&WIND))?;
    scene.set_shader_parameters(queue, canopy, bytemuck::bytes_of(&CANOPY_WIND))?;
    // White light turns (0.3, 0.7, 0.5) across 10 cm of glass.
    let tint: Params = [0.3f32, 0.7, 0.5, 1.].map(|color| -color.ln() / 0.1);
    scene.set_shader_parameters(queue, pane, bytemuck::bytes_of(&tint))?;
    let blade = cards(grass, BLADE, 0.);
    let blade = scene.add_model(
        device,
        queue,
        PreparedModel::with_shader_data(vec![blade.0], vec![blade.1])?,
    )?;
    for index in 0..BLADES {
        let position = Vec3::new(
            random(index, 1) * 16. - 8.,
            0.,
            random(index, 2) * -8.5 - 0.5,
        );
        let pose = Mat4::from_translation(position)
            * Mat4::from_rotation_y(random(index, 3) * std::f32::consts::TAU)
            * Mat4::from_scale(Vec3::splat(0.7 + 0.6 * random(index, 4)));
        plant(gpu, &mut scene, blade, pose, random(index, 5))?;
    }
    let crown = cards(canopy, [3., 3.], 1.6);
    let crown = scene.add_model(
        device,
        queue,
        PreparedModel::with_shader_data(vec![crown.0], vec![crown.1])?,
    )?;
    for (index, &tree) in trees.iter().enumerate() {
        plant(
            gpu,
            &mut scene,
            crown,
            Mat4::from_translation(tree),
            index as f32 * 0.37,
        )?;
    }
    // The glass: 30 cm thick at its foot and 2 cm at its top.
    let corners = [(-1., 0.), (1., 0.), (1., 1.6), (-1., 1.6)];
    let pane_mesh = ModelMesh {
        vertices: corners
            .map(|(x, y)| vertex(Vec3::new(x + 1.5, y, 3.), Vec3::Z, [0.; 2]))
            .to_vec(),
        indices: vec![0, 1, 2, 0, 2, 3],
        material: pane,
        deformation: Default::default(),
    };
    let thickness = corners
        .map(|(_, y)| [0.3 + (0.02 - 0.3) * y / 1.6, 0., 0., 0.])
        .to_vec();
    let pane_model = scene.add_model(
        device,
        queue,
        PreparedModel::with_shader_data(vec![pane_mesh], vec![thickness])?,
    )?;
    scene.add_instance(
        device,
        queue,
        InstanceState::new(pane_model),
        Mobility::Static,
    )?;
    scene.add_light(
        device,
        queue,
        Light {
            position: Vec3::new(-2., 4., -2.),
            shape: LightShape::Spot {
                direction: Vec3::new(0.3, -1., -0.2).normalize(),
                inner_angle: 0.4,
                outer_angle: 0.7,
                radius: LightShape::DEFAULT_RADIUS,
            },
            color: [1., 0.85, 0.6],
            intensity: 120.,
            range: 12.,
            casts_shadow: true,
            ..Default::default()
        },
    )?;
    Ok(scene)
}

fn output(device: &wgpu::Device, size: [u32; 2]) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("shaders example output"),
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
        label: Some("shaders example readback"),
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

fn main() -> Result<(), Box<dyn Error>> {
    let mut frames = 90;
    let mut size = [1280, 720];
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--frames" => frames = args.next().ok_or("--frames requires a count")?.parse()?,
            "--size" => {
                let value = args.next().ok_or("--size requires WxH")?;
                let (width, height) = value.split_once('x').ok_or("--size requires WxH")?;
                size = [width.parse()?, height.parse()?];
            }
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
            | sgl_3d::graphics_device::features(&adapter),
        required_limits: sgl_3d::graphics_device::limits(&adapter),
        ..Default::default()
    }))?;
    let mut scene = build(&device, &queue)?;
    let settings = Settings {
        antialiasing: Antialiasing::Taa,
        atmosphere: false,
        ..Settings::default()
    };
    let texture = output(&device, size);
    let view = texture.create_view(&Default::default());
    let mut renderer = Renderer::new(&device, &queue, texture.format(), size, 1., &settings)?;
    let mut timing = GpuTiming::new(&device, &queue);
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/shaders-example");
    std::fs::create_dir_all(&directory)?;
    let eye = Vec3::new(1.5, 1.6, 7.);
    let mut input = FrameInput::new(Camera {
        view: camera::rh::view::look_at_mat4(eye, Vec3::new(0., 0.9, -2.), Vec3::Y),
        projection: sgl_3d::perspective(55f32.to_radians(), size[0] as f32 / size[1] as f32, 0.1),
        eye,
    });
    input.directional_lights[0] = Some(DirectionalLight {
        direction: Vec3::new(-0.4, -1., -0.6),
        color: [1., 0.93, 0.82],
        illuminance: 4.,
        shadow: Some(DirectionalShadow::DEFAULT),
        ..Default::default()
    });
    input.hemisphere_light = HemisphereLight {
        sky_color: [0.35, 0.45, 0.6],
        ground_color: [0.1, 0.08, 0.05],
        intensity: 0.6,
    };
    input.backdrop = sgl_3d::Backdrop::Color([0.45, 0.6, 0.8]);
    let mut groups: BTreeMap<&'static str, Vec<f64>> = BTreeMap::new();
    let mut in_flight = None;
    for index in 0..frames {
        input.elapsed_seconds = index as f64 / 60.;
        input.camera_cut = index == 0;
        if let Some(timing) = &mut timing {
            for frame in timing.begin_frame(&device, &queue) {
                if frame.frame as usize > WARM_UP {
                    for pass in frame.passes {
                        let name = if pass.name.starts_with("directional shadow") {
                            "directional shadow"
                        } else {
                            pass.name
                        };
                        groups.entry(name).or_default().push(pass.ms);
                    }
                }
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
            &view,
            timing.as_ref(),
        );
        let submission = queue.submit([encoder.finish()]);
        if let Some(timing) = &mut timing {
            timing.submitted(&queue);
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
                &device,
                &queue,
                &texture,
                &directory.join(format!("frame-{index:03}.png")),
            )?;
        }
    }
    println!(
        "{frames} frames at {}x{}; GPU time median / p95",
        size[0], size[1]
    );
    for group in [
        "directional shadow",
        "local shadows",
        "opaque geometry + lighting",
        "geometry",
        "opaque lighting",
        "FSR2 composition",
        "transmission copy",
        "blended",
        "TAA",
    ] {
        if let Some(times) = groups.get_mut(group) {
            times.sort_by(f64::total_cmp);
            let at = |q: f64| times[((times.len() as f64 * q).ceil() as usize).saturating_sub(1)];
            println!("  {group:<26} {:7.3} / {:7.3} ms", at(0.5), at(0.95));
        }
    }
    println!("Frames: {}", directory.canonicalize()?.display());
    Ok(())
}
