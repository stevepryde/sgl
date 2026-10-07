//! The ray source: the scene's geometry and materials for shader ray queries,
//! through portable BVHs that need no hardware ray-tracing features. Each
//! texture, material and model owns ranges of the source buffer (its level
//! 0; its record; its mesh records, section tables, vertices, indices and
//! BVH (`model`); a deforming
//! model's influences and morph targets), and each deforming instance its
//! joint matrices, morph weights and deformed vertices
//! (`scene::deformation`), written when it is added or replaced and freed
//! for reuse when it is removed; the buffer grows when they do not fit.
//! Pulled raster passes read vertices from it too, so it is always current,
//! and shadow casters read a deforming instance's positions from it as a
//! vertex buffer. Above the model BVHs, the instances' entries and the
//! instance BVHs built over them are `instances`'; the hardware path's
//! acceleration structures, built over the same geometry and entries where
//! the device traces rays in hardware, are `acceleration`'s.
use std::ops::Range;

use super::SceneError;
use super::ranges::Ranges;
use crate::asset::{CompressedFormat, Image};
use crate::shading::material::MaterialUniform;
use crate::shading::packed_vertex::PackedVertex;

pub(crate) mod acceleration;
mod bvh;
mod charts;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod hardware_tests;
pub(crate) mod instances;
#[cfg(test)]
mod layout;
mod model;
#[cfg(test)]
pub(crate) use layout::{constants, mirrors};
pub(crate) use model::{PreparedRayModel, RayMesh, prepare_model};
#[cfg(test)]
mod query;
#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) use query::{Function, Query};
#[cfg(test)]
pub(crate) use query::{QUERY, SCENE_INTERSECT_ENTRY};

/// What a ray instance reads of its model.
#[derive(Clone, Copy)]
pub(crate) struct RayModel {
    /// The model's first mesh record.
    pub mesh_word: u32,
    /// Its BVH root node, zero when it has no triangles.
    pub bvh_root: u32,
}

/// A model's words in the source: what rays read, its range, and where each
/// mesh's vertices and indices lie.
pub(crate) struct RayModelWords {
    pub ray: RayModel,
    pub range: Range<u32>,
    pub meshes: Vec<RayMeshWords>,
}

/// Where a mesh's packed vertices and its indices lie in the source, which a
/// BLAS reads: its first vertex record, at a multiple of `VERTEX_WORDS`, and
/// its first index, with their counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RayMeshWords {
    pub vertices: u32,
    pub vertex_count: u32,
    pub indices: u32,
    pub index_count: u32,
}

/// A packed vertex's words (`shading::packed_vertex`): the stride a BLAS
/// reads a model's positions at in whole strides, so each mesh's vertex
/// block, and the model's range, starts at a multiple of it
/// (`BlasTriangleGeometry::first_vertex` counts strides).
pub(crate) const VERTEX_WORDS: u32 = (std::mem::size_of::<PackedVertex>() / 4) as u32;

impl RayModel {
    /// Mesh `mesh`'s record, which pulled raster passes read.
    pub fn mesh_word(&self, mesh: usize) -> u32 {
        self.mesh_word + (mesh * model::MESH_WORDS) as u32
    }
}

/// The source's first words. Word 0 starts no record, so a zero image or BVH
/// root word means none.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SourceHeader {
    /// The enabled material visibility groups.
    visibility_mask: u32,
    /// The static and moving instance BVHs' roots (`instances`), zero when
    /// one bounds nothing.
    static_root: u32,
    moving_root: u32,
    padding: u32,
}

/// An image's record in the source, followed by its level 0 row by row:
/// RGBA8 texels, or a BC7 image's blocks as stored, which rays decode texel
/// by texel.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ImageHeader {
    width: u32,
    height: u32,
    /// `IMAGE_RGBA8` or `IMAGE_BC7`.
    format: u32,
}

/// `ImageHeader::format` of RGBA8 texels.
const IMAGE_RGBA8: u32 = 0;
/// `ImageHeader::format` of BC7 blocks.
const IMAGE_BC7: u32 = 1;

/// A material's record in the source: its values, its textures, their wrap
/// modes and whether lightmap charts light it.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MaterialRecord {
    material: MaterialUniform,
    textures: MaterialTextures,
    wrap: [u32; 2],
    baked: u32,
}

