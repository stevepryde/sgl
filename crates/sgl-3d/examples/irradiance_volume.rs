//! The irradiance volume at a block game's scale: a world of 1 m blocks in
//! 16 m chunks, a path across open ground to a cliff and a tunnel into it
//! that winds to a chamber, lit by a sky, a sun with cascaded shadows and a
//! field the game computes as a block game does, sky light and block light
//! propagated cell by cell, 0 to 15. The game writes the field into an
//! irradiance volume of 160 × 128 × 160 cells about the camera (the
//! consumer's dense volume, 157 MB) and keeps it up as a game would: a
//! torch on the tunnel wall placed and taken away relights the 27 chunks
//! about it as one region write, the volume scrolls a chunk at a time as
//! the camera walks, the game writing only the chunks that enter, and the
//! render origin follows the camera. Eight creatures, moving instances,
//! walk in and out of the cave mouth through the field.
//!
//! `cargo run --release -p sgl-3d --example irradiance_volume [-- --night]
//! [--frames N] [--split]`
//!
//! It renders the walk twice at 1920×1080 with TAA, screen-space and
//! world-space reflections and shadows: with the volume, and with the
//! frame's ambient alone, as before the volume (a cave lit by the open
//! sky). It writes frames along the walk to
//! `target/irradiance-volume-example/`, then prints the GPU time of each
//! pass group in both runs, median / p95 over the frames after the first
//! 60, and what keeping the volume up costs, median / p95 over repeated
//! updates: for a torch's relight, the volume's whole install and a
//! scroll, the game's time turning its light levels into cells, the
//! preparation of the region (on worker threads), the write on the thread
//! that renders, and the wall-clock time from the write until an otherwise
//! idle GPU has finished it (uploads and copies are not passes, so they
//! have no pass timestamps). `--night` turns the sun off and dims the sky,
//! with no cell rewritten. Printed numbers are diagnostics, not image QA.
//!
//! The walk with the volume then prints what its views' draw lists cost
//! inside the cave, over the frames after the warm-up with the camera past
//! the cliff (`support/culling.rs`): the CPU time each GPU-built view's draw
//! list takes to build and record, and each pass group's GPU time.
//! `--split` renders the opaque stage's two-pass form instead of its fused
//! pass.
//!
//! The light field and its cells are the game's: each air cell's face
//! toward a side takes the levels of the air cell on that side (or its own
//! where that side is solid), so a wall facing the cave mouth takes the
//! brighter light from the mouth; a level's sky visibility and torch light
//! fall by a fifth a level; and a solid cell takes the mean of its air
//! neighbours', so the volume's filter blends no darkness from rock into
//! the faces about it.
use sgl_3d::glam::{IVec3, Mat4, Vec3, camera};
use sgl_3d::{
    Backdrop, Camera, DirectionalLight, DirectionalShadow, EnvironmentId, Exposure, FrameInput,
    HemisphereLight, InstanceId, InstanceState, IrradianceCell, IrradianceVolume, MaterialId,
    Mobility, ModelId, ModelMesh, PreparedIrradianceRegion, PreparedModel, Renderer, Scene,
    asset::{CpuMesh, Image, Material, Vertex},
    environment::{EnvironmentMap, PmremAtlas},
    settings::{
        Antialiasing, ReflectionMethod, SceneResolution, ScreenSpaceReflections, Settings,
        WorldSpaceReflections,
    },
    static_lighting::AmbientCube,
    timing::{FrameTime, GpuTiming},
};
use std::collections::VecDeque;
use std::error::Error;
use std::path::{Path, PathBuf};
use std::time::Instant;

#[path = "support/culling.rs"]
mod culling;

