//! A shadowed light's faces and its claim on the atlas: the views its faces
//! take and its screen coverage, which sizes its slots.
use crate::content::light::{Light, LightShape};
use crate::view::View;
use glam::{Mat4, Vec3};

/// The widest spot cone one face covers: a cube face's. A wider spot casts
/// through the cube faces its cone reaches.
const SPOT_FACE_ANGLE: f32 = std::f32::consts::FRAC_PI_4;

/// Each cube face's axis, then the two axes across it, in
/// `View::local_shadow_face`'s order.
const CUBE_AXES: [(Vec3, Vec3, Vec3); 6] = [
    (Vec3::X, Vec3::Y, Vec3::Z),
    (Vec3::NEG_X, Vec3::Y, Vec3::Z),
    (Vec3::Y, Vec3::X, Vec3::Z),
    (Vec3::NEG_Y, Vec3::X, Vec3::Z),
    (Vec3::Z, Vec3::X, Vec3::Y),
    (Vec3::NEG_Z, Vec3::X, Vec3::Y),
];

/// How a light's shadow is laid out.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Shape {
    /// Six faces around the light (`View::local_shadow_face`), of which the
    /// ones in mask `faces` are drawn: all of a point light's, those a
    /// spot's cone reaches for a spot wider than one face, and those that
    /// see some of the half-space in front of a rectangle, a point shadow
    /// from its centre.
    Cube { faces: u8 },
    /// One face along a spot's unit direction (`View::spot_shadow`).
    Spot { direction: Vec3, outer_angle: f32 },
}

/// What a light's shadow shows of the scene: where its faces look from and
/// how far they reach. A light whose `LightView` changes from one frame to
/// the next is a moving light.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct LightView {
    pub position: Vec3,
    pub range: f32,
    pub shape: Shape,
}

impl LightView {
    pub fn new(light: &Light) -> Option<Self> {
        let shape = match light.shape {
            LightShape::Point { .. } => Shape::Cube { faces: 0b11_1111 },
            LightShape::Spot {
                direction,
                outer_angle,
                ..
            } => {
                let direction = direction.normalize();
                if outer_angle <= SPOT_FACE_ANGLE {
                    Shape::Spot {
                        direction,
                        outer_angle,
                    }
                } else {
                    Shape::Cube {
                        faces: cone_faces(direction, outer_angle),
                    }
                }
            }
            // A point shadow from its centre over the half-space in front of
            // its face, as Godot b130438 shadows an area light from its
            // centre (`light_process_area`).
            LightShape::Rect { direction, .. } => Shape::Cube {
                faces: half_space_faces(direction.normalize()),
            },
        };
        let shadow = Self {
            position: light.position,
            range: light.range,
            shape,
        };
        // Closed and subnormal cones are valid animation content but can
        // have no representable perspective face. Use its actual matrix,
        // rather than guessing a minimum cone angle.
        if matches!(shape, Shape::Spot { .. }) && !shadow.face(0).view_projection().is_finite() {
            return None;
        }
        Some(shadow)
    }

    /// The slots it takes: one per face, drawn or not.
    pub fn slots(&self) -> usize {
        match self.shape {
            Shape::Cube { .. } => 6,
            Shape::Spot { .. } => 1,
        }
    }

    /// Whether face `face` is drawn.
    pub fn draws(&self, face: usize) -> bool {
        match self.shape {
            Shape::Cube { faces } => faces & (1 << face) != 0,
            Shape::Spot { .. } => face == 0,
        }
    }

    /// Face `face`'s view.
    pub fn face(&self, face: usize) -> View {
        match self.shape {
            Shape::Cube { .. } => View::local_shadow_face(face as u8, self.position, self.range),
            Shape::Spot {
                direction,
                outer_angle,
            } => View::spot_shadow(self.position, direction, outer_angle, self.range),
        }
    }

    /// The sphere its shadow covers on screen: its range's, or a spot's
    /// cone base's, as Godot sizes it.
    fn extent(&self) -> (Vec3, f32) {
        match self.shape {
            Shape::Cube { .. } => (self.position, self.range),
            Shape::Spot {
                direction,
                outer_angle,
            } => (
                self.position + direction * self.range * outer_angle.cos(),
                self.range * outer_angle.sin(),
            ),
        }
    }

