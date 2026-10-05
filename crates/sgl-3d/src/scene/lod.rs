//! Registration of a mesh's alternatives (`lod::MeshLod`); the camera's
//! choice among them is `view::lod`.
use super::{Scene, SceneError};
use crate::content::identity::ModelId;
use crate::lod::MeshLod;

impl Scene {
    /// Register alternatives ordered from detailed to coarse. Selection chooses
    /// the last admissible entry, conservatively bounded to half a primary pixel.
    /// Empty lists restore full detail. No geometry is generated or modified.
    /// Alternatives use the base mesh's transform and material binding, so
    /// lightmapped materials stay lightmapped. Their vertices must share its space.
    /// Primary beauty, depth and reflection source raster use the same selection.
    /// Probes, shadows and scene rays retain full-detail geometry, independently
    /// of the primary camera. Sampled/hardware reflection selections retain full
    /// primary geometry too, because their receiver/source ownership uses exact
    /// ray triangle identities. Alternatives are meshes of other models,
    /// since a model draws all its meshes; a model named here cannot be
    /// replaced or removed while it is, and replacing `model` clears its
    /// alternatives.
    pub fn set_mesh_lods(
        &mut self,
        model: ModelId,
        mesh: usize,
        alternatives: Vec<MeshLod>,
    ) -> Result<(), SceneError> {
        self.edited();
        let owner = self.models.get(model)?;
        let base = owner.meshes.get(mesh).ok_or(SceneError::MissingMesh)?;
        // A deforming instance's vertices are its model's, deformed.
        if owner.deformation.is_some() {
            return Err(SceneError::DeformingModel);
        }
        let material = base.material;
        let previous_untangented = base
            .lods
            .iter()
            .filter(|lod| !self.models.lod_tangents(lod))
            .count() as u32;
        // Preserve an existing tangent-capable material boundary even while
        // isotropic, because a later material edit may activate anisotropy.
        let requires_tangents = self.materials.get(material)?.untangented == previous_untangented;
        for lod in &alternatives {
            if !lod.max_error.is_finite() || lod.max_error < 0. {
                return Err(SceneError::InvalidLod);
            }
            if lod.model == model {
                return Err(SceneError::LodInSameModel);
            }
            if self.models.get(lod.model)?.deformation.is_some() {
                return Err(SceneError::DeformingModel);
            }
            let alternative = self
                .models
                .get(lod.model)?
                .meshes
                .get(lod.mesh)
                .ok_or(SceneError::MissingMesh)?;
            if requires_tangents && !alternative.tangents {
                return Err(SceneError::MissingAnisotropyTangents);
            }
        }
        let previous = std::mem::take(&mut self.models.get_mut(model)?.meshes[mesh].lods);
        self.models
            .clear_lods(&mut self.materials, material, &previous);
        for lod in &alternatives {
            let untangented = !self.models.lod_tangents(lod);
            self.models.get_mut(lod.model)?.lod_uses += 1;
            self.materials.get_mut(material)?.untangented += u32::from(untangented);
        }
        self.models.get_mut(model)?.meshes[mesh].lods = alternatives;
        Ok(())
    }
}
