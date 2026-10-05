//! Prepared geometry (the architecture's "Prepared geometry"): everything
//! of building a model that depends only on its meshes, done without a
//! device or a scene, so a game prepares its models on whichever threads
//! it chooses and `Scene::add_model` and `set_model` only place and copy
//! them (`models`).
use super::SceneError;
use super::deformation::{self, PreparedDeformation};
use super::mesh_ranges::MeshRanges;
use super::rays::{self, PreparedRayModel, RayMesh};
use super::shadow_clusters::ClusteredIndices;
use crate::asset::{self, Vertex};
use crate::content::identity::MaterialId;
use crate::content::model::ModelMesh;
use crate::counters::{BuildStep, step};
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

/// `vertices` and `indices` as a mesh: indices name vertices and positions
/// are finite. Returns whether every vertex has an authored tangent frame.
pub(super) fn validate_geometry(vertices: &[Vertex], indices: &[u32]) -> Result<bool, SceneError> {
    if indices
        .iter()
        .any(|&index| index as usize >= vertices.len())
    {
        return Err(SceneError::IndexOutOfRange);
    }
    if !vertices
        .iter()
        .all(|vertex| vertex.position.iter().all(|x| x.is_finite()))
    {
        return Err(SceneError::NonFiniteGeometry);
    }
    Ok(asset::tangent_frames(vertices))
}

impl PreparedModel {
    /// `meshes`, an ordered list each drawn with a material of the scene
    /// that will take them, prepared: validated (indices name vertices,
    /// positions are finite, a deformation fits its vertices), with each
    /// mesh's culling hierarchy and shadow-caster clusters, the model's BVH,
    /// and its ray-source words and shadow-caster geometry packed. What
    /// needs the scene or the device (its materials, the device's limits)
    /// is checked when it is added.
    pub fn new(meshes: Vec<ModelMesh>) -> Result<Self, SceneError> {
        let tangents = step(BuildStep::Validate, || {
            let tangents = meshes
                .iter()
                .map(|mesh| validate_geometry(&mesh.vertices, &mesh.indices))
                .collect::<Result<Vec<_>, _>>()?;
            deformation::validate(&meshes)?;
            Ok::<_, SceneError>(tangents)
        })?;
        let ray_meshes: Vec<_> = meshes
            .iter()
            .map(|mesh| RayMesh {
                vertices: &mesh.vertices,
                indices: &mesh.indices,
            })
            .collect();
        let rays = rays::prepare_model(&ray_meshes);
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
            .map(|(mesh, tangents)| {
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
                    ranges: step(BuildStep::Ranges, || {
                        MeshRanges::new(&mesh.vertices, &mesh.indices)
                    }),
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