const SIZE: [u32; 2] = [1920, 1080];
const CHUNK: i32 = 16;
/// The world's least block and its size in blocks.
const WORLD_MIN: IVec3 = IVec3::new(-80, 0, -80);
const WORLD_SIZE: IVec3 = IVec3::new(160, 128, 240);
/// The volume, in chunks of 1 m cells: the consumer's dense volume.
const VOLUME_CHUNKS: IVec3 = IVec3::new(10, 8, 10);
/// The walking surface outside and in the tunnel: the bottom of block 24.
const FLOOR: i32 = 24;
/// Where the hill's face stands and the tunnel opens, and where the tunnel
/// reaches its chamber.
const CLIFF: i32 = 40;
const TUNNEL_END: i32 = 104;
/// The torch's place along the tunnel.
const TORCH_Z: i32 = 72;
const MAX_LIGHT: u8 = 15;
const TORCH_LIGHT: u8 = 14;
/// A torch's light in its own cell, irradiance / PI.
const TORCH: [f32; 3] = [1.2, 0.75, 0.36];
/// The share of light each level keeps of the one above it.
const FALLOFF: f32 = 0.8;
const DT: f64 = 1. / 60.;
/// The camera's walking speed, in metres a second.
const WALK: f32 = 6.;
/// Frames before the GPU times count.
const WARM_UP: u64 = 60;
/// Frames at which the torch is placed (true) or taken away.
const TORCH_EVENTS: [(usize, bool); 3] = [(360, true), (540, false), (630, true)];
/// Frames written to images, and their names: walking up to the cliff,
/// at the cave mouth before the torch is lit and inside it with the torch
/// lit deep in the tunnel, and in the tunnel short of the torch. Beyond the
/// sky's 15 cells and the torch's 14 the tunnel is black, as the field has
/// it.
const CAPTURES: [(usize, &str); 4] = [
    (200, "approach"),
    (330, "mouth-dark"),
    (420, "mouth"),
    (660, "tunnel-torch"),
];
const CREATURES: usize = 8;
/// Repeated updates measured after the walk.
const REPEATS: usize = 12;
/// The faces of a cell in the ambient cube's order: +X, -X, +Y, -Y, +Z, -Z.
const FACES: [IVec3; 6] = [
    IVec3::X,
    IVec3::NEG_X,
    IVec3::Y,
    IVec3::NEG_Y,
    IVec3::Z,
    IVec3::NEG_Z,
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Block {
    Air,
    Grass,
    Dirt,
    Stone,
}

/// The tunnel's centre across x at `z`.
fn tunnel_centre(z: f32) -> f32 {
    5. * ((z - CLIFF as f32) * 0.06).sin()
}

/// Where the camera and the creatures walk across x at `z`.
fn path_x(z: f32) -> f32 {
    if z < CLIFF as f32 {
        0.
    } else {
        tunnel_centre(z.min(TUNNEL_END as f32))
    }
}

/// The top block of column (x, z): open ground about a flat path, then a
/// hill whose face stands at `CLIFF`.
fn ground(x: i32, z: i32) -> i32 {
    let (fx, fz) = (x as f32, z as f32);
    if z < CLIFF {
        let away = ((fx.abs() - 6.) / 10.).clamp(0., 1.);
        FLOOR - 1 + (away * 3. * (fx * 0.15).sin() * (fz * 0.11).cos()).round() as i32
    } else {
        50 + (4. * (fx * 0.09).sin() * (fz * 0.07).cos()).round() as i32
    }
}

/// The world's blocks and the game's light levels in each.
struct World {
    blocks: Vec<Block>,
    sky: Vec<u8>,
    torch: Vec<u8>,
    torches: Vec<IVec3>,
}

impl World {
    fn index(at: IVec3) -> Option<usize> {
        let local = at - WORLD_MIN;
        (local.cmpge(IVec3::ZERO).all() && local.cmplt(WORLD_SIZE).all())
            .then(|| (local.x + WORLD_SIZE.x * (local.y + WORLD_SIZE.y * local.z)) as usize)
    }

    fn position(index: usize) -> IVec3 {
        let index = index as i32;
        let x = index % WORLD_SIZE.x;
        let y = index / WORLD_SIZE.x % WORLD_SIZE.y;
        let z = index / (WORLD_SIZE.x * WORLD_SIZE.y);
        WORLD_MIN + IVec3::new(x, y, z)
    }

    /// Whether the block at `at` is solid; beyond the world's sides and
    /// floor everything is, above its top nothing is.
    fn solid(&self, at: IVec3) -> bool {
        match Self::index(at) {
            Some(index) => self.blocks[index] != Block::Air,
            None => at.y < WORLD_MIN.y + WORLD_SIZE.y,
        }
    }

    fn new() -> Self {
        let count = (WORLD_SIZE.x * WORLD_SIZE.y * WORLD_SIZE.z) as usize;
        let mut world = Self {
            blocks: vec![Block::Air; count],
            sky: vec![0; count],
            torch: vec![0; count],
            torches: Vec::new(),
        };
        for z in WORLD_MIN.z..WORLD_MIN.z + WORLD_SIZE.z {
            for x in WORLD_MIN.x..WORLD_MIN.x + WORLD_SIZE.x {
                let top = ground(x, z);
                for y in WORLD_MIN.y..=top {
                    let block = if y == top {
                        Block::Grass
                    } else if y > top - 4 {
                        Block::Dirt
                    } else {
                        Block::Stone
                    };
                    if let Some(index) = Self::index(IVec3::new(x, y, z)) {
                        world.blocks[index] = block;
                    }
                }
            }
        }
        // The tunnel, six blocks high and seven wide, and the chamber.
        let mut carve = |x: i32, y: i32, z: i32| {
            if let Some(index) = Self::index(IVec3::new(x, y, z)) {
                world.blocks[index] = Block::Air;
            }
        };
        for z in CLIFF..TUNNEL_END + 16 {
            let (half, height) = if z < TUNNEL_END { (3.5, 6) } else { (8.5, 10) };
            let centre = tunnel_centre((z.min(TUNNEL_END) as f32) + 0.5);
            for x in WORLD_MIN.x..WORLD_MIN.x + WORLD_SIZE.x {
                if (x as f32 + 0.5 - centre).abs() <= half {
                    for y in FLOOR..FLOOR + height {
                        carve(x, y, z);
                    }
                }
            }
        }
        world.light_sky();
        world
    }

    /// The air cell on the tunnel's +x wall, a block above the floor, at
    /// `TORCH_Z`.
    fn torch_cell(&self) -> IVec3 {
        let mut at = IVec3::new(
            tunnel_centre(TORCH_Z as f32 + 0.5) as i32,
            FLOOR + 1,
            TORCH_Z,
        );
        while !self.solid(at + IVec3::X) {
            at.x += 1;
        }
        at
    }

    /// Spreads `levels` from the cells of `queue` through air, one less a
    /// cell.
    fn spread(blocks: &[Block], levels: &mut [u8], mut queue: VecDeque<usize>) {
        while let Some(index) = queue.pop_front() {
            let level = levels[index];
            if level <= 1 {
                continue;
            }
            let at = Self::position(index);
            for face in FACES {
                if let Some(next) = Self::index(at + face)
                    && blocks[next] == Block::Air
                    && levels[next] < level - 1
                {
                    levels[next] = level - 1;
                    queue.push_back(next);
                }
            }
        }
    }

    /// Sky light: the most in every cell open to the sky above, spread from
    /// there.
    fn light_sky(&mut self) {
        let mut queue = VecDeque::new();
        for z in WORLD_MIN.z..WORLD_MIN.z + WORLD_SIZE.z {
            for x in WORLD_MIN.x..WORLD_MIN.x + WORLD_SIZE.x {
                for y in (WORLD_MIN.y..WORLD_MIN.y + WORLD_SIZE.y).rev() {
                    let index = Self::index(IVec3::new(x, y, z)).unwrap();
                    if self.blocks[index] != Block::Air {
                        break;
                    }
                    self.sky[index] = MAX_LIGHT;
                    queue.push_back(index);
                }
            }
        }
        Self::spread(&self.blocks, &mut self.sky, queue);
    }

    /// Block light from the torches.
    fn light_torches(&mut self) {
        self.torch.fill(0);
        let mut queue = VecDeque::new();
        for &torch in &self.torches {
            let index = Self::index(torch).unwrap();
            self.torch[index] = TORCH_LIGHT;
            queue.push_back(index);
        }
        Self::spread(&self.blocks, &mut self.torch, queue);
    }

    /// An air cell: each face the levels of the air cell on its side, or
    /// its own where that side is solid.
    fn air_cell(&self, at: IVec3, index: usize, curves: &Curves) -> IrradianceCell {
        let mut cell = IrradianceCell {
            irradiance: AmbientCube::default(),
            sky_visibility: [0.; 6],
        };
        for (face, offset) in FACES.into_iter().enumerate() {
            let side = Self::index(at + offset)
                .filter(|&side| self.blocks[side] == Block::Air)
                .unwrap_or(index);
            cell.irradiance.irradiance[face] = curves.torch[usize::from(self.torch[side])];
            cell.sky_visibility[face] = curves.sky[usize::from(self.sky[side])];
        }
        cell
    }

    /// The cell at `at`: an air cell's, or the mean of a solid one's air
    /// neighbours' (none: no light, no sky).
    fn cell(&self, at: IVec3, curves: &Curves) -> IrradianceCell {
        let Some(index) = Self::index(at) else {
            return IrradianceCell::default();
        };
        if self.blocks[index] == Block::Air {
            return self.air_cell(at, index, curves);
        }
        let mut sum = IrradianceCell {
            irradiance: AmbientCube::default(),
            sky_visibility: [0.; 6],
        };
        let mut count = 0.;
        for offset in FACES {
            let Some(side) = Self::index(at + offset) else {
                continue;
            };
            if self.blocks[side] != Block::Air {
                continue;
            }
            let neighbour = self.air_cell(at + offset, side, curves);
            for face in 0..6 {
                for channel in 0..3 {
                    sum.irradiance.irradiance[face][channel] +=
                        neighbour.irradiance.irradiance[face][channel];
                }
                sum.sky_visibility[face] += neighbour.sky_visibility[face];
            }
            count += 1.;
        }
        if count > 0. {
            for face in 0..6 {
                sum.irradiance.irradiance[face] =
                    sum.irradiance.irradiance[face].map(|v| v / count);
                sum.sky_visibility[face] /= count;
            }
        }
        sum
    }

    /// The cells of the box of `size` cells from `min`, x fastest, then y,
    /// then z.
    fn cells(&self, min: IVec3, size: IVec3, curves: &Curves) -> Vec<IrradianceCell> {
        let mut cells = Vec::with_capacity((size.x * size.y * size.z) as usize);
        for z in 0..size.z {
            for y in 0..size.y {
                for x in 0..size.x {
                    cells.push(self.cell(min + IVec3::new(x, y, z), curves));
                }
            }
        }
        cells
    }
}

/// Light levels as the game shows them: a level's sky visibility and torch
/// light, each a fifth less a level below the most.
struct Curves {
    sky: [f32; 16],
    torch: [[f32; 3]; 16],
}

impl Curves {
    fn new() -> Self {
        Self {
            sky: std::array::from_fn(|level| {
                if level == 0 {
                    0.
                } else {
                    FALLOFF.powi(i32::from(MAX_LIGHT) - level as i32)
                }
            }),
            torch: std::array::from_fn(|level| {
                if level == 0 {
                    [0.; 3]
                } else {
                    let scale = FALLOFF.powi(i32::from(TORCH_LIGHT) - level as i32);
                    TORCH.map(|c| c * scale)
                }
            }),
        }
    }
}

/// A quad of `mesh` from `corner` along `u` and `v`, facing u × v, its
/// texture coordinates spanning 0 to 1.
fn quad(mesh: &mut CpuMesh, corner: Vec3, u: Vec3, v: Vec3) {
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
            color: [1.; 4],
        });
    }
    mesh.indices
        .extend([0, 1, 2, 0, 2, 3].map(|index| start + index));
}

