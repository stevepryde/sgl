//! Finite glass volumes whose absorption follows the length of each view
//! ray inside them (`scene_volume_path`): the game's tinted glass shader
//! (`support/volume_glass.wgsl`), which SGL3D does not ship, reading the
//! path SGL3D measures from its volume layers.
//!
//! `cargo run --release -p sgl-3d --example volumes [-- --frames N] [--size WxH] [--volume-paths off]`
//!
//! Each shot is its own scene and fixed camera, under a sun with cascaded
//! shadows and a blue sky, against a checkered wall:
//!
//! - `slab-wall-3m`, `slab-wall-40m`: a 2 cm closed slab of tinted glass,
//!   half over the wall and half over the sky, with the wall 3 m and then
//!   40 m behind it, scaled to fill the same part of the view. The ray
//!   leaves the slab after 2 cm either way, so its tint should not deepen
//!   when the wall moves away.
//! - `block-outside`, `block-inside`: a 1.6 m glass block with an opaque
//!   cube inside it, which ends the path short of the block's far side,
//!   seen from outside and with the camera inside the glass, where the
//!   block's far faces absorb over the path from the eye.
//! - `blob-*`: a glass sphere scaled 1.5 whose vertex function wobbles it
//!   (static rest geometry, its displacement bound set), captured three
//!   times a third of a second apart: the path follows the deformed surface.
//! - `pair`: two glass boxes, the far one partly behind the near one,
//!   whose coverage of 0.6 shows the far one through it: the far box's
//!   path runs to its own exit, the second exit layer's.
//!
//! `--volume-paths off` turns `Settings::volume_paths` off, so the shader
//! takes its authored thickness, for comparison. It prints whether volume
//! paths are in effect, the median and 95th percentile GPU time of the
//! passes the volumes touch, and writes each shot's frames to
//! `target/volumes-example/on/` (or `off/`) for the owner to judge.
use sgl_3d::glam::{Mat4, Vec3, camera};
use sgl_3d::{
    AlphaMode, Camera, DirectionalLight, DirectionalShadow, FrameInput, HemisphereLight,
    InstanceState, MaterialId, MaterialShader, Mobility, ModelMesh, PreparedModel, Renderer, Scene,
    ShaderSource, SurfaceMaterial,
    asset::{Asset, CpuMesh, Image, Material, Vertex},
    settings::{Antialiasing, Settings},
    timing::GpuTiming,
};
use std::collections::{BTreeMap, HashSet};
use std::error::Error;
use std::path::Path;

const WARM_UP: usize = 8;
/// Frames between a moving shot's captures: a third of a second at 60 Hz.
const CAPTURE_GAP: usize = 20;

/// `volume_glass.wgsl`'s `ShaderParams`: the absorption per metre with the
/// authored thickness, and the wobble.
type Params = [[f32; 4]; 2];

/// What a shot's scene holds.
#[derive(Clone, Copy)]
enum Content {
    /// The thin slab, with the wall this many metres behind it.
    Slab { wall: f32 },
    /// The thick block with an opaque cube inside.
    Block,
    /// The wobbling sphere.
    Blob,
    /// Two boxes, one behind the other.
    Pair,
}

struct Shot {
    name: &'static str,
    content: Content,
    eye: Vec3,
    target: Vec3,
    /// Frames written, `CAPTURE_GAP` apart, ending at the last.
    captures: usize,
}

const SLAB_EYE: Vec3 = Vec3::new(0.35, 0.1, 3.);
const SHOTS: [Shot; 6] = [
    Shot {
        name: "slab-wall-3m",
        content: Content::Slab { wall: 3. },
        eye: SLAB_EYE,
        target: Vec3::ZERO,
        captures: 1,
    },
    Shot {
        name: "slab-wall-40m",
        content: Content::Slab { wall: 40. },
        eye: SLAB_EYE,
        target: Vec3::ZERO,
        captures: 1,
    },
    Shot {
        name: "block-outside",
        content: Content::Block,
        eye: Vec3::new(2.6, 1.4, 4.2),
        target: Vec3::ZERO,
        captures: 1,
    },
    Shot {
        name: "block-inside",
        content: Content::Block,
        eye: Vec3::new(-0.45, 0.35, 0.55),
        target: Vec3::new(0.25, -0.3, -2.),
        captures: 1,
    },
    Shot {
        name: "blob",
        content: Content::Blob,
        eye: Vec3::new(0., 0.2, 3.5),
        target: Vec3::ZERO,
        captures: 3,
    },
    Shot {
        name: "pair",
        content: Content::Pair,
        eye: Vec3::new(0., 0., 4.),
        target: Vec3::new(0.15, 0.1, -1.),
        captures: 1,
    },
];

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

