//! The irradiance volume (the architecture's "Irradiance volume"): the
//! game's lattice of ambient cubes in one RGBA16F 3D texture, which the
//! scene installs whole, writes by region and scrolls by copy, and lit group
//! 0 lends every view that shades (`shading/irradiance_volume.wgsl` samples
//! it).
//!
//! The texture of a volume of (Rx, Ry, Rz) cells is (Rx, 2Ry, 3Rz) texels:
//! the X, Y and Z faces in turn along z, a slab of Rz each, and in each slab
//! the positive face's cells at y and the negative face's at Ry + y, as
//! Bevy 9d120361303727a66b62f31f0d053793af62417a lays out and samples its
//! irradiance volumes (crates/bevy_pbr/src/light_probe/irradiance_volume.rs
//! 34-57 and irradiance_volume.wesl 59-65, MIT OR Apache-2.0,
//! src/LICENSE-bevy.txt; the doc's formula for t there puts the halves the
//! other way about, its table and its sample this way). Changed: a texel is
//! RGBA16F, not RGB9E5, its rgb the face's own light and its a the sky's
//! occlusion toward the face, one less its visibility, so a zero texel is
//! the fallback; the game writes cells by region, and a scroll moves the
//! cells that stay in place, through a stripe.
use super::{Scene, SceneError};
use crate::content::irradiance_volume::{IrradianceCell, IrradianceVolume};
use crate::static_lighting::irradiance_half;
use glam::{DVec3, I64Vec3, Vec3};

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
/// Bytes of one texel: four halves.
const TEXEL_BYTES: u32 = 8;
/// The cells a scroll moves at once along its axis: a voxel world's chunk.
const STRIPE: u32 = 16;

/// The texel extent of a volume of `cells`: Bevy's (Rx, 2Ry, 3Rz).
fn texture_size(cells: [u32; 3]) -> [u64; 3] {
    [
        u64::from(cells[0]),
        2 * u64::from(cells[1]),
        3 * u64::from(cells[2]),
    ]
}

/// The texel that holds face `face` (the cube's order: +X, −X, +Y, −Y, +Z,
/// −Z) of cell `cell` of a volume of `cells`.
fn face_texel(face: usize, cell: [u32; 3], cells: [u32; 3]) -> wgpu::Origin3d {
    let negative = (face % 2) as u32;
    let axis = (face / 2) as u32;
    wgpu::Origin3d {
        x: cell[0],
        y: cell[1] + negative * cells[1],
        z: cell[2] + axis * cells[2],
    }
}

/// A 3D texture of `size` texels, every texel zero (the fallback) until
/// something writes it: wgpu zero-initialises a texture on its first use.
fn zeroed(
    device: &wgpu::Device,
    label: &str,
    [width, height, depth]: [u32; 3],
    usage: wgpu::TextureUsages,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: depth,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D3,
        format: FORMAT,
        usage,
        view_formats: &[],
    })
}

/// A volume of `cells`' texture.
fn texture(device: &wgpu::Device, cells: [u32; 3]) -> wgpu::Texture {
    let size = texture_size(cells).map(|side| side as u32);
    let usage = wgpu::TextureUsages::TEXTURE_BINDING
        | wgpu::TextureUsages::COPY_SRC
        | wgpu::TextureUsages::COPY_DST;
    zeroed(device, "irradiance volume", size, usage)
}

/// What a scroll along one axis moves cells through: a face's box of cells
/// `STRIPE` thick across that axis, and one of zeros, which clears the
/// cells that enter.
struct Stripe {
    staging: wgpu::Texture,
    zero: wgpu::Texture,
    /// Cells across the axis.
    thickness: u32,
}

impl Stripe {
    fn new(device: &wgpu::Device, cells: [u32; 3], axis: usize) -> Self {
        let mut size = cells;
        size[axis] = cells[axis].min(STRIPE);
        let copy = wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::COPY_DST;
        Self {
            staging: zeroed(device, "irradiance volume scroll", size, copy),
            zero: zeroed(
                device,
                "irradiance volume zeros",
                size,
                wgpu::TextureUsages::COPY_SRC,
            ),
            thickness: size[axis],
        }
    }
}

