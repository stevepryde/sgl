//! A streamed and edited voxel world at the scale of a block game: 16 m
//! chunks of terrain meshed into quads, each one model placed as a static
//! instance at its integer chunk origin, streamed in around a camera that
//! walks, sprints or flies in a straight line and out behind it, edited
//! block by block, remeshed in waves, and lit by a sky, a sun with cascaded
//! shadows and torches. Water is a blended receiver of screen-space reflections, a
//! moving instance per surface chunk that holds any. Fifty creatures walk
//! about the camera. The render origin follows the camera, chunk-aligned.
//!
//! `cargo run --release -p sgl-3d --example streaming [-- RUN... | --check]`
//!
//! The game's side is modelled on a block game's: its mesher finishes up to
//! 24 chunks a 33 ms tick, nearest the camera first, and the game gives them
//! to the scene under a budget of 16 chunks and 8 MiB of mesh a frame. Up to
//! 30 block edits a second raise or lower a column's top, each remeshing the
//! chunk that holds the block it changed and that block's solid neighbours'
//! chunks across a border; waves remesh 5 to 20 chunks every 16 frames; and
//! one frame places a torch and remeshes the 27 chunks about it. Every run
//! renders at 1920×1080 with TAA, full-resolution screen-space reflections,
//! world-space reflections and shadows. It streams its first window in with
//! the camera still, then measures `--frames` frames (600) and prints, as
//! median / p95: the CPU time of each scene operation by kind and size; the
//! CPU time a frame spends in scene calls, apart from the game's meshing,
//! and recording; what the library counted (`diagnostics::counters`): bytes
//! uploaded by call site, buffers created, each step of building a model,
//! static-edit boxes and ray-source growths; the scene's buffer sizes
//! (`Scene::diagnostic_resources`); draws per view
//! (`Renderer::diagnostic_draws`); the local-light shadow faces and layers
//! redrawn; what streamed; and GPU time per pass group. The last frame of
//! each run is written to `target/streaming-example/<run>.png`. Examples
//! build with the `diagnostics` feature, whose counters add a thread-local
//! update to each upload and build step, so these CPU times sit slightly
//! above a game's without it.
//!
//! `--check` holds the camera still in the world while the render origin
//! moves by a chunk and by 256 m, and fails unless static content shows no
//! motion and no shadow is redrawn; then it remeshes the chunks holding
//! shadowed torches and fails unless the next submitted frame redraws their
//! static shadow layers and the one after redraws none. An abandoned frame
//! precedes each submitted one.
use sgl_3d::diagnostics::{Counters, DiagnosticTarget, SceneResources};
use sgl_3d::glam::{DVec3, IVec3, Mat4, Vec3, camera};
use sgl_3d::{
    AlphaMode, Camera, DirectionalLight, DirectionalShadow, EnvironmentId, Exposure, FrameInput,
    InstanceId, InstanceState, Light, LightId, LightShape, MaterialId, Mobility, ModelId,
    ModelMesh, Renderer, Scene,
    asset::{CpuMesh, Image, Material, Vertex},
    environment::{EnvironmentMap, PmremAtlas},
    settings::{Antialiasing, ReflectionMethod, ScreenSpaceReflections, Settings},
    timing::{FrameTime, GpuTiming},
};
use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::error::Error;
use std::hash::BuildHasherDefault;
use std::path::Path;
use std::time::Instant;

/// Maps and sets of chunks that iterate alike in every run.
type Chunks<V> = HashMap<IVec3, V, BuildHasherDefault<DefaultHasher>>;
type ChunkSet = HashSet<IVec3, BuildHasherDefault<DefaultHasher>>;

/// A chunk's side, in metres and blocks.
const CHUNK: i32 = 16;
/// The water's surface: the top of block 29.
const SEA: i32 = 30;
const SIZE: [u32; 2] = [1920, 1080];
/// Frames the first window may take to stream in.
const STREAM_IN_LIMIT: usize = 2_000;
/// The simulation's frame time: 60 frames a second.
const DT: f64 = 1. / 60.;
/// Meshes the game's mesher finishes per tick, and the tick.
const MESHED_PER_TICK: usize = 24;
const TICK: f64 = 0.033;
/// What the game gives the scene in one frame at most.
const CHUNKS_PER_FRAME: usize = 16;
const BYTES_PER_FRAME: usize = 8 << 20;
/// Block edits a second, and frames between remeshing waves.
const EDITS_PER_SECOND: f64 = 30.;
const WAVE_FRAMES: usize = 16;

/// One run of the example.
#[derive(Clone)]
struct Run {
    name: &'static str,
    /// Metres a second along +x, from the first to the last frame.
    speed: [f64; 2],
    /// Chunks streamed about the camera's: ±x and ±z, and below and above.
    radius: [i32; 2],
    /// Chunks between render origin moves, along x.
    origin_cell: i32,
    /// The sun's shadow distance in metres, in four cascades.
    shadow_distance: f32,
    /// Torches in each surface chunk.
    torches: u32,
    /// Every water chunk in view is meshed anew each frame.
    water_remesh: bool,
    /// Each block a mesh of its own material, rather than one atlas mesh.
    per_block: bool,
    /// Block edits, waves and the torch frame happen.
    edits: bool,
    /// Frames measured once the first window streamed in.
    frames: usize,
}

fn runs(frames: usize) -> Vec<Run> {
    let walk = Run {
        name: "walk",
        speed: [4.3, 4.3],
        radius: [4, 2],
        origin_cell: 1,
        shadow_distance: 150.,
        torches: 1,
        water_remesh: false,
        per_block: false,
        edits: true,
        frames,
    };
    vec![
        walk.clone(),
        Run {
            name: "sprint",
            speed: [5.6, 5.6],
            ..walk.clone()
        },
        Run {
            name: "fly",
            speed: [20., 50.],
            ..walk.clone()
        },
        Run {
            name: "fly-margin",
            speed: [20., 50.],
            radius: [5, 3],
            ..walk.clone()
        },
        Run {
            name: "origin-256",
            speed: [20., 50.],
            origin_cell: 16,
            ..walk.clone()
        },
        Run {
            name: "cascades-512",
            shadow_distance: 512.,
            ..walk.clone()
        },
        Run {
            name: "torches-1024",
            torches: 8,
            radius: [5, 3],
            ..walk.clone()
        },
        Run {
            name: "per-block-materials",
            per_block: true,
            ..walk.clone()
        },
        Run {
            name: "water-static",
            edits: false,
            ..walk.clone()
        },
        Run {
            name: "water-remesh",
            edits: false,
            water_remesh: true,
            ..walk.clone()
        },
        Run {
            name: "headroom",
            speed: [20., 50.],
            radius: [14, 3],
            ..walk
        },
    ]
}

/// A small deterministic hash.
fn hash(mut value: u64) -> u64 {
    value ^= value >> 33;
    value = value.wrapping_mul(0xff51_afd7_ed55_8ccd);
    value ^= value >> 33;
    value = value.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
    value ^ (value >> 33)
}

fn hash3(at: IVec3, salt: u64) -> u64 {
    hash((at.x as u32 as u64) ^ ((at.y as u32 as u64) << 21) ^ ((at.z as u32 as u64) << 42) ^ salt)
}

