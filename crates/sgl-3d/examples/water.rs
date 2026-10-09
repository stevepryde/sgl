//! A lake of programmable water (`Scene::add_shader`): the example's own
//! Gerstner waves (`support/gerstner.wgsl`, which SGL3D does not ship) move
//! the vertices of `--chunks` static 16 m chunk instances (64 by default),
//! and its normal map's two scrolling layers (`asset::Material::normal_layers`)
//! add the fine detail over them. The water is a blended receiver of
//! screen-space reflections and transmits the light behind it: its shader
//! takes the water column behind each fragment from the scene depth to tint
//! it, fade it into the shore and set its volume's thickness. It lies over
//! submerged rocks and a sunken block, between an opaque shoreline and posts
//! standing in it, under a bright panel above the far shore, with a second
//! receiver sheet over part of it and a glass pane in front. Halfway through
//! each run the render origin moves by whole chunks with the camera, which
//! the waves cross unchanged.
//!
//! `cargo run --release -p sgl-3d --example water [-- --frames N] [--run NAME]... [--chunks N] [--morph] [--size WxH]`
//!
//! Renders each run below at `--size` (1920×1080 by default) through the
//! public `Scene` and `Renderer` API and prints, per run, the median and
//! 95th percentile GPU time of the frame and of the pass groups the water
//! touches, over the frames after a warm-up, with up to two frames in
//! flight, the scene's ray-source and geometry bytes, and what each frame
//! uploads for the water. `--run` renders only the runs it names. `--morph`
//! animates the same waves with twelve sine and cosine morph targets per
//! chunk instead, Retrocar's sea's technique: moving instances whose
//! weights are set every frame and deformed by the deform pass, with a
//! constant 2 m volume. `set-model` animates a 128×128 grid of the lake's
//! normals replaced every frame with `Scene::set_model`, as before material
//! layers and shaders existed. Every 30th frame and the last of each run are
//! written to `target/water-example/<run>/` for the owner to judge.
use sgl_3d::glam::{Mat4, Quat, Vec3, camera};
use sgl_3d::{
    AlphaMode, Camera, DirectionalLight, DirectionalShadow, Exposure, FrameInput, InstanceId,
    InstanceState, MaterialId, MaterialShader, Mobility, ModelMesh, MotionBlurParameters,
    NormalLayer, PreparedModel, Renderer, Scene, ShaderSource, SurfaceMaterial,
    asset::{Asset, CpuMesh, Image, Material, Vertex},
    deformation::{MeshDeformation, MorphDelta, MorphTarget},
    environment::{EnvironmentMap, PmremAtlas},
    settings::{
        Antialiasing, Fsr2Quality, MotionBlur, ReflectionMethod, ScreenSpaceReflections, Settings,
    },
    timing::{FrameTime, GpuTiming},
};
use std::collections::BTreeMap;
use std::error::Error;
use std::path::Path;
use std::time::Instant;

const SIZE: [u32; 2] = [1920, 1080];
/// The size the `resize-and-cut` run switches to a third of the way through.
const RESIZED: [u32; 2] = [1280, 720];
const WARM_UP: usize = 10;
/// Grid cells along each side of the `set-model` lake's surface.
const CELLS: usize = 128;
/// The lake's open water spans x in ±40 m and z from 2.5 m to -52 m, at
/// y = 0; its chunks reach beyond it, calmer under the shores.
const LAKE: [[f32; 2]; 2] = [[-40., 2.5], [40., -52.]];
/// Metres of lake one repeat of the wave normal map covers, at a layer's
/// scale 1: the lake's UVs are its x and -z over this.
const TILE: f32 = 16.;
/// Texels on each side of the wave normal map.
const MAP: u32 = 256;
/// A water chunk's side, in metres, and its grid's cells along each side.
const CHUNK: f32 = 16.;
const CHUNK_CELLS: usize = 32;
/// The metres over which the waves repeat along x and z: each wave's
/// vector is a whole multiple of 2π / PERIOD on each axis.
const PERIOD: f32 = 64.;
/// Each wave's cycles across PERIOD along x and z.
const WAVE_CYCLES: [[f32; 2]; 6] = [
    [3., 1.],
    [-2., 5.],
    [6., -3.],
    [1., 8.],
    [-9., 4.],
    [11., 7.],
];
/// The waves' steepness, amplitude per metre of wavelength, and the water's
/// deep column in metres.
const STEEPNESS: f32 = 0.5;
const AMPLITUDE: f32 = 0.004;
const DEEP: f32 = 3.;
/// Where the render origin moves halfway through a run: whole chunks.
const ORIGIN_MOVE: Vec3 = Vec3::new(2. * CHUNK, 0., -CHUNK);

