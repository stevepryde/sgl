//! Prepared geometry (the architecture's "Prepared geometry"): everything
//! of building a model that depends only on its meshes, done without a
//! device or a scene, so a game prepares its models on whichever threads
//! it chooses and `Scene::add_model` and `set_model` only place and copy
//! them (`models`).
use super::SceneError;
use super::deformation::{self, PreparedDeformation};
use super::mesh_ranges::{INDICES_PER_LEAF, MeshRanges};
use super::rays::{self, PreparedRayModel, RayMesh};
use super::shadow_clusters::ClusteredIndices;
use crate::asset::{self, Vertex};
use crate::content::identity::MaterialId;
use crate::content::model::ModelMesh;
use crate::counters::{BuildStep, step};
use crate::shading::culling::MAX_MESH_SECTIONS;
use crate::shading::vertex::CasterVertex;
use glam::Vec3;

/// A model's geometry, prepared for `Scene::add_model` or `Scene::set_model`
/// by `PreparedModel::new`, which needs no device and no scene: build it on
/// any thread and give it to the scene on the one that edits it. The
/// operation consumes it.
pub struct PreparedModel {
    pub(super) bounds: [Vec3; 2],
    pub(super) meshes: Vec<PreparedMesh>,
    pub(super) rays: PreparedRayModel,
    pub(super) deformation: Option<PreparedDeformation>,
}

/// One mesh of a prepared model.
pub(super) struct PreparedMesh {
    pub material: MaterialId,
    /// Every vertex has an authored tangent frame.
    pub tangents: bool,
    /// Its shadow casters' positions; none when its model deforms, whose
    /// instances' casters read their deformed positions from the ray source.
    pub positions: Vec<CasterVertex>,
    pub indices: Vec<u32>,
    pub ranges: MeshRanges,
    /// None without triangles.
    pub clusters: Option<ClusteredIndices>,
}

/// `vertices` and `indices` as a mesh: indices name vertices, positions are
/// finite and normals finite and not zero, as a packed vertex's frame needs
/// (`shading::packed_vertex`). Returns whether every vertex has an authored
/// tangent frame.
pub(super) fn validate_geometry(vertices: &[Vertex], indices: &[u32]) -> Result<bool, SceneError> {
    if indices
        .iter()
        .any(|&index| index as usize >= vertices.len())
    {
        return Err(SceneError::IndexOutOfRange);
    }
    if !vertices.iter().all(|vertex| {
        vertex.position.iter().all(|x| x.is_finite())
            && Vec3::from_array(vertex.normal)
                .as_dvec3()
                .try_normalize()
                .is_some()
    }) {
        return Err(SceneError::NonFiniteGeometry);
    }
    Ok(asset::tangent_frames(vertices))
}

/// Whether a mesh of `triangles` holds at most `MAX_MESH_SECTIONS`
/// sections, the most a GPU-built list's section cull strides over.
pub(crate) fn fits_sections(triangles: usize) -> bool {
    triangles.div_ceil(INDICES_PER_LEAF / 3) <= MAX_MESH_SECTIONS as usize
}

