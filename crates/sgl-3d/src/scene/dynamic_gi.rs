//! The dynamic GI volume's placement: content the game installs whole, which
//! the dynamic GI stage keeps its probes for, and scrolls by whole spacings.
use super::{Scene, SceneError};
use crate::content::dynamic_gi::DynamicGiVolume;
use glam::{DVec3, I64Vec3, Vec3};

/// The installed volume, its origin in the frame the scene was created in,
/// so a move of the render origin translates it without rounding it again
/// and the stage sees the same placement.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct InstalledVolume {
    pub origin: DVec3,
    pub spacing: Vec3,
    pub probes: [u32; 3],
    /// The lattice it lies on, among every lattice installed: another
    /// spacing or count, or an origin off the lattice, is another one, whose
    /// probes start afresh.
    pub lattice: u64,
    /// The whole spacings its origin has moved along each axis since its
    /// lattice was installed.
    pub scroll: I64Vec3,
}

/// A volume as a frame packs it: its placement in the scene's render frame,
/// and where its probes are stored.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ProbePlacement {
    pub volume: DynamicGiVolume,
    /// Each probe is stored at its lattice coordinate plus this, wrapping
    /// (`ddgi_probe_stored` in shading/dynamic_gi.wgsl), so a scroll moves
    /// no probe: its scroll, modulo the probe counts.
    pub scroll: [u32; 3],
}

impl InstalledVolume {
    /// It as a frame packs it, with its origin in a render frame at
    /// `render_origin`.
    pub fn placement(&self, render_origin: DVec3) -> ProbePlacement {
        ProbePlacement {
            volume: DynamicGiVolume {
                origin: (self.origin - render_origin).as_vec3(),
                spacing: self.spacing,
                probes: self.probes,
            },
            scroll: std::array::from_fn(|axis| {
                self.scroll[axis].rem_euclid(i64::from(self.probes[axis])) as u32
            }),
        }
    }
}

impl Scene {
    /// Installs the dynamic GI volume, replacing the one installed; `None`
    /// removes it. A scene holds one, and one without pays nothing for it.
    /// Installing the same spacing and counts at an origin moved by whole
    /// spacings scrolls the volume, as a volume that follows the camera
    /// does: the probes that stay keep what they hold, and those that enter
    /// start afresh. Another spacing or count, or an origin off the lattice
    /// beyond a small tolerance of the spacing, is another placement, all of
    /// whose probes start afresh. Until a probe has been traced, it lights
    /// nothing and surfaces keep their other indirect light. Installing the
    /// same placement again changes nothing, and `Scene::move_origin`
    /// translates the volume with everything else, its probes kept. Fails
    /// with `SceneError::InvalidDynamicGiVolume` for a placement that is not
    /// a lattice, and with `SceneError::DeviceLimit` where its probes would
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
        let origin = volume.origin.as_dvec3() + self.origin();
        // RTXGI's scroll anchor: the whole spacings nearest the move, on the
        // same lattice.
        let scrolled = self.dynamic_gi.and_then(|current| {
            let lattice = current.spacing == volume.spacing && current.probes == volume.probes;
            let steps = lattice
                .then(|| super::lattice::steps(current.origin, current.spacing, origin, volume.origin))
                .flatten()?;
            Some(InstalledVolume {
                origin: current.origin + steps.as_dvec3() * current.spacing.as_dvec3(),
                scroll: current.scroll + steps,
                ..current
            })
        });
        self.dynamic_gi = Some(scrolled.unwrap_or_else(|| InstalledVolume {
            origin,
            spacing: volume.spacing,
            probes: volume.probes,
            lattice: super::next_generation(),
            scroll: I64Vec3::ZERO,
        }));
        Ok(())
    }

    /// The installed dynamic GI volume, in the scene's render frame.
    pub fn dynamic_gi_volume(&self) -> Option<DynamicGiVolume> {
        self.dynamic_gi_placement().map(|placement| placement.volume)
    }

    /// The installed dynamic GI volume as a frame packs it.
    pub(crate) fn dynamic_gi_placement(&self) -> Option<ProbePlacement> {
        self.dynamic_gi
            .map(|volume| volume.placement(self.origin()))
    }
}
