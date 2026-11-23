//! The surfaces of a scene whose material names something a frame tests
//! against its views on the CPU: a render texture (a screen) or a planar
//! reflector (a mirror).
use super::Scene;
use crate::content::material::SurfaceMaterial;

/// A mesh of a visible instance drawn with a material that names `name`.
pub(crate) struct Named<K> {
    pub name: K,
    /// Its instance's pose.
    pub pose: glam::Mat4,
    /// Its mesh's bounds in its model's space, with those of its levels of
    /// detail, grown by what its material's shader may move a vertex.
    pub bounds: [glam::Vec3; 2],
    /// Its material's visibility group.
    group: u32,
}

impl<K> Named<K> {
    /// Whether a view whose visibility mask is `mask` draws it.
    pub fn enabled(&self, mask: u32) -> bool {
        super::materials::group_enabled(self.group, Some(mask))
    }
}

impl Scene {
    /// The surfaces whose material `names` something: none without such a
    /// material. The cull draws a mesh, or the level of detail it chooses
    /// in its place, by its own bounds, so a surface's hold them all.
    pub(crate) fn named_surfaces<K: Copy>(
        &self,
        names: impl Fn(&SurfaceMaterial) -> Option<K>,
    ) -> Vec<Named<K>> {
        // The models drawn with a material that names something: few.
        let mut models = Vec::new();
        for (_, material) in self.materials.slots.iter() {
            if names(&material.values).is_some() {
                for &model in material.users.keys() {
                    if !models.contains(&model) {
                        models.push(model);
                    }
                }
            }
        }
        let mut surfaces = Vec::new();
        if models.is_empty() {
            return surfaces;
        }
        for (_, instance) in self.instances.slots.iter() {
            if !instance.state.visible || !models.contains(&instance.state.model) {
                continue;
            }
            let model = self.drawn_model(instance.state.model);
            for (index, mesh) in model.meshes.iter().enumerate() {
                let material = self.drawn_material(mesh.material);
                let Some(name) = names(&material.values) else {
                    continue;
                };
                let levels = mesh
                    .lods
                    .iter()
                    .filter_map(|lod| self.drawn_model(lod.model).meshes[lod.mesh].ranges.bounds());
                let Some(bounds) = instance
                    .mesh_bounds(model, index)
                    .into_iter()
                    .chain(levels)
                    .reduce(|[low, high], [a, b]| [low.min(a), high.max(b)])
                else {
                    continue;
                };
                surfaces.push(Named {
                    name,
                    pose: instance.state.pose,
                    bounds: super::static_edits::grown(bounds, material.displacement_bound()),
                    group: material.values.visibility_group,
                });
            }
        }
        surfaces
    }
}
