//! Caller-authored spatial mesh alternatives, bounded in primary-camera pixels.
use super::identity::ModelId;

/// An alternative to one mesh: mesh `mesh` of `model`, drawn with the base
/// mesh's material and transform. Split large assets into spatial meshes
/// before adding them; selection is per mesh.
#[derive(Clone, Copy, Debug)]
pub struct MeshLod {
    pub model: ModelId,
    pub mesh: usize,
    /// Maximum displacement in the base mesh's local metres. The caller must
    /// also preserve material, UV, normals, tangent frames, color and baked-light interpolation.
    /// Empty replacements require a bound on the entire disappearing feature.
    pub max_error: f32,
}