    /// Its screen coverage from a camera with `view` and `projection`:
    /// Godot b130438's `renderer_scene_cull.cpp` `_render_scene`, the
    /// screen diameter of its range (a spot's cone base) over the sum of the
    /// view's half extents at its depth, both on the near plane. Godot snaps
    /// points behind the near plane onto it; this keeps their depth at least
    /// `MIN_DEPTH`. Either way such a light's coverage is large, and
    /// `Atlas::update` caps it at the largest slot.
    pub fn coverage(&self, view: Mat4, projection: Mat4) -> f32 {
        let (center, radius) = self.extent();
        let depth = -(view * center.extend(1.)).z;
        let orthographic = projection.w_axis.w == 1.;
        // The half extents at a distance of one along the view, and the
        // distance its points are seen at, kept in front of the camera.
        let half_extents = 1. / projection.x_axis.x.abs() + 1. / projection.y_axis.y.abs();
        let scale = if orthographic {
            half_extents
        } else {
            half_extents * depth.max(MIN_DEPTH)
        };
        2. * radius / scale
    }

    /// Its coverage of a probe capture's face at `center` that looks at it:
    /// `coverage` for a 90° view.
    pub fn capture_coverage(&self, center: Vec3) -> f32 {
        let (middle, radius) = self.extent();
        radius / middle.distance(center).max(MIN_DEPTH)
    }
}

/// The nearest a light is taken to be to a camera, in metres.
const MIN_DEPTH: f32 = 1e-2;

/// The cube faces that see some of the half-space in front of unit
/// `normal`: those with a corner direction in front of it. A face's
/// directions span its four corners, so one in front is the face's farthest
/// reach toward `normal`.
fn half_space_faces(normal: Vec3) -> u8 {
    let mut faces = 0;
    for (face, (axis, across, along)) in CUBE_AXES.into_iter().enumerate() {
        let reached = [
            across + along,
            across - along,
            -across + along,
            -across - along,
        ]
        .into_iter()
        .any(|corner| normal.dot(axis + corner) > 0.);
        if reached {
            faces |= 1 << face;
        }
    }
    faces
}

/// The cube faces a spot cone around unit `direction`, `outer_angle` wide,
/// reaches: those whose pyramid holds a direction within `outer_angle` of
/// `direction`. Exact for any cone narrower than a right angle, as a
/// scene's spots are (`scene::lights::validate`), so it keeps every face
/// the cone reaches and no other.
fn cone_faces(direction: Vec3, outer_angle: f32) -> u8 {
    let reach = outer_angle.cos();
    let mut faces = 0;
    for (face, (axis, across, along)) in CUBE_AXES.into_iter().enumerate() {
        if nearest_cos(direction, axis, across, along) >= reach {
            faces |= 1 << face;
        }
    }
    faces
}

/// The cosine of the angle from unit `direction` to the nearest direction
/// of the cube face along `axis`, the pyramid `x >= |y|, x >= |z|` in its
/// coordinates (`axis`, `across`, `along`), where it is positive; at most 0
/// where every direction of the face is a right angle or more away.
///
/// That cosine is the length of `direction`'s projection onto the convex
/// pyramid, whose remainder is perpendicular to it and points out of the
/// pyramid (Moreau's decomposition). Mirrored into `y >= z >= 0`, a
/// direction with `x >= y` lies inside; otherwise it projects onto the side
/// plane `x = y`, at `(m, m, z)` with `m = (x + y) / 2`, while that point
/// keeps `m >= z`, and onto the corner ray `(1, 1, 1)` beyond it, or onto
/// the light itself where its dot with that ray is negative.
fn nearest_cos(direction: Vec3, axis: Vec3, across: Vec3, along: Vec3) -> f32 {
    let x = direction.dot(axis);
    let (a, b) = (direction.dot(across).abs(), direction.dot(along).abs());
    let (y, z) = (a.max(b), a.min(b));
    if x >= y {
        return 1.;
    }
    let m = 0.5 * (x + y);
    if m >= z {
        (2. * m * m + z * z).sqrt()
    } else {
        (x + y + z) / 3f32.sqrt()
    }
}

#[cfg(test)]
mod tests;
