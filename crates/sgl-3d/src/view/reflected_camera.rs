//! A planar reflector's camera (the architecture's Frame, "Planar
//! reflections"): the one owner of the mirrored view, the mirrored and
//! cropped projection, the oblique near plane and the screen bounds a
//! picture covers.
//!
//! Following three.js r187 (456a78f, MIT) `ReflectorNode.updateBefore`,
//! nothing copied: the virtual camera stands at the mirrored eye as a
//! proper camera, so its image is the mirror image flipped left to right
//! and winding never changes; the projection's horizontal offset is
//! mirrored with it; and the reflector's plane becomes the near plane after
//! Lengyel, "Oblique View Frustum Depth Projection and Clipping" (Journal
//! of Game Development 1(2), 2005). The reversed-Z form of Lengyel's
//! construction and the crop are SGL3D's own (D-77).
use crate::Camera;
use glam::{Mat4, Vec2, Vec3, Vec4};

/// The flip that turns the mirror image into a proper camera's image:
/// view-space and clip-space x negated.
const FLIP: Mat4 = Mat4::from_cols(Vec4::NEG_X, Vec4::Y, Vec4::Z, Vec4::W);

/// The reflection in `plane` (unit normal, d): a point's mirror image.
pub(crate) fn mirror(plane: Vec4) -> Mat4 {
    let n = plane.truncate();
    let column = |axis: Vec3| (axis - 2. * n * n.dot(axis)).extend(0.);
    Mat4::from_cols(
        column(Vec3::X),
        column(Vec3::Y),
        column(Vec3::Z),
        (-2. * plane.w * n).extend(1.),
    )
}

/// A rectangle of a camera's image in normalized device coordinates, x
/// right and y up, each within -1..=1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Rect {
    pub min: Vec2,
    pub max: Vec2,
}

impl Rect {
    /// The whole image.
    #[cfg(test)]
    pub const FULL: Self = Self {
        min: Vec2::splat(-1.),
        max: Vec2::splat(1.),
    };
}

/// The camera that renders `consumer`'s reflection in `plane` (unit normal,
/// d, positive on the reflecting side) over `rect` of the mirrored image:
/// its view is the consumer's mirrored in the plane and flipped left to
/// right, a rigid transform; its projection the consumer's with the flip
/// applied on both sides, which mirrors its horizontal offset, cropped so
/// that `rect` fills the image. The projection is not yet clipped at the
/// plane (`oblique`).
pub(crate) fn camera(consumer: &Camera, plane: Vec4, rect: Rect) -> Camera {
    let reflection = mirror(plane);
    let center = (rect.min + rect.max) * 0.5;
    let half = (rect.max - rect.min) * 0.5;
    // Clip x′ = (x − center.x w) / half.x, and y alike.
    let crop = Mat4::from_cols(
        Vec4::new(1. / half.x, 0., 0., 0.),
        Vec4::new(0., 1. / half.y, 0., 0.),
        Vec4::Z,
        Vec4::new(-center.x / half.x, -center.y / half.y, 0., 1.),
    );
    Camera {
        view: FLIP * consumer.view * reflection,
        projection: crop * FLIP * consumer.projection * FLIP,
        eye: reflection.transform_point3(consumer.eye),
    }
}

/// Where a point the consumer sees at `ndc` lies in its mirrored image: the
/// same point of a surface in the plane, flipped left to right.
pub(crate) fn mirrored(ndc: Vec2) -> Vec2 {
    Vec2::new(-ndc.x, ndc.y)
}

