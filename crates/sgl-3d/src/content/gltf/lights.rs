//! A glTF's `KHR_lights_punctual` lights read into [`AssetLights`]: each
//! light the file defines, as authored, and each node of the loaded scene
//! that places one, at its world transform. [`PunctualLight::place`] is the
//! one conversion of a placed glTF light into SGL3D's [`Light`] or
//! [`DirectionalLight`].
//!
//! The extension (Khronos glTF 5b00b91,
//! `extensions/2.0/Khronos/KHR_lights_punctual/README.md`) is the authority.
//! three.js r187's `GLTFLoader` (`GLTFLightsExtension`, MIT) shows the
//! direction (a target at the light's local (0, 0, -1)), the cone defaults
//! and the linear colour. Bevy's `bevy_gltf` loader (ea06813, MIT/Apache-2.0)
//! maps the units onto a photometric renderer: it scales candela by 4π only
//! because its lights take lumens, where SGL3D takes candela and lux as they
//! are. Godot's `GLTFLight::to_node` (67421f3, MIT) holds a light without a
//! range, which the extension calls infinite, at 4096 m.
use glam::{Mat4, Vec3};
use gltf::khr_lights_punctual::Kind;
use std::f32::consts::FRAC_PI_2;

use super::super::asset::Result;
use super::super::light::{Light, LightShape};
use super::super::lighting::DirectionalLight;

/// A glTF's authored lights (`KHR_lights_punctual`): what each light is,
/// and where the loaded scene's nodes place them. Empty for a file without
/// the extension and for content built in code. Loading installs none of
/// them: the game places each ([`PunctualLight::place`]), adds the local
/// ones to its `Scene` and picks which directional ones fill
/// `FrameInput::directional_lights`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AssetLights {
    /// The lights the file defines, in its order, as authored.
    pub definitions: Vec<PunctualLight>,
    /// Each node of the loaded scene that places a light, depth-first with
    /// the scene's roots and each node's children in declared order. Two
    /// nodes may place one definition.
    pub placements: Vec<LightPlacement>,
}

/// One light a glTF defines, in the light's own frame: it shines down its
/// local -Z axis from its origin. Values are as authored, already checked
/// (a cone angle rounded just above π/2 is π/2): what becomes of them in
/// SGL3D is [`PunctualLight::place`]'s.
#[derive(Clone, Debug, PartialEq)]
pub struct PunctualLight {
    /// Its authored name.
    pub name: Option<String>,
    /// Linear RGB, nonnegative (glTF's default white).
    pub color: [f32; 3],
    /// Nonnegative: candela (lm/sr) for a point or spot light, the
    /// brightness inside a spot's inner cone; lux (lm/m²) for a directional
    /// light. glTF's default is 1.
    pub intensity: f32,
    /// Directional, point or spot, with what that kind adds.
    pub kind: PunctualKind,
}

/// What kind of light a [`PunctualLight`] is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PunctualKind {
    /// Infinitely far, shining down its node's -Z axis, unattenuated.
    Directional,
    /// Shining every way alike, with the inverse square of distance.
    /// `range` is metres, positive, beyond which it lights nothing; `None`
    /// is unbounded.
    Point { range: Option<f32> },
    /// A cone down its node's -Z axis: whole within `inner_angle` of it,
    /// none beyond `outer_angle`, in radians (glTF's defaults 0 and π/4,
    /// `0 <= inner_angle <= outer_angle <= π/2`, `outer_angle > 0`).
    /// `range` as a point's.
    Spot {
        range: Option<f32>,
        inner_angle: f32,
        outer_angle: f32,
    },
}

/// A node of the loaded scene that places a light.
#[derive(Clone, Debug, PartialEq)]
pub struct LightPlacement {
    /// The light it places: an index into [`AssetLights::definitions`].
    pub light: usize,
    /// The node's index in the glTF.
    pub node: usize,
    /// The node's authored name, by which a game can pick among its lights.
    pub name: Option<String>,
    /// The node's world transform in the asset's space: every ancestor's
    /// applied, at the scene's authored pose (animation does not move it).
    pub transform: Mat4,
}

/// A placed glTF light as SGL3D takes it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PlacedLight {
    /// A point or spot light, for `Scene::add_light`.
    Local(Light),
    /// A directional light, for one of `FrameInput::directional_lights`.
    Directional(DirectionalLight),
}

impl PunctualLight {
    /// The range of a point or spot light the file leaves unbounded, which
    /// the extension calls infinite, in metres: Godot's cap. Every view
    /// cluster within it takes the light, so a game that knows a smaller
    /// reach sets `Light::range` to it.
    pub const UNBOUNDED_RANGE: f32 = 4096.;