/// Each face's edges from its corner, u × v along the face's normal.
const FACE_EDGES: [(Vec3, Vec3); 6] = [
    (Vec3::NEG_Z, Vec3::Y),
    (Vec3::Z, Vec3::Y),
    (Vec3::X, Vec3::NEG_Z),
    (Vec3::X, Vec3::Z),
    (Vec3::X, Vec3::Y),
    (Vec3::NEG_X, Vec3::Y),
];

fn empty_mesh() -> CpuMesh {
    CpuMesh {
        vertices: Vec::new(),
        indices: Vec::new(),
        material: 0,
        deformation: Default::default(),
    }
}

/// Chunk `chunk`'s exposed block faces in its own space: grass tops, dirt
/// and stone, any of them empty.
fn mesh_chunk(world: &World, chunk: IVec3) -> [CpuMesh; 3] {
    let mut meshes = [empty_mesh(), empty_mesh(), empty_mesh()];
    let base = chunk * CHUNK;
    for z in 0..CHUNK {
        for y in 0..CHUNK {
            for x in 0..CHUNK {
                let local = IVec3::new(x, y, z);
                let at = base + local;
                let Some(index) = World::index(at) else {
                    continue;
                };
                let block = world.blocks[index];
                if block == Block::Air {
                    continue;
                }
                for (face, offset) in FACES.into_iter().enumerate() {
                    if world.solid(at + offset) {
                        continue;
                    }
                    let (u, v) = FACE_EDGES[face];
                    let centre = local.as_vec3() + Vec3::splat(0.5) + offset.as_vec3() * 0.5;
                    let tile = match (block, face) {
                        (Block::Grass, 2) => 0,
                        (Block::Grass | Block::Dirt, _) => 1,
                        _ => 2,
                    };
                    quad(&mut meshes[tile], centre - (u + v) * 0.5, u, v);
                }
            }
        }
    }
    meshes
}

