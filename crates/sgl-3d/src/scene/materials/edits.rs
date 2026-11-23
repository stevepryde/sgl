//! The `Scene` operations that add, read, edit and remove materials.
use super::super::candidates::CandidateMesh;
use super::super::{Scene, SceneError};
use crate::asset::{Image, Material as AuthoredMaterial};
use crate::content::identity::{MaterialId, ModelId};
use crate::content::material::SurfaceMaterial;

impl Scene {
    /// Adds `materials`, whose texture indices index `images`. Materials
    /// added together share the textures they use; a texture lives while a
    /// material uses it. A decoded image's mips are filtered here; a
    /// compressed one's are uploaded as stored. Returns their identities in
    /// order.
    pub fn add_materials(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        materials: &[AuthoredMaterial],
        images: &[Image],
    ) -> Result<Vec<MaterialId>, SceneError> {
        let ids = self
            .materials
            .add(device, queue, &mut self.rays, materials, images);
        if ids.is_ok() {
            self.edited();
        }
        // A failure after the source grew still leaves it grown.
        self.refresh_scene_group(device);
        ids
    }

    /// A material's current values.
    pub fn material(&self, id: MaterialId) -> Result<SurfaceMaterial, SceneError> {
        Ok(self.materials.get(id)?.values)
    }

    /// Replaces a material's values, its alpha mode among them. Anisotropy
    /// needs authored tangent frames on every mesh drawn with the material.
    /// Textures stay as added; `emission_render_texture` names a render
    /// texture of this scene or none (`SceneError::UnknownRenderTexture`),
    /// and `planar_reflector` a planar reflector of this scene or none
    /// (`UnknownPlanarReflector`), on an opaque or masked material
    /// (`BlendedPlanarReflector`).
    /// Non-finite `normal_scale`, `coat_normal_scale` or `bump_scale`
    /// returns `SceneError::InvalidMaterialFactors` without changing the material.
    pub fn set_material(
        &mut self,
        queue: &wgpu::Queue,
        id: MaterialId,
        values: SurfaceMaterial,
    ) -> Result<(), SceneError> {
        let old = self.materials.get(id)?.values;
        if old != values {
            self.edited();
        }
        // Blended and transmissive meshes have no draw candidates: a
        // material that becomes or stops being either adds or removes its
        // users' instances' ones.
        let blending = old.blended() != values.blended();
        let users: Vec<ModelId> = self.materials.get(id)?.users.keys().copied().collect();
        if blending {
            // Each user model's instances, in the order they are placed
            // again below, take the material's meshes as blended or not.
            let meshes: Vec<Vec<CandidateMesh>> = users
                .iter()
                .map(|&model| {
                    let mut meshes = self.candidate_meshes(model, self.drawn_model(model));
                    let owner = &self.drawn_model(model).meshes;
                    for (mesh, shape) in owner.iter().zip(&mut meshes) {
                        if mesh.material == id {
                            shape.blended = values.blended();
                        }
                    }
                    meshes
                })
                .collect();
            let plan: Vec<_> = users
                .iter()
                .zip(&meshes)
                .map(|(&model, meshes)| (model, meshes.as_slice(), None))
                .collect();
            if !self.candidates_fit(&plan) {
                return Err(SceneError::DeviceLimit);
            }
        }
        self.materials.set(
            queue,
            (
                &self.rays,
                &self.shaders,
                &self.render_textures,
                &self.planar_reflectors,
            ),
            id,
            values,
        )?;
        let shader = |values: &SurfaceMaterial| values.shader.map(|shader| shader.shader);
        self.shaders.count(shader(&old), -1);
        self.shaders.count(shader(&values), 1);
        self.render_textures.count(old.emission_render_texture, -1);
        self.render_textures
            .count(values.emission_render_texture, 1);
        self.planar_reflectors.count(old.planar_reflector, -1);
        self.planar_reflectors.count(values.planar_reflector, 1);
        if blending || std::mem::discriminant(&old.alpha) != std::mem::discriminant(&values.alpha) {
            self.models.classify_users(id, &self.materials);
        }
        if blending {
            self.place_candidates_of(&users);
        }
        self.candidates
            .material_changed(id, super::super::candidates::look(self.materials.get(id)?));
        // What its users draw appearing or departing at rest re-arms
        // temporal rejection, as a geometry replacement does.
        if old.draws_other_geometry(&values) {
            for &model in &users {
                self.instances.geometry_changed(model);
            }
        }
        Ok(())
    }

    /// Removes a material no model's mesh uses.
    pub fn remove_material(&mut self, id: MaterialId) -> Result<(), SceneError> {
        self.edited();
        let values = self.materials.get(id)?.values;
        self.materials.remove(&mut self.rays, id)?;
        self.shaders
            .count(values.shader.map(|shader| shader.shader), -1);
        self.render_textures
            .count(values.emission_render_texture, -1);
        self.planar_reflectors.count(values.planar_reflector, -1);
        Ok(())
    }
}