/// A box of irradiance volume cells packed for
/// `Scene::write_irradiance_cells`: validated and encoded as the volume's
/// texture holds them, without a device, on whichever thread the game
/// chooses, so a relight's packing stays off the thread that renders. The
/// write then only copies it to the queue.
#[derive(Clone)]
pub struct PreparedIrradianceRegion {
    corner: Vec3,
    cells: [u32; 3],
    /// Each face's cells in turn, in the cube's face order, x fastest, then
    /// y, then z: RGBA16F texels.
    texels: Vec<u16>,
}

// Relight packing runs on the game's worker threads.
const _: () = {
    const fn send<T: Send>() {}
    send::<PreparedIrradianceRegion>();
};

impl std::fmt::Debug for PreparedIrradianceRegion {
    /// Its box and how many texels it holds, not the texels: a relight
    /// holds millions.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedIrradianceRegion")
            .field("corner", &self.corner)
            .field("cells", &self.cells)
            .field("texels", &(self.texels.len() / 4))
            .finish()
    }
}

impl PreparedIrradianceRegion {
    /// The box of `cells` cells along each axis whose first cell's least
    /// corner lies at `corner`, in the scene's render frame, holding
    /// `values`, one per cell, x fastest, then y, then z. Fails with
    /// `SceneError::InvalidIrradianceRegion` for a corner that is not
    /// finite, no cell along an axis, a number of values other than the
    /// box's cells, or a value that is not a valid `IrradianceCell`.
    pub fn new(
        corner: Vec3,
        cells: [u32; 3],
        values: &[IrradianceCell],
    ) -> Result<Self, SceneError> {
        let count = cells
            .iter()
            .try_fold(1usize, |count, &n| count.checked_mul(n as usize));
        if !corner.is_finite()
            || cells.contains(&0)
            || count != Some(values.len())
            || !values.iter().all(IrradianceCell::valid)
        {
            return Err(SceneError::InvalidIrradianceRegion);
        }
        let mut texels = Vec::with_capacity(values.len() * 24);
        for face in 0..6 {
            for cell in values {
                let [r, g, b] = cell.irradiance.irradiance[face];
                let occlusion = 1. - cell.sky_visibility[face];
                texels.extend([r, g, b, occlusion].map(irradiance_half));
            }
        }
        Ok(Self {
            corner,
            cells,
            texels,
        })
    }
}

/// The installed volume: its placement, with its origin in the frame the
/// scene was created in, so a move of the render origin translates it
/// without rounding it again, its texture and, once it has scrolled along
/// an axis, that axis's stripe.
struct Placement {
    origin: DVec3,
    cell_size: Vec3,
    cells: [u32; 3],
    texture: wgpu::Texture,
    stripes: [Option<Stripe>; 3],
}

impl Placement {
    /// The whole cells from this volume's origin to `at`, a position in the
    /// frame the scene was created in that the game gave as `given` in its
    /// render frame, or `None` where `at` lies off the lattice beyond its
    /// tolerance.
    fn cells_to(&self, at: DVec3, given: Vec3) -> Option<I64Vec3> {
        super::lattice::steps(self.origin, self.cell_size, at, given)
    }
}

/// The scene's irradiance volume: its placement and what lit group 0 binds.
pub(crate) struct IrradianceCells {
    placement: Option<Placement>,
    /// One zero texel, which group 0 binds while no volume is installed.
    stand_in: wgpu::TextureView,
    view: wgpu::TextureView,
}

impl IrradianceCells {
    pub fn new(device: &wgpu::Device) -> Self {
        let stand_in = texture(device, [1; 3]).create_view(&Default::default());
        Self {
            placement: None,
            view: stand_in.clone(),
            stand_in,
        }
    }

    /// The installed volume's texture, or the stand-in.
    pub fn view(&self) -> &wgpu::TextureView {
        &self.view
    }

    fn set(&mut self, placement: Option<Placement>) {
        self.view = placement
            .as_ref()
            .map_or(self.stand_in.clone(), |placement| {
                placement.texture.create_view(&Default::default())
            });
        self.placement = placement;
    }
}