/// The terrain: rolling hills with ridges and rough ground, a column's top
/// block's height. A surface chunk meshes to 300-1,400 quads.
fn ground(x: i32, z: i32) -> i32 {
    let rough = (i64::from(x).wrapping_mul(73_856_093) ^ i64::from(z).wrapping_mul(19_349_663))
        .rem_euclid(7) as f64
        / 6.;
    let (x, z) = (f64::from(x), f64::from(z));
    let hills = 14. * (x * 0.031).sin() * (z * 0.027).cos() + 8. * (x * 0.083 + z * 0.051).sin();
    let ridges = 8. * ((x * 0.21).sin() * (z * 0.17).sin()).abs();
    let fine = 5. * (x * 0.61 + 1.3 * (z * 0.37).sin()).sin() * (z * 0.53).cos();
    (34. + hills + ridges + fine + 4. * rough).floor() as i32
}

/// The world as the game edits it: the terrain with its edited columns.
#[derive(Default)]
struct World {
    /// Height changes of edited columns.
    edits: BTreeMap<(i32, i32), i32>,
    /// Remeshes of each chunk since it was first meshed, which shade it.
    revisions: Chunks<u32>,
}

impl World {
    fn height(&self, x: i32, z: i32) -> i32 {
        ground(x, z) + self.edits.get(&(x, z)).copied().unwrap_or(0)
    }
}

/// The atlas's tiles.
#[derive(Clone, Copy)]
enum Block {
    Grass,
    Dirt,
    Stone,
    Sand,
}

fn empty(material: usize) -> CpuMesh {
    CpuMesh {
        vertices: Vec::new(),
        indices: Vec::new(),
        material,
        deformation: Default::default(),
    }
}

/// A quad of a mesh: its corner, the two edges from it and the vertex
/// colour's shade, its texture coordinates spanning 0 to 1.
fn quad(mesh: &mut CpuMesh, corner: Vec3, u: Vec3, v: Vec3, shade: f32) {
    let start = mesh.vertices.len() as u32;
    let normal = u.cross(v).normalize();
    for [s, t] in [[0., 0.], [1., 0.], [1., 1.], [0., 1.]] {
        mesh.vertices.push(Vertex {
            tangent: u.normalize().extend(1.).to_array(),
            lightmap_bounds: [0., 0., 1., 1.],
            lightmap_uv: [0.; 2],
            position: (corner + u * s + v * t).to_array(),
            normal: normal.to_array(),
            uv: [s, t],
            color: [shade, shade, shade, 1.],
        });
    }
    mesh.indices
        .extend([0, 1, 2, 0, 2, 3].map(|index| start + index));
}

/// One mesh of `meshes`, one per block, with each block's texture
/// coordinates moved into its tile of the atlas.
fn atlas_mesh(meshes: Vec<CpuMesh>) -> CpuMesh {
    let mut merged = empty(0);
    for mesh in meshes {
        let tile = mesh.material as f32;
        let start = merged.vertices.len() as u32;
        merged
            .vertices
            .extend(mesh.vertices.into_iter().map(|vertex| Vertex {
                uv: [(tile + vertex.uv[0]) * 0.25, vertex.uv[1]],
                ..vertex
            }));
        merged
            .indices
            .extend(mesh.indices.into_iter().map(|index| start + index));
    }
    merged
}

/// Chunk `chunk`'s meshes in its own space: its opaque blocks' exposed
/// faces, one mesh per block (each mesh's material is its block), and its
/// water's surface, any of them empty. `seconds` moves the water's normals.
fn mesh_chunk(world: &World, chunk: IVec3, seconds: f32) -> (Vec<CpuMesh>, CpuMesh) {
    let mut blocks: Vec<CpuMesh> = (0..4).map(empty).collect();
    let mut water = empty(0);
    let base = chunk * CHUNK;
    let shade = 1. - 0.04 * (world.revisions.get(&chunk).copied().unwrap_or(0) % 4) as f32;
    let mut surface = false;
    for local_z in 0..CHUNK {
        for local_x in 0..CHUNK {
            let (x, z) = (base.x + local_x, base.z + local_z);
            let top = world.height(x, z);
            let in_chunk = |y: i32| y >= base.y && y < base.y + CHUNK;
            let at = |y: i32| Vec3::new(local_x as f32, (y - base.y) as f32, local_z as f32);
            if in_chunk(top) {
                surface = true;
                let tile = if top < SEA + 1 {
                    Block::Sand
                } else {
                    Block::Grass
                };
                quad(
                    &mut blocks[tile as usize],
                    at(top) + Vec3::new(0., 1., 1.),
                    Vec3::X,
                    Vec3::NEG_Z,
                    shade,
                );
            }
            // Each side face down to the neighbouring column's top.
            for (dx, dz, corner, u) in [
                (1, 0, Vec3::new(1., 0., 1.), Vec3::NEG_Z),
                (-1, 0, Vec3::new(0., 0., 0.), Vec3::Z),
                (0, 1, Vec3::new(0., 0., 1.), Vec3::X),
                (0, -1, Vec3::new(1., 0., 0.), Vec3::NEG_X),
            ] {
                let neighbour = world.height(x + dx, z + dz);
                for y in (neighbour + 1).max(base.y)..=top.min(base.y + CHUNK - 1) {
                    // A block's kind is the terrain's: stone below the
                    // ground's top four, dirt above and wherever placed.
                    let tile = if ground(x, z) - y > 3 {
                        Block::Stone
                    } else {
                        Block::Dirt
                    };
                    quad(
                        &mut blocks[tile as usize],
                        at(y) + corner,
                        u,
                        Vec3::Y,
                        shade * 0.75,
                    );
                }
            }
            if top < SEA && in_chunk(SEA - 1) {
                // A water surface over the column; its normal sways.
                let phase = 0.4 * x as f32 + 0.3 * z as f32 - 2. * seconds;
                let tilt = Vec3::new(0.08 * phase.sin(), 1., 0.06 * phase.cos()).normalize();
                let start = water.vertices.len() as u32;
                for [s, t] in [[0., 0.], [1., 0.], [1., 1.], [0., 1.]] {
                    water.vertices.push(Vertex {
                        tangent: [1., 0., 0., 1.],
                        lightmap_bounds: [0., 0., 1., 1.],
                        lightmap_uv: [0.; 2],
                        position: (at(SEA - 1) + Vec3::new(s, 0.9, 1. - t)).to_array(),
                        normal: tilt.to_array(),
                        uv: [s, t],
                        color: [1.; 4],
                    });
                }
                water
                    .indices
                    .extend([0, 1, 2, 0, 2, 3].map(|index| start + index));
            }
        }
    }
    if !surface && base.y + CHUNK <= ground(base.x, base.z) {
        // Underground: a cave's few faces in some chunks.
        let faces = match hash3(chunk, 7) % 4 {
            0 => (hash3(chunk, 11) % 51) as usize,
            _ => 0,
        };
        for face in 0..faces {
            let cell = hash3(chunk, face as u64);
            let at = Vec3::new(
                (cell % 16) as f32,
                ((cell >> 8) % 16) as f32,
                ((cell >> 16) % 16) as f32,
            );
            quad(
                &mut blocks[Block::Stone as usize],
                at,
                Vec3::X,
                Vec3::Z,
                0.4,
            );
        }
    }
    (blocks, water)
}

/// The bytes the game gives the scene for `mesh`.
fn bytes(mesh: &CpuMesh) -> usize {
    mesh.vertices.len() * std::mem::size_of::<Vertex>() + mesh.indices.len() * 4
}

/// Each block's colour: grass, dirt, stone and sand.
const COLOURS: [[u8; 3]; 4] = [
    [96, 160, 64],
    [134, 96, 67],
    [128, 128, 132],
    [218, 204, 150],
];

