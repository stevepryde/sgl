//! The dynamic GI volume's placement: content the game installs whole, which
//! the dynamic GI stage keeps its probes for.
use super::{Scene, SceneError};
use crate::content::dynamic_gi::DynamicGiVolume;
use glam::{DVec3, Vec3};

/// The installed volume, its origin in the frame the scene was created in,
/// so a move of the render origin translates it without rounding it again
/// and the stage sees the same placement.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct InstalledVolume {
    pub origin: DVec3,
    pub spacing: Vec3,
    pub probes: [u32; 3],
}

impl Scene {
    /// Installs the dynamic GI volume, replacing the one installed; `None`
    /// removes it. A scene holds one, and one without pays nothing for it.
    /// The probes start afresh whenever the placement changes: until a
    /// probe has been traced, it lights nothing and surfaces keep their
    /// other indirect light. Installing the same placement again changes
    /// nothing, and `Scene::move_origin` translates the volume with
    /// everything else, its probes kept. Fails with
    /// `SceneError::InvalidDynamicGiVolume` for a placement that is not a
    /// lattice, and with `SceneError::DeviceLimit` where its probes would
    /// exceed the device's texture or buffer limits; either keeps the
    /// installed volume.
    pub fn set_dynamic_gi_volume(
        &mut self,
        device: &wgpu::Device,
        volume: Option<DynamicGiVolume>,
    ) -> Result<(), SceneError> {
        let Some(volume) = volume else {
            self.dynamic_gi = None;
            return Ok(());
        };
        if !volume.valid() {
            return Err(SceneError::InvalidDynamicGiVolume);
        }
        if !crate::shading::dynamic_gi::fits(volume.probes, &device.limits()) {
            return Err(SceneError::DeviceLimit);
        }
        let installed = InstalledVolume {
            origin: volume.origin.as_dvec3() + self.origin(),
            spacing: volume.spacing,
            probes: volume.probes,
        };
        // The placement the scene returns after a move, installed again, is
        // the same one, though its origin rounded once in the new frame.
        let rounding = volume.origin.abs().max_element() * f32::EPSILON;
        let same = self.dynamic_gi.is_some_and(|current| {
            current.spacing == installed.spacing
                && current.probes == installed.probes
                && (current.origin - installed.origin).abs().max_element() <= f64::from(rounding)
        });
        if !same {
            self.dynamic_gi = Some(installed);
        }
        Ok(())
    }

    /// The installed dynamic GI volume, in the scene's render frame.
    pub fn dynamic_gi_volume(&self) -> Option<DynamicGiVolume> {
        self.dynamic_gi.map(|volume| DynamicGiVolume {
            origin: (volume.origin - self.origin()).as_vec3(),
            spacing: volume.spacing,
            probes: volume.probes,
        })
    }
}
