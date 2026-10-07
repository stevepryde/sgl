//! Ray classes (the architecture's Hardware ray tracing, *Portable
//! coverage*): what rays see of each model, from the alpha modes of its
//! meshes that have triangles (a transmissive one is drawn blended), which decides whether a hardware query's
//! committed hit can judge it, and which of its meshes are masked, whose
//! BLAS geometries the candidate form builds without `OPAQUE`. The scene
//! keeps both for each model and recomputes them for a material's users,
//! through the material's use list, when its alpha mode or whether it is
//! drawn blended changes, a cost proportional to its users.
use super::materials::Materials;
use super::models::{Model, Models};
use crate::content::identity::MaterialId;
use crate::content::material::{AlphaMode, SurfaceMaterial};
use crate::shading::RayQueryForm;

/// What rays see of a model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RayClass {
    /// Nothing a ray stops at: every mesh with triangles is blended or
    /// transmissive, or it has none. Rays pass through it.
    None,
    /// A masked mesh, whose cut-out texels a committed hit cannot judge
    /// cheaply: under the baseline form its instances that do not deform are
    /// predicate instances, which the portable walk covers, and it has no
    /// BLAS; under the candidate form it has one, whose masked meshes'
    /// geometries are not opaque, so the query's candidate loop judges them.
    Masked,
    /// Any other: opaque meshes, and blended ones whose hits the predicate
    /// rejects. Its BLAS holds every mesh.
    Opaque,
}

impl RayClass {
    /// Whether a model of this class that does not deform has a BLAS, and
    /// its instances join the TLAS, under `form`.
    pub fn traced(self, form: RayQueryForm) -> bool {
        match self {
            Self::None => false,
            Self::Masked => form == RayQueryForm::Candidates,
            Self::Opaque => true,
        }
    }

    /// The class of a model whose meshes with triangles have `materials`.
    pub fn of<'a>(materials: impl Iterator<Item = &'a SurfaceMaterial>) -> Self {
        let mut class = Self::None;
        for values in materials.filter(|values| !values.blended()) {
            if matches!(values.alpha, AlphaMode::Mask { .. }) {
                return Self::Masked;
            }
            class = Self::Opaque;
        }
        class
    }

    /// The class of `model` under `materials`.
    pub fn of_model(model: &Model, materials: &Materials) -> Self {
        Self::of(
            model
                .meshes
                .iter()
                .zip(&model.ray_meshes)
                .filter(|(_, words)| words.index_count >= 3)
                .map(|(mesh, _)| {
                    &materials
                        .get(mesh.material)
                        .expect("a mesh's material lives")
                        .values
                }),
        )
    }
}

/// Which of `model`'s meshes are masked under `materials`.
fn masked_meshes(model: &Model, materials: &Materials) -> Vec<bool> {
    model
        .meshes
        .iter()
        .map(|mesh| {
            let material = materials
                .get(mesh.material)
                .expect("a mesh's material lives");
            !material.values.blended() && matches!(material.values.alpha, AlphaMode::Mask { .. })
        })
        .collect()
}

/// Sets `model`'s ray class and its masked meshes under `materials`.
pub(crate) fn classify(model: &mut Model, materials: &Materials) {
    model.ray_class = RayClass::of_model(model, materials);
    model.ray_masked = masked_meshes(model, materials);
}

impl Models {
    /// Recomputes the ray class and the masked meshes of every model that
    /// uses `material`, whose alpha mode or population changed.
    pub fn classify_users(&mut self, material: MaterialId, materials: &Materials) {
        let users = &materials.get(material).expect("a live material").users;
        for &id in users.keys() {
            classify(
                self.slots.get_mut(id).expect("a material's user lives"),
                materials,
            );
        }
    }
}