/// A box of `size` about `centre`.
fn cuboid(centre: Vec3, size: Vec3) -> CpuMesh {
    let mut mesh = empty_mesh();
    for (face, offset) in FACES.into_iter().enumerate() {
        let (u, v) = FACE_EDGES[face];
        let corner = centre + (offset.as_vec3() - u - v) * 0.5 * size;
        quad(&mut mesh, corner, u * size, v * size);
    }
    mesh
}

/// Block `tile`'s speckled 16-texel texture: grass, dirt or stone.
fn tile_texture(tile: usize) -> Image {
    const COLOURS: [[u8; 3]; 3] = [[96, 160, 64], [134, 96, 67], [128, 128, 132]];
    let [r, g, b] = COLOURS[tile];
    Image::Rgba8(image::RgbaImage::from_fn(16, 16, |x, y| {
        let speck = ((x * 7919 + y * 104_729) % 24) as u8;
        image::Rgba([r - speck.min(r), g - speck.min(g), b - speck.min(b), 255])
    }))
}

/// A uniform blue sky.
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

/// Median and 95th percentile.
fn median_p95(values: &[f64]) -> (f64, f64) {
    if values.is_empty() {
        return (f64::NAN, f64::NAN);
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let at = |q: f64| sorted[((sorted.len() - 1) as f64 * q).round() as usize];
    (at(0.5), at(0.95))
}

fn ms(since: Instant) -> f64 {
    since.elapsed().as_secs_f64() * 1e3
}

/// What one kind of update cost, each time it ran.
#[derive(Default)]
struct Update {
    cells: usize,
    /// The game turning its levels into cells.
    field: Vec<f64>,
    /// `PreparedIrradianceRegion::new`, on worker threads.
    prepare: Vec<f64>,
    /// The scene call on the thread that renders.
    write: Vec<f64>,
    /// From the scene call until an otherwise idle GPU finished it.
    complete: Vec<f64>,
}

impl Update {
    fn report(&self, name: &str) {
        let bytes = self.cells as f64 * 48. / 1e6;
        println!("{name}: {} cells, {bytes:.2} MB", self.cells);
        for (what, values) in [
            ("cells from light levels", &self.field),
            ("prepare (worker threads)", &self.prepare),
            ("scene call", &self.write),
            ("call to GPU done", &self.complete),
        ] {
            if values.is_empty() {
                continue;
            }
            let (median, p95) = median_p95(values);
            println!(
                "  {what:>26}: {median:8.2} / {p95:8.2} ms over {}",
                values.len()
            );
        }
    }
}

/// Every update measured.
#[derive(Default)]
struct Measured {
    relight: Update,
    install: Update,
    scroll: Update,
    entering: Update,
    /// An empty submission's completion, which every completion includes.
    idle: Vec<f64>,
}

/// Runs `call`, then waits until the GPU has finished it and everything
/// before it: the scene call's time and its completion's.
fn completed<R>(gpu: (&wgpu::Device, &wgpu::Queue), call: impl FnOnce() -> R) -> (R, f64, f64) {
    let (device, queue) = gpu;
    let _ = device.poll(wgpu::PollType::wait_indefinitely());
    let start = Instant::now();
    let result = call();
    let called = ms(start);
    queue.submit([]);
    let _ = device.poll(wgpu::PollType::wait_indefinitely());
    (result, called, ms(start))
}

/// A region of the world's cells from `min` of `size`, built and prepared
/// on a worker thread: the region, and the times building and preparing it
/// took. The example waits for it so that it can time the write that
/// follows; a game sends the region back from its worker and writes it in
/// a later frame, so the thread that renders never waits on the packing.
fn prepare(
    world: &World,
    curves: &Curves,
    min: IVec3,
    size: IVec3,
    corner: Vec3,
) -> (PreparedIrradianceRegion, f64, f64) {
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                let start = Instant::now();
                let cells = world.cells(min, size, curves);
                let field = ms(start);
                let start = Instant::now();
                let region =
                    PreparedIrradianceRegion::new(corner, size.as_uvec3().to_array(), &cells)
                        .expect("valid cells");
                (region, field, ms(start))
            })
            .join()
            .unwrap()
    })
}

