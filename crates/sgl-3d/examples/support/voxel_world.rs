//! The voxel world of the `streaming` example, which the `browser_streaming`
//! example builds too: 16 m chunks of terrain meshed into quads with an
//! atlas or a material per block, water surfaces, creatures, the sky, and
//! the camera, frame input and settings every run renders with. Both
//! examples include this file by path.
use sgl_3d::glam::{DVec3, IVec3, Mat4, Vec3, camera};
use sgl_3d::{
    AlphaMode, Camera, DirectionalLight, DirectionalShadow, EnvironmentId, Exposure, FrameInput,
    Light, LightShape, MaterialId, ModelMesh, PreparedModel, Scene, SceneError,
    asset::{CpuMesh, Image, Material, Vertex},
    environment::{EnvironmentMap, PmremAtlas},
    settings::{Antialiasing, ReflectionMethod, ScreenSpaceReflections, Settings},
};
use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::BuildHasherDefault;

/// Maps and sets of chunks that iterate alike in every run.
pub type Chunks<V> = HashMap<IVec3, V, BuildHasherDefault<DefaultHasher>>;
pub type ChunkSet = HashSet<IVec3, BuildHasherDefault<DefaultHasher>>;

/// A chunk's side, in metres and blocks.
pub const CHUNK: i32 = 16;
/// The water's surface: the top of block 29.
pub const SEA: i32 = 30;
/// Every run's output size.
pub const SIZE: [u32; 2] = [1920, 1080];

/// A small deterministic hash.
pub fn hash(mut value: u64) -> u64 {
    value ^= value >> 33;
    value = value.wrapping_mul(0xff51_afd7_ed55_8ccd);
    value ^= value >> 33;
    value = value.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
    value ^ (value >> 33)
}

pub fn hash3(at: IVec3, salt: u64) -> u64 {
    hash((at.x as u32 as u64) ^ ((at.y as u32 as u64) << 21) ^ ((at.z as u32 as u64) << 42) ^ salt)
}

