//! A model's words in the ray source, prepared without the source
//! (`prepare_model`): its mesh records, chart table, section tables, packed
//! vertices, indices and BVH, addressed from zero, which
//! `SceneRays::place_model` places and rebases.
use super::charts::{CHART_WORDS, ChartTables, chart_tables};
use super::{RayMeshWords, RayModel, VERTEX_WORDS, bvh, words};
use crate::asset::Vertex;
use crate::counters::{BuildStep, step};
use crate::scene::SceneError;
use crate::scene::mesh_ranges::MeshRanges;
use crate::shading::packed_vertex::{self, PackedVertex, UvRect};
use glam::Vec3;
use std::ops::Range;

/// One mesh of a model being prepared: its geometry and its range
/// hierarchy, whose leaves are its sections.
pub(crate) struct RayMesh<'a> {
    pub vertices: &'a [Vertex],
    pub indices: &'a [u32],
    pub ranges: &'a MeshRanges,
    /// Each vertex's shader data, or none
    /// (`PreparedModel::with_shader_data`).
    pub shader_data: &'a [[f32; 4]],
}

/// A model's words prepared without the source (`prepare_model`): its mesh
/// records, vertices, indices and BVH, addressed from zero, with its mesh
/// records' material words still to name.
pub(crate) struct PreparedRayModel {
    words: Vec<u32>,
    meshes: usize,
    /// Where its BVH starts in `words`, and its root, zero when it has no
    /// triangles.
    bvh: usize,
    root: u32,
    /// Where each mesh's vertices and indices lie in `words`.
    mesh_words: Vec<RayMeshWords>,
}

