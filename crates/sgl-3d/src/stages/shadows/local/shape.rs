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
    pub fn new(light: &Light) -> Self {
        let shape = match light.shape {
            LightShape::Point => Shape::Cube { faces: 0b11_1111 },
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
        Self {
            position: light.position,
            range: light.range,
            shape,
        }
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
/// can reach: those whose four side planes it does not lie wholly beyond.
/// Conservative: it keeps every face the cone reaches.
fn cone_faces(direction: Vec3, outer_angle: f32) -> u8 {
    let reach = -outer_angle.sin();
    let mut faces = 0;
    for (face, (axis, across, along)) in CUBE_AXES.into_iter().enumerate() {
        let reached = [across, -across, along, -along]
            .into_iter()
            .all(|side| direction.dot((axis + side).normalize()) >= reach);
        if reached {
            faces |= 1 << face;
        }
    }
    faces
}
