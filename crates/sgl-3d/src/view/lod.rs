//! The camera's choice among a mesh's registered alternatives
//! (`Scene::set_mesh_lods`), bounded in its pixels.
use crate::lod::MeshLod;
use crate::scene::models::{Mesh, Models};
use crate::shading::lod::{LOD_PIXELS, projected_error};
use glam::Mat4;

/// Selects alternatives for one camera: its view, projection and pixel size.
#[derive(Clone, Copy)]
pub(crate) struct LodSelector {
    view: Mat4,
    projection: Mat4,
    size: [u32; 2],
}

impl LodSelector {
    /// None for a camera without pixels, which keeps full detail.
    pub fn new(view: Mat4, projection: Mat4, size: [u32; 2]) -> Option<Self> {
        (!size.contains(&0)).then_some(Self {
            view,
            projection,
            size,
        })
    }

    /// The last admissible alternative of `mesh` at `pose`, bounded to half a
    /// pixel, or None for the mesh itself.
    pub fn select<'a>(&self, mesh: &'a Mesh, models: &Models, pose: Mat4) -> Option<&'a MeshLod> {
        let bounds = mesh.ranges.bounds()?;
        let transforms = [pose, self.view, self.projection];
        mesh.lods.iter().rev().find(|lod| {
            let alternative = &models
                .slots
                .get(lod.model)
                .expect("a level of detail's model lives")
                .meshes[lod.mesh];
            let bounds = alternative.ranges.bounds().map_or(bounds, |other| {
                [bounds[0].min(other[0]), bounds[1].max(other[1])]
            });
            projected_error(transforms, bounds, lod.max_error, self.size) <= f64::from(LOD_PIXELS)
        })
    }
}