/// `projection`, a reversed-Z projection (device depth 1 near, 0 far), with
/// its near plane moved onto `plane` (world space, kept side positive) as
/// seen through `view`: device depth is 1 on the plane, above 1 behind it,
/// which raster clips, and falls to 0 at the far corner on the plane's
/// side, so nothing in the frustum beyond the plane is clipped at the far
/// side. Only the z row changes. None where the frustum holds nothing
/// beyond the plane.
///
/// With C the plane in view space and Q the far corner (clip z 0) whose x
/// and y take C's signs, the near plane of a reversed-Z projection is
/// `row_w − row_z`, so `row_z = row_w − a C` puts it on C, and the far
/// plane `row_z` passes through Q where `a = (row_w · Q) / (C · Q)`.
pub(crate) fn oblique(projection: Mat4, view: Mat4, plane: Vec4) -> Option<Mat4> {
    // A plane transforms by the inverse transpose of what moves points.
    let c = view.inverse().transpose() * plane;
    let corner = projection.inverse() * Vec4::new(c.x.signum(), c.y.signum(), 0., 1.);
    let row_w = projection.row(3);
    let scale = row_w.dot(corner) / c.dot(corner);
    if !scale.is_finite() || scale <= 0. {
        return None;
    }
    let row_z = row_w - c * scale;
    let mut clipped = projection.transpose();
    clipped.z_axis = row_z;
    let clipped = clipped.transpose();
    clipped.is_finite().then_some(clipped)
}

/// The bounds in `view_projection`'s image of the boxes `bounds` posed by
/// `pose`s, clipped at its near plane (reversed Z: clip z above clip w is
/// nearer) and to the image: none where no part of a box is in it.
pub(crate) fn image_bounds(
    view_projection: Mat4,
    boxes: impl Iterator<Item = (Mat4, [Vec3; 2])>,
) -> Option<Rect> {
    let mut min = Vec2::splat(f32::INFINITY);
    let mut max = Vec2::splat(f32::NEG_INFINITY);
    let mut take = |clip: Vec4| {
        let ndc = clip.truncate().truncate() / clip.w;
        min = min.min(ndc);
        max = max.max(ndc);
    };
    for (pose, [low, high]) in boxes {
        let transform = view_projection * pose;
        let corner = |index: usize| {
            transform
                * Vec3::new(
                    if index & 1 == 0 { low.x } else { high.x },
                    if index & 2 == 0 { low.y } else { high.y },
                    if index & 4 == 0 { low.z } else { high.z },
                )
                .extend(1.)
        };
        // In front of the near plane, by a margin that keeps w positive.
        let inside = |clip: Vec4| clip.w - clip.z;
        for a in 0..8 {
            let from = corner(a);
            if inside(from) >= 0. && from.w > 0. {
                take(from);
            }
            // Each edge once: toward the corners one bit greater.
            for bit in [1, 2, 4] {
                if a & bit != 0 {
                    continue;
                }
                let to = corner(a | bit);
                let (near_from, near_to) = (inside(from), inside(to));
                if (near_from < 0.) != (near_to < 0.) {
                    let crossing = from.lerp(to, near_from / (near_from - near_to));
                    if crossing.w > 0. {
                        take(crossing);
                    }
                }
            }
        }
    }
    let min = min.max(Vec2::splat(-1.));
    let max = max.min(Vec2::splat(1.));
    (min.x < max.x && min.y < max.y).then_some(Rect { min, max })
}

/// The texels of an image of `full` texels a side that hold `rect` of it,
/// as (first texel, count) per axis, rows from the top: padded by a texel
/// on each side (a picture's bilinear lookup; a view's jitter) and at least
/// one texel.
pub(crate) fn texels(rect: Rect, full: [u32; 2]) -> [(u32, u32); 2] {
    let span = |low: f32, high: f32, side: u32| {
        let side_f = side as f32;
        let first = ((low * 0.5 + 0.5) * side_f - 1.).floor().max(0.) as u32;
        let last = ((high * 0.5 + 0.5) * side_f + 1.).ceil().min(side_f) as u32;
        let first = first.min(side.saturating_sub(1));
        (first, last.saturating_sub(first).max(1))
    };
    [
        span(rect.min.x, rect.max.x, full[0]),
        // Row 0 is the top: y up in the image.
        span(-rect.max.y, -rect.min.y, full[1]),
    ]
}

/// The steps a picture's side grows and shrinks in: eighths of the full
/// side, so a moving camera reallocates its picture rarely (D-77).
const CROP_STEPS: u32 = 8;