/// Each texture's image word, zero when absent.
#[repr(C)]
#[derive(Clone, Copy, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct MaterialTextures {
    pub base: u32,
    pub metallic_roughness: u32,
    pub emission: u32,
    pub normal: u32,
    pub bump: u32,
    pub anisotropy: u32,
    pub clearcoat: u32,
    pub coat_roughness: u32,
    pub coat_normal: u32,
    pub iridescence: u32,
    pub iridescence_thickness: u32,
}

/// Words of `T`.
fn words<T>() -> usize {
    std::mem::size_of::<T>() / 4
}

pub(crate) struct SceneRays {
    source: wgpu::Buffer,
    words: Ranges,
    /// The largest source the device binds, in words.
    word_limit: u64,
    visibility_mask: u32,
}

impl SceneRays {
    /// A source holding only its header.
    pub fn new(device: &wgpu::Device) -> Self {
        let limits = device.limits();
        let header = words::<SourceHeader>();
        Self {
            source: source_buffer(device, header as u64),
            words: Ranges::new(header as u32),
            word_limit: limits
                .max_storage_buffer_binding_size
                .min(limits.max_buffer_size)
                / 4,
            visibility_mask: 0,
        }
    }

    /// The source buffer, which group 1 binds and the deform stage writes.
    pub fn source(&self) -> &wgpu::Buffer {
        &self.source
    }

    /// `len` words, growing the source to hold them. Content already written
    /// keeps its words.
    pub fn allocate(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        len: usize,
    ) -> Result<Range<u32>, SceneError> {
        self.allocate_aligned(device, queue, len, 1)
    }