/// A speckled texel of block `tile`.
fn texel(tile: usize, x: u32, y: u32) -> image::Rgba<u8> {
    let speck = (hash(u64::from(x * 64 + y)) % 24) as u8;
    let [r, g, b] = COLOURS[tile];
    image::Rgba([r - speck.min(r), g - speck.min(g), b - speck.min(b), 255])
}

/// The atlas: a row of the blocks' 16-texel tiles.
fn atlas() -> Image {
    Image::Rgba8(image::RgbaImage::from_fn(64, 16, |x, y| {
        texel((x / 16) as usize, x % 16, y)
    }))
}

/// Block `tile`'s own texture.
fn tile_texture(tile: usize) -> Image {
    Image::Rgba8(image::RgbaImage::from_fn(16, 16, |x, y| texel(tile, x, y)))
}

/// The sky: a constant blue radiance, as the water example's, which draws
/// the backdrop, lights what the sun does not reach and backs reflections.
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

/// A creature's box, about a metre and a half tall.
fn creature() -> CpuMesh {
    let mut mesh = CpuMesh {
        vertices: Vec::new(),
        indices: Vec::new(),
        material: 0,
        deformation: Default::default(),
    };
    let size = Vec3::new(0.6, 1.5, 0.6);
    for (normal, u, v) in [
        (Vec3::X, Vec3::NEG_Z, Vec3::Y),
        (Vec3::NEG_X, Vec3::Z, Vec3::Y),
        (Vec3::Y, Vec3::X, Vec3::NEG_Z),
        (Vec3::NEG_Y, Vec3::X, Vec3::Z),
        (Vec3::Z, Vec3::X, Vec3::Y),
        (Vec3::NEG_Z, Vec3::NEG_X, Vec3::Y),
    ] {
        let corner = (normal - u - v) * 0.5 * size + Vec3::Y * 0.75;
        quad(&mut mesh, corner, u * size, v * size, 0.9);
    }
    mesh
}

/// A chunk in the scene.
#[derive(Default)]
struct Resident {
    terrain: Option<(ModelId, InstanceId)>,
    water: Option<(ModelId, InstanceId)>,
    lights: Vec<LightId>,
    /// Whether any of its lights casts a shadow.
    shadowed: bool,
    /// Its terrain's quads.
    quads: usize,
}

/// A chunk the mesher finished, waiting to be given to the scene: its
/// terrain's meshes, as the run gives them, and its water's.
struct Meshed {
    chunk: IVec3,
    terrain: Vec<CpuMesh>,
    water: CpuMesh,
}

/// CPU times of scene operations of one kind, in microseconds, by size, and
/// the frame's time in scene calls so far.
#[derive(Default)]
struct OperationTimes {
    times: BTreeMap<(&'static str, usize), Vec<f64>>,
    frame_ms: f64,
}

impl OperationTimes {
    fn time<T>(&mut self, kind: &'static str, size: usize, operation: impl FnOnce() -> T) -> T {
        let started = Instant::now();
        let result = operation();
        let elapsed = started.elapsed().as_secs_f64();
        self.frame_ms += elapsed * 1e3;
        self.times
            .entry((kind, size))
            .or_default()
            .push(elapsed * 1e6);
        result
    }

