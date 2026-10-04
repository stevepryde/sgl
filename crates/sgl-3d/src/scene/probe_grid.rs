//! The world grid over a specular probe collection's influence boxes, which
//! probe captures and ray hits walk instead of every probe
//! (probe_grid.wgsl). The pattern is Wicked Engine's surfel grid (revision
//! 4323a33): a ray hit in surfel_raytraceCS.hlsl reads its cell's
//! `SurfelGridCell` count and offset (ShaderInterop_Renderer.h) into a list of
//! the surfels that overlap the cell, which surfel_binningCS.hlsl fills.
//! Probes are static, so SGL3D builds the grid once, on the CPU, when they are
//! installed: one level over the collection's bounds, not around the camera.
use crate::baked_specular_probe::SpecularProbeBox;
use glam::{Mat4, UVec3, Vec3};

/// Cells at most: 256 KiB of cell records.
const MAX_CELLS: u32 = 1 << 15;

/// One cell's record in `ProbeGrid::words`: its list's first index into
/// `words` and its probe count. probe_grid.wgsl reads it as
/// PROBE_GRID_CELL_WORDS words, the fields at PROBE_GRID_CELL_FIRST and
/// PROBE_GRID_CELL_COUNT.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GridCell {
    first: u32,
    count: u32,
}

/// `GridCell`'s size in words.
const CELL_WORDS: usize = std::mem::size_of::<GridCell>() / 4;

pub(super) struct ProbeGrid {
    /// The minimum corner of cell (0, 0, 0).
    pub origin: Vec3,
    /// Cells per metre.
    pub scale: f32,
    /// Cells on each axis; zero without probes.
    pub size: UVec3,
    /// Each cell's `GridCell`, x fastest, then y, then z, followed by the
    /// cells' probe indices, each list ascending.
    pub words: Vec<u32>,
}

impl ProbeGrid {
    /// No cells; one word, so the collection's grid is never empty.
    pub fn empty() -> Self {
        Self {
            origin: Vec3::ZERO,
            scale: 0.,
            size: UVec3::ZERO,
            words: vec![0],
        }
    }

    /// The grid over probes whose influence boxes are `influences`, each a
    /// world-to-local transform and the box in that frame.
    pub fn new(influences: &[(Mat4, SpecularProbeBox)]) -> Self {
        if influences.is_empty() {
            return Self::empty();
        }
        let bounds: Vec<[Vec3; 2]> = influences.iter().map(world_bounds).collect();
        let (min, max) = bounds.iter().fold(
            (Vec3::INFINITY, Vec3::NEG_INFINITY),
            |(min, max), [lo, hi]| (min.min(*lo), max.max(*hi)),
        );
        // Cubic cells, as fine as the budget allows.
        let extent = max - min;
        // No finer than the longest axis allows, which also keeps a thin
        // collection's cell positive.
        let mut cell = (extent.x * extent.y * extent.z / MAX_CELLS as f32)
            .cbrt()
            .max(extent.max_element() / MAX_CELLS as f32);
        // A cell lists every probe whose bounds, widened by `margin`, overlap
        // it, so rounding in the GPU's local transform and cell arithmetic
        // cannot move a point a probe reaches into a cell that omits it.
        let magnitude = min.abs().max(max.abs()).max_element();
        let (margin, size) = loop {
            let margin = cell / 64. + magnitude * f32::EPSILON * 512.;
            let size = ((extent + Vec3::splat(2. * margin)) / cell)
                .ceil()
                .max(Vec3::ONE)
                .as_uvec3();
            if size.x * size.y * size.z <= MAX_CELLS {
                break (margin, size);
            }
            cell *= 1.125;
        };
        let mut grid = Self {
            origin: min - Vec3::splat(margin),
            scale: 1. / cell,
            size,
            words: Vec::new(),
        };
        let last_cell = (size - UVec3::ONE).as_vec3();
        let mut cells = vec![Vec::new(); (size.x * size.y * size.z) as usize];
        for (index, [lo, hi]) in bounds.iter().enumerate() {
            let first = grid
                .coordinates(*lo - Vec3::splat(margin))
                .clamp(Vec3::ZERO, last_cell)
                .as_uvec3();
            let last = grid
                .coordinates(*hi + Vec3::splat(margin))
                .clamp(Vec3::ZERO, last_cell)
                .as_uvec3();
            for z in first.z..=last.z {
                for y in first.y..=last.y {
                    for x in first.x..=last.x {
                        cells[grid.flatten(UVec3::new(x, y, z))].push(index as u32);
                    }
                }
            }
        }
        let mut first = (CELL_WORDS * cells.len()) as u32;
        let records: Vec<GridCell> = cells
            .iter()
            .map(|list| {
                let cell = GridCell {
                    first,
                    count: list.len() as u32,
                };
                first += cell.count;
                cell
            })
            .collect();
        grid.words.extend_from_slice(bytemuck::cast_slice(&records));
        grid.words.extend(cells.into_iter().flatten());
        grid
    }

    /// The cell coordinates of `point`, as probe_grid.wgsl computes them.
    fn coordinates(&self, point: Vec3) -> Vec3 {
        ((point - self.origin) * self.scale).floor()
    }

    fn flatten(&self, cell: UVec3) -> usize {
        (cell.x + self.size.x * (cell.y + self.size.y * cell.z)) as usize
    }
}

