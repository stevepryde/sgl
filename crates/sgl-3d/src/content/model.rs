//! A model's geometry as a scene takes it, and the identities an asset
//! added whole receives.
use super::asset::Vertex;
use super::deformation::MeshDeformation;
use super::identity::{MaterialId, ModelId};

/// One mesh of a model: an indexed triangle list drawn with one material of
/// the scene.
#[derive(Clone)]
pub struct ModelMesh {
    pub vertices: Vec<Vertex>,
    /// Triangle-list indices into `vertices`, three per face.
    pub indices: Vec<u32>,
    pub material: MaterialId,
    /// Its skin and morph targets; rigid by default. A model with any
    /// deforming mesh deforms: its instances move and are posed with
    /// `Scene::set_instance_deformation`.
    pub deformation: MeshDeformation,
}

/// The identities `Scene::add_asset` gave an asset's content: its material
/// `i` is `materials[i]`, its meshes are `model`'s, in order (none where
/// all its mesh nodes are instanced), and its instanced mesh `i`
/// (`Asset::instanced`) is `instanced[i]`'s.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssetIds {
    pub materials: Vec<MaterialId>,
    pub model: ModelId,
    pub instanced: Vec<ModelId>,
}
