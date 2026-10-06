//! One BLAS's build: its geometries over the ray source, each opaque or
//! not under the form in effect, where each one's vertices and indices
//! start, whether the device holds it, and its creation.
use super::super::allocated;
use crate::content::identity::{InstanceId, ModelId};
use crate::scene::models::Model;
use crate::scene::rays::{RayMeshWords, VERTEX_WORDS};
use crate::shading::RayQueryForm;

/// Whether each of `model`'s geometries is built without `OPAQUE` under
/// `form`: a masked mesh's under the candidate form, whose query's loop
/// judges its candidates; none under the baseline, whose query forces
/// opacity.
pub(super) fn non_opaque(model: &Model, form: RayQueryForm) -> impl Iterator<Item = bool> + '_ {
    let candidates = form == RayQueryForm::Candidates;
    model
        .ray_masked
        .iter()
        .map(move |&masked| masked && candidates)
}

/// What a frame's BLAS build makes.
pub(super) enum Built {
    Model {
        id: ModelId,
        geometry: u64,
        vertices: u32,
    },
    Deformed {
        id: InstanceId,
        geometry: u64,
        revision: u64,
    },
}

/// Where one geometry's vertices and indices start in the ray source: its
/// first vertex in whole strides and its first index.
pub(super) struct GeometryStart {
    pub first_vertex: u32,
    pub first_index: u32,
}

/// One BLAS a frame builds: its geometries' sizes and starts, read with
/// `stride` bytes between vertices.
pub(in super::super) struct BlasBuild {
    pub(super) built: Built,
    pub(super) blas: wgpu::Blas,
    pub(super) sizes: Vec<wgpu::BlasTriangleGeometrySizeDescriptor>,
    pub(super) starts: Vec<GeometryStart>,
    pub(super) stride: u64,
}

impl BlasBuild {
    /// Its build entry over `source`, the ray source.
    pub fn entry<'a>(&'a self, source: &'a wgpu::Buffer) -> wgpu::BlasBuildEntry<'a> {
        wgpu::BlasBuildEntry {
            blas: &self.blas,
            geometry: wgpu::BlasGeometries::TriangleGeometries(
                self.sizes
                    .iter()
                    .zip(&self.starts)
                    .map(|(size, start)| wgpu::BlasTriangleGeometry {
                        size,
                        vertex_buffer: source,
                        first_vertex: start.first_vertex,
                        vertex_stride: self.stride,
                        index_buffer: Some(source),
                        first_index: Some(start.first_index),
                        transform_buffer: None,
                        transform_buffer_offset: None,
                    })
                    .collect(),
            ),
        }
    }

    /// Counts the build (`counters`).
    pub fn count(&self) {
        match self.built {
            Built::Model { vertices, .. } => crate::counters::blas_build(vertices),
            Built::Deformed { .. } => crate::counters::deformed_blas_build(),
        }
    }
}

/// Whether the device holds a BLAS over `meshes`: no more geometries than
/// `max_blas_geometry_count`, no more triangles than
/// `max_blas_primitive_count`, and, as wgpu 30 also checks each geometry's
/// vertex count against that limit (`wgpu-core` `device/ray_tracing.rs`
/// 93–98), no mesh of more vertices than it.
pub(super) fn fits(meshes: &[RayMeshWords], limits: &wgpu::Limits) -> bool {
    let triangles: u64 = meshes
        .iter()
        .map(|mesh| u64::from(mesh.index_count / 3))
        .sum();
    meshes.len() as u64 <= u64::from(limits.max_blas_geometry_count)
        && triangles <= u64::from(limits.max_blas_primitive_count)
        && meshes
            .iter()
            .all(|mesh| mesh.vertex_count <= limits.max_blas_primitive_count)
}