impl PreparedModel {
    /// `meshes`, an ordered list each drawn with a material of the scene
    /// that will take them, prepared: validated (indices name vertices,
    /// positions are finite, a deformation fits its vertices, a mesh holds
    /// at most 65,536 sections of 128 triangles), with each mesh's culling
    /// hierarchy, whose leaves are its sections, and shadow-caster clusters,
    /// the model's BVH, and its ray-source words (its section tables among
    /// them) and shadow-caster geometry packed. What
    /// needs the scene or the device (its materials, the device's limits)
    /// is checked when it is added.
    pub fn new(meshes: Vec<ModelMesh>) -> Result<Self, SceneError> {
        let tangents = step(BuildStep::Validate, || {
            if !meshes
                .iter()
                .all(|mesh| fits_sections(mesh.indices.len() / 3))
            {
                return Err(SceneError::TooManySections);
            }
            let tangents = meshes
                .iter()
                .map(|mesh| validate_geometry(&mesh.vertices, &mesh.indices))
                .collect::<Result<Vec<_>, _>>()?;
            deformation::validate(&meshes)?;
            Ok::<_, SceneError>(tangents)
        })?;
        let ranges: Vec<_> = step(BuildStep::Ranges, || {
            meshes
                .iter()
                .map(|mesh| MeshRanges::new(&mesh.vertices, &mesh.indices))
                .collect()
        });
        let ray_meshes: Vec<_> = meshes
            .iter()
            .zip(&ranges)
            .map(|(mesh, ranges)| RayMesh {
                vertices: &mesh.vertices,
                indices: &mesh.indices,
                ranges,
            })
            .collect();
        let rays = rays::prepare_model(&ray_meshes)?;
        let deformation = step(BuildStep::Pack, || PreparedDeformation::new(&meshes));
        let deforms = deformation.is_some();
        let bounds = meshes.iter().flat_map(|mesh| &mesh.vertices).fold(
            [Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY)],
            |bounds, vertex| {
                let p = Vec3::from_array(vertex.position);
                [bounds[0].min(p), bounds[1].max(p)]
            },
        );
        let meshes = meshes
            .into_iter()
            .zip(tangents)
            .zip(ranges)
            .map(|((mesh, tangents), ranges)| {
                let positions = step(BuildStep::Pack, || {
                    if deforms {
                        Vec::new()
                    } else {
                        mesh.vertices
                            .iter()
                            .map(|vertex| CasterVertex {
                                position: vertex.position,
                            })
                            .collect()
                    }
                });
                PreparedMesh {
                    material: mesh.material,
                    tangents,
                    positions,
                    ranges,
                    clusters: step(BuildStep::Clusters, || {
                        ClusteredIndices::new(&mesh.vertices, &mesh.indices)
                    }),
                    indices: mesh.indices,
                }
            })
            .collect();
        Ok(Self {
            bounds,
            meshes,
            rays,
            deformation,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::identity::Identity;
    use wasm_bindgen_test::wasm_bindgen_test;

    /// One mesh of triangles at `vertices`, three to a triangle.
    fn model(vertices: Vec<Vertex>) -> Result<PreparedModel, SceneError> {
        meshes(vec![vertices])
    }

    /// A mesh of triangles at each of `meshes`' vertices, three to a
    /// triangle.
    fn meshes(meshes: Vec<Vec<Vertex>>) -> Result<PreparedModel, SceneError> {
        PreparedModel::new(
            meshes
                .into_iter()
                .map(|vertices| ModelMesh {
                    material: MaterialId::issue(0, 0),
                    indices: (0..vertices.len() as u32).collect(),
                    vertices,
                    deformation: Default::default(),
                })
                .collect(),
        )
    }

    fn vertex(normal: [f32; 3], lightmap_bounds: [f32; 4]) -> Vertex {
        Vertex {
            position: [0.; 3],
            normal,
            lightmap_bounds,
            ..bytemuck::Zeroable::zeroed()
        }
    }

    // Plausible defects: a vertex whose normal has no frame reaching the
    // encoder, which cannot pack it; a mesh past what a packed vertex's
    // 16-bit chart index can name accepted, aliasing charts, or one at the
    // limit refused (an off-by-one); the limit counted across a model's
    // meshes rather than within each, which refused Hyperdrive's courses
    // (#182); or the refusal naming another mesh. The oracle is the Vertex
    // encoding's contract: a zero or non-finite normal refuses the mesh as a
    // non-finite position does, and each mesh may name up to 65,536
    // distinct chart bounds, a refusal naming the mesh's index. CPU only.
    #[wasm_bindgen_test(unsupported = test)]
    fn unpackable_vertices_are_refused() {
        let unit = [0., 0., 1.];
        assert!(model(vec![vertex(unit, [0.; 4]); 3]).is_ok());
        for normal in [[0.; 3], [f32::NAN, 0., 1.], [f32::INFINITY, 0., 0.]] {
            assert!(matches!(
                model(vec![
                    vertex(unit, [0.; 4]),
                    vertex(normal, [0.; 4]),
                    vertex(unit, [0.; 4])
                ]),
                Err(SceneError::NonFiniteGeometry)
            ));
        }
        // `count` distinct chart bounds, offset by `first` so that no two
        // meshes name the same one.
        let charts = |first: usize, count: usize| {
            (0..count.div_ceil(3) * 3)
                .map(|index| {
                    let chart = first + index.min(count - 1);
                    vertex(unit, [chart as f32, 0., 1., 1.])
                })
                .collect::<Vec<_>>()
        };
        assert!(model(charts(0, 1 << 16)).is_ok());
        assert!(matches!(
            model(charts(0, (1 << 16) + 1)),
            Err(SceneError::TooManyLightmapCharts { mesh: 0 })
        ));
        assert!(meshes(vec![charts(0, 40_000), charts(40_000, 40_000)]).is_ok());
        assert!(matches!(
            meshes(vec![charts(0, 3), charts(3, (1 << 16) + 1)]),
            Err(SceneError::TooManyLightmapCharts { mesh: 1 })
        ));
    }
}