    /// `allocate`, starting at a multiple of `align` words.
    pub fn allocate_aligned(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        len: usize,
        align: u32,
    ) -> Result<Range<u32>, SceneError> {
        let len = u32::try_from(len).map_err(|_| SceneError::DeviceLimit)?;
        let range = self
            .words
            .allocate_aligned(len, align)
            .ok_or(SceneError::DeviceLimit)?;
        let needed = u64::from(self.words.end());
        if needed > self.word_limit {
            self.words.free(range);
            return Err(SceneError::DeviceLimit);
        }
        let capacity = self.source.size() / 4;
        if needed > capacity {
            crate::counters::ray_source_growth();
            let grown = source_buffer(device, needed.max(capacity * 2).min(self.word_limit));
            // Through the queue, never a frame's encoder: an abandoned frame
            // loses no content.
            let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("grow scene ray source"),
            });
            encoder.copy_buffer_to_buffer(&self.source, 0, &grown, 0, self.source.size());
            queue.submit([encoder.finish()]);
            self.source = grown;
        }
        Ok(range)
    }

    /// The words up to the last one content holds, and the words content
    /// holds.
    #[cfg(any(test, feature = "diagnostics"))]
    pub fn words_in_use(&self) -> (u64, u64) {
        let end = u64::from(self.words.end());
        (end, end - self.words.free_units())
    }

    /// Frees words for reuse.
    pub fn free(&mut self, range: Range<u32>) {
        self.words.free(range);
    }

    /// Writes `values` at `word`.
    pub fn write(&self, queue: &wgpu::Queue, word: u32, values: &[u32]) {
        if !values.is_empty() {
            crate::counters::write_buffer(
                queue,
                &self.source,
                u64::from(word) * 4,
                bytemuck::cast_slice(values),
            );
        }
    }

    /// An image's record and level 0, as stored; its word starts the range.
    pub fn add_image(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        image: &Image,
    ) -> Result<Range<u32>, SceneError> {
        let [width, height] = image.size();
        let (format, bytes) = match image {
            Image::Rgba8(image) => (IMAGE_RGBA8, image.as_raw().as_slice()),
            Image::Compressed(image) => match image.format {
                CompressedFormat::Bc7 => (IMAGE_BC7, image.levels[0].as_slice()),
            },
        };
        let header = ImageHeader {
            width,
            height,
            format,
        };
        let range = self.allocate(device, queue, words::<ImageHeader>() + bytes.len() / 4)?;
        let mut record = Vec::with_capacity(range.len());
        record.extend_from_slice(bytemuck::cast_slice(&[header]));
        record.extend(
            bytes
                .chunks_exact(4)
                .map(|p| u32::from_le_bytes([p[0], p[1], p[2], p[3]])),
        );
        self.write(queue, range.start, &record);
        Ok(range)
    }

    /// A material's record, which lightmap charts do not light; its word
    /// starts the range.
    pub fn add_material(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        values: &MaterialUniform,
        textures: MaterialTextures,
        wrap: [gltf::texture::WrappingMode; 2],
    ) -> Result<Range<u32>, SceneError> {
        let record = MaterialRecord {
            material: *values,
            textures,
            wrap: wrap.map(|w| match w {
                gltf::texture::WrappingMode::Repeat => 0,
                gltf::texture::WrappingMode::MirroredRepeat => 1,
                gltf::texture::WrappingMode::ClampToEdge => 2,
            }),
            baked: 0,
        };
        let range = self.allocate(device, queue, words::<MaterialRecord>())?;
        self.write(queue, range.start, bytemuck::cast_slice(&[record]));
        Ok(range)
    }

    /// Replaces the values of the material record at `word`.
    pub fn write_material(&self, queue: &wgpu::Queue, word: u32, values: &MaterialUniform) {
        let field = std::mem::offset_of!(MaterialRecord, material) / 4;
        self.write(queue, word + field as u32, bytemuck::cast_slice(&[*values]));
    }

    /// Whether lightmap charts light the material whose record is at `word`.
    pub fn write_baked(&self, queue: &wgpu::Queue, word: u32, baked: bool) {
        let field = std::mem::offset_of!(MaterialRecord, baked) / 4;
        self.write(queue, word + field as u32, &[u32::from(baked)]);
    }

    /// Places prepared `model`: allocates its range at a multiple of
    /// `VERTEX_WORDS`, so its vertex blocks keep theirs, names each mesh's
    /// material record (`materials`, in mesh order) and adds the range's
    /// start to every word that addresses the source, so its words are
    /// ready to write at the range's start.
    pub fn place_model(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        model: &mut PreparedRayModel,
        materials: &[u32],
    ) -> Result<RayModelWords, SceneError> {
        let range = self.allocate_aligned(device, queue, model.len(), VERTEX_WORDS)?;
        let (ray, meshes) = model.rebase(range.start, materials);
        Ok(RayModelWords { ray, range, meshes })
    }

    /// Names the static and moving instance BVHs' roots in the header.
    pub fn set_instance_roots(&self, queue: &wgpu::Queue, roots: [u32; 2]) {
        crate::counters::write_buffer(
            queue,
            &self.source,
            std::mem::offset_of!(SourceHeader, static_root) as u64,
            bytemuck::cast_slice(&roots),
        );
    }

    /// Set the enabled material visibility groups before tracing this frame.
    pub fn set_visibility_mask(&mut self, queue: &wgpu::Queue, mask: u32) {
        if self.visibility_mask != mask {
            crate::counters::write_buffer(
                queue,
                &self.source,
                std::mem::offset_of!(SourceHeader, visibility_mask) as u64,
                bytemuck::bytes_of(&mask),
            );
            self.visibility_mask = mask;
        }
    }
}

/// A source of `words`. Where the device traces rays in hardware, BLASes
/// read their positions and indices from it (`BLAS_INPUT`).
fn source_buffer(device: &wgpu::Device, words: u64) -> wgpu::Buffer {
    let blas_input = if acceleration::supported(device) {
        wgpu::BufferUsages::BLAS_INPUT
    } else {
        wgpu::BufferUsages::empty()
    };
    crate::counters::buffer(
        device,
        &wgpu::BufferDescriptor {
            label: Some("scene ray source geometry and materials"),
            size: words * 4,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::VERTEX
                | wgpu::BufferUsages::COPY_DST
                | wgpu::BufferUsages::COPY_SRC
                | blas_input,
            mapped_at_creation: false,
        },
    )
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;

#[cfg(all(test, not(target_arch = "wasm32")))]
mod portable_tests;

#[cfg(all(test, not(target_arch = "wasm32")))]
mod secondary_normal_tests;

#[cfg(all(test, not(target_arch = "wasm32")))]
mod instance_tests;
