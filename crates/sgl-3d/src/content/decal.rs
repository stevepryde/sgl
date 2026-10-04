//! A scene's decals, as the game describes them to `Scene::add_decal` and
//! `Scene::set_decal`: images projected through a box onto the surfaces
//! inside it, as Godot b130438's `Decal` projects them. Metres, linear RGB.
use super::identity::DecalImageId;
use glam::{Quat, Vec3};

/// A box that projects images onto the lit surfaces inside it, changing their
/// base colour and, with the maps it has, their normal, roughness and
/// metallic before they are lit, so reflections and every view see the
/// change: scene content, added with `Scene::add_decal`. Godot's `Decal`.
///
/// The box spans `size` about `position`, turned by `rotation`. It projects
/// down its local −Y onto surfaces: its images lie across its local X (the
/// images' U, left to right) and Z (their V, top to bottom), and every point
/// of the box between its lower and upper faces takes the texel above or
/// below it. Unlit materials take no decals.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Decal {
    /// The box's centre.
    pub position: Vec3,
    /// The box's orientation, a unit quaternion.
    pub rotation: Quat,
    /// The box's extent along its local X, Y and Z, in metres, positive.
    pub size: Vec3,
    /// The base colour image, sampled as sRGB colour. Its alpha is the
    /// decal's coverage: it also weights the normal and metallic-roughness
    /// maps, as Godot's albedo alpha does.
    pub base_color: DecalImageId,
    /// A tangent-space normal map, sampled as linear data, in glTF's
    /// convention (+Y toward the image's top); its red and green give the
    /// normal and its blue is rebuilt from them, as Godot reads decal normal
    /// maps. Where the decal covers a surface, its normal turns toward the
    /// map's, about the box's +Y.
    pub normal: Option<DecalImageId>,
    /// A metallic-roughness image, sampled as linear data, in glTF's
    /// layout: roughness in green, metallic in blue. Where the decal covers
    /// a surface, they replace its own.
    pub metallic_roughness: Option<DecalImageId>,
    /// Multiplies the base colour image: linear RGB and alpha, each in
    /// [0, 1] (Godot's `modulate`).
    pub color: [f32; 4],
    /// How much of a covered surface's base colour the decal's replaces, in
    /// [0, 1] (Godot's `albedo_mix`): 0 keeps the surface's colour and
    /// changes only what the other maps change.
    pub base_color_mix: f32,
    /// How the decal fades toward its upper and lower faces, nonnegative:
    /// its coverage at a point a fraction `d` of the way from the centre
    /// plane to a face is `(1 − d)` raised to that face's fade, so 0 does
    /// not fade (Godot's `upper_fade` and `lower_fade`).
    pub upper_fade: f32,
    pub lower_fade: f32,
    /// Fades the decal on surfaces turned from its +Y, in [0, 1): 0 does not
    /// fade; otherwise its coverage falls smoothly to nothing as the
    /// surface's geometry normal turns from +Y, and is gone where
    /// `(1 + n·Y) / 2` is at or below this (Godot's `normal_fade`).
    pub normal_fade: f32,
}