/// A closed box of `size` centred at `center`: its vertices and indices.
fn cuboid(center: Vec3, size: Vec3) -> (Vec<Vertex>, Vec<u32>) {
    let half = size * 0.5;
    let (mut vertices, mut indices) = (Vec::new(), Vec::new());
    for (normal, u, v) in [
        (Vec3::X, -Vec3::Z, Vec3::Y),
        (-Vec3::X, Vec3::Z, Vec3::Y),
        (Vec3::Y, Vec3::X, -Vec3::Z),
        (-Vec3::Y, Vec3::X, Vec3::Z),
        (Vec3::Z, Vec3::X, Vec3::Y),
        (-Vec3::Z, -Vec3::X, Vec3::Y),
    ] {
        let start = vertices.len() as u32;
        for [s, t] in [[-1., -1.], [1., -1.], [1., 1.], [-1., 1.]] {
            vertices.push(vertex(
                center + (normal + u * s + v * t) * half,
                normal,
                [(s + 1.) * 0.5, (t + 1.) * 0.5],
            ));
        }
        indices.extend([0, 1, 2, 0, 2, 3].map(|i| start + i));
    }
    (vertices, indices)
}

/// A closed sphere of `radius` about the origin: 32 segments by 16 rings.
fn sphere(radius: f32) -> (Vec<Vertex>, Vec<u32>) {
    const SEGMENTS: u32 = 32;
    const RINGS: u32 = 16;
    let mut vertices = Vec::new();
    for ring in 0..=RINGS {
        let polar = std::f32::consts::PI * ring as f32 / RINGS as f32;
        for segment in 0..=SEGMENTS {
            let azimuth = std::f32::consts::TAU * segment as f32 / SEGMENTS as f32;
            let normal = Vec3::new(
                polar.sin() * azimuth.cos(),
                polar.cos(),
                -polar.sin() * azimuth.sin(),
            );
            let uv = [segment as f32 / SEGMENTS as f32, ring as f32 / RINGS as f32];
            vertices.push(vertex(normal * radius, normal, uv));
        }
    }
    let mut indices = Vec::new();
    for ring in 0..RINGS {
        for segment in 0..SEGMENTS {
            let a = ring * (SEGMENTS + 1) + segment;
            let b = a + SEGMENTS + 1;
            indices.extend([a, b, a + 1, a + 1, b, b + 1]);
        }
    }
    (vertices, indices)
}

/// The wall's light and dark squares, eight a side.
fn checker() -> Image {
    Image::Rgba8(image::RgbaImage::from_fn(64, 64, |x, y| {
        if (x / 8 + y / 8) % 2 == 0 {
            image::Rgba([200, 190, 170, 255])
        } else {
            image::Rgba([90, 60, 50, 255])
        }
    }))
}

/// The absorption per metre that turns white light `color` over `over`
/// metres, with `over` as the authored thickness in the mesh's units.
fn tint(color: [f32; 3], over: f32) -> [f32; 4] {
    [
        -color[0].ln() / over,
        -color[1].ln() / over,
        -color[2].ln() / over,
        over,
    ]
}

