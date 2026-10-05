//! The dynamic GI volume's placement, as the game describes it to
//! `Scene::set_dynamic_gi_volume`. Metres.
use glam::Vec3;

/// Where a scene's dynamic diffuse GI probes lie: a lattice of probes the
/// game places over the part of its world it wants lit by bounced light, as
/// RTXGI's and Wicked Engine's DDGI volumes are. Each frame traces rays
/// from the probes through the scene, keeping each probe's irradiance and
/// the distances to what surrounds it up to date, and surfaces within the
/// lattice take their indirect diffuse light from the probes about them
/// (`Settings::dynamic_gi` sets how many rays). How the probes are kept up
/// is SGL3D's.
///
/// Probes cost texture memory, a few kilobytes each, and rays each frame:
/// a few for a probe whose light has settled, up to the quality's most
/// while it changes. A spacing of one to a few metres suits rooms and
/// streets; a lattice need not cover the world, and surfaces beyond it keep
/// their other indirect light.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DynamicGiVolume {
    /// The first probe's position: the lattice's corner at its least x, y
    /// and z. Moved by whole spacings from the installed volume's, it
    /// scrolls that volume (`Scene::set_dynamic_gi_volume`).
    pub origin: Vec3,
    /// The distance between neighbouring probes along each axis, positive.
    pub spacing: Vec3,
    /// How many probes lie along each axis, at least two.
    pub probes: [u32; 3],
}

impl DynamicGiVolume {
    /// The lattice's corner opposite `origin`: its last probe's position.
    pub(crate) fn end(&self) -> Vec3 {
        self.origin + self.spacing * (Vec3::from_array(self.probes.map(|n| n as f32)) - 1.)
    }

    /// Whether its values describe a lattice: finite, with a positive
    /// spacing and at least two probes on each axis.
    pub(crate) fn valid(&self) -> bool {
        self.origin.is_finite()
            && self.spacing.is_finite()
            && self.spacing.cmpgt(Vec3::ZERO).all()
            && self.probes.iter().all(|&n| n >= 2)
            && self.end().is_finite()
    }
}