/// The game: its world and light, the scene's content, the camera and the
/// volume about it.
struct Game {
    world: World,
    curves: Curves,
    sky: EnvironmentId,
    creature: ModelId,
    creatures: Vec<InstanceId>,
    torch_model: ModelId,
    torch: Option<InstanceId>,
    torch_cell: IVec3,
    /// The render origin, in the world, and the volume's least cell.
    origin: IVec3,
    volume: Option<IVec3>,
    with_volume: bool,
    night: bool,
    measured: Measured,
}

impl Game {
    fn new(
        scene: &mut Scene,
        (device, queue): (&wgpu::Device, &wgpu::Queue),
        with_volume: bool,
        night: bool,
    ) -> Result<Self, Box<dyn Error>> {
        let world = World::new();
        let block = |tile: usize| Material {
            name: ["grass", "dirt", "stone"][tile].into(),
            base_texture: Some(tile),
            metallic: 0.,
            roughness: 0.9,
            ..Default::default()
        };
        let terrain = scene.add_materials(
            device,
            queue,
            &[block(0), block(1), block(2)],
            &[tile_texture(0), tile_texture(1), tile_texture(2)],
        )?;
        let materials = scene.add_materials(
            device,
            queue,
            &[
                Material {
                    name: "creature".into(),
                    base: [0.7, 0.35, 0.25, 1.],
                    metallic: 0.,
                    roughness: 0.5,
                    ..Default::default()
                },
                Material {
                    name: "torch".into(),
                    base: [0.3, 0.2, 0.1, 1.],
                    emissive: [6., 3.6, 1.4],
                    metallic: 0.,
                    roughness: 0.8,
                    ..Default::default()
                },
            ],
            &[],
        )?;
        let model = |mesh: CpuMesh, material: MaterialId| ModelMesh {
            vertices: mesh.vertices,
            indices: mesh.indices,
            material,
            deformation: Default::default(),
        };
        // Each chunk with faces, a static instance at its integer origin.
        let first = WORLD_MIN / CHUNK;
        let last = (WORLD_MIN + WORLD_SIZE) / CHUNK;
        for z in first.z..last.z {
            for y in first.y..last.y {
                for x in first.x..last.x {
                    let chunk = IVec3::new(x, y, z);
                    let meshes: Vec<ModelMesh> = mesh_chunk(&world, chunk)
                        .into_iter()
                        .zip(&terrain)
                        .filter(|(mesh, _)| !mesh.indices.is_empty())
                        .map(|(mesh, &material)| model(mesh, material))
                        .collect();
                    if meshes.is_empty() {
                        continue;
                    }
                    let id = scene.add_model(device, queue, PreparedModel::new(meshes)?)?;
                    scene.add_instance(
                        device,
                        queue,
                        InstanceState {
                            pose: Mat4::from_translation((chunk * CHUNK).as_vec3()),
                            ..InstanceState::new(id)
                        },
                        Mobility::Static,
                    )?;
                }
            }
        }
        let creature = scene.add_model(
            device,
            queue,
            PreparedModel::new(vec![model(
                cuboid(Vec3::Y * 0.75, Vec3::new(0.6, 1.5, 0.6)),
                materials[0],
            )])?,
        )?;
        let creatures = (0..CREATURES)
            .map(|_| {
                scene.add_instance(
                    device,
                    queue,
                    InstanceState::new(creature),
                    Mobility::Moving,
                )
            })
            .collect::<Result<_, _>>()?;
        let torch_cell = world.torch_cell();
        let torch_model = scene.add_model(
            device,
            queue,
            PreparedModel::new(vec![model(
                cuboid(Vec3::new(0.85, 0.5, 0.5), Vec3::new(0.16, 0.6, 0.16)),
                materials[1],
            )])?,
        )?;
        let sky = scene.add_environment(device, queue, &sky())?;
        Ok(Self {
            world,
            curves: Curves::new(),
            sky,
            creature,
            creatures,
            torch_model,
            torch: None,
            torch_cell,
            origin: IVec3::ZERO,
            volume: None,
            with_volume,
            night,
            measured: Measured::default(),
        })
    }

    /// The camera's eye in the world at `seconds`.
    fn eye(seconds: f64) -> Vec3 {
        let z = WALK * seconds as f32;
        Vec3::new(path_x(z), FLOOR as f32 + 1.7, z)
    }

    /// The volume's least cell about a camera at `eye`: centred on its
    /// chunk across x and z, within the world.
    fn volume_for(eye: Vec3) -> IVec3 {
        let size = VOLUME_CHUNKS * CHUNK;
        let chunk = (eye.z / CHUNK as f32).floor() as i32;
        let z = ((chunk - VOLUME_CHUNKS.z / 2) * CHUNK)
            .clamp(WORLD_MIN.z, WORLD_MIN.z + WORLD_SIZE.z - size.z);
        IVec3::new(WORLD_MIN.x, WORLD_MIN.y, z)
    }

    /// `at`, a position in the world, in the render frame.
    fn render(&self, at: IVec3) -> Vec3 {
        (at - self.origin).as_vec3()
    }

    fn placement(&self, min: IVec3) -> IrradianceVolume {
        IrradianceVolume {
            origin: self.render(min),
            cell_size: Vec3::ONE,
            cells: (VOLUME_CHUNKS * CHUNK).as_uvec3().to_array(),
        }
    }

