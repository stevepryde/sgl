//! The irradiance volume's placement and cells, as the game describes them
//! to `Scene::set_irradiance_volume` and `PreparedIrradianceRegion::new`.
//! Metres; light is irradiance / PI on the directional lights' scale.
use super::static_lighting::AmbientCube;
use glam::Vec3;

/// Where a scene's irradiance volume lies: a lattice of cells the game
/// places over the part of its world it lights from a field it computes,
/// such as a voxel world's propagated sky and block light at a metre or a
/// level's bake, and keeps up by region (`Scene::write_irradiance_cells`).
/// Every surface within the lattice, static or moving, takes its indirect
/// diffuse light from the cells about it unless a lightmap or irradiance
/// atlas chart lights it; across the lattice the volume stands in for the
/// dynamic GI volume and ambient cubes. A cell the game has not written
/// reads as the frame's ambient whole with no light of its own.
///
/// Each cell costs 48 bytes of texture memory (160 × 128 × 160 cells of a
/// metre, 157 MB); nothing is uploaded per frame. Installing the same cell
/// size and counts at another origin scrolls the volume by whole cells,
/// keeping the cells that stay.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct IrradianceVolume {
    /// The least corner of its first cell: the lattice's corner at its
    /// least x, y and z.
    pub origin: Vec3,
    /// A cell's size along each axis, positive.
    pub cell_size: Vec3,
    /// How many cells lie along each axis, at least one. The device's 3D
    /// texture limit bounds them: under WebGPU's default of 2048, at most
    /// 2048 on x, 1024 on y and 682 on z.
    pub cells: [u32; 3],
}

impl IrradianceVolume {
    /// Whether its values describe a lattice: finite, with a positive cell
    /// size and at least one cell along each axis.
    pub(crate) fn valid(&self) -> bool {
        let end = self.origin + self.cell_size * Vec3::from_array(self.cells.map(|n| n as f32));
        self.origin.is_finite()
            && self.cell_size.is_finite()
            && self.cell_size.cmpgt(Vec3::ZERO).all()
            && self.cells.iter().all(|&n| n >= 1)
            && end.is_finite()
    }
}

/// One cell of an irradiance volume: an ambient cube of its own light and
/// how much of the frame's ambient reaches it, each in the cube's face
/// order +X, −X, +Y, −Y, +Z, −Z. A face is what a surface facing that way
/// receives: a surface takes the three faces its normal points to, weighted
/// by the normal's squared components (Valve's ambient cube, as Bevy's
/// irradiance volume samples it).
#[derive(Clone, Copy, Debug)]
pub struct IrradianceCell {
    /// The cell's own light toward each face, from the world's emitters and
    /// whatever bounce the game's field carries: irradiance / PI, finite,
    /// nonnegative and within RGBA16F range. It is not a scene light: a
    /// fixture whose light the game writes here is not also added as a
    /// baked light, or the game leaves the field's share of it out.
    pub irradiance: AmbientCube,
    /// The share of the frame's ambient (the environment's diffuse light and
    /// the hemisphere fill) that reaches the cell from each face's side, 0
    /// to 1: 1 under open sky, 0 deep in a cave. It also darkens the sky's
    /// share of a surface's environment specular there.
    pub sky_visibility: [f32; 6],
}

impl Default for IrradianceCell {
    /// A cell as the volume starts: no light of its own and the frame's
    /// ambient whole.
    fn default() -> Self {
        Self {
            irradiance: AmbientCube::default(),
            sky_visibility: [1.; 6],
        }
    }
}

impl IrradianceCell {
    /// Whether its light is finite, nonnegative and within RGBA16F range,
    /// and its sky visibility within 0..=1.
    pub(crate) fn valid(&self) -> bool {
        self.irradiance
            .irradiance
            .iter()
            .flatten()
            .all(|&value| value.is_finite() && (0. ..=65504.).contains(&value))
            && self
                .sky_visibility
                .iter()
                .all(|&value| (0. ..=1.).contains(&value))
    }
}
