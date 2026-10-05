//! Ray classes (the architecture's Hardware ray tracing, *Portable
//! coverage*): what rays see of each model, from the alpha modes of its
//! meshes that have triangles, which decides whether a hardware query's
//! committed hit can judge it. The scene keeps each model's class and
//! recomputes it for a material's users, through the material's use list,
//! when its alpha mode changes, a cost proportional to its users.
use super::materials::Materials;
use super::models::{Model, Models};
use crate::content::identity::MaterialId;
use crate::content::material::AlphaMode;

/// What rays see of a model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RayClass {
    /// Nothing a ray stops at: every mesh with triangles is blended, or it
    /// has none. Rays pass through it.
    None,
    /// A masked mesh, whose cut-out texels a committed hit cannot judge
    /// cheaply: under the baseline form its instances that do not deform are
    /// predicate instances, which the portable walk covers, and it has no
    /// BLAS.
    Masked,
    /// Any other: opaque meshes, and blended ones whose hits the predicate
    /// rejects. Its BLAS holds every mesh.
    Opaque,
}

impl RayClass {
    /// The class of a model whose meshes with triangles have `alphas`.
    pub fn of(alphas: impl Iterator<Item = AlphaMode>) -> Self {
        let mut class = Self::None;
        for alpha in alphas {
            match alpha {
                AlphaMode::Mask { .. } => return Self::Masked,
                AlphaMode::Opaque => class = Self::Opaque,
                AlphaMode::Blend { .. } => {}
            }
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
                    materials
                        .get(mesh.material)
                        .expect("a mesh's material lives")
                        .values
                        .alpha
                }),
        )
    }
}

impl Models {
    /// Recomputes the ray class of every model that uses `material`, whose
    /// alpha mode changed.
    pub fn classify_users(&mut self, material: MaterialId, materials: &Materials) {
        let users = &materials.get(material).expect("a live material").users;
        for &id in users.keys() {
            let model = self.slots.get_mut(id).expect("a material's user lives");
            model.ray_class = RayClass::of_model(model, materials);
        }
    }
}