/// `meshes`' words, addressed from zero, which `SceneRays::place_model`
/// places: its mesh records, each mesh's chart table, each mesh's section
/// table, each mesh's packed vertices (`shading::packed_vertex`), from a
/// multiple of `VERTEX_WORDS`, indices and shader data, where it has any,
/// and its BVH. The geometry is
/// validated: indices name vertices, positions are finite and normals are
/// finite and not zero. Refuses a mesh whose vertices name more than 65,536
/// distinct lightmap chart bounds.
pub(crate) fn prepare_model(meshes: &[RayMesh<'_>]) -> Result<PreparedRayModel, SceneError> {
    let table_word = meshes.len() * MESH_WORDS;
    let vertex_block = |word: usize| word.next_multiple_of(VERTEX_WORDS as usize);
    let (mut words, mesh_words, len) = step(BuildStep::Pack, || {
        let ChartTables {
            entries: table,
            starts,
            indices: charts,
        } = chart_tables(meshes)?;
        let sections: Vec<Vec<SectionRecord>> = meshes
            .iter()
            .map(|mesh| {
                mesh.ranges
                    .sections()
                    .map(|section| SectionRecord::of(mesh.indices, section))
                    .collect()
            })
            .collect();
        let section_words: usize = sections
            .iter()
            .map(|table| table.len() * SECTION_WORDS)
            .sum();
        let len = meshes.iter().fold(
            table_word + table.len() * CHART_WORDS + section_words,
            |len, mesh| {
                vertex_block(len)
                    + mesh.vertices.len() * words::<PackedVertex>()
                    + mesh.indices.len()
                    + mesh.shader_data.len() * words::<[f32; 4]>()
            },
        );
        let mut words = vec![0u32; table_word];
        words.reserve(len - words.len());
        words.extend_from_slice(bytemuck::cast_slice(&table));
        let mut section_tables = Vec::with_capacity(meshes.len());
        for table in &sections {
            section_tables.push(words.len() as u32);
            words.extend_from_slice(bytemuck::cast_slice(table));
        }
        let mut first_vertex = 0;
        let mut mesh_words = Vec::with_capacity(meshes.len());
        for (index, (mesh, charts)) in meshes.iter().zip(&charts).enumerate() {
            let uv = UvRect::of(mesh.vertices);
            words.resize(vertex_block(words.len()), 0);
            let vertices = words.len() as u32;
            for (vertex, &chart) in mesh.vertices.iter().zip(charts) {
                words.extend_from_slice(bytemuck::cast_slice(&[packed_vertex::pack(
                    vertex, &uv, chart,
                )]));
            }
            let indices = words.len() as u32;
            words.extend_from_slice(mesh.indices);
            let shader_data = if mesh.shader_data.is_empty() {
                0
            } else {
                words.len() as u32
            };
            words.extend_from_slice(bytemuck::cast_slice(mesh.shader_data));
            mesh_words.push(RayMeshWords {
                vertices,
                vertex_count: mesh.vertices.len() as u32,
                indices,
                index_count: mesh.indices.len() as u32,
            });
            let at = index * MESH_WORDS;
            words[at..at + MESH_WORDS].copy_from_slice(bytemuck::cast_slice(&[MeshRecord {
                vertices,
                indices,
                material_word: 0,
                first_vertex,
                charts: (table_word + starts[index] * CHART_WORDS) as u32,
                uv_rect: uv.words(),
                sections: section_tables[index],
                section_count: sections[index].len() as u32,
                shader_data,
            }]));
            first_vertex += mesh.vertices.len() as u32;
        }
        Ok::<_, SceneError>((words, mesh_words, len))
    })?;
    debug_assert_eq!(words.len(), len, "a model's geometry fills its words");
    // Its BVH's size follows its splits, so it grows the words it follows.
    let bvh = words.len();
    let root = step(BuildStep::RayBvh, || bvh::append(meshes, &mut words, 0));
    Ok(PreparedRayModel {
        words,
        meshes: meshes.len(),
        bvh,
        root,
        mesh_words,
    })
}

/// Names each mesh's material record in the mesh records `records` holds,
/// one material word per record, and adds `base` to the words where each
/// record's vertices, indices, chart table, section table and shader data
/// (where it has any) start; a section's first index is relative to its
/// mesh's indices and needs none.
fn rebase_records(records: &mut [u32], base: u32, materials: &[u32]) {
    let records: &mut [MeshRecord] = bytemuck::cast_slice_mut(records);
    assert_eq!(
        records.len(),
        materials.len(),
        "a material word for each mesh"
    );
    for (record, &material_word) in records.iter_mut().zip(materials) {
        record.vertices += base;
        record.indices += base;
        record.charts += base;
        record.sections += base;
        if record.shader_data != 0 {
            record.shader_data += base;
        }
        record.material_word = material_word;
    }
}

impl PreparedRayModel {
    /// Its words, which the scene writes where `SceneRays::place_model`
    /// placed them.
    pub fn words(&self) -> &[u32] {
        &self.words
    }

    /// Its words' length.
    pub(super) fn len(&self) -> usize {
        self.words.len()
    }

    /// Readies its words to write at `base`: names each mesh's material
    /// record (`materials`, in mesh order) and adds `base` to every word
    /// that addresses the source. Returns what rays read of it and where
    /// each mesh's vertices and indices lie.
    pub(super) fn rebase(&mut self, base: u32, materials: &[u32]) -> (RayModel, Vec<RayMeshWords>) {
        rebase_records(&mut self.words[..self.meshes * MESH_WORDS], base, materials);
        bvh::rebase(&mut self.words[self.bvh..], base);
        let ray = RayModel {
            mesh_word: base,
            bvh_root: if self.root == 0 { 0 } else { self.root + base },
        };
        let meshes = self
            .mesh_words
            .iter()
            .map(|mesh| RayMeshWords {
                vertices: mesh.vertices + base,
                indices: mesh.indices + base,
                ..*mesh
            })
            .collect();
        (ray, meshes)
    }
}

/// A mesh's record in the source: where its packed vertices and indices
/// start, its material's record, its first vertex among its model's, where
/// its own chart table starts, the rectangle its packed UVs span
/// (`UvRect::words`), where its section table starts, with its sections,
/// which the GPU draw lists cull by, and where its shader data starts, 0
/// without any.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct MeshRecord {
    pub vertices: u32,
    pub indices: u32,
    pub material_word: u32,
    pub first_vertex: u32,
    pub charts: u32,
    pub uv_rect: [f32; 4],
    pub sections: u32,
    pub section_count: u32,
    pub shader_data: u32,
}

