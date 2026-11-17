//! The motion's tile maximum and its 3x3 tile neighbourhood maximum
//! (`motion_tiles.wgsl`, Wicked Engine's motion blur tile passes), which
//! motion blur and TAA's motion reach each run on their own targets
//! (`view::motion_tiles`).
use super::Module;

pub(crate) static MOTION_TILES: Module = Module {
    name: "motion_tiles",
    source: include_str!("motion_tiles.wgsl"),
    deps: &[],
};

/// The tile passes' entry points, in the order they run: each tile column's
/// largest and smallest velocity per frame row, each tile's, and each tile
/// neighbourhood's.
pub(crate) const ENTRY_POINTS: [&str; 3] = [
    "motionblur_tileMaxVelocity_horizontal",
    "motionblur_tileMaxVelocity_vertical",
    "motionblur_neighborhoodMaxVelocity",
];

/// Wicked's `MOTIONBLUR_TILESIZE`: frame pixels per tile side, which the
/// shader takes as its override. A 3x3 tile neighbourhood reaches at least
/// a tile from every pixel of its centre tile.
pub(crate) const TILE_SIZE: u32 = 32;

/// The shader's workgroup side.
pub(crate) const GROUP_SIZE: u32 = 8;

/// `MotionTiles` in `motion_tiles.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct MotionTilesUniform {
    /// Frame pixels per unit of the motion target.
    pub velocity_scale: [f32; 2],
    /// The motion target's size over the frame's.
    pub render_scale: [f32; 2],
}

/// The layouts this module mirrors.
#[cfg(test)]
pub(crate) fn mirrors() -> Vec<crate::shading::layout_tests::Mirror> {
    use crate::shading::layout_tests::mirror;
    vec![mirror!(
        "motion_tiles",
        "MotionTiles",
        MotionTilesUniform,
        [velocity_scale, render_scale]
    )]
}