impl Scene {
    /// Installs the irradiance volume, replacing the one installed; `None`
    /// removes it. A scene holds one, and one without pays nothing for it.
    /// Its cells start as the fallback (`IrradianceCell::default()`) until
    /// the game writes them (`Scene::write_irradiance_cells`). Installing
    /// the same cell size and counts at another origin moves the volume by
    /// the whole number of cells nearest the move, its origin staying on
    /// the lattice, and scrolls it: the cells that stay keep their content,
    /// and those that enter start as the fallback, so the game writes only
    /// the entering cells; the copy is submitted at once on `queue`, before
    /// any write the game makes after this call. Another cell size or
    /// count, or an origin off the lattice beyond a small tolerance of the
    /// cell size, is another placement, all of whose cells start as the
    /// fallback. Installing the same placement again changes nothing, and
    /// `Scene::move_origin` translates the volume with everything else, its
    /// cells kept. Fails with `SceneError::InvalidIrradianceVolume` for a
    /// placement that is not a lattice, and with `SceneError::DeviceLimit`
    /// where its texture, twice the cells on y and three times on z, would
    /// exceed the device's 3D texture limit; either keeps the installed
    /// volume.
    pub fn set_irradiance_volume(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        volume: Option<IrradianceVolume>,
    ) -> Result<(), SceneError> {
        self.edited();
        let Some(volume) = volume else {
            if self.irradiance_cells.placement.is_some() {
                self.irradiance_cells.set(None);
                self.resources = super::next_generation();
            }
            return Ok(());
        };
        if !volume.valid() {
            return Err(SceneError::InvalidIrradianceVolume);
        }
        let limit = u64::from(device.limits().max_texture_dimension_3d);
        if texture_size(volume.cells).iter().any(|&side| side > limit) {
            return Err(SceneError::DeviceLimit);
        }
        let origin = volume.origin.as_dvec3() + self.origin();
        let shift = self
            .irradiance_cells
            .placement
            .as_ref()
            .and_then(|current| {
                let lattice =
                    current.cell_size == volume.cell_size && current.cells == volume.cells;
                lattice
                    .then(|| current.cells_to(origin, volume.origin))
                    .flatten()
            });
        if let (Some(current), Some(shift)) = (&mut self.irradiance_cells.placement, shift) {
            if shift != I64Vec3::ZERO {
                scroll(device, queue, current, shift);
            }
            return Ok(());
        }
        let placement = Placement {
            origin,
            cell_size: volume.cell_size,
            cells: volume.cells,
            texture: texture(device, volume.cells),
            stripes: [None, None, None],
        };
        // Group 0 binds the new texture.
        self.irradiance_cells.set(Some(placement));
        self.resources = super::next_generation();
        Ok(())
    }

    /// The installed irradiance volume, in the scene's render frame.
    pub fn irradiance_volume(&self) -> Option<IrradianceVolume> {
        self.irradiance_cells
            .placement
            .as_ref()
            .map(|placement| IrradianceVolume {
                origin: (placement.origin - self.origin()).as_vec3(),
                cell_size: placement.cell_size,
                cells: placement.cells,
            })
    }

    /// Writes a prepared box of cells into the installed irradiance volume,
    /// through `queue` like any upload: one write per face, which the next
    /// submission carries. The box's corner names a cell's least corner on
    /// the volume's lattice, within a small tolerance of the cell size. A
    /// write is an edit, not a static edit: it records no bounds, makes no
    /// cache stale and cuts no history; a specular probe capture that showed
    /// the old light is the game's to capture again. Fails with
    /// `SceneError::IrradianceRegionOutside`, writing nothing, while no
    /// volume is installed or where the box is off its lattice or not wholly
    /// within it.
    pub fn write_irradiance_cells(
        &mut self,
        queue: &wgpu::Queue,
        region: &PreparedIrradianceRegion,
    ) -> Result<(), SceneError> {
        self.edited();
        let placement = self
            .irradiance_cells
            .placement
            .as_ref()
            .ok_or(SceneError::IrradianceRegionOutside)?;
        let corner = region.corner.as_dvec3() + self.origin();
        let first = placement
            .cells_to(corner, region.corner)
            .ok_or(SceneError::IrradianceRegionOutside)?;
        let inside = (0..3).all(|axis| {
            first[axis] >= 0
                && first[axis] + i64::from(region.cells[axis]) <= i64::from(placement.cells[axis])
        });
        if !inside {
            return Err(SceneError::IrradianceRegionOutside);
        }
        let first = first.to_array().map(|cell| cell as u32);
        let [width, height, depth] = region.cells;
        let face_texels = region.texels.len() / 6;
        for (face, texels) in region.texels.chunks_exact(face_texels).enumerate() {
            crate::counters::write_texture(
                queue,
                wgpu::TexelCopyTextureInfo {
                    texture: &placement.texture,
                    mip_level: 0,
                    origin: face_texel(face, first, placement.cells),
                    aspect: wgpu::TextureAspect::All,
                },
                bytemuck::cast_slice(texels),
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(width * TEXEL_BYTES),
                    rows_per_image: Some(height),
                },
                wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: depth,
                },
            );
        }
        Ok(())
    }
}