    /// Installs the volume at `min` as a new placement and writes every
    /// cell, its slabs of a chunk along z built and prepared in parallel.
    fn install(
        &mut self,
        scene: &mut Scene,
        gpu: (&wgpu::Device, &wgpu::Queue),
        min: IVec3,
    ) -> Result<(), Box<dyn Error>> {
        let (device, queue) = gpu;
        scene.set_irradiance_volume(device, queue, None)?;
        scene.set_irradiance_volume(device, queue, Some(self.placement(min)))?;
        let size = VOLUME_CHUNKS * CHUNK;
        let slab = IVec3::new(size.x, size.y, CHUNK);
        let start = Instant::now();
        let slabs: Vec<(PreparedIrradianceRegion, f64, f64)> = std::thread::scope(|scope| {
            let (world, curves) = (&self.world, &self.curves);
            let handles: Vec<_> = (0..VOLUME_CHUNKS.z)
                .map(|index| {
                    let first = min + IVec3::Z * index * CHUNK;
                    let corner = self.render(first);
                    scope.spawn(move || {
                        let start = Instant::now();
                        let cells = world.cells(first, slab, curves);
                        let field = ms(start);
                        let start = Instant::now();
                        let region = PreparedIrradianceRegion::new(
                            corner,
                            slab.as_uvec3().to_array(),
                            &cells,
                        )
                        .expect("valid cells");
                        (region, field, ms(start))
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        let parallel = ms(start);
        let field: f64 = slabs.iter().map(|s| s.1).sum();
        let packing: f64 = slabs.iter().map(|s| s.2).sum();
        let ((), write, complete) = completed(gpu, || {
            for (region, _, _) in &slabs {
                scene
                    .write_irradiance_cells(queue, region)
                    .expect("on the lattice");
            }
        });
        let install = &mut self.measured.install;
        install.cells = (size.x * size.y * size.z) as usize;
        // Wall-clock over the threads, apportioned by their summed times.
        install.field.push(parallel * field / (field + packing));
        install.prepare.push(parallel * packing / (field + packing));
        install.write.push(write);
        install.complete.push(complete);
        self.volume = Some(min);
        Ok(())
    }

    /// Writes the cells of the 27 chunks about the torch, within the
    /// volume.
    fn relight(
        &mut self,
        scene: &mut Scene,
        gpu: (&wgpu::Device, &wgpu::Queue),
    ) -> Result<(), Box<dyn Error>> {
        self.world.light_torches();
        let Some(volume) = self.volume else {
            return Ok(());
        };
        let chunk = self.torch_cell.div_euclid(IVec3::splat(CHUNK));
        let min = ((chunk - 1) * CHUNK).max(volume);
        let max = ((chunk + 2) * CHUNK).min(volume + VOLUME_CHUNKS * CHUNK);
        let size = max - min;
        let (region, field, packing) =
            prepare(&self.world, &self.curves, min, size, self.render(min));
        let (result, write, complete) =
            completed(gpu, || scene.write_irradiance_cells(gpu.1, &region));
        result?;
        let relight = &mut self.measured.relight;
        relight.cells = (size.x * size.y * size.z) as usize;
        relight.field.push(field);
        relight.prepare.push(packing);
        relight.write.push(write);
        relight.complete.push(complete);
        Ok(())
    }

    /// Places or takes away the torch: its post, and with the volume its
    /// light.
    fn set_torch(
        &mut self,
        scene: &mut Scene,
        gpu: (&wgpu::Device, &wgpu::Queue),
        lit: bool,
    ) -> Result<(), Box<dyn Error>> {
        let (device, queue) = gpu;
        if let Some(post) = self.torch.take() {
            scene.remove_instance(post)?;
        }
        self.world.torches.clear();
        if lit {
            self.torch = Some(scene.add_instance(
                device,
                queue,
                InstanceState {
                    pose: Mat4::from_translation(self.render(self.torch_cell)),
                    ..InstanceState::new(self.torch_model)
                },
                Mobility::Static,
            )?);
            self.world.torches.push(self.torch_cell);
        }
        if self.with_volume {
            self.relight(scene, gpu)?;
        }
        Ok(())
    }

    /// Scrolls the volume to `min`, then writes the chunks that entered.
    fn scroll(
        &mut self,
        scene: &mut Scene,
        gpu: (&wgpu::Device, &wgpu::Queue),
        min: IVec3,
    ) -> Result<(), Box<dyn Error>> {
        let (device, queue) = gpu;
        let Some(from) = self.volume else {
            return Ok(());
        };
        let placement = self.placement(min);
        let (result, call, complete) = completed(gpu, || {
            scene.set_irradiance_volume(device, queue, Some(placement))
        });
        result?;
        let size = VOLUME_CHUNKS * CHUNK;
        let scroll = &mut self.measured.scroll;
        scroll.cells = (size.x * size.y * size.z) as usize;
        scroll.write.push(call);
        scroll.complete.push(complete);
        self.volume = Some(min);
        // The slab that entered: ahead along z, or behind.
        let by = min.z - from.z;
        let (first, depth) = if by > 0 {
            (min + IVec3::Z * (size.z - by), by)
        } else {
            (min, -by)
        };
        let slab = IVec3::new(size.x, size.y, depth.min(size.z));
        let (region, field, packing) =
            prepare(&self.world, &self.curves, first, slab, self.render(first));
        let (result, write, complete) =
            completed(gpu, || scene.write_irradiance_cells(queue, &region));
        result?;
        let entering = &mut self.measured.entering;
        entering.cells = (slab.x * slab.y * slab.z) as usize;
        entering.field.push(field);
        entering.prepare.push(packing);
        entering.write.push(write);
        entering.complete.push(complete);
        Ok(())
    }

    /// Moves the render origin with the camera, 32 m at a time, and the
    /// volume with it, a chunk at a time.
    fn follow(
        &mut self,
        scene: &mut Scene,
        gpu: (&wgpu::Device, &wgpu::Queue),
        eye: Vec3,
    ) -> Result<(), Box<dyn Error>> {
        let (device, queue) = gpu;
        let origin = IVec3::new(0, 0, (eye.z / 32.).floor() as i32 * 32);
        if origin != self.origin {
            scene.move_origin(device, queue, (origin - self.origin).as_vec3())?;
            self.origin = origin;
        }
        if !self.with_volume {
            return Ok(());
        }
        let min = Self::volume_for(eye);
        match self.volume {
            None => self.install(scene, gpu, min)?,
            Some(current) if current != min => self.scroll(scene, gpu, min)?,
            Some(_) => {}
        }
        Ok(())
    }

    /// Poses the creatures at `seconds`, each walking to and fro through
    /// the cave mouth.
    fn creatures(
        &self,
        scene: &mut Scene,
        queue: &wgpu::Queue,
        seconds: f64,
    ) -> Result<(), Box<dyn Error>> {
        for (index, &creature) in self.creatures.iter().enumerate() {
            let phase = seconds as f32 * 0.35 + index as f32 * 0.8;
            let z = 52. + 22. * phase.sin();
            let heading = if phase.cos() >= 0. {
                0.
            } else {
                std::f32::consts::PI
            };
            let x = path_x(z) + (index as f32 - 3.5) * 0.7;
            let world = Vec3::new(x, FLOOR as f32, z);
            let pose = Mat4::from_translation(world - self.origin.as_vec3())
                * Mat4::from_rotation_y(heading);
            scene.set_instance(
                queue,
                creature,
                InstanceState {
                    pose,
                    ..InstanceState::new(self.creature)
                },
            )?;
        }
        Ok(())
    }

    fn input(&self, seconds: f64) -> FrameInput {
        let eye = Self::eye(seconds);
        let ahead = Vec3::new(path_x(eye.z + 8.), eye.y - 0.4, eye.z + 8.);
        let origin = self.origin.as_vec3();
        let mut input = FrameInput::new(Camera {
            view: camera::rh::view::look_at_mat4(eye - origin, ahead - origin, Vec3::Y),
            projection: sgl_3d::perspective(
                70f32.to_radians(),
                SIZE[0] as f32 / SIZE[1] as f32,
                0.1,
            ),
            eye: eye - origin,
        });
        input.elapsed_seconds = seconds;
        input.environment = Some(self.sky);
        input.exposure = Exposure {
            stops: 0.,
            automatic: None,
        };
        input.hemisphere_light = HemisphereLight {
            sky_color: [0.3, 0.35, 0.45],
            ground_color: [0.1, 0.09, 0.07],
            intensity: 0.4,
        };
        if self.night {
            // Night: no sun and a dim sky, with no cell rewritten.
            input.diffuse_environment.intensity = 0.05;
            input.reflection_environment.intensity = 0.05;
            input.hemisphere_light.intensity *= 0.05;
            input.backdrop = Backdrop::Environment {
                yaw: 0.,
                brightness: 0.05,
            };
        } else {
            input.directional_lights[0] = Some(DirectionalLight {
                direction: Vec3::new(-0.35, -1., 0.6),
                color: [1., 0.95, 0.85],
                illuminance: 3.,
                shadow: Some(DirectionalShadow {
                    distance: 100.,
                    cascades: 4,
                }),
                ..Default::default()
            });
        }
        input
    }
}

fn settings() -> Settings {
    Settings {
        scene_resolution: SceneResolution::Full,
        atmosphere: false,
        antialiasing: Antialiasing::Taa,
        screen_space_reflections: ScreenSpaceReflections::Full,
        reflection_method: ReflectionMethod::Crystal,
        world_space_reflections: WorldSpaceReflections::Moving,
        ..Settings::default()
    }
}

fn output(device: &wgpu::Device) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("irradiance volume example output"),
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
    })
}

/// One walk: with the volume or the frame's ambient alone. Returns the GPU
/// time of each frame after the warm-up and the updates measured, after
/// `REPEATS` more of each with the volume.
fn walk(
    gpu: (&wgpu::Device, &wgpu::Queue),
    (with_volume, night): (bool, bool),
    frames: usize,
    directory: &Path,
    options: culling::Options,
) -> Result<(Vec<FrameTime>, Measured, culling::Culling), Box<dyn Error>> {
    let (device, queue) = gpu;
    let mut settings = settings();
    let mut culling = culling::Culling::new(options);
    let mut scene = Scene::new(device, queue);
    let mut game = Game::new(&mut scene, gpu, with_volume, night)?;
    let texture = output(device);
    let view = texture.create_view(&Default::default());
    let mut renderer = Renderer::new(device, queue, texture.format(), SIZE, 1., &settings)?;
    let mut timing = GpuTiming::new(device, queue);
    let mut times = Vec::new();
    let mut in_flight = None;
    let name = match (with_volume, night) {
        (true, false) => "volume",
        (false, false) => "ambient",
        (true, true) => "volume-night",
        (false, true) => "ambient-night",
    };
    for frame in 0..frames {
        let seconds = frame as f64 * DT;
        game.follow(&mut scene, gpu, Game::eye(seconds))?;
        if let Some(&(_, lit)) = TORCH_EVENTS.iter().find(|(at, _)| *at == frame) {
            game.set_torch(&mut scene, gpu, lit)?;
        }
        game.creatures(&mut scene, queue, seconds)?;
        let mut input = game.input(seconds);
        input.camera_cut = frame == 0;
        options.apply(&mut settings);
        if let Some(timing) = &mut timing {
            let done: Vec<_> = timing.begin_frame(device, queue).collect();
            done.iter().for_each(|done| culling.gpu(done));
            times.extend(done);
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
            &view,
            timing.as_ref(),
        );
        let rendered = ms(started);
        let commands = encoder.finish();
        let finished = ms(started) - rendered;
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
        // Inside the cave: past the cliff, after the warm-up.
        let inside = frame >= WARM_UP as usize && Game::eye(seconds).z >= CLIFF as f32;
        culling.frame(
            &renderer,
            frame,
            with_volume && inside,
            (rendered, finished),
        );
        if let Some(&(_, capture)) = CAPTURES.iter().find(|(at, _)| *at == frame) {
            let pixels = sgl_3d::diagnostics::read(device, queue, &texture, 4);
            image::save_buffer(
                directory.join(format!("{name}-{capture}.png")),
                &pixels,
                SIZE[0],
                SIZE[1],
                image::ColorType::Rgba8,
            )?;
        }
    }
    if let Some(timing) = &mut timing {
        for _ in 0..4 {
            let _ = device.poll(wgpu::PollType::wait_indefinitely());
            let done: Vec<_> = timing.begin_frame(device, queue).collect();
            done.iter().for_each(|done| culling.gpu(done));
            times.extend(done);
        }
    }
    times.retain(|frame| frame.frame >= WARM_UP);
    if with_volume {
        // Repeated updates on an otherwise idle GPU: the torch toggled,
        // the volume scrolled a chunk back and forth, and installed whole.
        for repeat in 0..REPEATS {
            game.set_torch(&mut scene, gpu, repeat % 2 == 0)?;
        }
        let here = game.volume.expect("installed");
        let back = here - IVec3::Z * CHUNK;
        for repeat in 0..REPEATS {
            let min = if repeat % 2 == 0 { back } else { here };
            game.scroll(&mut scene, gpu, min)?;
        }
        for _ in 0..REPEATS {
            game.install(&mut scene, gpu, here)?;
        }
        for _ in 0..REPEATS {
            let ((), _, complete) = completed(gpu, || ());
            game.measured.idle.push(complete);
        }
    }
    Ok((times, game.measured, culling))
}

/// Each pass group's GPU time with and without the volume.
fn report_passes(volume: &[FrameTime], ambient: &[FrameTime]) {
    let mut names: Vec<&str> = Vec::new();
    for pass in volume.iter().chain(ambient).flat_map(|frame| &frame.passes) {
        if !names.contains(&pass.name) {
            names.push(pass.name);
        }
    }
    let group = |frames: &[FrameTime], name: &str| -> Vec<f64> {
        frames
            .iter()
            .map(|frame| {
                frame
                    .passes
                    .iter()
                    .filter(|pass| pass.name == name)
                    .map(|pass| pass.ms)
                    .sum()
            })
            .collect()
    };
    println!(
        "GPU time per pass group, median / p95 ms over {} and {} frames:",
        volume.len(),
        ambient.len()
    );
    println!("{:>34}  {:>17}  {:>17}", "", "volume", "ambient alone");
    for name in names {
        let (a, b) = median_p95(&group(volume, name));
        let (c, d) = median_p95(&group(ambient, name));
        println!("{name:>34}  {a:7.3} / {b:7.3}  {c:7.3} / {d:7.3}");
    }
    let totals = |frames: &[FrameTime]| frames.iter().map(|f| f.total_ms).collect::<Vec<_>>();
    let (a, b) = median_p95(&totals(volume));
    let (c, d) = median_p95(&totals(ambient));
    println!("{:>34}  {a:7.3} / {b:7.3}  {c:7.3} / {d:7.3}", "frame");
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut night = false;
    let mut frames = 960;
    let mut options = culling::Options::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--night" => night = true,
            "--frames" => frames = args.next().ok_or("--frames requires a count")?.parse()?,
            "--help" | "-h" => {
                println!("irradiance_volume [--night] [--frames N] [--split]");
                return Ok(());
            }
            option if options.take(option) => {}
            other => return Err(format!("unknown option {other}").into()),
        }
    }
    if frames <= WARM_UP as usize {
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
    let directory: PathBuf =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/irradiance-volume-example");
    std::fs::create_dir_all(&directory)?;
    let directory = directory.canonicalize()?;
    println!(
        "{frames} frames a walk at {}x{}{}; frames in {}",
        SIZE[0],
        SIZE[1],
        if night { ", night" } else { "" },
        directory.display()
    );
    let (volume, measured, culling) = walk(gpu, (true, night), frames, &directory, options)?;
    let (ambient, _, _) = walk(gpu, (false, night), frames, &directory, options)?;
    report_passes(&volume, &ambient);
    println!("Keeping the volume up, median / p95:");
    measured
        .relight
        .report("relight of the 27 chunks about the torch");
    measured
        .entering
        .report("a scroll's entering chunks (10 x 8 x 1)");
    measured
        .scroll
        .report("scroll copy (set_irradiance_volume)");
    measured
        .install
        .report("whole install (10 slabs on as many threads)");
    let (median, p95) = median_p95(&measured.idle);
    println!("an empty submission, call to GPU done: {median:.3} / {p95:.3} ms");
    println!("Inside the cave, with the volume:");
    print!("{}", culling.report());
    Ok(())
}