    /// The frame's time in scene calls; the next frame's starts at zero.
    fn take_frame(&mut self) -> f64 {
        std::mem::take(&mut self.frame_ms)
    }
}

/// A run's measurements, from the frame after its first window streamed in.
#[derive(Default)]
struct Measured {
    operations: OperationTimes,
    /// The loop index of the first measured frame.
    from: usize,
    gpu: BTreeMap<&'static str, Vec<f64>>,
    gpu_totals: Vec<f64>,
    recording: Vec<f64>,
    scene_calls: Vec<f64>,
    meshing: Vec<f64>,
    draws: Vec<sgl_3d::diagnostics::ViewDraws>,
    triangles: Vec<f64>,
    faces: Vec<f64>,
    layers: Vec<f64>,
    shadow_draws: Vec<f64>,
    given_bytes: Vec<f64>,
    uploaded_bytes: Vec<f64>,
    buffers_created: Vec<f64>,
    /// What the library counted over the measured frames.
    counted: Counters,
    resources: Vec<SceneResources>,
    resident: Vec<f64>,
    quads: Vec<f64>,
    inserted: usize,
    replaced: usize,
    removed: usize,
    moves: usize,
    lights: usize,
}

impl Measured {
    fn gpu(&mut self, frame: FrameTime) {
        // `begin_frame` numbers loop index i's frame i + 1.
        if (frame.frame as usize) <= self.from {
            return;
        }
        let measured = self.gpu_totals.len();
        self.gpu_totals.push(frame.total_ms);
        for pass in frame.passes {
            let times = self.gpu.entry(pass.name).or_default();
            times.resize(measured + 1, 0.);
            times[measured] += pass.ms;
        }
    }
}

/// The median and 95th percentile of `values`, padded with zeros to `count`.
fn quantiles(values: &[f64], count: usize) -> (f64, f64) {
    let mut sorted = values.to_vec();
    sorted.resize(count.max(values.len()).max(1), 0.);
    sorted.sort_by(f64::total_cmp);
    let at = |q: f64| sorted[((sorted.len() as f64 * q).ceil() as usize).saturating_sub(1)];
    (at(0.5), at(0.95))
}

/// A size bucket: triangles rounded up to a power of two (0 for none).
fn bucket(triangles: usize) -> usize {
    triangles.next_power_of_two() * usize::from(triangles > 0)
}

impl Measured {
    fn report(&self, run: &Run) {
        let frames = self.recording.len();
        let line = |label: &str, values: &[f64], unit: &str| {
            let (median, p95) = quantiles(values, values.len());
            println!("  {label:<34} {median:12.2} / {p95:12.2} {unit}");
        };
        println!(
            "\n{} ({frames} frames measured after {} streaming in; {:.1}-{:.1} m/s, {} chunks streamed about the camera's)",
            run.name,
            self.from,
            run.speed[0],
            run.speed[1],
            (2 * run.radius[0] + 1).pow(2) * (2 * run.radius[1] + 1)
        );
        line("resident chunks with geometry", &self.resident, "");
        line("resident terrain quads", &self.quads, "");
        line("CPU recording a frame", &self.recording, "ms");
        line("CPU in scene calls a frame", &self.scene_calls, "ms");
        line("CPU meshing (the game's) a frame", &self.meshing, "ms");
        line("mesh bytes given a frame", &self.given_bytes, "B");
        line("bytes uploaded a frame", &self.uploaded_bytes, "B");
        line("buffers created a frame", &self.buffers_created, "");
        let view = |pick: &dyn Fn(&sgl_3d::diagnostics::ViewDraws) -> f64| {
            quantiles(&self.draws.iter().map(pick).collect::<Vec<_>>(), frames).0
        };
        let cascades = self.draws.first().map_or(0, |draws| draws.cascades.len());
        let cascade_draws: Vec<String> = (0..cascades)
            .map(|cascade| format!("{:.0}", view(&|draws| draws.cascades[cascade] as f64)))
            .collect();
        println!(
            "  draws per view (median): camera {:.0}, blended {:.0}, cascades {}",
            view(&|draws| draws.camera as f64),
            view(&|draws| draws.blended as f64),
            cascade_draws.join(", ")
        );
        line("camera triangles", &self.triangles, "");
        line("local shadow faces drawn", &self.faces, "");
        line("local shadow layers drawn", &self.layers, "");
        line("local shadow atlas draws", &self.shadow_draws, "");
        let counted = &self.counted;
        let per_frame = |count: u64| count as f64 / frames.max(1) as f64;
        println!(
            "  static-edit boxes {:.2} a frame, {:.2} merged into another; ray source growths {}, geometry slab growths {}",
            per_frame(counted.static_edit_boxes),
            per_frame(counted.static_edit_boxes_merged),
            counted.ray_source_growths,
            counted.geometry_growths
        );
        for time in &counted.steps {
            println!(
                "  {:<34} {:12.1} us a call ({} calls)",
                format!("{:?}", time.step),
                time.nanoseconds as f64 / time.calls as f64 / 1e3,
                time.calls
            );
        }
        let mut sites = counted.uploads.clone();
        sites.sort_by_key(|site| std::cmp::Reverse(site.bytes));
        for site in sites.iter().take(12) {
            let at = format!(
                "{}:{}",
                site.file.trim_start_matches("crates/sgl-3d/"),
                site.line
            );
            println!(
                "  upload at {at:<40} {:12.0} B a frame, {:6.1} writes",
                per_frame(site.bytes),
                per_frame(site.writes)
            );
        }
        if let (Some(first), Some(last)) = (self.resources.first(), self.resources.last()) {
            let most = |pick: fn(&SceneResources) -> u64| {
                self.resources.iter().map(pick).max().unwrap_or(0)
            };
            let row = |label: &str, pick: fn(&SceneResources) -> u64| {
                println!(
                    "  {label:<34} first {:>11} last {:>11} most {:>11}",
                    pick(first),
                    pick(last),
                    most(pick)
                );
            };
            row("ray source bytes", |r| r.ray_source);
            row("ray source bytes to its last word", |r| r.ray_source_used);
            row("ray source bytes content holds", |r| r.ray_source_live);
            row("object record bytes", |r| r.object_records);
            row("instance entry bytes", |r| r.instance_entries);
            row("light record bytes", |r| r.light_records);
            row("geometry buffer bytes", |r| r.geometry);
            row("geometry bytes content holds", |r| r.geometry_live);
            row("geometry buffers", |r| r.geometry_buffers);
            let quads = self.quads.last().copied().unwrap_or(0.).max(1.);
            println!(
                "  bytes a resident quad: ray source {:.0}, geometry {:.0}",
                last.ray_source_live as f64 / quads,
                last.geometry_live as f64 / quads
            );
        }
        println!(
            "  streamed: {} inserted, {} replaced, {} removed; {} origin moves; {} lights at the end",
            self.inserted, self.replaced, self.removed, self.moves, self.lights
        );
        for ((kind, size), times) in &self.operations.times {
            let (median, p95) = quantiles(times, times.len());
            println!(
                "  {kind:<16} {size:>6} {:<10} {median:9.1} / {p95:9.1} us ({} calls)",
                if kind.ends_with("model") {
                    "triangles"
                } else {
                    "instances"
                },
                times.len()
            );
        }
        if self.gpu_totals.is_empty() {
            println!("  no GPU timestamps");
            return;
        }
        let count = self.gpu_totals.len();
        let (median, p95) = quantiles(&self.gpu_totals, count);
        println!("  GPU frame                          {median:12.3} / {p95:12.3} ms");
        let mut groups: Vec<_> = self
            .gpu
            .iter()
            .map(|(name, times)| (name, quantiles(times, count)))
            .collect();
        groups.sort_by(|a, b| b.1.0.total_cmp(&a.1.0));
        for (name, (median, p95)) in groups.into_iter().take(16) {
            println!("    {name:<32} {median:12.3} / {p95:12.3} ms");
        }
    }
}

/// The game's view of one run: its world, what the scene holds of it, and
/// where its camera and render origin are.
struct Game {
    run: Run,
    world: World,
    resident: Chunks<Resident>,
    /// Chunks the mesher works on, nearest first, and what it finished.
    requested: VecDeque<IVec3>,
    meshed: VecDeque<Meshed>,
    tick: f64,
    /// The terrain's materials, by a mesh's material index, and the water's.
    terrain: Vec<MaterialId>,
    water: MaterialId,
    creatures: Vec<InstanceId>,
    creature: ModelId,
    sky: EnvironmentId,
    /// The render origin, in world blocks: a multiple of the origin cell.
    origin: IVec3,
    eye: DVec3,
    seconds: f64,
    edits_owed: f64,
    /// The frame's time in the game's mesher so far, in milliseconds.
    meshing_ms: f64,
    measured: Measured,
}

impl Game {
    fn new(
        run: &Run,
        scene: &mut Scene,
        (device, queue): (&wgpu::Device, &wgpu::Queue),
    ) -> Result<Self, Box<dyn Error>> {
        let block = |name: &str, texture| Material {
            name: name.into(),
            base_texture: Some(texture),
            roughness: 0.9,
            ..Default::default()
        };
        let (blocks, images) = if run.per_block {
            (
                ["grass", "dirt", "stone", "sand"]
                    .into_iter()
                    .enumerate()
                    .map(|(tile, name)| block(name, tile))
                    .collect(),
                (0..4).map(tile_texture).collect(),
            )
        } else {
            (vec![block("atlas", 0)], vec![atlas()])
        };
        let terrain = scene.add_materials(device, queue, &blocks, &images)?;
        let materials = scene.add_materials(
            device,
            queue,
            &[
                Material {
                    name: "water".into(),
                    base: [0.05, 0.12, 0.16, 0.6],
                    roughness: 0.05,
                    alpha: AlphaMode::Blend {
                        receives_screen_space_reflections: true,
                    },
                    casts_directional_shadow: false,
                    ..Default::default()
                },
                Material {
                    name: "creature".into(),
                    base: [0.7, 0.35, 0.25, 1.],
                    roughness: 0.6,
                    ..Default::default()
                },
            ],
            &[],
        )?;
        let creature = creature();
        let creature = scene.add_model(
            device,
            queue,
            vec![ModelMesh {
                vertices: creature.vertices,
                indices: creature.indices,
                material: materials[1],
                deformation: Default::default(),
            }],
        )?;
        let sky = scene.add_environment(device, queue, &sky())?;
        let mut game = Self {
            run: run.clone(),
            world: World::default(),
            resident: Chunks::default(),
            requested: VecDeque::new(),
            meshed: VecDeque::new(),
            tick: 0.,
            terrain,
            water: materials[0],
            creatures: Vec::new(),
            creature,
            sky,
            origin: IVec3::ZERO,
            eye: DVec3::ZERO,
            seconds: 0.,
            edits_owed: 0.,
            meshing_ms: 0.,
            measured: Measured::default(),
        };
        game.walk(0.5, 0.);
        game.origin = game.origin_for(game.eye);
        for index in 0..50 {
            let state = InstanceState {
                pose: game.creature_pose(index),
                ..InstanceState::new(creature)
            };
            game.creatures
                .push(scene.add_instance(device, queue, state, Mobility::Moving)?);
        }
        Ok(game)
    }

    /// Puts the eye at `x` along the line it travels, at a walker's height
    /// over the ground or, faster than 10 m/s, a flyer's.
    fn walk(&mut self, x: f64, speed: f64) {
        let ground = f64::from(self.world.height(x.floor() as i32, 0));
        self.eye = DVec3::new(x, ground + if speed > 10. { 24. } else { 2.6 }, 0.5);
    }

    /// The chunk holding world position `at`.
    fn chunk_of(at: DVec3) -> IVec3 {
        (at / f64::from(CHUNK)).floor().as_ivec3()
    }

