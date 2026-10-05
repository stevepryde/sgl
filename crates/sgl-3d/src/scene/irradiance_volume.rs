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
//! the fallback; the game writes cells by region, and a scroll copies the
//! cells that stay.
use super::{Scene, SceneError};
use crate::content::irradiance_volume::{IrradianceCell, IrradianceVolume};
use crate::static_lighting::irradiance_half;
use glam::{DVec3, I64Vec3, Vec3};

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
/// Bytes of one texel: four halves.
const TEXEL_BYTES: u32 = 8;
/// How far, in cells, a position may lie from the lattice and still name a
/// place on it, beyond the rounding of its `f32` magnitude.
const LATTICE_TOLERANCE: f64 = 1e-3;

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

/// A texture for a volume of `cells`, every texel zero (the fallback).
///
/// It is initialised at once, through `queue`, by a write of one zero
/// texel, which clears the rest: wgpu-core 29 tracks a 3D texture's
/// initialisation as one layer, which a queue write takes whole
/// (device/queue.rs `write_texture`), but registers a command encoder's
/// copy by its depth slices (command/transfer.rs `handle_texture_init`), so
/// a scroll's copy at a nonzero depth into a texture not yet initialised
/// leaves it marked uninitialised, and its next use clears what the copy
/// wrote.
fn texture(device: &wgpu::Device, queue: &wgpu::Queue, cells: [u32; 3]) -> wgpu::Texture {
    let [width, height, depth] = texture_size(cells).map(|side| side as u32);
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("irradiance volume"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: depth,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D3,
        format: FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_DST
            | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    queue.write_texture(
        texture.as_image_copy(),
        &[0; TEXEL_BYTES as usize],
        wgpu::TexelCopyBufferLayout::default(),
        wgpu::Extent3d::default(),
    );
    texture
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
/// without rounding it again, and its texture.
struct Placement {
    origin: DVec3,
    cell_size: Vec3,
    cells: [u32; 3],
    texture: wgpu::Texture,
}

impl Placement {
    /// The whole cells from this volume's origin to `at`, a position in the
    /// frame the scene was created in that the game gave as `given` in its
    /// render frame, or `None` where `at` lies off the lattice beyond its
    /// tolerance.
    fn cells_to(&self, at: DVec3, given: Vec3) -> Option<I64Vec3> {
        let cell_size = self.cell_size.as_dvec3();
        let cells = (at - self.origin) / cell_size;
        let nearest = cells.round();
        let rounding = f64::from(given.abs().max_element() * f32::EPSILON) / cell_size;
        let tolerance = rounding + LATTICE_TOLERANCE;
        (cells - nearest)
            .abs()
            .cmple(tolerance)
            .all()
            .then(|| nearest.as_i64vec3())
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
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let stand_in = texture(device, queue, [1; 3]).create_view(&Default::default());
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
        if shift == Some(I64Vec3::ZERO) {
            return Ok(());
        }
        let placement = match (self.irradiance_cells.placement.take(), shift) {
            (Some(current), Some(shift)) => scroll(device, queue, current, shift),
            _ => Placement {
                origin,
                cell_size: volume.cell_size,
                cells: volume.cells,
                texture: texture(device, queue, volume.cells),
            },
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
            queue.write_texture(
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

/// `current` moved by `shift` whole cells: a fresh texture, all fallback,
/// into which one command buffer, submitted at once, copies the cells that
/// stay, each face's box of them to its new texels, as Godot ed1daf0's
/// SDFGI scrolls its cascades by a copy when its camera crosses a cell
/// (servers/rendering/renderer_rd/shaders/environment/sdfgi_preprocess.glsl
/// `MODE_SCROLL` 174-183, servers/rendering/renderer_rd/environment/gi.cpp
/// 2121-2175). A cell at index i before the move is at i − shift after it.
fn scroll(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    current: Placement,
    shift: I64Vec3,
) -> Placement {
    let cells = current.cells;
    let moved = Placement {
        origin: current.origin + shift.as_dvec3() * current.cell_size.as_dvec3(),
        cell_size: current.cell_size,
        cells,
        texture: texture(device, queue, cells),
    };
    let mut source = [0; 3];
    let mut target = [0; 3];
    let mut kept = [0; 3];
    for axis in 0..3 {
        let count = i64::from(cells[axis]);
        let by = shift[axis].clamp(-count, count);
        source[axis] = by.max(0) as u32;
        target[axis] = (-by).max(0) as u32;
        kept[axis] = (count - by.abs()) as u32;
    }
    if kept.contains(&0) {
        return moved;
    }
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("irradiance volume scroll"),
    });
    for face in 0..6 {
        encoder.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &current.texture,
                mip_level: 0,
                origin: face_texel(face, source, cells),
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyTextureInfo {
                texture: &moved.texture,
                mip_level: 0,
                origin: face_texel(face, target, cells),
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d {
                width: kept[0],
                height: kept[1],
                depth_or_array_layers: kept[2],
            },
        );
    }
    queue.submit([encoder.finish()]);
    moved
}

#[cfg(all(test, not(target_arch = "wasm32")))]
#[path = "irradiance_volume_tests.rs"]
mod tests;