/// A picture's crop: the size of its target in texels and the rectangle of
/// the mirrored image it holds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Crop {
    pub size: [u32; 2],
    pub rect: Rect,
}

/// The crop of a picture that must hold `needed` of a mirrored image of
/// `full` texels (the consumer's render size at the level's scale): each
/// side the needed texels rounded up to a step of an eighth of the full
/// side, kept from `held`, the size the picture has, while that still
/// holds them with less than a step to spare, so a side grows at once and
/// shrinks only when a whole step is unused; the rectangle starts on a
/// texel and holds the needed one.
pub(crate) fn crop(needed: Rect, full: [u32; 2], held: Option<[u32; 2]>) -> Crop {
    let spans = texels(needed, full);
    let mut size = [0; 2];
    let mut start = [0; 2];
    for axis in 0..2 {
        let (first, count) = spans[axis];
        let side = full[axis].max(1);
        let step = side.div_ceil(CROP_STEPS);
        let wanted = (count.div_ceil(step) * step).min(side);
        size[axis] = match held {
            Some(held) if held[axis] >= wanted && held[axis] < wanted + 2 * step => {
                held[axis].min(side)
            }
            _ => wanted,
        };
        start[axis] = first.min(side - size[axis]);
    }
    let ndc = |texel: u32, side: u32| texel as f32 / side.max(1) as f32 * 2. - 1.;
    let (left, right) = (ndc(start[0], full[0]), ndc(start[0] + size[0], full[0]));
    let (top, bottom) = (-ndc(start[1], full[1]), -ndc(start[1] + size[1], full[1]));
    Crop {
        size,
        rect: Rect {
            min: Vec2::new(left, bottom),
            max: Vec2::new(right, top),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn consumer() -> Camera {
        let eye = Vec3::new(0.7, 1.6, 3.);
        Camera {
            view: glam::camera::rh::view::look_at_mat4(eye, Vec3::new(-0.4, 0.9, -2.), Vec3::Y),
            projection: crate::perspective(1.1, 16. / 9., 0.1),
            eye,
        }
    }

    /// A tilted plane through (0, 1, -2), not axis-aligned.
    fn plane() -> Vec4 {
        let normal = Vec3::new(0.2, 0.1, 1.).normalize();
        normal.extend(-normal.dot(Vec3::new(0., 1., -2.)))
    }

    fn ndc(camera: &Camera, projection: Mat4, point: Vec3) -> Vec3 {
        let clip = projection * camera.view * point.extend(1.);
        clip.truncate() / clip.w
    }

    /// A point's mirror image by construction: twice its foot on the plane
    /// less itself.
    fn image(plane: Vec4, point: Vec3) -> Vec3 {
        let normal = plane.truncate();
        let foot = point - normal * (normal.dot(point) + plane.w);
        2. * foot - point
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn a_point_reflects_to_its_image_and_the_plane_stays() {
        let plane = plane();
        let point = Vec3::new(1.5, -0.3, 4.);
        let reflected = mirror(plane).transform_point3(point);
        assert!((reflected - image(plane, point)).length() < 1e-5);
        let on_plane =
            Vec3::new(1., 1., -2.) - plane.truncate() * plane.dot(Vec4::new(1., 1., -2., 1.));
        assert!((mirror(plane).transform_point3(on_plane) - on_plane).length() < 1e-5);
    }

    /// The reflected view is a rigid transform, so triangles keep their
    /// winding, and its eye is the consumer's mirror image.
    #[wasm_bindgen_test(unsupported = test)]
    fn the_reflected_view_keeps_handedness() {
        let consumer = consumer();
        let reflected = camera(&consumer, plane(), Rect::FULL);
        let rotation = glam::Mat3::from_mat4(reflected.view);
        assert!((rotation.determinant() - 1.).abs() < 1e-5);
        assert!((reflected.eye - image(plane(), consumer.eye)).length() < 1e-5);
        assert!(reflected.view.transform_point3(reflected.eye).length() < 1e-4);
    }

    /// What the consumer sees in the mirror at a pixel is what lies along
    /// the reflected ray: an object's mirror image, seen by the consumer
    /// through the plane, and the object itself, seen by the reflected
    /// camera, land on mirrored pixels.
    #[wasm_bindgen_test(unsupported = test)]
    fn an_object_shows_where_its_mirror_image_would() {
        let consumer = consumer();
        let plane = plane();
        let reflected = camera(&consumer, plane, Rect::FULL);
        // In front of the plane, behind the consumer's back.
        let object = Vec3::new(1.2, 2.1, 6.);
        assert!(plane.dot(object.extend(1.)) > 0.);
        let through = ndc(&consumer, consumer.projection, image(plane, object));
        let seen = ndc(&reflected, reflected.projection, object);
        assert!((mirrored(through.truncate()) - seen.truncate()).length() < 1e-4);
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn the_oblique_plane_is_the_near_plane() {
        let consumer = consumer();
        let plane = plane();
        let reflected = camera(&consumer, plane, Rect::FULL);
        let clipped = oblique(reflected.projection, reflected.view, plane).unwrap();
        // The x, y and w rows are the unclipped projection's.
        for row in [0, 1, 3] {
            assert_eq!(clipped.row(row), reflected.projection.row(row));
        }
        // A point of the plane inside the frustum has depth 1.
        let on_plane = image(plane, Vec3::new(0., 1., -2.));
        let depth = ndc(&reflected, clipped, on_plane).z;
        assert!((depth - 1.).abs() < 1e-4, "{depth}");
        // Beyond the plane, depth is within 0..1 and falls with distance
        // along the reflected camera's ray.
        let direction = (on_plane - reflected.eye).normalize();
        let mut last = 1.;
        for metres in [0.1, 1., 10., 100., 10_000.] {
            let depth = ndc(&reflected, clipped, on_plane + direction * metres).z;
            assert!(depth > 0. && depth < last, "{metres}: {depth}");
            last = depth;
        }
        // Between the reflected eye and the plane, depth is above 1: clipped.
        let behind = ndc(&reflected, clipped, on_plane - direction * 0.05).z;
        assert!(behind > 1., "{behind}");
    }

    /// Every direction of the frustum that runs away from the plane keeps a
    /// depth of at least 0 at any distance: the far side clips nothing.
    #[wasm_bindgen_test(unsupported = test)]
    fn the_far_side_clips_nothing_beyond_the_plane() {
        let consumer = consumer();
        let plane = plane();
        let reflected = camera(&consumer, plane, Rect::FULL);
        let clipped = oblique(reflected.projection, reflected.view, plane).unwrap();
        let unproject = (reflected.projection * reflected.view).inverse();
        for x in [-1., -0.5, 0., 0.5, 1.] {
            for y in [-1., -0.5, 0., 0.5, 1.] {
                let near = unproject.project_point3(Vec3::new(x, y, 1.));
                let direction = (near - reflected.eye).normalize();
                if plane.truncate().dot(direction) <= 0. {
                    continue;
                }
                let far = reflected.eye + direction * 1e6;
                let depth = ndc(&reflected, clipped, far).z;
                assert!(depth >= -1e-6, "({x}, {y}): {depth}");
            }
        }
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn a_plane_the_frustum_looks_away_from_has_no_projection() {
        let consumer = consumer();
        // The reflecting side is behind the reflected camera's whole view.
        let away = -plane();
        let reflected = camera(&consumer, plane(), Rect::FULL);
        assert!(oblique(reflected.projection, reflected.view, away).is_none());
    }

    /// A cropped camera's image is the rectangle of the whole one's.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_crop_shows_its_rectangle_of_the_whole_image() {
        let consumer = consumer();
        let plane = plane();
        let rect = Rect {
            min: Vec2::new(-0.5, 0.),
            max: Vec2::new(0.25, 0.75),
        };
        let whole = camera(&consumer, plane, Rect::FULL);
        let cropped = camera(&consumer, plane, rect);
        let object = Vec3::new(1.2, 2.1, 6.);
        let in_whole = ndc(&whole, whole.projection, object).truncate();
        let in_crop = ndc(&cropped, cropped.projection, object).truncate();
        let expected = (in_whole - rect.min) / (rect.max - rect.min) * 2. - 1.;
        assert!((in_crop - expected).length() < 1e-4);
    }

    /// A box wholly in view projects to the extremes of its corners; one
    /// that passes behind the camera is clipped at the near plane and to
    /// the image, not turned inside out.
    #[wasm_bindgen_test(unsupported = test)]
    fn image_bounds_clip_at_the_near_plane() {
        let camera = Camera {
            view: glam::camera::rh::view::look_at_mat4(Vec3::ZERO, Vec3::NEG_Z, Vec3::Y),
            projection: crate::perspective(std::f32::consts::FRAC_PI_2, 1., 0.1),
            eye: Vec3::ZERO,
        };
        let view_projection = camera.projection * camera.view;
        // A unit card 2 m ahead: its corners at ±0.5 / 2.
        let card = [Vec3::new(-0.5, -0.5, -2.), Vec3::new(0.5, 0.5, -2.)];
        let rect = image_bounds(view_projection, [(Mat4::IDENTITY, card)].into_iter()).unwrap();
        assert!((rect.min - Vec2::splat(-0.25)).length() < 1e-5);
        assert!((rect.max - Vec2::splat(0.25)).length() < 1e-5);
        // A floor from behind the camera to 10 m ahead, 1 m below it: its
        // far edge at y = -1 / 10, and it fills the bottom of the image.
        let floor = [Vec3::new(-50., -1., -10.), Vec3::new(50., -1., 10.)];
        let rect = image_bounds(view_projection, [(Mat4::IDENTITY, floor)].into_iter()).unwrap();
        assert_eq!(rect.min, Vec2::new(-1., -1.));
        assert!((rect.max.y + 0.1).abs() < 1e-5 && rect.max.x == 1.);
        // Behind the camera: nothing.
        let behind = [Vec3::new(-1., -1., 2.), Vec3::new(1., 1., 3.)];
        assert!(image_bounds(view_projection, [(Mat4::IDENTITY, behind)].into_iter()).is_none());
    }

    /// The crop holds the needed rectangle, in eighths of the full size,
    /// and keeps a size until a whole step is spare.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_crop_holds_what_is_needed_in_eighths() {
        let full = [800, 400];
        let needed = Rect {
            min: Vec2::new(-0.2, -0.1),
            max: Vec2::new(0.3, 0.5),
        };
        // Texels 320..520 of 800 and rows 100..220 of 400, padded by one.
        let first = crop(needed, full, None);
        assert_eq!(first.size, [300, 150]);
        let holds = |crop: Crop, rect: Rect| {
            crop.rect.min.x <= rect.min.x
                && crop.rect.min.y <= rect.min.y
                && crop.rect.max.x >= rect.max.x
                && crop.rect.max.y >= rect.max.y
        };
        assert!(holds(first, needed));
        // The rectangle's share of the image is the size's share of full.
        let share = (first.rect.max - first.rect.min) * 0.5;
        assert!((share.x - 300. / 800.).abs() < 1e-6 && (share.y - 150. / 400.).abs() < 1e-6);
        // A little less needed keeps the size; a whole step less shrinks it.
        let smaller = Rect {
            min: Vec2::new(-0.2, -0.1),
            max: Vec2::new(0.1, 0.5),
        };
        assert_eq!(crop(smaller, full, Some(first.size)).size, [300, 150]);
        let much_smaller = Rect {
            min: Vec2::new(-0.2, -0.1),
            max: Vec2::new(-0.1, 0.5),
        };
        let shrunk = crop(much_smaller, full, Some(first.size));
        assert_eq!(shrunk.size, [100, 150]);
        assert!(holds(shrunk, much_smaller));
        // More needed grows at once, and never past the full size.
        assert_eq!(crop(Rect::FULL, full, Some(first.size)).size, full);
    }
}
