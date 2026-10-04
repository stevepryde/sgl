//! Caller-authored transient geometry: additive glow and heat shimmer
//! vertices, and their limits, and fog volumes.
/// One vertex in an additive triangle list, expressed in world space.
///
/// The caller owns geometry generation, lifetime, topology and presentation time.
/// `Default` is all zero: a uniform (kind 0), hard-edged vertex at the origin
/// that adds nothing until it has a colour.
#[repr(C)]
#[derive(Clone, Copy, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Glow {
    /// World-space vertex or line endpoint position.
    pub position: [f32; 3],
    /// Tapered profile coordinates for kind 1. For kind 2, X is the signed
    /// screen-space offset in pixels; -0.5 and 0.5 span a one-pixel line.
    pub uv: [f32; 2],
    /// Linear RGB radiance and alpha. RGB is added with source-alpha weighting.
    pub color: [f32; 4],
    /// Shading profile: 0 is uniform, 1 modulates alpha along a tapered
    /// oscillating profile, and 2 expands a projected line using `other`.
    pub kind: f32,
    /// Opposite world-space endpoint for kind 2; unused for other profiles.
    pub other: [f32; 3],
    /// Intersection fade distance in metres of view depth; zero keeps hard edges.
    /// Keep constant across a triangle. Use zero for screen-space motion lines.
    pub soft_distance: f32,
}

/// Maximum retained triangle-list vertices. Excess submissions fail without replacing the list.
pub const MAX_VERTICES: usize = 6144;
/// Maximum displacement per axis in scene-resolution pixels.
pub const MAX_DISPLACEMENT_PIXELS: f32 = 32.;

/// Caller-owned world-space triangle vertex. Animation and plume coverage belong to the game.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct HeatDistortion {
    pub position: [f32; 3],
    /// Signed scene-pixel offset, X right and Y down; bounded to +/-32 per axis.
    pub displacement: [f32; 2],
    /// Interpolated displacement weight in 0..=1. Use zero at plume edges.
    pub weight: f32,
}

/// A box of medium added to the frame's fog (`FrameInput::fog`) where it
/// lies: Godot's box `FogVolume` with its `FogMaterial`'s density, albedo and
/// edge fade. Volumes add their density where they overlap.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FogVolume {
    /// The box's centre in the world.
    pub center: glam::Vec3,
    /// How the box is turned about its centre; normalized.
    pub rotation: glam::Quat,
    /// Its size along its own x, y and z in metres, positive.
    pub size: glam::Vec3,
    /// Extinction per metre it adds, nonnegative.
    pub density: f32,
    /// Linear RGB single-scattering albedo of what it adds.
    pub albedo: [f32; 3],
    /// Nonnegative: 0 keeps the density whole to within 0.1 m of the box's
    /// faces, across which it fades out; higher also thins it toward them
    /// from the box's middle (Godot's `edge_fade`).
    pub edge_fade: f32,
}

impl Default for FogVolume {
    /// Godot's `FogVolume` and `FogMaterial` defaults (b130438
    /// `scene/3d/fog_volume.h`, `scene/resources/3d/fog_material.h`): a 2 m
    /// cube at the origin, unturned, of density 1, white albedo and edge
    /// fade 0.1. Set what differs and take the rest with
    /// `..Default::default()`.
    fn default() -> Self {
        Self {
            center: glam::Vec3::ZERO,
            rotation: glam::Quat::IDENTITY,
            size: glam::Vec3::splat(2.),
            density: 1.,
            albedo: [1.; 3],
            edge_fade: 0.1,
        }
    }
}