    /// The render origin for an eye at `eye`: its chunk's corner, rounded
    /// down to the origin cell along x and z.
    fn origin_for(&self, eye: DVec3) -> IVec3 {
        let cell = self.run.origin_cell * CHUNK;
        let chunk = Self::chunk_of(eye) * CHUNK;
        IVec3::new(
            chunk.x.div_euclid(cell) * cell,
            0,
            chunk.z.div_euclid(cell) * cell,
        )
    }

    /// World position `at` in the render frame.
    fn render(&self, at: DVec3) -> Vec3 {
        (at - self.origin.as_dvec3()).as_vec3()
    }

    /// Creature `index`'s pose: walking circles about the eye.
    fn creature_pose(&self, index: usize) -> Mat4 {
        let angle = self.seconds * 0.3 + index as f64 * 0.7;
        let radius = 6. + (index % 10) as f64 * 3.;
        let x = self.eye.x + radius * angle.cos();
        let z = self.eye.z + radius * angle.sin();
        let y = f64::from(self.world.height(x.floor() as i32, z.floor() as i32) + 1);
        Mat4::from_translation(self.render(DVec3::new(x, y, z)))
            * Mat4::from_rotation_y(-angle as f32)
    }

    /// The chunks streamed about the eye's, nearest first.
    fn window(&self) -> Vec<IVec3> {
        let centre = Self::chunk_of(self.eye);
        let [across, up] = self.run.radius;
        let mut chunks = Vec::new();
        for dy in -up..=up {
            for dz in -across..=across {
                for dx in -across..=across {
                    chunks.push(centre + IVec3::new(dx, dy, dz));
                }
            }
        }
        chunks.sort_by_key(|chunk| (*chunk - centre).length_squared());
        chunks
    }

    /// The game's mesher: chunk `chunk`'s terrain meshes, as the run gives
    /// them, and its water's, timed apart from the scene's calls.
    fn mesh(&mut self, chunk: IVec3) -> (Vec<CpuMesh>, CpuMesh) {
        let started = Instant::now();
        let (blocks, water) = mesh_chunk(&self.world, chunk, self.seconds as f32);
        let terrain = self.terrain_meshes(blocks);
        self.meshing_ms += started.elapsed().as_secs_f64() * 1e3;
        (terrain, water)
    }

    /// Removes the chunks that left the window, with their lights, and
    /// meshes those that entered it, nearest first.
    fn stream(&mut self, scene: &mut Scene) -> Result<(), Box<dyn Error>> {
        let window = self.window();
        let wanted: ChunkSet = window.iter().copied().collect();
        let leaving: Vec<IVec3> = self
            .resident
            .keys()
            .filter(|chunk| !wanted.contains(chunk))
            .copied()
            .collect();
        for chunk in leaving {
            let resident = self.resident.remove(&chunk).unwrap();
            let ops = &mut self.measured.operations;
            for (model, instance) in [resident.terrain, resident.water].into_iter().flatten() {
                ops.time("remove_instance", 1, || scene.remove_instance(instance))?;
                ops.time("remove_model", 1, || scene.remove_model(model))?;
                self.measured.removed += 1;
            }
            for light in resident.lights {
                ops.time("remove_light", 1, || scene.remove_light(light))?;
            }
        }
        self.meshed.retain(|meshed| wanted.contains(&meshed.chunk));
        // What the mesher has yet to finish, nearest the eye first as it
        // moves.
        let finished: ChunkSet = self.meshed.iter().map(|meshed| meshed.chunk).collect();
        self.requested = window
            .into_iter()
            .filter(|chunk| !self.resident.contains_key(chunk) && !finished.contains(chunk))
            .collect();
        // The mesher finishes up to 24 chunks a tick.
        self.tick += DT;
        while self.tick >= TICK {
            self.tick -= TICK;
            for _ in 0..MESHED_PER_TICK {
                let Some(chunk) = self.requested.pop_front() else {
                    break;
                };
                let (terrain, water) = self.mesh(chunk);
                self.meshed.push_back(Meshed {
                    chunk,
                    terrain,
                    water,
                });
            }
        }
        Ok(())
    }

    /// Gives the scene what the mesher finished, within the frame's budget,
    /// and returns the mesh bytes given.
    fn arrive(
        &mut self,
        scene: &mut Scene,
        (device, queue): (&wgpu::Device, &wgpu::Queue),
    ) -> Result<usize, Box<dyn Error>> {
        let mut given = 0;
        for _ in 0..CHUNKS_PER_FRAME {
            let Some(next) = self.meshed.front() else {
                break;
            };
            let size = next.terrain.iter().map(bytes).sum::<usize>() + bytes(&next.water);
            if given > 0 && given + size > BYTES_PER_FRAME {
                break;
            }
            let Meshed {
                chunk,
                terrain,
                water,
            } = self.meshed.pop_front().unwrap();
            given += size;
            let mut resident = Resident {
                quads: quads(&terrain),
                ..Resident::default()
            };
            let pose = Mat4::from_translation(self.render((chunk * CHUNK).as_dvec3()));
            for (meshes, mobility) in [
                (self.model_meshes(terrain, false), Mobility::Static),
                (self.model_meshes(vec![water], true), Mobility::Moving),
            ] {
                if meshes.is_empty() {
                    continue;
                }
                let triangles = meshes.iter().map(|m| m.indices.len() / 3).sum();
                let ops = &mut self.measured.operations;
                let model = ops.time("add_model", bucket(triangles), || {
                    scene.add_model(device, queue, meshes)
                })?;
                let state = InstanceState {
                    pose,
                    ..InstanceState::new(model)
                };
                let instance = ops.time("add_instance", 1, || {
                    scene.add_instance(device, queue, state, mobility)
                })?;
                self.measured.inserted += 1;
                match mobility {
                    Mobility::Static => resident.terrain = Some((model, instance)),
                    Mobility::Moving => resident.water = Some((model, instance)),
                }
            }
            if resident.terrain.is_some() {
                (resident.lights, resident.shadowed) =
                    self.torches(scene, chunk, (device, queue))?;
            }
            self.resident.insert(chunk, resident);
        }
        Ok(given)
    }

    /// Adds a torch's light over world block `at`, `shadowed` or not.
    fn torch(
        &mut self,
        scene: &mut Scene,
        at: IVec3,
        shadowed: bool,
        (device, queue): (&wgpu::Device, &wgpu::Queue),
    ) -> Result<LightId, Box<dyn Error>> {
        let light = Light {
            position: self.render(at.as_dvec3() + DVec3::new(0.5, 1., 0.5)),
            shape: LightShape::Point,
            color: [1., 0.7, 0.4],
            intensity: 20.,
            range: 10.,
            casts_shadow: shadowed,
            ..Default::default()
        };
        Ok(self
            .measured
            .operations
            .time("add_light", 1, || scene.add_light(device, queue, light))?)
    }

    /// The torches chunk `chunk` holds, `torches` on its surface's blocks
    /// and one in eight shadowed, and whether any is.
    fn torches(
        &mut self,
        scene: &mut Scene,
        chunk: IVec3,
        gpu: (&wgpu::Device, &wgpu::Queue),
    ) -> Result<(Vec<LightId>, bool), Box<dyn Error>> {
        let base = chunk * CHUNK;
        let mut lights = Vec::new();
        let mut any_shadowed = false;
        for torch in 0..self.run.torches {
            let roll = hash3(chunk, 3 + u64::from(torch));
            let (x, z) = (
                base.x + ((roll >> 8) % CHUNK as u64) as i32,
                base.z + ((roll >> 16) % CHUNK as u64) as i32,
            );
            let top = self.world.height(x, z);
            if top < base.y || top >= base.y + CHUNK {
                continue;
            }
            let shadowed = (roll >> 24).is_multiple_of(8);
            any_shadowed |= shadowed;
            lights.push(self.torch(scene, IVec3::new(x, top + 1, z), shadowed, gpu)?);
        }
        Ok((lights, any_shadowed))
    }