/// A section table's entry: a leaf of its mesh's range hierarchy
/// (`MeshRanges::sections`), at most `SECTION_VERTICES` / 3 triangles in the
/// mesh's own order, with its bounds in the model's space, its first index,
/// relative to its mesh's indices, and its triangles, with `SECTION_PAIRED`
/// where they pair.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct SectionRecord {
    pub bounds_min: [f32; 3],
    pub bounds_max: [f32; 3],
    pub first_index: u32,
    pub triangles: u32,
}

/// `SectionRecord::triangles`' bit marking a section whose triangles pair:
/// each even triangle and the next are (a, b, c) and (a, c, d), a quad split
/// along its first diagonal, as Blender's tessellation and quad meshers
/// emit it, a lone last triangle aside. A GPU-built cascade draws such a
/// section indexed over one fixed pattern (`shading::culling::PAIRED_INDICES`),
/// so the post-transform cache shades each pair's shared corners once. Its
/// count is the bits below.
pub(crate) const SECTION_PAIRED: u32 = 1 << 31;

/// Whether `indices`, a section's, pair (`SECTION_PAIRED`).
fn paired(indices: &[u32]) -> bool {
    indices
        .chunks_exact(6)
        .all(|pair| pair[3] == pair[0] && pair[4] == pair[2])
}

impl SectionRecord {
    /// Section `range` of a mesh with `indices`, whose bounds are `bounds`.
    fn of(indices: &[u32], (bounds, range): ([Vec3; 2], Range<u32>)) -> Self {
        let section = &indices[range.start as usize..range.end as usize];
        Self {
            bounds_min: bounds[0].to_array(),
            bounds_max: bounds[1].to_array(),
            first_index: range.start,
            triangles: ((range.end - range.start) / 3)
                | if paired(section) { SECTION_PAIRED } else { 0 },
        }
    }
}

/// A section table entry's words.
const SECTION_WORDS: usize = std::mem::size_of::<SectionRecord>() / 4;

/// A mesh record's words; a model's records are consecutive.
pub(super) const MESH_WORDS: usize = std::mem::size_of::<MeshRecord>() / 4;

#[cfg(test)]
mod record_tests {
    use super::{
        CHART_WORDS, MESH_WORDS, MeshRecord, RayMesh, SECTION_PAIRED, SECTION_WORDS, SectionRecord,
        VERTEX_WORDS, prepare_model, rebase_records,
    };
    use crate::asset::Vertex;
    use crate::scene::mesh_ranges::MeshRanges;
    use crate::shading::packed_vertex::PackedVertex;
    use glam::Vec3;
    use wasm_bindgen_test::wasm_bindgen_test;