/// The opaque content: the checkered wall and, for the block, the cube
/// inside it.
fn add_opaque(
    (device, queue): (&wgpu::Device, &wgpu::Queue),
    scene: &mut Scene,
    shot: &Shot,
) -> Result<(), Box<dyn Error>> {
    let (center, size) = match shot.content {
        // The wall at 3 m fills the lower half of the view behind the slab;
        // farther away it is scaled about the eye to fill the same part.
        Content::Slab { wall } => {
            let near = Vec3::new(0., -0.9, -3.);
            let scale = (shot.eye.z + wall) / (shot.eye.z + 3.);
            let mut center = shot.eye + (near - shot.eye) * scale;
            center.z = -wall;
            (center, Vec3::new(6. * scale, 1.8 * scale, 0.2))
        }
        Content::Block => (Vec3::new(0., 0., -4.), Vec3::new(10., 6., 0.2)),
        Content::Blob => (Vec3::new(0., 0., -3.), Vec3::new(8., 5., 0.2)),
        Content::Pair => (Vec3::new(0., 0., -5.), Vec3::new(10., 6., 0.2)),
    };
    let mut meshes = vec![cuboid(center, size)];
    if matches!(shot.content, Content::Block) {
        meshes.push(cuboid(Vec3::new(0.25, -0.3, -0.2), Vec3::splat(0.5)));
    }
    let meshes = meshes
        .into_iter()
        .enumerate()
        .map(|(material, (vertices, indices))| CpuMesh {
            vertices,
            indices,
            material,
            deformation: Default::default(),
        })
        .collect();
    let world = scene.add_asset(
        device,
        queue,
        Asset {
            meshes,
            materials: vec![
                Material {
                    name: "wall".into(),
                    base_texture: Some(0),
                    metallic: 0.,
                    roughness: 0.7,
                    ..Default::default()
                },
                Material {
                    name: "interior".into(),
                    base: [0.9, 0.45, 0.1, 1.],
                    metallic: 0.,
                    roughness: 0.5,
                    ..Default::default()
                },
            ],
            images: vec![checker()],
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
    Ok(())
}

/// The glass of `shot`: its material through the volume glass shader, its
/// meshes and their poses.
fn add_glass(
    (device, queue): (&wgpu::Device, &wgpu::Queue),
    scene: &mut Scene,
    shot: &Shot,
) -> Result<(), Box<dyn Error>> {
    let shader = scene.add_shader(ShaderSource {
        wgsl: include_str!("support/volume_glass.wgsl").into(),
        label: "volume glass".into(),
    })?;
    let layout = scene.shader_parameters_layout(shader)?;
    if layout.size as usize != size_of::<Params>() || layout.fields.len() != 2 {
        return Err(format!("volume glass: ShaderParams is not two vec4s: {layout:?}").into());
    }
    // Each shot's tint, over its glass's own thickness, and the blob's
    // wobble: 6 cm in mesh units, about one wave across it, two seconds a
    // cycle.
    let (coverage, params, meshes, poses): (f32, Params, _, Vec<Mat4>) = match shot.content {
        Content::Slab { .. } => (
            1.,
            [tint([0.45, 0.75, 0.6], 0.02), [0.; 4]],
            vec![cuboid(Vec3::ZERO, Vec3::new(1.2, 1.2, 0.02))],
            vec![Mat4::IDENTITY],
        ),
        Content::Block => (
            1.,
            [tint([0.55, 0.8, 0.7], 1.6), [0.; 4]],
            vec![cuboid(Vec3::ZERO, Vec3::splat(1.6))],
            vec![Mat4::IDENTITY],
        ),
        Content::Blob => (
            1.,
            [tint([0.5, 0.75, 0.9], 1.), [0.06, 6., 1800., 0.]],
            vec![sphere(0.5)],
            vec![Mat4::from_scale(Vec3::splat(1.5))],
        ),
        Content::Pair => (
            0.6,
            [tint([0.4, 0.7, 0.9], 0.8), [0.; 4]],
            vec![cuboid(Vec3::ZERO, Vec3::splat(0.8))],
            vec![
                Mat4::IDENTITY,
                Mat4::from_translation(Vec3::new(0.35, 0.25, -2.)),
            ],
        ),
    };
    let material = scene.add_materials(
        device,
        queue,
        &[Material {
            name: "volume glass".into(),
            base: [1., 1., 1., coverage],
            metallic: 0.,
            roughness: 0.02,
            ior: 1.5,
            transmission: 1.,
            alpha: AlphaMode::Blend {
                receives_screen_space_reflections: false,
                keeps_specular: false,
            },
            double_sided: true,
            casts_directional_shadow: false,
            ..Default::default()
        }],
        &[],
    )?[0];
    set_shader(scene, queue, material, (shader, params[1][0]))?;
    scene.set_shader_parameters(queue, material, bytemuck::bytes_of(&params))?;
    let meshes = meshes
        .into_iter()
        .map(|(vertices, indices)| ModelMesh {
            vertices,
            indices,
            material,
            deformation: Default::default(),
        })
        .collect();
    let model = scene.add_model(device, queue, PreparedModel::new(meshes)?)?;
    for pose in poses {
        let state = InstanceState {
            pose,
            ..InstanceState::new(model)
        };
        scene.add_instance(device, queue, state, Mobility::Static)?;
    }
    Ok(())
}

/// Draws `material` through `shader`, moving its vertices at most `bound`.
fn set_shader(
    scene: &mut Scene,
    queue: &wgpu::Queue,
    material: MaterialId,
    (shader, bound): (sgl_3d::ShaderId, f32),
) -> Result<(), Box<dyn Error>> {
    let values = scene.material(material)?;
    let shader = Some(MaterialShader {
        shader,
        displacement_bound: bound,
    });
    scene.set_material(queue, material, SurfaceMaterial { shader, ..values })?;
    Ok(())
}

fn build(device: &wgpu::Device, queue: &wgpu::Queue, shot: &Shot) -> Result<Scene, Box<dyn Error>> {
    let mut scene = Scene::new(device, queue);
    add_opaque((device, queue), &mut scene, shot)?;
    add_glass((device, queue), &mut scene, shot)?;
    Ok(scene)
}

fn output(device: &wgpu::Device, size: [u32; 2]) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("volumes example output"),
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
        label: Some("volumes example readback"),
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
    let mut frames = 60;
    let mut size = [1280, 720];
    let mut volume_paths = true;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--frames" => frames = args.next().ok_or("--frames requires a count")?.parse()?,
            "--size" => {
                let value = args.next().ok_or("--size requires WxH")?;
                let (width, height) = value.split_once('x').ok_or("--size requires WxH")?;
                size = [width.parse()?, height.parse()?];
            }
            "--volume-paths" => {
                volume_paths = match args.next().as_deref() {
                    Some("on") => true,
                    Some("off") => false,
                    _ => return Err("--volume-paths requires on or off".into()),
                }
            }
            other => return Err(format!("unknown argument {other}").into()),
        }
    }
    let least = WARM_UP + 2 * CAPTURE_GAP + 3;
    if frames < least {
        return Err(format!("--frames must be at least {least}").into());
    }
    let adapter =
        pollster::block_on(wgpu::Instance::default().request_adapter(&Default::default()))?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_features: (adapter.features() & wgpu::Features::TIMESTAMP_QUERY)
            | sgl_3d::graphics_device::features(&adapter),
        required_limits: sgl_3d::graphics_device::limits(&adapter),
        ..Default::default()
    }))?;
    let settings = Settings {
        antialiasing: Antialiasing::Taa,
        atmosphere: false,
        volume_paths,
        ..Settings::default()
    };
    let texture = output(&device, size);
    let view = texture.create_view(&Default::default());
    let mut renderer = Renderer::new(&device, &queue, texture.format(), size, 1., &settings)?;
    let mut timing = GpuTiming::new(&device, &queue);
    let directory = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/volumes-example")
        .join(if volume_paths { "on" } else { "off" });
    std::fs::create_dir_all(&directory)?;
    println!(
        "{:?} binding tier; volume paths in effect: {}",
        renderer.binding_tier(),
        renderer.volume_paths_in_effect(&settings)
    );
    let mut groups: BTreeMap<&'static str, Vec<f64>> = BTreeMap::new();
    // The frames `GpuTiming` counts that are past their shot's warm-up.
    let mut measured = HashSet::new();
    let mut begun = 0u64;
    let mut in_flight = None;
    for shot in &SHOTS {
        let mut scene = build(&device, &queue, shot)?;
        let mut input = FrameInput::new(Camera {
            view: camera::rh::view::look_at_mat4(shot.eye, shot.target, Vec3::Y),
            projection: sgl_3d::perspective(
                50f32.to_radians(),
                size[0] as f32 / size[1] as f32,
                0.05,
            ),
            eye: shot.eye,
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
        for index in 0..frames {
            input.elapsed_seconds = index as f64 / 60.;
            input.camera_cut = index == 0;
            if let Some(timing) = &mut timing {
                for frame in timing.begin_frame(&device, &queue) {
                    if measured.contains(&frame.frame) {
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
                begun += 1;
                if index >= WARM_UP {
                    measured.insert(begun);
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
            let from_end = frames - 1 - index;
            if from_end % CAPTURE_GAP == 0 && from_end / CAPTURE_GAP < shot.captures {
                let name = if shot.captures == 1 {
                    format!("{}.png", shot.name)
                } else {
                    format!("{}-{index:03}.png", shot.name)
                };
                save(&device, &queue, &texture, &directory.join(name))?;
            }
        }
    }
    println!(
        "{} shots of {frames} frames at {}x{}; GPU time median / p95",
        SHOTS.len(),
        size[0],
        size[1]
    );
    for group in [
        "directional shadow",
        "opaque geometry + lighting",
        "geometry",
        "opaque lighting",
        "volume layers",
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