/// `placement` moved by `shift` whole cells, in place: along each axis
/// in turn, each face's cells that stay move to their new texels a stripe
/// at a time, through the axis's stripe, in the order that reads each
/// stripe before another overwrites it, and the cells that enter are
/// cleared from its stripe of zeros, in one command buffer submitted at
/// once, as Godot ed1daf0's SDFGI scrolls its cascades by a copy when its
/// camera crosses a cell
/// (servers/rendering/renderer_rd/shaders/environment/sdfgi_preprocess.glsl
/// `MODE_SCROLL` 174-183, servers/rendering/renderer_rd/environment/gi.cpp
/// 2121-2175). A cell at index i before the move is at i − shift after it.
/// A stripe, a chunk thick, replaces copying into a fresh texture, which
/// wgpu clears whole first (it clears a 3D texture by buffer copies), at
/// twice the copies of the cells that stay.
fn scroll(device: &wgpu::Device, queue: &wgpu::Queue, placement: &mut Placement, shift: I64Vec3) {
    let cells = placement.cells;
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("irradiance volume scroll"),
    });
    // A move of the volume's size or more along an axis leaves no cell:
    // one clear along that axis, and nothing to move along the others.
    let whole = (0..3).find(|&axis| shift[axis].unsigned_abs() >= u64::from(cells[axis]));
    let axes = whole.map_or(0..3, |axis| axis..axis + 1);
    for axis in axes {
        let by = shift[axis];
        if by == 0 {
            continue;
        }
        let entering = by.unsigned_abs().min(u64::from(cells[axis])) as u32;
        let kept = cells[axis] - entering;
        let stripe =
            placement.stripes[axis].get_or_insert_with(|| Stripe::new(device, cells, axis));
        let texture = &placement.texture;
        // A box of `depth` cells across the axis from `at` along it.
        let extent = |depth: u32| {
            let mut size = cells;
            size[axis] = depth;
            wgpu::Extent3d {
                width: size[0],
                height: size[1],
                depth_or_array_layers: size[2],
            }
        };
        let face_at = |face: usize, at: u32| {
            let mut cell = [0; 3];
            cell[axis] = at;
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: face_texel(face, cell, cells),
                aspect: wgpu::TextureAspect::All,
            }
        };
        for face in 0..6 {
            let mut moved = 0;
            while moved < kept {
                let depth = (kept - moved).min(stripe.thickness);
                // Moving down the axis, the lowest cells first; moving up
                // it, the highest.
                let to = if by > 0 {
                    moved
                } else {
                    cells[axis] - moved - depth
                };
                let from = if by > 0 { to + entering } else { to - entering };
                encoder.copy_texture_to_texture(
                    face_at(face, from),
                    stripe.staging.as_image_copy(),
                    extent(depth),
                );
                encoder.copy_texture_to_texture(
                    stripe.staging.as_image_copy(),
                    face_at(face, to),
                    extent(depth),
                );
                moved += depth;
            }
            let first = if by > 0 { kept } else { 0 };
            let mut cleared = 0;
            while cleared < entering {
                let depth = (entering - cleared).min(stripe.thickness);
                encoder.copy_texture_to_texture(
                    stripe.zero.as_image_copy(),
                    face_at(face, first + cleared),
                    extent(depth),
                );
                cleared += depth;
            }
        }
    }
    queue.submit([encoder.finish()]);
    placement.origin += shift.as_dvec3() * placement.cell_size.as_dvec3();
}

#[cfg(all(test, not(target_arch = "wasm32")))]
#[path = "irradiance_volume_tests.rs"]
mod tests;