/// How the lake's waves move.
#[derive(Clone, Copy, PartialEq)]
enum Waves {
    /// Its chunks' vertices by the shader, with the frame's time.
    Shader,
    /// Its chunks' vertices by the shader, with the time held at zero.
    Still,
    /// Its chunks' morph targets, weighted every frame (`--morph`).
    Morph,
    /// Its grid's normals, replaced every frame with `Scene::set_model`.
    Mesh,
}

/// One run: the settings it renders with and what it exercises.
struct Run {
    name: &'static str,
    settings: Settings,
    /// The camera orbits; otherwise it stands still.
    orbit: bool,
    waves: Waves,
    /// A resize a third of the way through and a camera cut at two thirds.
    resize_and_cut: bool,
    /// The glass pane transmits the light behind it.
    transmissive_glass: bool,
}

fn runs(morph: bool) -> Vec<Run> {
    let base = Settings {
        scene_resolution: sgl_3d::settings::SceneResolution::Full,
        atmosphere: false,
        antialiasing: Antialiasing::Taa,
        screen_space_reflections: ScreenSpaceReflections::Full,
        reflection_method: ReflectionMethod::Velvet,
        ..Settings::default()
    };
    let waves = if morph { Waves::Morph } else { Waves::Shader };
    let run = |name, settings: Settings| Run {
        name,
        settings,
        orbit: true,
        waves,
        resize_and_cut: false,
        transmissive_glass: false,
    };
    let with = |change: fn(&mut Settings)| {
        let mut settings = base;
        change(&mut settings);
        settings
    };
    vec![
        run("velvet", base),
        run(
            "ssr-off",
            with(|s| s.screen_space_reflections = ScreenSpaceReflections::Off),
        ),
        run(
            "crystal",
            with(|s| s.reflection_method = ReflectionMethod::Crystal),
        ),
        run("motion-blur", with(|s| s.motion_blur = MotionBlur::Full)),
        run(
            "fsr2",
            with(|s| {
                s.antialiasing = Antialiasing::Fsr2;
                s.fsr2_quality = Fsr2Quality::Quality;
            }),
        ),
        Run {
            orbit: false,
            ..run("stationary", base)
        },
        Run {
            waves: Waves::Still,
            ..run("still-waves", base)
        },
        Run {
            waves: Waves::Mesh,
            ..run("set-model", base)
        },
        Run {
            transmissive_glass: true,
            ..run("transmissive-glass", base)
        },
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

/// The lake's surroundings, the second sheet, a receiver, and the glass
/// pane, transmissive where `transmissive`; the lake is its own models.
fn world(transmissive: bool) -> Asset {
    let receiver = AlphaMode::Blend {
        receives_screen_space_reflections: true,
        keeps_specular: false,
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
    let glass = if transmissive {
        // Thin, clear glass: it transmits all of what lies behind it, tinted.
        Material {
            double_sided: true,
            transmission: 1.,
            ..material("glass", [0.85, 0.95, 1., 1.], 0.05)
        }
    } else {
        Material {
            double_sided: true,
            alpha: AlphaMode::Blend {
                receives_screen_space_reflections: false,
                keeps_specular: true,
            },
            ..material("glass", [0.6, 0.8, 0.9, 0.25], 0.05)
        }
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
        ignored: Vec::new(),
    }
}

/// A tileable wave normal map: the normals of a height field summing
/// sines of whole cycles across the tile in a dozen directions, each as
/// steep as the next, encoded as glTF's tangent-space normal textures are,
/// +X along U and +Y along V.
fn wave_map() -> Image {
    // Cycles across the tile along U and V, and phase.
    const WAVES: [([f32; 2], f32); 12] = [
        ([1., 0.], 0.3),
        ([0., 1.], 2.1),
        ([1., 1.], 4.0),
        ([2., -1.], 1.2),
        ([1., -2.], 5.5),
        ([3., 1.], 0.8),
        ([-2., 3.], 3.3),
        ([3., -2.], 2.7),
        ([4., 1.], 5.9),
        ([1., 4.], 1.7),
        ([5., -3.], 4.4),
        ([-4., 5.], 0.1),
    ];
    // Each wave's steepest slope.
    const SLOPE: f32 = 0.035;
    Image::Rgba8(image::RgbaImage::from_fn(MAP, MAP, |x, y| {
        let uv = [x, y].map(|t| (t as f32 + 0.5) / MAP as f32);
        let mut gradient = [0f32; 2];
        for (cycles, phase) in WAVES {
            let length = (cycles[0] * cycles[0] + cycles[1] * cycles[1]).sqrt();
            let angle = std::f32::consts::TAU * (cycles[0] * uv[0] + cycles[1] * uv[1]) + phase;
            for axis in 0..2 {
                gradient[axis] += SLOPE * cycles[axis] / length * angle.cos();
            }
        }
        let normal = Vec3::new(-gradient[0], -gradient[1], 1.).normalize();
        let byte = |v: f32| ((v * 0.5 + 0.5) * 255.).round() as u8;
        image::Rgba([byte(normal.x), byte(normal.y), byte(normal.z), 255])
    }))
}

/// The water's two normal layers: the map's swell at the tile's size, and
/// its chop at 2.7 times smaller, crossing it.
fn water_layers() -> [NormalLayer; 2] {
    [
        NormalLayer {
            velocity: [0.06, 0.025],
            scale: 1.,
            strength: 1.,
        },
        NormalLayer {
            velocity: [-0.04, 0.07],
            scale: 2.7,
            strength: 0.6,
        },
    ]
}

/// The waves' parameters as `support/gerstner.wgsl` declares
/// `ShaderParams`, which `main` checks against the layout naga gave the
/// shader.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct WaterParams {
    waves: [[f32; 4]; 6],
    shape: [f32; 4],
    deep: [f32; 4],
    shallow: [f32; 4],
    absorption: [f32; 4],
}

/// Each wave's unit direction along x and z, wavelength in metres and
/// whole wavelengths travelled per hour: a lake's waves at a third of deep
/// water's speed, sqrt(g / k), rounded to whole cycles.
fn waves() -> [[f32; 4]; 6] {
    WAVE_CYCLES.map(|[m, n]| {
        let cycles = (m * m + n * n).sqrt();
        let wavelength = PERIOD / cycles;
        let k = std::f32::consts::TAU / wavelength;
        let speed = (9.81 / k).sqrt() / 3.;
        [
            m / cycles,
            n / cycles,
            wavelength,
            (speed * 3600. / wavelength).round(),
        ]
    })
}

/// The water's parameters: the waves, a blue-green tint deepening over
/// 1.5 m of column, a fade into the shore over its last 0.4 m, and white
/// light turning (0.45, 0.8, 0.75) over 3 m.
fn water_params() -> WaterParams {
    let absorption = [0.45f32, 0.8, 0.75].map(|color| -color.ln() / 3.);
    WaterParams {
        waves: waves(),
        shape: [STEEPNESS, AMPLITUDE, DEEP, 0.],
        deep: [0.55, 0.8, 0.85, 1.5],
        shallow: [0.95, 1., 0.97, 0.4],
        absorption: [absorption[0], absorption[1], absorption[2], 0.],
    }
}

/// The farthest the waves move a vertex: the steepness's horizontal pinch
/// and the height of every wave at once, at most twice their amplitudes'
/// sum.
fn displacement_bound() -> f32 {
    2. * waves().iter().map(|wave| AMPLITUDE * wave[2]).sum::<f32>()
}

/// How much of the waves water at `x`, `z` takes: all of it on the open
/// lake, falling to a sixth of it 4 m beyond, under the shores.
fn wave_state(x: f32, z: f32) -> f32 {
    let [[x0, z0], [x1, z1]] = LAKE;
    let outside = Vec3::new(
        (x0 - x).max(x - x1).max(0.),
        0.,
        (z1 - z).max(z - z0).max(0.),
    )
    .length();
    (1. - outside / 4.).clamp(1. / 6., 1.)
}

/// Each chunk's origin, its least x and z, in the chunks' row order: a
/// grid as near square as `chunks` fills, about the lake's centre.
fn chunk_origins(chunks: usize) -> Vec<Vec3> {
    let columns = (chunks as f32).sqrt().ceil() as usize;
    let rows = chunks.div_ceil(columns);
    let center = Vec3::new(0., 0., -25.);
    (0..chunks)
        .map(|index| {
            let (i, j) = ((index % columns) as f32, (index / columns) as f32);
            center
                + Vec3::new(
                    (i - columns as f32 / 2.) * CHUNK,
                    0.,
                    (j - rows as f32 / 2.) * CHUNK,
                )
        })
        .collect()
}

/// A chunk at `origin`: a flat grid in its own space facing +Y, its UVs the
/// world's x and -z over `TILE`, and each vertex's wave state
/// (`wave_state`) as its shader data; with morph targets of the waves where
/// `morph`.
fn chunk(origin: Vec3, material: MaterialId, morph: bool) -> (ModelMesh, Vec<[f32; 4]>) {
    let step = CHUNK / CHUNK_CELLS as f32;
    let row = CHUNK_CELLS + 1;
    let mut vertices = Vec::with_capacity(row * row);
    let mut data = Vec::with_capacity(row * row);
    for j in 0..row {
        for i in 0..row {
            let (x, z) = (i as f32 * step, j as f32 * step);
            let world = origin + Vec3::new(x, 0., z);
            vertices.push(Vertex {
                tangent: [0.; 4],
                lightmap_bounds: [0., 0., 1., 1.],
                lightmap_uv: [0.; 2],
                position: [x, 0., z],
                normal: [0., 1., 0.],
                uv: [world.x / TILE, -world.z / TILE],
                color: [1.; 4],
            });
            data.push([wave_state(world.x, world.z), 0., 0., 0.]);
        }
    }
    let indices = (0..CHUNK_CELLS as u32)
        .flat_map(|j| (0..CHUNK_CELLS as u32).map(move |i| j * row as u32 + i))
        .flat_map(|a| {
            let row = row as u32;
            [a, a + row, a + 1, a + 1, a + row, a + row + 1]
        })
        .collect();
    let deformation = if morph {
        morph_targets(origin, &vertices, &data)
    } else {
        MeshDeformation::default()
    };
    (
        ModelMesh {
            vertices,
            indices,
            material,
            deformation,
        },
        data,
    )
}

/// The waves as twelve morph targets of a chunk at `origin` (example-only
/// code, Retrocar's sea's technique): a wave's displacement at its phase θ =
/// φ − ωt is its cos ωt's share of the displacement at φ and its sin ωt's
/// of the displacement a quarter cycle on, so two targets a wave, weighted
/// cos ωt and sin ωt (`morph_weights`), each scaled by the vertex's state.
fn morph_targets(origin: Vec3, vertices: &[Vertex], data: &[[f32; 4]]) -> MeshDeformation {
    let mut targets = Vec::with_capacity(12);
    for (index, wave) in waves().into_iter().enumerate() {
        let k = std::f32::consts::TAU / wave[2];
        let amplitude = AMPLITUDE * wave[2];
        for quarter in [false, true] {
            let deltas = vertices
                .iter()
                .zip(data)
                .map(|(vertex, state)| {
                    let anchor = origin + Vec3::from_array(vertex.position);
                    let phi = k * (wave[0] * anchor.x + wave[1] * anchor.z);
                    // cos θ = cos φ cos ωt + sin φ sin ωt and
                    // sin θ = sin φ cos ωt − cos φ sin ωt.
                    let (c, s) = if quarter {
                        (phi.sin(), -phi.cos())
                    } else {
                        (phi.cos(), phi.sin())
                    };
                    let a = amplitude * state[0];
                    MorphDelta {
                        position: [
                            STEEPNESS * a * wave[0] * c,
                            a * s,
                            STEEPNESS * a * wave[1] * c,
                        ],
                        normal: [
                            -wave[0] * k * a * c,
                            -STEEPNESS * k * a * s,
                            -wave[1] * k * a * c,
                        ],
                        tangent: [0.; 3],
                    }
                })
                .collect();
            targets.push(MorphTarget {
                weight: (2 * index + usize::from(quarter)) as u32,
                deltas,
            });
        }
    }
    MeshDeformation {
        influences: Vec::new(),
        morph_targets: targets,
    }
}

/// The morph targets' weights at `seconds`: cos ωt and sin ωt of each wave,
/// at its whole cycles per hour.
fn morph_weights(seconds: f64) -> [f32; 12] {
    let phase = seconds.rem_euclid(3600.) / 3600.;
    let waves = waves();
    std::array::from_fn(|index| {
        let angle = std::f64::consts::TAU * f64::from(waves[index / 2][3]) * phase;
        if index % 2 == 0 {
            angle.cos() as f32
        } else {
            angle.sin() as f32
        }
    })
}

/// The lake's chunks: their instances and whether they deform.
struct Chunks {
    instances: Vec<InstanceId>,
    morph: bool,
}

/// `chunks` water chunks of `material` placed in `scene`: static instances
/// whose shader data anchors them on the waves' period, or, `morph`,
/// moving instances of deforming chunks.
fn place_chunks(
    (device, queue): (&wgpu::Device, &wgpu::Queue),
    scene: &mut Scene,
    material: MaterialId,
    chunks: usize,
    morph: bool,
) -> Result<Chunks, Box<dyn Error>> {
    let mut instances = Vec::with_capacity(chunks);
    for origin in chunk_origins(chunks) {
        let (mesh, data) = chunk(origin, material, morph);
        let prepared = PreparedModel::with_shader_data(vec![mesh], vec![data])?;
        let model = scene.add_model(device, queue, prepared)?;
        let state = InstanceState {
            pose: Mat4::from_translation(origin),
            ..InstanceState::new(model)
        };
        let mobility = if morph {
            Mobility::Moving
        } else {
            Mobility::Static
        };
        let instance = scene.add_instance(device, queue, state, mobility)?;
        if !morph {
            let anchor = [
                origin.x.rem_euclid(PERIOD),
                origin.z.rem_euclid(PERIOD),
                0.,
                0.,
            ];
            scene.set_instance_shader_data(queue, instance, anchor)?;
        }
        instances.push(instance);
    }
    Ok(Chunks { instances, morph })
}

/// The `set-model` lake's surface at `seconds`: a flat grid whose normals
/// follow a sum of sines, and its indices.
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

/// The camera of frame `index` of a run at `size`, in the render frame of
/// an origin at `origin`.
fn camera(index: usize, orbit: bool, size: [u32; 2], origin: Vec3) -> Camera {
    let center = Vec3::new(0., 0.5, -25.) - origin;
    let eye = if orbit {
        let angle = 0.5 * (index as f32 / 120. * std::f32::consts::TAU).sin();
        center + Vec3::new(33. * angle.sin(), 3.5, 33. * angle.cos())
    } else {
        Vec3::new(0., 3., 8.) - origin
    };
    Camera {
        eye,
        view: camera::rh::view::look_at_mat4(eye, center, Vec3::Y),
        projection: sgl_3d::perspective(50f32.to_radians(), size[0] as f32 / size[1] as f32, 0.1),
    }
}

/// Each pass group's time in each measured frame, and what the frames
/// uploaded for the water.
#[derive(Default)]
struct Times {
    groups: BTreeMap<&'static str, Vec<f64>>,
    totals: Vec<f64>,
    /// Bytes a frame gave the scene for the water: morph weights, or a
    /// replaced model's vertices and indices.
    uploaded: usize,
    /// Each measured frame's `PreparedModel::new` and `Scene::set_model`
    /// calls, in CPU milliseconds (`set-model`).
    prepares: Vec<f64>,
    edits: Vec<f64>,
    /// The scene's ray-source and geometry bytes, live, once set up.
    ray_source: u64,
    geometry: u64,
}

impl Times {
    fn add(&mut self, frame: FrameTime) {
        if frame.frame as usize <= WARM_UP {
            return;
        }
        let measured = self.totals.len();
        self.totals.push(frame.total_ms);
        for pass in frame.passes {
            // Crystal's, Velvet's, DiligentFX's and the cascades' passes,
            // each as one group.
            let name = [
                "Godot SSR",
                "SSR",
                "DiligentFX",
                "world reflection",
                "directional shadow",
            ]
            .into_iter()
            .find(|prefix| pass.name.starts_with(prefix))
            .unwrap_or(pass.name);
            let times = self.groups.entry(name).or_default();
            times.resize(measured + 1, 0.);
            times[measured] += pass.ms;
        }
    }

    fn edit(&mut self, [prepare, edit]: [f64; 2]) {
        self.prepares.push(prepare);
        self.edits.push(edit);
    }

    fn report(&self, name: &str) {
        // The median and 95th percentile of `times`, a frame without one
        // counting zero.
        let quantiles = |times: &[f64], frames: usize| {
            let mut sorted = times.to_vec();
            sorted.resize(frames, 0.);
            sorted.sort_by(f64::total_cmp);
            let at = |q: f64| sorted[((sorted.len() as f64 * q).ceil() as usize).saturating_sub(1)];
            (at(0.5), at(0.95))
        };
        if self.totals.is_empty() {
            println!("{name:<20} no GPU timestamps");
        } else {
            let (median, p95) = quantiles(&self.totals, self.totals.len());
            println!(
                "{name:<20} frame {median:7.3} / {p95:7.3} ms ({} frames)",
                self.totals.len()
            );
        }
        println!(
            "  ray source {:.2} MB, geometry {:.2} MB; uploads {} bytes a frame for the water",
            self.ray_source as f64 / 1e6,
            self.geometry as f64 / 1e6,
            self.uploaded
        );
        if !self.edits.is_empty() {
            let (prepare, prepare_p95) = quantiles(&self.prepares, self.prepares.len());
            let (median, p95) = quantiles(&self.edits, self.edits.len());
            println!(
                "  prepared in {prepare:.3} / {prepare_p95:.3} ms and set_model {median:.3} / {p95:.3} ms CPU"
            );
        }
        for group in [
            "deform",
            "cull",
            "directional shadow",
            "opaque geometry + lighting",
            "geometry",
            "opaque lighting",
            "receivers",
            "DiligentFX",
            "SSR",
            "Godot SSR",
            "reflection composition",
            "transmission copy",
            "blended",
            "TAA",
            "FSR2 composition",
            "FSR2",
            "motion blur",
        ] {
            if let Some(times) = self.groups.get(group) {
                let (median, p95) = quantiles(times, self.totals.len());
                println!("  {group:<26} {median:7.3} / {p95:7.3} ms");
            }
        }
    }
}

/// What the example's command line chose.
struct Options {
    frames: usize,
    chunks: usize,
    size: [u32; 2],
    directory: std::path::PathBuf,
}

fn render(
    run: &Run,
    options: &Options,
    (device, queue): (&wgpu::Device, &wgpu::Queue),
) -> Result<Times, Box<dyn Error>> {
    let directory = options.directory.join(run.name);
    std::fs::create_dir_all(&directory)?;
    let mut scene = Scene::new(device, queue);
    let world = scene.add_asset(device, queue, world(run.transmissive_glass))?;
    scene.add_instance(
        device,
        queue,
        InstanceState::new(world.model),
        Mobility::Static,
    )?;
    let mut times = Times::default();
    let water = Material {
        alpha: AlphaMode::Blend {
            receives_screen_space_reflections: true,
            keeps_specular: true,
        },
        casts_directional_shadow: false,
        ior: 1.33,
        transmission: 1.,
        attenuation_distance: 3.,
        attenuation_color: [0.45, 0.8, 0.75],
        ..material("water", [1.; 4], 0.04)
    };
    let mut chunks = None;
    let mut grid = None;
    match run.waves {
        Waves::Shader | Waves::Still => {
            let shader = scene.add_shader(ShaderSource {
                wgsl: include_str!("support/gerstner.wgsl").into(),
                label: "gerstner water".into(),
            })?;
            check_layout(&scene, shader)?;
            let water = Material {
                normal_texture: Some(0),
                normal_layers: Some(water_layers()),
                // The shader replaces this constant slab with the column.
                thickness: 1.,
                ..water
            };
            let water = scene.add_materials(device, queue, &[water], &[wave_map()])?[0];
            // A material names its shader once added.
            let values = SurfaceMaterial {
                shader: Some(MaterialShader {
                    shader,
                    displacement_bound: displacement_bound(),
                }),
                ..scene.material(water)?
            };
            scene.set_material(queue, water, values)?;
            scene.set_shader_parameters(queue, water, bytemuck::bytes_of(&water_params()))?;
            let placed = place_chunks((device, queue), &mut scene, water, options.chunks, false)?;
            chunks = Some(placed);
        }
        Waves::Morph => {
            let water = Material {
                normal_texture: Some(0),
                normal_layers: Some(water_layers()),
                thickness: 2.,
                ..water
            };
            let water = scene.add_materials(device, queue, &[water], &[wave_map()])?[0];
            let placed = place_chunks((device, queue), &mut scene, water, options.chunks, true)?;
            times.uploaded = placed.instances.len() * size_of::<[f32; 12]>();
            chunks = Some(placed);
        }
        Waves::Mesh => {
            let water = Material {
                thickness: 2.,
                ..water
            };
            let water = scene.add_materials(device, queue, &[water], &[])?[0];
            // Moving: replacing its geometry each frame is no static edit.
            let model =
                scene.add_model(device, queue, PreparedModel::new(vec![lake(0., water)])?)?;
            scene.add_instance(device, queue, InstanceState::new(model), Mobility::Moving)?;
            grid = Some((model, water));
        }
    }
    let resources = scene.diagnostic_resources();
    times.ray_source = resources.ray_source_live;
    times.geometry = resources.geometry_live;
    let environment = scene.add_environment(device, queue, &sky())?;
    let mut size = options.size;
    let mut texture = output(device, size);
    let mut renderer = Renderer::new(device, queue, texture.format(), size, 1., &run.settings)?;
    let mut timing = GpuTiming::new(device, queue);
    let mut in_flight = None;
    let mut origin = Vec3::ZERO;
    let frames = options.frames;
    for index in 0..frames {
        if run.resize_and_cut && index == frames / 3 {
            size = RESIZED;
            texture = output(device, size);
        }
        // Halfway, the render origin moves by whole chunks, the camera with
        // it: the waves cross it unchanged.
        if index == frames / 2 {
            scene.move_origin(device, queue, ORIGIN_MOVE)?;
            origin += ORIGIN_MOVE;
        }
        renderer.resize(device, size, 1., &run.settings);
        let seconds = match run.waves {
            Waves::Still => 0.,
            _ => index as f64 / 60.,
        };
        if let Some(chunks) = chunks.as_ref().filter(|chunks| chunks.morph) {
            let weights = morph_weights(seconds);
            for &instance in &chunks.instances {
                scene.set_instance_deformation(queue, instance, &[], &weights)?;
            }
        }
        if let Some((model, water)) = grid {
            let surface = lake(seconds as f32, water);
            times.uploaded = surface.vertices.len() * size_of::<Vertex>()
                + surface.indices.len() * size_of::<u32>();
            let start = Instant::now();
            let prepared = PreparedModel::new(vec![surface])?;
            let prepared_at = Instant::now();
            scene.set_model(device, queue, model, prepared)?;
            if index >= WARM_UP {
                let ms = |from: Instant, to: Instant| (to - from).as_secs_f64() * 1000.;
                times.edit([ms(start, prepared_at), ms(prepared_at, Instant::now())]);
            }
        }
        let mut input = FrameInput::new(camera(index, run.orbit, size, origin));
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

/// `WaterParams` against the layout naga gave the shader's `ShaderParams`:
/// a mirror that drifted from the WGSL would write its members at the wrong
/// offsets.
fn check_layout(scene: &Scene, shader: sgl_3d::ShaderId) -> Result<(), Box<dyn Error>> {
    let layout = scene.shader_parameters_layout(shader)?;
    let mirror = [
        ("waves", std::mem::offset_of!(WaterParams, waves)),
        ("shape", std::mem::offset_of!(WaterParams, shape)),
        ("deep", std::mem::offset_of!(WaterParams, deep)),
        ("shallow", std::mem::offset_of!(WaterParams, shallow)),
        ("absorption", std::mem::offset_of!(WaterParams, absorption)),
    ];
    let offsets: Vec<_> = layout
        .fields
        .iter()
        .map(|field| (field.name.as_str(), field.offset as usize))
        .collect();
    if offsets != mirror || layout.size as usize != size_of::<WaterParams>() {
        return Err(format!("WaterParams does not mirror ShaderParams: {layout:?}").into());
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut options = Options {
        frames: 120,
        chunks: 64,
        size: SIZE,
        directory: Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/water-example"),
    };
    let mut morph = false;
    let mut only = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--frames" => {
                options.frames = args.next().ok_or("--frames requires a count")?.parse()?
            }
            "--run" => only.push(args.next().ok_or("--run requires a run's name")?),
            "--chunks" => {
                options.chunks = args.next().ok_or("--chunks requires a count")?.parse()?
            }
            "--morph" => morph = true,
            "--size" => {
                let size = args.next().ok_or("--size requires WxH")?;
                let (width, height) = size.split_once('x').ok_or("--size requires WxH")?;
                options.size = [width.parse()?, height.parse()?];
            }
            other => return Err(format!("unknown argument {other}").into()),
        }
    }
    if options.frames < WARM_UP + 3 {
        return Err(format!("--frames must be at least {}", WARM_UP + 3).into());
    }
    if options.chunks == 0 {
        return Err("--chunks must be at least 1".into());
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
    println!(
        "{} frames per run at {}x{}, {} chunks{}; GPU time median / p95",
        options.frames,
        options.size[0],
        options.size[1],
        options.chunks,
        if morph { " of morph targets" } else { "" }
    );
    let runs: Vec<Run> = runs(morph)
        .into_iter()
        .filter(|run| only.is_empty() || only.iter().any(|name| name == run.name))
        .collect();
    if runs.is_empty() {
        return Err(format!("no run is named {only:?}").into());
    }
    for run in runs {
        render(&run, &options, (&device, &queue))?.report(run.name);
    }
    println!("Frames: {}", options.directory.canonicalize()?.display());
    Ok(())
}