    /// This light placed by `transform`: a placement's, or a placement's
    /// after the game's own placement of the asset (`model * transform`).
    /// Candela stays candela and lux lux; the colour is linear RGB as
    /// glTF's is. The light sits at the transform's translation and shines
    /// down the local -Z axis as the transform carries it, made unit (zero
    /// where the transform collapses it): scale moves and turns the light
    /// but leaves its range, intensity and cone alone. A missing range is
    /// [`Self::UNBOUNDED_RANGE`]; a spot's outer angle of π/2, which
    /// `LightShape::Spot` keeps below, is the largest `f32` below it. A
    /// glTF light is a point (a directional one a point at infinity), so it
    /// has no radius or angular diameter. Every other field takes its
    /// `Default`: live, physical specular, fog energy 1, no shadow or
    /// cookie. Set what the game wants differently on the result.
    pub fn place(&self, transform: Mat4) -> PlacedLight {
        let direction = transform.transform_vector3(Vec3::NEG_Z).normalize_or_zero();
        let local = |shape, range: Option<f32>| {
            PlacedLight::Local(Light {
                position: transform.transform_point3(Vec3::ZERO),
                shape,
                color: self.color,
                intensity: self.intensity,
                range: range.unwrap_or(Self::UNBOUNDED_RANGE),
                ..Light::default()
            })
        };
        match self.kind {
            PunctualKind::Directional => PlacedLight::Directional(DirectionalLight {
                direction,
                color: self.color,
                illuminance: self.intensity,
                angular_diameter: 0.,
                ..DirectionalLight::default()
            }),
            PunctualKind::Point { range } => local(LightShape::Point { radius: 0. }, range),
            PunctualKind::Spot {
                range,
                inner_angle,
                outer_angle,
            } => {
                let outer_angle = outer_angle.min(FRAC_PI_2.next_down());
                local(
                    LightShape::Spot {
                        direction,
                        inner_angle: inner_angle.min(outer_angle),
                        outer_angle,
                        radius: 0.,
                    },
                    range,
                )
            }
        }
    }
}

/// How far above π/2 an authored outer cone angle may lie and still be
/// taken as π/2: π/2 written to five decimal places (1.5708) is 3.7e-6
/// above it.
const RIGHT_ANGLE_ROUNDING: f32 = 1e-5;

/// The lights `document` defines and where `scene`'s nodes place them. A
/// document without the extension's lights is read no further. Like the
/// node hierarchy, they are not charged to the decoded budget: each comes
/// from its own JSON object, which the source budget charged, in a few
/// times its size at most.
pub(super) fn read(document: &gltf::Document, scene: &gltf::Scene<'_>) -> Result<AssetLights> {
    let Some(lights) = document.lights() else {
        return Ok(AssetLights::default());
    };
    let definitions = lights.map(definition).collect::<Result<Vec<_>>>()?;
    let mut placements = Vec::new();
    // The node walk of `meshes::read_node`: a stack, never the call stack,
    // with each node's children reversed onto it so they are visited in
    // declared order.
    let mut pending: Vec<_> = scene.nodes().map(|node| (node, Mat4::IDENTITY)).collect();
    pending.reverse();
    while let Some((node, parent)) = pending.pop() {
        let transform = parent * Mat4::from_cols_array_2d(&node.transform().matrix());
        if let Some(light) = node.light() {
            let index = light.index();
            check_placement(&definitions[index], transform).map_err(|error| {
                format!(
                    "node {} places light {index}, but {error}; re-export valid glTF",
                    node.index()
                )
            })?;
            placements.push(LightPlacement {
                light: index,
                node: node.index(),
                name: node.name().map(str::to_owned),
                transform,
            });
        }
        let first_child = pending.len();
        pending.extend(node.children().map(|child| (child, transform)));
        pending[first_child..].reverse();
    }
    Ok(AssetLights {
        definitions,
        placements,
    })
}

/// `light`, checked against the extension's ranges.
fn definition(light: gltf::khr_lights_punctual::Light<'_>) -> Result<PunctualLight> {
    let index = light.index();
    let invalid = |what: &str| format!("light {index}: {what}; re-export valid glTF");
    let nonnegative = |value: f32| value.is_finite() && value >= 0.;
    if !light.color().into_iter().all(nonnegative) {
        return Err(invalid("color must be finite and nonnegative").into());
    }
    if !nonnegative(light.intensity()) {
        return Err(invalid("intensity must be finite and nonnegative").into());
    }
    let range = light.range();
    if range.is_some_and(|range| !(range.is_finite() && range > 0.)) {
        return Err(invalid("range must be finite and greater than 0").into());
    }
    let kind = match light.kind() {
        // The extension allows a range only on point and spot lights; an
        // infinitely distant light has none to take.
        Kind::Directional => PunctualKind::Directional,
        Kind::Point => PunctualKind::Point { range },
        Kind::Spot {
            inner_cone_angle,
            outer_cone_angle,
        } => {
            // The extension asks inner < outer; Blender writes them equal
            // for a spot blend of 0, a hard edge `LightShape::Spot` takes.
            // An outer angle of 0 (never inner's) would light nothing.
            if !(nonnegative(inner_cone_angle)
                && inner_cone_angle <= outer_cone_angle
                && outer_cone_angle > 0.
                && outer_cone_angle <= FRAC_PI_2 + RIGHT_ANGLE_ROUNDING)
            {
                return Err(invalid(
                    "spot cone angles must satisfy 0 <= innerConeAngle <= outerConeAngle <= π/2, with outerConeAngle above 0",
                )
                .into());
            }
            PunctualKind::Spot {
                range,
                inner_angle: inner_cone_angle.min(FRAC_PI_2),
                outer_angle: outer_cone_angle.min(FRAC_PI_2),
            }
        }
    };
    Ok(PunctualLight {
        name: light.name().map(str::to_owned),
        color: light.color(),
        intensity: light.intensity(),
        kind,
    })
}

/// Whether `transform` can place `light`: a finite position for a point or
/// spot light, and for a spot or directional light a -Z axis it does not
/// collapse (a zero scale leaves it no direction).
fn check_placement(light: &PunctualLight, transform: Mat4) -> std::result::Result<(), &str> {
    if !transform.is_finite() {
        return Err("its world transform is not finite");
    }
    let directed = !matches!(light.kind, PunctualKind::Point { .. });
    if directed
        && transform
            .transform_vector3(Vec3::NEG_Z)
            .try_normalize()
            .is_none()
    {
        return Err("its world transform collapses the light's -Z axis, leaving it no direction");
    }
    Ok(())
}