    // Plausible defects: a rebased mesh record that names another mesh's
    // vertices, indices or chart table, or the block's first words, so a
    // ray or pulled pass reads the wrong geometry (as when the base is added
    // to the wrong field, or a record is skipped); a chart that several
    // meshes name indexed in only the first one's table; a material word
    // given to the wrong mesh; or a section table that is not the mesh's own, or
    // whose sections skip, repeat or misbound its triangles, so a GPU-built
    // list draws another mesh's triangles or culls one it shows. The oracle
    // is the test's own meshes: each rebased record, less the base, must
    // start at its mesh's own vertices (their positions, which pack as
    // given) and indices in the prepared words, name through its vertices'
    // chart indices its mesh's own lightmap bounds in its chart table, carry
    // its mesh's material word, and name a section table whose sections of at
    // most 128 triangles each take up the mesh's indices in order, once,
    // each bounding its own triangles' positions. A BLAS reads
    // a mesh's positions from the same words in whole packed-vertex strides
    // (the architecture's Hardware ray tracing), so each mesh's vertex block
    // must start at a multiple of the stride, and the words the scene gives
    // a BLAS must be the record's. CPU only.
    #[wasm_bindgen_test(unsupported = test)]
    fn rebased_records_address_their_own_geometry() {
        // Each mesh's vertices, indices and chart bounds differ from every
        // other's from their first word; its odd vertices name a chart every
        // mesh names.
        let mesh = |count: u32, salt: f32| -> (Vec<Vertex>, Vec<u32>) {
            let vertices = (0..count)
                .map(|index| Vertex {
                    position: [salt, index as f32, -salt],
                    normal: [0., 0., 1.],
                    lightmap_bounds: if index % 2 == 1 {
                        [0., 0., 1., 1.]
                    } else {
                        [salt, 0., salt + 0.5, 1.]
                    },
                    ..bytemuck::Zeroable::zeroed()
                })
                .collect();
            let indices = (0..3 * salt as u32)
                .map(|index| count - 1 - index % count)
                .collect();
            (vertices, indices)
        };
        // The last mesh holds three sections, the last a partial one.
        let meshes = [mesh(3, 1.), mesh(5, 2.), mesh(4, 3.), mesh(7, 300.)];
        let ranges: Vec<_> = meshes
            .iter()
            .map(|(vertices, indices)| MeshRanges::new(vertices, indices))
            .collect();
        let rays: Vec<_> = meshes
            .iter()
            .zip(&ranges)
            .map(|((vertices, indices), ranges)| RayMesh {
                vertices,
                indices,
                ranges,
                shader_data: &[],
            })
            .collect();
        let mut prepared = prepare_model(&rays).unwrap();
        // A model's range starts at a multiple of the stride.
        let (base, materials) = (8_750 * VERTEX_WORDS, [11, 22, 33, 44]);
        rebase_records(
            &mut prepared.words[..meshes.len() * MESH_WORDS],
            base,
            &materials,
        );
        let records: &[MeshRecord] =
            bytemuck::cast_slice(&prepared.words[..meshes.len() * MESH_WORDS]);
        let packed_words = std::mem::size_of::<PackedVertex>() / 4;
        for ((((vertices, indices), record), material), words) in meshes
            .iter()
            .zip(records)
            .zip(materials)
            .zip(&prepared.mesh_words)
        {
            assert_eq!(record.vertices % VERTEX_WORDS, 0, "an aligned vertex block");
            assert_eq!(
                [words.vertices + base, words.indices + base],
                [record.vertices, record.indices]
            );
            assert_eq!(
                [words.vertex_count, words.index_count],
                [vertices.len(), indices.len()].map(|count| count as u32)
            );
            let first = (record.vertices - base) as usize;
            let charts = (record.charts - base) as usize;
            for (index, vertex) in vertices.iter().enumerate() {
                let packed: &PackedVertex = bytemuck::from_bytes(bytemuck::cast_slice(
                    &prepared.words[first + index * packed_words..][..packed_words],
                ));
                assert_eq!(packed.position, vertex.position);
                let chart = (packed.angle_chart >> 16) as usize;
                let bounds: &[f32] = bytemuck::cast_slice(
                    &prepared.words[charts + chart * CHART_WORDS..][..CHART_WORDS],
                );
                assert_eq!(bounds, vertex.lightmap_bounds);
            }
            let at = (record.indices - base) as usize;
            assert_eq!(&prepared.words[at..at + indices.len()], &indices[..]);
            assert_eq!(record.material_word, material);
            let table = (record.sections - base) as usize;
            let sections: &[SectionRecord] = bytemuck::cast_slice(
                &prepared.words[table..][..record.section_count as usize * SECTION_WORDS],
            );
            let mut next = 0;
            for section in sections {
                assert_eq!(
                    section.first_index, next,
                    "sections take the indices in order"
                );
                let triangles = section.triangles & !SECTION_PAIRED;
                assert!((1..=128).contains(&triangles));
                next += triangles * 3;
                let [min, max] = [section.bounds_min, section.bounds_max].map(Vec3::from_array);
                for &index in &indices[section.first_index as usize..next as usize] {
                    let p = Vec3::from_array(vertices[index as usize].position);
                    assert!(
                        p.cmpge(min).all() && p.cmple(max).all(),
                        "{p} outside its section"
                    );
                }
            }
            assert_eq!(next as usize, indices.len(), "sections take every index");
        }
    }
}