/// probe_grid.wgsl's twins of `GridCell`'s layout, in words.
#[cfg(test)]
pub(crate) fn constants() -> [crate::shading::layout_tests::Constant; 3] {
    use crate::shading::layout_tests::Constant;
    use std::mem::offset_of;
    let words = |bytes: usize| naga::Literal::U32((bytes / 4) as u32);
    [
        Constant::new("geometry", "PROBE_GRID_CELL_WORDS", words(CELL_WORDS * 4)),
        Constant::new(
            "geometry",
            "PROBE_GRID_CELL_FIRST",
            words(offset_of!(GridCell, first)),
        ),
        Constant::new(
            "geometry",
            "PROBE_GRID_CELL_COUNT",
            words(offset_of!(GridCell, count)),
        ),
    ]
}

/// The world-space bounds of a probe's influence box.
fn world_bounds((world_to_local, influence): &(Mat4, SpecularProbeBox)) -> [Vec3; 2] {
    let to_world = world_to_local.inverse();
    let [lo, hi] = [influence.min, influence.max];
    (0..8)
        .map(|corner| {
            let pick = glam::BVec3::new(corner & 1 != 0, corner & 2 != 0, corner & 4 != 0);
            to_world.transform_point3(Vec3::select(pick, hi, lo))
        })
        .fold([Vec3::INFINITY, Vec3::NEG_INFINITY], |[min, max], p| {
            [min.min(p), max.max(p)]
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::baked_specular_probe::{
        BakedSpecularProbe, SpecularProbeRadiance, SpecularProbeTexels,
    };
    use glam::Quat;
    use wasm_bindgen_test::wasm_bindgen_test;

    /// SplitMix64 from a fixed seed, as uniform values in [0, 1).
    struct Random(u64);
    impl Random {
        fn next(&mut self) -> f32 {
            self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            ((z ^ (z >> 31)) >> 40) as f32 / (1u64 << 24) as f32
        }
        fn range(&mut self, lo: f32, hi: f32) -> f32 {
            lo + (hi - lo) * self.next()
        }
        fn vec(&mut self, lo: f32, hi: f32) -> Vec3 {
            Vec3::new(self.range(lo, hi), self.range(lo, hi), self.range(lo, hi))
        }
    }

    /// A rigidly rotated and translated influence box far from the origin.
    fn probe(random: &mut Random) -> BakedSpecularProbe {
        let rotation = Quat::from_axis_angle(
            random.vec(-1., 1.).try_normalize().unwrap_or(Vec3::Y),
            random.range(0., std::f32::consts::TAU),
        );
        let center = random.vec(-400., 400.) + Vec3::new(1500., 0., -900.);
        let half = random.vec(0.5, 30.);
        BakedSpecularProbe {
            center,
            world_to_local: Mat4::from_rotation_translation(rotation, center).inverse(),
            influence: SpecularProbeBox {
                min: -half,
                max: half,
            },
            blend: Vec3::ZERO,
            proxy: None,
            radiance: SpecularProbeRadiance {
                face_size: 64,
                texels: SpecularProbeTexels::Rgba16Float(Vec::new()),
            },
        }
    }

    // Defect: a cell omits a probe whose influence box reaches a point in it
    // (a rotated box's bounds, an off-by-one cell range), so walking the cell
    // differs from walking every probe. Independent signal:
    // brute-force containment in each probe's local box, at points inside, on
    // and around the boxes.
    #[wasm_bindgen_test(unsupported = test)]
    fn each_cell_lists_every_probe_that_reaches_it_in_ascending_order() {
        let mut random = Random(0x5eed);
        for collection in 0..8 {
            let probes: Vec<_> = (0..1 + collection * 24)
                .map(|_| probe(&mut random))
                .collect();
            let influences: Vec<_> = probes
                .iter()
                .map(|probe| (probe.world_to_local, probe.influence))
                .collect();
            let grid = ProbeGrid::new(&influences);
            for _ in 0..4000 {
                let owner = &probes[(random.next() * probes.len() as f32) as usize];
                let [lo, hi] = [owner.influence.min, owner.influence.max];
                let mut local = Vec3::new(
                    random.range(lo.x, hi.x),
                    random.range(lo.y, hi.y),
                    random.range(lo.z, hi.z),
                ) * random.range(0.9, 1.1);
                if random.next() < 0.25 {
                    local.x = if random.next() < 0.5 { lo.x } else { hi.x };
                }
                let point = owner.world_to_local.inverse().transform_point3(local);
                let cell = grid.coordinates(point);
                let records: &[GridCell] = bytemuck::cast_slice(
                    &grid.words[..CELL_WORDS * grid.size.element_product() as usize],
                );
                let listed =
                    if cell.cmpge(Vec3::ZERO).all() && cell.cmplt(grid.size.as_vec3()).all() {
                        let GridCell { first, count } = records[grid.flatten(cell.as_uvec3())];
                        &grid.words[first as usize..(first + count) as usize]
                    } else {
                        &[][..]
                    };
                assert!(
                    listed.windows(2).all(|pair| pair[0] < pair[1]),
                    "cell list out of order: {listed:?}"
                );
                for (index, probe) in probes.iter().enumerate() {
                    let p = probe.world_to_local.transform_point3(point);
                    let reaches =
                        p.cmpge(probe.influence.min).all() && p.cmple(probe.influence.max).all();
                    assert!(
                        !reaches || listed.contains(&(index as u32)),
                        "probe {index} reaches {point} but its cell lists {listed:?}"
                    );
                }
            }
        }
    }
}
