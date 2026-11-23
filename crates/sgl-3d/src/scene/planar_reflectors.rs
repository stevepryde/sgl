//! Planar reflectors: each one's plane and how many materials name it, with
//! the `Scene` operations that add, read, move and remove them (the
//! architecture's Scene content, Planar reflector). The scene holds no GPU
//! state for one: each view that shows a reflector renders its own picture
//! (the architecture's Frame, "Planar reflections").
use super::slots::Slots;
use super::{Scene, SceneError};
use crate::content::identity::PlanarReflectorId;
use crate::content::planar_reflector::PlanarReflector;
use glam::{Vec3, Vec4};

pub(crate) struct Held {
    pub description: PlanarReflector,
    /// The plane as (unit normal, d), positive on the reflecting side.
    pub plane: Vec4,
    /// The materials that name it.
    users: u32,
}

#[derive(Default)]
pub(crate) struct PlanarReflectors {
    slots: Slots<PlanarReflectorId, Held>,
    /// The materials that name any reflector.
    named: u32,
}

impl PlanarReflectors {
    pub fn get(&self, id: PlanarReflectorId) -> Result<&Held, SceneError> {
        self.slots.get(id).ok_or(SceneError::UnknownPlanarReflector)
    }

    /// Counts `by` more materials naming `id`, a live reflector.
    pub fn count(&mut self, id: Option<PlanarReflectorId>, by: i32) {
        if let Some(id) = id {
            let held = self.slots.get_mut(id).expect("a named reflector lives");
            held.users = held.users.checked_add_signed(by).unwrap();
            self.named = self.named.checked_add_signed(by).unwrap();
        }
    }

    /// Whether a material names a reflector: a frame of a scene where none
    /// does tests nothing.
    pub fn named(&self) -> bool {
        self.named > 0
    }

    pub fn move_origin(&mut self, to: Vec3) {
        for (_, held) in self.slots.iter_mut() {
            held.description.origin -= to;
            held.plane = held
                .description
                .plane()
                .expect("a held reflector's plane stays finite");
        }
    }
}

/// A surface that shows a planar reflector: a mesh of a visible instance
/// drawn with a material that names it.
pub(crate) type Mirror = super::named_surfaces::Named<PlanarReflectorId>;

impl Scene {
    /// The surfaces that show a planar reflector, which a frame tests
    /// against its views (`renderer::reflected`): none without a material
    /// that names one.
    pub(crate) fn mirrors(&self) -> Vec<Mirror> {
        if !self.planar_reflectors.named() {
            return Vec::new();
        }
        self.named_surfaces(|material| material.planar_reflector)
    }

    /// Adds a planar reflector: a plane whose materials' surfaces
    /// (`SurfaceMaterial::planar_reflector`) mirror the scene.
    /// `SceneError::InvalidPlanarReflector` for an origin that is not
    /// finite or a normal without a direction.
    pub fn add_planar_reflector(
        &mut self,
        reflector: PlanarReflector,
    ) -> Result<PlanarReflectorId, SceneError> {
        let plane = reflector
            .plane()
            .ok_or(SceneError::InvalidPlanarReflector)?;
        Ok(self.planar_reflectors.slots.insert(Held {
            description: reflector,
            plane,
            users: 0,
        }))
    }

    /// A planar reflector's description.
    pub fn planar_reflector(&self, id: PlanarReflectorId) -> Result<PlanarReflector, SceneError> {
        Ok(self.planar_reflectors.get(id)?.description)
    }

    /// Moves a planar reflector's plane; errors as `add_planar_reflector`,
    /// leaving it as it was.
    pub fn set_planar_reflector(
        &mut self,
        id: PlanarReflectorId,
        reflector: PlanarReflector,
    ) -> Result<(), SceneError> {
        let plane = reflector
            .plane()
            .ok_or(SceneError::InvalidPlanarReflector)?;
        let held = self
            .planar_reflectors
            .slots
            .get_mut(id)
            .ok_or(SceneError::UnknownPlanarReflector)?;
        held.description = reflector;
        held.plane = plane;
        Ok(())
    }

    /// Removes a planar reflector no material names.
    pub fn remove_planar_reflector(&mut self, id: PlanarReflectorId) -> Result<(), SceneError> {
        if self.planar_reflectors.get(id)?.users > 0 {
            return Err(SceneError::PlanarReflectorInUse);
        }
        self.planar_reflectors.slots.remove(id);
        Ok(())
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