    /// Meshes resident chunk `chunk` anew and replaces its `terrain`'s and
    /// its `water`'s geometry; a chunk that had none gains it.
    fn remesh(
        &mut self,
        scene: &mut Scene,
        chunk: IVec3,
        [terrain, water]: [bool; 2],
        gpu: (&wgpu::Device, &wgpu::Queue),
    ) -> Result<(), Box<dyn Error>> {
        if !self.resident.contains_key(&chunk) {
            return Ok(());
        }
        if terrain {
            *self.world.revisions.entry(chunk).or_default() += 1;
        }
        let (blocks, waves) = self.mesh(chunk);
        if terrain {
            self.resident.get_mut(&chunk).unwrap().quads = quads(&blocks);
            let meshes = self.model_meshes(blocks, false);
            self.replace(scene, chunk, meshes, Mobility::Static, gpu)?;
        }
        if water {
            let meshes = self.model_meshes(vec![waves], true);
            self.replace(scene, chunk, meshes, Mobility::Moving, gpu)?;
        }
        Ok(())
    }

    /// Gives resident chunk `chunk`'s terrain (static) or water (moving)
    /// model `meshes`, adding the model and its instance if it had none.
    fn replace(
        &mut self,
        scene: &mut Scene,
        chunk: IVec3,
        meshes: Vec<ModelMesh>,
        mobility: Mobility,
        (device, queue): (&wgpu::Device, &wgpu::Queue),
    ) -> Result<(), Box<dyn Error>> {
        let pose = Mat4::from_translation(self.render((chunk * CHUNK).as_dvec3()));
        let resident = self.resident.get_mut(&chunk).unwrap();
        let held = match mobility {
            Mobility::Static => &mut resident.terrain,
            Mobility::Moving => &mut resident.water,
        };
        let ops = &mut self.measured.operations;
        let triangles: usize = meshes.iter().map(|m| m.indices.len() / 3).sum();
        match held {
            Some((model, _)) => {
                let model = *model;
                ops.time("set_model", bucket(triangles), || {
                    scene.set_model(device, queue, model, meshes)
                })?;
                self.measured.replaced += 1;
            }
            None if triangles > 0 => {
                let model = ops.time("add_model", bucket(triangles), || {
                    scene.add_model(device, queue, meshes)
                })?;
                let state = InstanceState {
                    pose,
                    ..InstanceState::new(model)
                };
                let instance = ops.time("add_instance", 1, || {
                    scene.add_instance(device, queue, state, mobility)
                })?;
                *held = Some((model, instance));
                self.measured.inserted += 1;
            }
            None => {}
        }
        Ok(())
    }

    /// Raises or lowers column (x, z)'s top by a block and remeshes what
    /// that changed. A solid block owns its faces toward air, so the block
    /// that came or went changes its own chunk and those of its solid
    /// neighbours across a chunk border: the block below, and the columns
    /// beside it that reach as high. The block above is air. The water over
    /// the column changes when the top came out of or went under the sea.
    fn edit_column(
        &mut self,
        scene: &mut Scene,
        (x, z): (i32, i32),
        change: i32,
        gpu: (&wgpu::Device, &wgpu::Queue),
    ) -> Result<(), Box<dyn Error>> {
        let before = self.world.height(x, z);
        *self.world.edits.entry((x, z)).or_default() += change;
        let after = before + change;
        let block = IVec3::new(x, before.max(after), z);
        let chunk = block.div_euclid(IVec3::splat(CHUNK));
        let mut chunks = vec![chunk];
        for step in [IVec3::NEG_Y, IVec3::X, IVec3::NEG_X, IVec3::Z, IVec3::NEG_Z] {
            let next = block + step;
            let across = next.div_euclid(IVec3::splat(CHUNK));
            if across != chunk && next.y <= self.world.height(next.x, next.z) {
                chunks.push(across);
            }
        }
        for chunk in chunks {
            self.remesh(scene, chunk, [true, false], gpu)?;
        }
        if (before < SEA) != (after < SEA) {
            let sea = IVec3::new(chunk.x, (SEA - 1).div_euclid(CHUNK), chunk.z);
            self.remesh(scene, sea, [false, true], gpu)?;
        }
        Ok(())
    }

    /// Measured frame `frame`'s edits: block edits, a wave of remeshes
    /// every 16 frames, the torch frame halfway, and with `water_remesh`
    /// every water chunk.
    fn edit(
        &mut self,
        scene: &mut Scene,
        frame: usize,
        gpu: (&wgpu::Device, &wgpu::Queue),
    ) -> Result<(), Box<dyn Error>> {
        let centre = Self::chunk_of(self.eye);
        let near: Vec<IVec3> = self
            .resident
            .iter()
            .filter(|(chunk, resident)| {
                resident.terrain.is_some() && (**chunk - centre).abs().max_element() <= 2
            })
            .map(|(chunk, _)| *chunk)
            .collect();
        if self.run.edits && !near.is_empty() {
            self.edits_owed += EDITS_PER_SECOND * DT;
            while self.edits_owed >= 1. {
                self.edits_owed -= 1.;
                let roll = hash(frame as u64 * 977 + self.edits_owed.to_bits());
                let base = near[roll as usize % near.len()] * CHUNK;
                let column = (
                    base.x + ((roll >> 20) % CHUNK as u64) as i32,
                    base.z + ((roll >> 28) % CHUNK as u64) as i32,
                );
                let change = if roll >> 40 & 1 == 0 { 1 } else { -1 };
                self.edit_column(scene, column, change, gpu)?;
            }
            if frame.is_multiple_of(WAVE_FRAMES) {
                let count = 5 + hash(frame as u64) as usize % 16;
                for index in 0..count.min(near.len()) {
                    let chunk = near[(hash(frame as u64 + index as u64) as usize) % near.len()];
                    self.remesh(scene, chunk, [true, false], gpu)?;
                }
            }
            if frame == self.run.frames / 2 {
                // A torch placed ahead of the eye: its light, and the 27
                // chunks about it remeshed for the light the game bakes into
                // their shading.
                let (x, z) = (self.eye.x.floor() as i32 + 3, self.eye.z.floor() as i32);
                let at = IVec3::new(x, self.world.height(x, z) + 1, z);
                let chunk = at.div_euclid(IVec3::splat(CHUNK));
                if self.resident.contains_key(&chunk) {
                    let light = self.torch(scene, at, true, gpu)?;
                    let resident = self.resident.get_mut(&chunk).unwrap();
                    resident.lights.push(light);
                    resident.shadowed = true;
                }
                for dy in -1..=1 {
                    for dz in -1..=1 {
                        for dx in -1..=1 {
                            let about = chunk + IVec3::new(dx, dy, dz);
                            self.remesh(scene, about, [true, false], gpu)?;
                        }
                    }
                }
            }
        }
        if self.run.water_remesh {
            let water: Vec<IVec3> = self
                .resident
                .iter()
                .filter(|(_, resident)| resident.water.is_some())
                .map(|(chunk, _)| *chunk)
                .collect();
            for chunk in water {
                self.remesh(scene, chunk, [false, true], gpu)?;
            }
        }
        Ok(())
    }

