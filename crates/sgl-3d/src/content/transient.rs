//! Caller-authored transient geometry: additive glow and heat shimmer
//! vertices, and their limits, and fog volumes.
/// One vertex in an additive triangle list, expressed in world space.
///
/// The caller owns geometry generation, lifetime, topology and presentation time.
/// `Default` is a uniform, hard-edged vertex at the origin that adds nothing
/// until it has a colour.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Glow {
    /// World-space vertex or line endpoint position.
    pub position: [f32; 3],
    /// Linear RGB radiance and alpha. RGB is added with source-alpha weighting.
    pub color: [f32; 4],
    /// How its triangle shades. Keep the kind, and a tapered profile, the
    /// same across a triangle.
    pub kind: GlowKind,
    /// Intersection fade distance in metres of view depth; zero keeps hard edges.
    /// Keep constant across a triangle. Use zero for screen-space motion lines.
    pub soft_distance: f32,
}

/// How a glow triangle shades.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum GlowKind {
    /// Its colour as interpolated across the triangle.
    #[default]
    Uniform,
    /// Its alpha shaped by `profile` at the vertex's `uv`, interpolated
    /// across the triangle.
    Tapered { uv: [f32; 2], profile: GlowProfile },
    /// A line one pixel wide from `position` to `other`, the opposite
    /// world-space endpoint: the vertex lies `offset` pixels across the
    /// projected segment, signed; -0.5 and 0.5 span the line.
    Line { other: [f32; 3], offset: f32 },
}

/// The shape of a tapered glow's alpha over its `uv` (u and v): whole at
/// v = 0, fading to nothing at v = 1, and rippling in bands across u that
/// slant along v. Alpha is scaled by
/// (1 − v)^taper × (1 − a + a × sin(u × f\[0\] + v × f\[1\])), where a is
/// `ripple_amplitude` and f `ripple_frequency`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GlowProfile {
    /// How sharply alpha fades toward v = 1; positive, higher fades sooner.
    pub taper: f32,
    /// The ripple's phase in radians per unit of u and of v.
    pub ripple_frequency: [f32; 2],
    /// How deep the ripple cuts, 0..=0.5: alpha ranges from 1 − 2 ×
    /// `ripple_amplitude` in its troughs to 1 on its crests; 0 is smooth.
    pub ripple_amplitude: f32,
}

impl Default for GlowProfile {
    /// SGL3D's own, from Hyperdrive: a quadratic taper with about ten bands
    /// across u and a ripple that dims alpha to 0.4 in its troughs.
    fn default() -> Self {
        Self {
            taper: 2.,
            ripple_frequency: [62.83, 18.],
            ripple_amplitude: 0.3,
        }
    }
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