/// Each geometry's sizes over `meshes`: `Float32x3` positions, which need no
/// extended vertex format, and `Uint32` indices, each geometry `OPAQUE` but
/// those `non_opaque` names, as Wicked Engine clears `FLAG_OPAQUE` from an
/// alpha-tested material's geometry (`wiScene.cpp` 4232–4244). A mesh's indices
/// count its whole triangles only, as its BVH and culling ranges do: wgpu
/// refuses a count that is not a multiple of three (`wgpu-core`
/// `command/ray_tracing.rs` 755–760).
pub(super) fn sizes(
    meshes: &[RayMeshWords],
    non_opaque: impl Iterator<Item = bool>,
) -> Vec<wgpu::BlasTriangleGeometrySizeDescriptor> {
    meshes
        .iter()
        .zip(non_opaque)
        .map(
            |(mesh, non_opaque)| wgpu::BlasTriangleGeometrySizeDescriptor {
                vertex_format: wgpu::VertexFormat::Float32x3,
                vertex_count: mesh.vertex_count,
                index_format: Some(wgpu::IndexFormat::Uint32),
                index_count: Some(mesh.index_count / 3 * 3),
                flags: if non_opaque {
                    wgpu::AccelerationStructureGeometryFlags::empty()
                } else {
                    wgpu::AccelerationStructureGeometryFlags::OPAQUE
                },
            },
        )
        .collect()
}

/// The geometries `sizes` builds without `OPAQUE`.
pub(super) fn built_non_opaque(sizes: &[wgpu::BlasTriangleGeometrySizeDescriptor]) -> Vec<bool> {
    sizes
        .iter()
        .map(|size| {
            !size
                .flags
                .contains(wgpu::AccelerationStructureGeometryFlags::OPAQUE)
        })
        .collect()
}

/// A BLAS for `sizes` with `flags`, unless the device's memory cannot hold
/// it.
pub(super) fn create(
    device: &wgpu::Device,
    label: &str,
    sizes: &[wgpu::BlasTriangleGeometrySizeDescriptor],
    flags: wgpu::AccelerationStructureFlags,
) -> Option<wgpu::Blas> {
    allocated(device, || {
        device.create_blas(
            &wgpu::CreateBlasDescriptor {
                label: Some(label),
                flags,
                update_mode: wgpu::AccelerationStructureUpdateMode::Build,
            },
            wgpu::BlasGeometrySizeDescriptors::Triangles {
                descriptors: sizes.to_vec(),
            },
        )
    })
}

/// The build of `model`'s BLAS (`id`) under `form`, unless the device
/// cannot hold it. Bevy's flags (`blas.rs` 158–164): a game changes a rigid
/// model's geometry by replacing it, never by refitting it.
pub(super) fn model_build(
    device: &wgpu::Device,
    id: ModelId,
    model: &Model,
    form: RayQueryForm,
) -> Option<BlasBuild> {
    let sizes = sizes(&model.ray_meshes, non_opaque(model, form));
    let blas = create(
        device,
        "scene model BLAS",
        &sizes,
        wgpu::AccelerationStructureFlags::PREFER_FAST_TRACE
            | wgpu::AccelerationStructureFlags::ALLOW_COMPACTION,
    )?;
    let starts = model
        .ray_meshes
        .iter()
        .map(|mesh| {
            debug_assert_eq!(mesh.vertices % VERTEX_WORDS, 0, "a vertex block is aligned");
            GeometryStart {
                first_vertex: mesh.vertices / VERTEX_WORDS,
                first_index: mesh.indices,
            }
        })
        .collect();
    Some(BlasBuild {
        built: Built::Model {
            id,
            geometry: model.geometry,
            vertices: model.ray_meshes.iter().map(|mesh| mesh.vertex_count).sum(),
        },
        blas,
        sizes,
        starts,
        stride: u64::from(VERTEX_WORDS) * 4,
    })
}

#[cfg(test)]
mod tests {
    use super::sizes;
    use crate::scene::rays::RayMeshWords;
    use wasm_bindgen_test::wasm_bindgen_test;

    // Plausible defect: a mesh's raw index count given to its BLAS when it
    // is not a multiple of three, as a mesh the scene accepts may have (its
    // BVH and culling ranges drop the indices past its last whole
    // triangle): wgpu then refuses the frame's build (`InvalidIndexCount`),
    // an uncaptured error that panics the game. The oracle is wgpu's rule,
    // each geometry's count a multiple of three, holding each mesh's whole
    // triangles.
    #[wasm_bindgen_test(unsupported = test)]
    fn blas_geometries_hold_whole_triangles() {
        let mesh = |index_count| RayMeshWords {
            vertices: 0,
            vertex_count: 4,
            indices: 0,
            index_count,
        };
        let counts: Vec<_> = sizes(
            &[mesh(7), mesh(6), mesh(2), mesh(0)],
            [false; 4].into_iter(),
        )
        .iter()
        .map(|size| size.index_count)
        .collect();
        assert_eq!(counts, [Some(6), Some(6), Some(0), Some(0)]);
    }
}