    /// Moves the render origin with the eye.
    fn follow(
        &mut self,
        scene: &mut Scene,
        (device, queue): (&wgpu::Device, &wgpu::Queue),
    ) -> Result<(), Box<dyn Error>> {
        let origin = self.origin_for(self.eye);
        if origin == self.origin {
            return Ok(());
        }
        let to = (origin - self.origin).as_vec3();
        let instances = self
            .resident
            .values()
            .map(|r| usize::from(r.terrain.is_some()) + usize::from(r.water.is_some()))
            .sum::<usize>()
            + self.creatures.len();
        self.measured
            .operations
            .time("move_origin", instances.next_power_of_two(), || {
                scene.move_origin(device, queue, to)
            })?;
        self.origin = origin;
        self.measured.moves += 1;
        Ok(())
    }

    /// Poses the creatures for the frame.
    fn creatures(&mut self, scene: &mut Scene, queue: &wgpu::Queue) -> Result<(), Box<dyn Error>> {
        for index in 0..self.creatures.len() {
            let state = InstanceState {
                pose: self.creature_pose(index),
                ..InstanceState::new(self.creature)
            };
            let creature = self.creatures[index];
            self.measured.operations.time("set_instance", 1, || {
                scene.set_instance(queue, creature, state)
            })?;
        }
        Ok(())
    }

    /// The frame's camera, looking ahead along +x and a little down.
    fn camera(&self) -> Camera {
        let eye = self.render(self.eye);
        Camera {
            eye,
            view: camera::rh::view::look_to_mat4(eye, Vec3::new(1., -0.25, 0.2), Vec3::Y),
            projection: sgl_3d::perspective(
                70f32.to_radians(),
                SIZE[0] as f32 / SIZE[1] as f32,
                0.1,
            ),
        }
    }

    fn input(&self) -> FrameInput {
        let mut input = FrameInput::new(self.camera());
        input.elapsed_seconds = self.seconds;
        input.environment = Some(self.sky);
        input.exposure = Exposure {
            stops: 0.,
            automatic: None,
        };
        input.directional_lights[0] = Some(DirectionalLight {
            direction: Vec3::new(-0.4, -1., -0.3),
            color: [1., 0.95, 0.85],
            illuminance: 3.,
            shadow: Some(DirectionalShadow {
                distance: self.run.shadow_distance,
                cascades: 4,
            }),
            ..Default::default()
        });
        input
    }
}

/// The quads of `meshes`.
fn quads(meshes: &[CpuMesh]) -> usize {
    meshes.iter().map(|mesh| mesh.indices.len() / 6).sum()
}

impl Game {
    /// The terrain meshes the run gives the scene for a chunk's `blocks`:
    /// one atlas mesh, or each block's own.
    fn terrain_meshes(&self, blocks: Vec<CpuMesh>) -> Vec<CpuMesh> {
        if self.run.per_block {
            blocks
        } else {
            vec![atlas_mesh(blocks)]
        }
    }

    /// `meshes` with their materials, the water's or the terrain's by
    /// index, without the empty ones.
    fn model_meshes(&self, meshes: Vec<CpuMesh>, water: bool) -> Vec<ModelMesh> {
        meshes
            .into_iter()
            .filter(|mesh| !mesh.indices.is_empty())
            .map(|mesh| ModelMesh {
                material: if water {
                    self.water
                } else {
                    self.terrain[mesh.material]
                },
                vertices: mesh.vertices,
                indices: mesh.indices,
                deformation: Default::default(),
            })
            .collect()
    }
}

fn settings() -> Settings {
    Settings {
        scene_resolution: sgl_3d::settings::SceneResolution::Full,
        atmosphere: false,
        antialiasing: Antialiasing::Taa,
        screen_space_reflections: ScreenSpaceReflections::Full,
        reflection_method: ReflectionMethod::Crystal,
        world_space_reflections: true,
        ..Settings::default()
    }
}

/// The frame's output texture, which the last frame is read back from.
fn output(device: &wgpu::Device) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("streaming example output"),
        size: wgpu::Extent3d {
            width: SIZE[0],
            height: SIZE[1],
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&Default::default());
    (texture, view)
}

/// Whether the mesher has finished every chunk of the window and the scene
/// holds them all.
fn streamed_in(game: &Game) -> bool {
    !game.resident.is_empty() && game.requested.is_empty() && game.meshed.is_empty()
}

/// Renders `run`: streams its first window in, then measures `run.frames`
/// frames and writes the last to `directory`.
fn render(
    run: &Run,
    gpu: (&wgpu::Device, &wgpu::Queue),
    directory: &Path,
) -> Result<Game, Box<dyn Error>> {
    let (device, queue) = gpu;
    let settings = settings();
    let mut scene = Scene::new(device, queue);
    let mut game = Game::new(run, &mut scene, gpu)?;
    let (texture, output) = output(device);
    let mut renderer = Renderer::new(
        device,
        queue,
        wgpu::TextureFormat::Rgba8UnormSrgb,
        SIZE,
        1.,
        &settings,
    )?;
    let mut timing = GpuTiming::new(device, queue);
    let mut in_flight = None;
    // The loop index of the first measured frame, and the counters then.
    let mut from = None;
    let mut counted = Counters::default();
    for index in 0.. {
        if from.is_none() && streamed_in(&game) {
            from = Some(index);
            game.measured = Measured {
                from: index,
                ..Measured::default()
            };
            counted = sgl_3d::diagnostics::counters();
        }
        let step = from.map(|from| index - from);
        if step == Some(run.frames) {
            break;
        }
        if index == STREAM_IN_LIMIT && from.is_none() {
            return Err(format!("{}'s window did not stream in", run.name).into());
        }
        game.seconds += DT;
        if let Some(step) = step {
            let progress = step as f64 / run.frames as f64;
            let speed = run.speed[0] + (run.speed[1] - run.speed[0]) * progress;
            game.walk(game.eye.x + speed * DT, speed);
        } else {
            game.walk(game.eye.x, run.speed[0]);
        }
        let before = sgl_3d::diagnostics::counters();
        game.follow(&mut scene, gpu)?;
        game.stream(&mut scene)?;
        let given = game.arrive(&mut scene, gpu)?;
        if let Some(step) = step {
            game.edit(&mut scene, step, gpu)?;
        }
        game.creatures(&mut scene, queue)?;
        let scene_calls = game.measured.operations.take_frame();
        let meshing = std::mem::take(&mut game.meshing_ms);
        let mut input = game.input();
        input.camera_cut = index == 0;
        if let Some(timing) = &mut timing {
            for done in timing.begin_frame(device, queue) {
                game.measured.gpu(done);
            }
        }
        let mut encoder = device.create_command_encoder(&Default::default());
        let started = Instant::now();
        renderer.render(
            device,
            queue,
            &mut encoder,
            &mut scene,
            &input,
            &settings,
            &output,
            timing.as_ref(),
        );
        let commands = encoder.finish();
        let recording = started.elapsed().as_secs_f64() * 1e3;
        let submission = queue.submit([commands]);
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
        if step.is_some() {
            let frame = sgl_3d::diagnostics::counters().since(&before);
            let m = &mut game.measured;
            m.recording.push(recording);
            m.scene_calls.push(scene_calls);
            m.meshing.push(meshing);
            m.given_bytes.push(given as f64);
            m.uploaded_bytes.push(frame.uploaded_bytes() as f64);
            m.buffers_created.push(frame.buffers_created as f64);
            m.draws.push(renderer.diagnostic_draws());
            m.triangles.push(renderer.geometry_stats().total().1 as f64);
            let shadows = renderer.local_shadow_stats();
            m.faces.push(shadows.faces_drawn as f64);
            m.layers.push(shadows.layers_drawn as f64);
            m.shadow_draws.push(shadows.draws as f64);
            m.resources.push(scene.diagnostic_resources());
            m.quads
                .push(game.resident.values().map(|r| r.quads).sum::<usize>() as f64);
            m.resident.push(
                game.resident
                    .values()
                    .filter(|r| r.terrain.is_some() || r.water.is_some())
                    .count() as f64,
            );
        }
    }
    game.measured.counted = sgl_3d::diagnostics::counters().since(&counted);
    if let Some(timing) = &mut timing {
        for _ in 0..3 {
            let _ = device.poll(wgpu::PollType::wait_indefinitely());
            for done in timing.begin_frame(device, queue) {
                game.measured.gpu(done);
            }
        }
    }
    game.measured.lights = game.resident.values().map(|r| r.lights.len()).sum();
    let pixels = sgl_3d::diagnostics::read(device, queue, &texture, 4);
    image::save_buffer(
        directory.join(format!("{}.png", run.name)),
        &pixels,
        SIZE[0],
        SIZE[1],
        image::ColorType::Rgba8,
    )?;
    Ok(game)
}

