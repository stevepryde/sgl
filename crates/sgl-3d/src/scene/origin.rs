//! The scene's render origin (the architecture's "Render origin"): where the
//! frame its positions are expressed in lies in the frame the scene was
//! created in, and `Scene::move_origin`, which translates every position the
//! scene retains into a new one. It is an edit, not a static edit: it
//! records no bounds, makes no cache stale, writes no motion and cuts no
//! history. Renderers carry the origin with their histories and translate
//! what they retain by the difference.
use super::{Scene, SceneError};
use glam::{DVec3, Mat4, Vec3};

/// `pose` in a render frame whose origin lies `by` from its own: its
/// translation less `by`, rounded once.
pub(crate) fn translated(pose: Mat4, by: Vec3) -> Mat4 {
    Mat4::from_translation(-by) * pose
}

impl Scene {
    /// Moves the scene's render origin to `to`, a finite position in the
    /// current render frame, exactly as given: every position the scene
    /// holds becomes what it was less `to`, and from then on the game
    /// expresses the camera, the frame input and what it edits in the new
    /// frame. A game whose world is larger than `f32` renders precisely
    /// keeps its own coordinates and moves the origin to stay near what it
    /// renders, chunk-aligned in a streamed world. Instances keep their
    /// motion, static caches and histories stay valid, and a game that
    /// never moves the origin pays nothing.
    pub fn move_origin(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        to: Vec3,
    ) -> Result<(), SceneError> {
        if !to.is_finite() {
            return Err(SceneError::InvalidOrigin);
        }
        if to == Vec3::ZERO {
            return Ok(());
        }
        // The dynamic GI volume's origin is kept in the frame the scene was
        // created in (`dynamic_gi`), so it translates with the sum.
        self.origin += to.as_dvec3();
        self.instances
            .move_origin(queue, to, &self.models, &mut self.ray_instances);
        self.ray_instances.rebuild_statics();
        self.static_edits.move_origin(to);
        self.lights.move_origin(queue, to);
        self.decals.move_origin(to);
        self.transient.move_origin(device, queue, to);
        if let Some(probes) = &mut self.baked_specular_probes {
            probes.move_origin(device, to);
            // Group 0 binds the probes' metadata.
            self.resources = super::next_generation();
        }
        Ok(())
    }

    /// Where the render origin lies in the frame the scene was created in:
    /// the sum of its moves.
    pub(crate) fn origin(&self) -> DVec3 {
        self.origin
    }
}