/// The terrain: rolling hills with ridges and rough ground, a column's top
/// block's height. A surface chunk meshes to 300-1,400 quads.
pub fn ground(x: i32, z: i32) -> i32 {
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
pub struct World {
    /// Height changes of edited columns.
    pub edits: BTreeMap<(i32, i32), i32>,
    /// Remeshes of each chunk since it was first meshed, which shade it.
    pub revisions: Chunks<u32>,
}

impl World {
    pub fn height(&self, x: i32, z: i32) -> i32 {
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

pub fn empty(material: usize) -> CpuMesh {
    CpuMesh {
        vertices: Vec::new(),
        indices: Vec::new(),
        material,
        deformation: Default::default(),
    }
}

/// A quad of a mesh: its corner, the two edges from it and the vertex
/// colour's shade, its texture coordinates spanning 0 to 1.
pub fn quad(mesh: &mut CpuMesh, corner: Vec3, u: Vec3, v: Vec3, shade: f32) {
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
pub fn atlas_mesh(meshes: Vec<CpuMesh>) -> CpuMesh {
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
pub fn mesh_chunk(world: &World, chunk: IVec3, seconds: f32) -> (Vec<CpuMesh>, CpuMesh) {
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
pub fn atlas() -> Image {
    Image::Rgba8(image::RgbaImage::from_fn(64, 16, |x, y| {
        texel((x / 16) as usize, x % 16, y)
    }))
}

/// Block `tile`'s own texture.
pub fn tile_texture(tile: usize) -> Image {
    Image::Rgba8(image::RgbaImage::from_fn(16, 16, |x, y| texel(tile, x, y)))
}

/// The sky: a constant blue radiance, as the water example's, which draws
/// the backdrop, lights what the sun does not reach and backs reflections.
pub fn sky() -> EnvironmentMap {
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
pub fn creature() -> CpuMesh {
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

/// The materials a world's chunks and creatures draw with.
pub struct Materials {
    /// The terrain's, by a mesh's material index: the atlas, or one a block.
    pub terrain: Vec<MaterialId>,
    pub water: MaterialId,
    pub creature: MaterialId,
}

/// Adds the world's materials to `scene`: the terrain's atlas or, `per_block`,
/// a material for each block; the water, a blended receiver of screen-space
/// reflections; and the creatures'.
pub fn add_materials(
    scene: &mut Scene,
    (device, queue): (&wgpu::Device, &wgpu::Queue),
    per_block: bool,
) -> Result<Materials, SceneError> {
    let block = |name: &str, texture| Material {
        name: name.into(),
        base_texture: Some(texture),
        roughness: 0.9,
        ..Default::default()
    };
    let (blocks, images) = if per_block {
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
    Ok(Materials {
        terrain,
        water: materials[0],
        creature: materials[1],
    })
}

/// What the game's mesher prepares for a chunk: its terrain's model, as the
/// run gives it, and its water's, each with its triangles (none for an
/// empty one), or None when it was not asked for; its terrain's quads; and
/// the bytes of vertices and indices it gives the scene.
pub struct Prepared {
    pub terrain: Option<(PreparedModel, usize)>,
    pub water: Option<(PreparedModel, usize)>,
    pub quads: usize,
    pub bytes: usize,
}

/// What the game's mesher reads: the world as it stands, and the run's
/// materials. Worker threads share it.
pub struct Mesher<'a> {
    pub world: &'a World,
    pub seconds: f32,
    pub per_block: bool,
    pub terrain: &'a [MaterialId],
    pub water: MaterialId,
}

impl Mesher<'_> {
    /// Meshes chunk `chunk` and prepares the parts `[terrain, water]` asks
    /// for.
    pub fn prepare(
        &self,
        chunk: IVec3,
        [terrain, water]: [bool; 2],
    ) -> Result<Prepared, SceneError> {
        let (blocks, waves) = mesh_chunk(self.world, chunk, self.seconds);
        let terrain_meshes = if self.per_block {
            blocks
        } else {
            vec![atlas_mesh(blocks)]
        };
        let mut prepared = Prepared {
            terrain: None,
            water: None,
            quads: quads(&terrain_meshes),
            bytes: 0,
        };
        for (meshes, wanted, water) in
            [(terrain_meshes, terrain, false), (vec![waves], water, true)]
        {
            if !wanted {
                continue;
            }
            let meshes: Vec<ModelMesh> = meshes
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
                .collect();
            prepared.bytes += meshes
                .iter()
                .map(|mesh| mesh.vertices.len() * size_of::<Vertex>() + mesh.indices.len() * 4)
                .sum::<usize>();
            let triangles = meshes.iter().map(|mesh| mesh.indices.len() / 3).sum();
            let model = Some((PreparedModel::new(meshes)?, triangles));
            if water {
                prepared.water = model;
            } else {
                prepared.terrain = model;
            }
        }
        Ok(prepared)
    }
}

/// The quads of `meshes`.
fn quads(meshes: &[CpuMesh]) -> usize {
    meshes.iter().map(|mesh| mesh.indices.len() / 6).sum()
}

/// The torches chunk `chunk` holds, `count` on its surface's blocks and
/// one in eight shadowed: the block each stands on and whether it is.
pub fn torches(world: &World, chunk: IVec3, count: u32) -> Vec<(IVec3, bool)> {
    let base = chunk * CHUNK;
    (0..count)
        .filter_map(|torch| {
            let roll = hash3(chunk, 3 + u64::from(torch));
            let (x, z) = (
                base.x + ((roll >> 8) % CHUNK as u64) as i32,
                base.z + ((roll >> 16) % CHUNK as u64) as i32,
            );
            let top = world.height(x, z);
            if top < base.y || top >= base.y + CHUNK {
                return None;
            }
            Some((IVec3::new(x, top + 1, z), (roll >> 24).is_multiple_of(8)))
        })
        .collect()
}

/// A torch's light over world block `at`, `shadowed` or not, in the render
/// frame of `origin`.
pub fn torch(at: IVec3, shadowed: bool, origin: DVec3) -> Light {
    Light {
        position: (at.as_dvec3() + DVec3::new(0.5, 1., 0.5) - origin).as_vec3(),
        shape: LightShape::Point,
        color: [1., 0.7, 0.4],
        intensity: 20.,
        range: 10.,
        casts_shadow: shadowed,
        ..Default::default()
    }
}

/// The chunk holding world position `at`.
pub fn chunk_of(at: DVec3) -> IVec3 {
    (at / f64::from(CHUNK)).floor().as_ivec3()
}

/// The eye at `x` along the line it travels, at a walker's height over the
/// ground or, faster than 10 m/s, a flyer's.
pub fn eye(world: &World, x: f64, speed: f64) -> DVec3 {
    let ground = f64::from(world.height(x.floor() as i32, 0));
    DVec3::new(x, ground + if speed > 10. { 24. } else { 2.6 }, 0.5)
}

/// Creature `index`'s pose in the world at `seconds` with the eye at `eye`:
/// walking circles about it. Returns its feet's position and its heading.
pub fn creature_place(world: &World, eye: DVec3, seconds: f64, index: usize) -> (DVec3, f32) {
    let angle = seconds * 0.3 + index as f64 * 0.7;
    let radius = 6. + (index % 10) as f64 * 3.;
    let x = eye.x + radius * angle.cos();
    let z = eye.z + radius * angle.sin();
    let y = f64::from(world.height(x.floor() as i32, z.floor() as i32) + 1);
    (DVec3::new(x, y, z), -angle as f32)
}

/// A creature's pose in the render frame, at `place` less the render
/// `origin`.
pub fn creature_pose((at, heading): (DVec3, f32), origin: DVec3) -> Mat4 {
    Mat4::from_translation((at - origin).as_vec3()) * Mat4::from_rotation_y(heading)
}

/// The camera at `eye` in the render frame, looking ahead along +x and a
/// little down.
pub fn camera(eye: Vec3) -> Camera {
    Camera {
        eye,
        view: camera::rh::view::look_to_mat4(eye, Vec3::new(1., -0.25, 0.2), Vec3::Y),
        projection: sgl_3d::perspective(70f32.to_radians(), SIZE[0] as f32 / SIZE[1] as f32, 0.1),
    }
}

/// A frame seen by `camera` at `seconds`, lit by the `sky` and a sun whose
/// shadow reaches `shadow_distance` metres in four cascades.
pub fn input(camera: Camera, seconds: f64, sky: EnvironmentId, shadow_distance: f32) -> FrameInput {
    let mut input = FrameInput::new(camera);
    input.elapsed_seconds = seconds;
    input.environment = Some(sky);
    input.exposure = Exposure {
        stops: 0.,
        automatic: None,
    };
    input.directional_lights[0] = Some(DirectionalLight {
        direction: Vec3::new(-0.4, -1., -0.3),
        color: [1., 0.95, 0.85],
        illuminance: 3.,
        shadow: Some(DirectionalShadow {
            distance: shadow_distance,
            cascades: 4,
        }),
        ..Default::default()
    });
    input
}

/// Every run's settings: TAA, full-resolution screen-space reflections,
/// world-space reflections and shadows, at the full scene resolution.
pub fn settings() -> Settings {
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