/// The largest motion, in UV, the last frame's motion target holds.
fn largest_motion(
    renderer: &Renderer,
    (device, queue): (&wgpu::Device, &wgpu::Queue),
) -> Result<f32, Box<dyn Error>> {
    let motion = renderer
        .diagnostic_target(DiagnosticTarget::Motion)
        .ok_or("no motion target")?;
    let bytes = sgl_3d::diagnostics::read(device, queue, motion.texture(), 4);
    Ok(bytes
        .chunks_exact(2)
        .map(|half| sgl_3d::diagnostics::half(half).abs())
        .fold(0., f32::max))
}

/// `--check`: a still camera across render origin moves of one chunk and
/// of 256 m, then a remesh of the chunks holding shadowed torches, each
/// followed by an abandoned frame and then a submitted one.
fn check(gpu: (&wgpu::Device, &wgpu::Queue)) -> Result<(), Box<dyn Error>> {
    let (device, queue) = gpu;
    let run = Run {
        name: "check",
        speed: [0., 0.],
        radius: [3, 1],
        origin_cell: 1,
        shadow_distance: 150.,
        torches: 2,
        water_remesh: false,
        per_block: false,
        edits: false,
        frames: 0,
    };
    let settings = settings();
    let mut scene = Scene::new(device, queue);
    let mut game = Game::new(&run, &mut scene, gpu)?;
    // Creatures move; this check is about static content.
    for &creature in &game.creatures {
        let mut state = *scene.instance(creature)?;
        state.visible = false;
        state.capture_visible = false;
        scene.set_instance(queue, creature, state)?;
    }
    let (_, output) = output(device);
    let mut renderer = Renderer::new(
        device,
        queue,
        wgpu::TextureFormat::Rgba8UnormSrgb,
        SIZE,
        1.,
        &settings,
    )?;
    let frame = |renderer: &mut Renderer, scene: &mut Scene, game: &Game, submit: bool| {
        let mut encoder = device.create_command_encoder(&Default::default());
        renderer.render(
            device,
            queue,
            &mut encoder,
            scene,
            &game.input(),
            &settings,
            &output,
            None,
        );
        if submit {
            queue.submit([encoder.finish()]);
            renderer.finish_frame(scene);
        }
    };
    // Stream the window in, then let every cache settle.
    let mut settled = 0;
    for _ in 0..STREAM_IN_LIMIT {
        game.stream(&mut scene)?;
        game.arrive(&mut scene, gpu)?;
        frame(&mut renderer, &mut scene, &game, true);
        settled = if streamed_in(&game) { settled + 1 } else { 0 };
        if settled == 30 {
            break;
        }
    }
    if settled < 30 {
        return Err("the check's window did not stream in".into());
    }
    for (label, by) in [("one chunk", CHUNK), ("256 m", 256)] {
        let to = IVec3::new(by, 0, -by);
        scene.move_origin(device, queue, to.as_vec3())?;
        game.origin += to;
        frame(&mut renderer, &mut scene, &game, false);
        frame(&mut renderer, &mut scene, &game, true);
        let motion = largest_motion(&renderer, gpu)?;
        let shadows = renderer.local_shadow_stats();
        println!(
            "origin moved by {label}: largest static motion {motion:e} UV, {} local shadow faces of {} lights redrawn",
            shadows.faces_drawn, shadows.shadowed
        );
        // 1e-4 UV is a fifth of a pixel across 1920.
        if motion > 1e-4 {
            return Err(format!("static content moved {motion} UV after the {label} move").into());
        }
        if shadows.shadowed == 0 {
            return Err("no shadowed torch is in view, so the check sees no local shadow".into());
        }
        if shadows.draws != 0 {
            return Err(format!("the {label} move redrew shadows: {shadows:?}").into());
        }
    }
    // A streaming edit: new geometry for every chunk holding a shadowed
    // torch, whose shadows the next submitted frame redraws once, though a
    // frame was abandoned between.
    let shadowed: Vec<IVec3> = game
        .resident
        .iter()
        .filter(|(_, resident)| resident.shadowed)
        .map(|(chunk, _)| *chunk)
        .collect();
    for chunk in shadowed {
        game.remesh(&mut scene, chunk, [true, false], gpu)?;
    }
    frame(&mut renderer, &mut scene, &game, false);
    frame(&mut renderer, &mut scene, &game, true);
    let edited = renderer.local_shadow_stats();
    frame(&mut renderer, &mut scene, &game, true);
    let next = renderer.local_shadow_stats();
    println!(
        "remeshed chunks holding shadowed torches: {} static shadow layers redrawn, then {}",
        edited.layers_drawn, next.layers_drawn
    );
    if edited.layers_drawn == 0 {
        return Err(format!("the remesh redrew no static shadow layer: {edited:?}").into());
    }
    if next.layers_drawn != 0 {
        return Err(format!("the frame after the remesh redrew shadows: {next:?}").into());
    }
    println!("check passed");
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut frames = 600;
    let mut names = Vec::new();
    let mut check_only = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--frames" => frames = args.next().ok_or("--frames requires a count")?.parse()?,
            "--check" => check_only = true,
            name => names.push(name.to_owned()),
        }
    }
    if frames < 60 {
        return Err("--frames must be at least 60".into());
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
    if check_only {
        return check(gpu);
    }
    let all = runs(frames);
    let chosen: Vec<&Run> = if names.is_empty() {
        all.iter().collect()
    } else {
        names
            .iter()
            .map(|name| {
                all.iter()
                    .find(|run| run.name == name)
                    .ok_or(format!("unknown run {name}"))
            })
            .collect::<Result<_, _>>()?
    };
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/streaming-example");
    std::fs::create_dir_all(&directory)?;
    let directory = directory.canonicalize()?;
    println!(
        "{frames} frames a run at {}x{}, measured once the first window streamed in; median / p95; last frames in {}",
        SIZE[0],
        SIZE[1],
        directory.display()
    );
    for run in chosen {
        let game = render(run, gpu, &directory)?;
        game.measured.report(run);
    }
    Ok(())
}
