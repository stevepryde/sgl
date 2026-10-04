//! Conservative clipping of retained triangle ranges, without changing primitive order.
use crate::scene::mesh_ranges::MeshRanges;
use glam::{DMat4, DVec4, Mat4, Vec3};
use std::ops::Range;

pub(crate) struct Frustum {
    planes: [DVec4; 6],
    error: [DVec4; 6],
    /// Whether the near plane (the last) culls.
    near: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Relation {
    Outside,
    Crossing,
    Inside,
}

impl Frustum {
    pub fn new(view: Mat4, projection: Mat4, pose: Mat4, jitter: [f32; 2]) -> Self {
        let view = view.as_dmat4();
        let pose = pose.as_dmat4();
        let projection = projection.as_dmat4();
        let mut shift = DMat4::IDENTITY;
        shift.w_axis.x = 2. * f64::from(jitter[0]);
        shift.w_axis.y = 2. * f64::from(jitter[1]);
        let rows = (shift * projection * view * pose).transpose();
        // Bound cancellation in the shader's separate model/view/projection
        // operations. Double precision on the CPU alone would not cover that
        // f32 error, particularly in large worlds or at the far clip plane.
        let magnitude =
            (absolute(shift) * absolute(projection) * absolute(view) * absolute(pose)).transpose();
        let planes = |m: DMat4| {
            [
                m.w_axis + m.x_axis,
                m.w_axis - m.x_axis,
                m.w_axis + m.y_axis,
                m.w_axis - m.y_axis,
                m.z_axis,
                m.w_axis - m.z_axis,
            ]
        };
        let mut error = planes(magnitude);
        error[1] = error[0];
        error[3] = error[2];
        error[5] = magnitude.w_axis + magnitude.z_axis;
        Self {
            planes: planes(rows),
            error: error.map(|row| row * (32. * f64::from(f32::EPSILON))),
            near: true,
        }
    }

    /// This frustum without its near plane, for a directional shadow
    /// cascade, whose casters between the light and the cascade still cast
    /// (Bevy pushes a cascade frustum's near plane to infinity).
    pub fn without_near(self) -> Self {
        Self {
            near: false,
            ..self
        }
    }

    /// Whether any part of `bounds` may lie inside it.
    pub fn reaches(&self, bounds: [Vec3; 2]) -> bool {
        self.classify(bounds) != Relation::Outside
    }

    fn classify(&self, bounds: [Vec3; 2]) -> Relation {
        // Empty or nonfinite input must not produce a false visibility rejection.
        if !bounds[0].is_finite() || !bounds[1].is_finite() {
            return Relation::Crossing;
        }
        let lo = bounds[0].as_dvec3();
        let hi = bounds[1].as_dvec3();
        let center = ((lo + hi) * 0.5).extend(1.);
        let extent = ((hi - lo) * 0.5).extend(0.);
        let largest = lo.abs().max(hi.abs()).extend(1.);
        let mut inside = true;
        let planes = if self.near { 6 } else { 5 };
        for (plane, error) in self.planes[..planes].iter().zip(&self.error) {
            let distance = plane.dot(center);
            let radius = plane.abs().dot(extent);
            let tolerance = error.dot(largest);
            if distance + radius < -tolerance {
                return Relation::Outside;
            }
            inside &= distance - radius > tolerance;
        }
        if inside {
            Relation::Inside
        } else {
            Relation::Crossing
        }
    }
}

/// Whether any part of `bounds` may lie inside the clip volume of
/// `transform`: false only when all eight corners are outside one clip plane.
/// Local-light shadow faces' test for their casters and the static edits
/// that reach them; deliberately not `Frustum`'s, whose conservative
/// tolerance would draw another set.
pub(crate) fn clip_intersects(bounds: [Vec3; 2], transform: Mat4) -> bool {
    let mut outside = [true; 6];
    for corner in 0..8 {
        let p = Vec3::new(
            bounds[corner & 1].x,
            bounds[(corner >> 1) & 1].y,
            bounds[(corner >> 2) & 1].z,
        );
        let c = transform * p.extend(1.);
        for (out, plane) in
            outside
                .iter_mut()
                .zip([c.x + c.w, c.w - c.x, c.y + c.w, c.w - c.y, c.z, c.w - c.z])
        {
            *out &= plane < 0.;
        }
    }
    !outside.into_iter().any(|out| out)
}

fn absolute(matrix: DMat4) -> DMat4 {
    DMat4::from_cols_array(&matrix.to_cols_array().map(f64::abs))
}

impl MeshRanges {
    /// Visit visible ranges in original order, coalescing adjacent accepted nodes.
    pub fn visible(&self, frustum: Option<&Frustum>, mut draw: impl FnMut(Range<u32>)) {
        let Some(root) = self.nodes.first() else {
            return;
        };
        let Some(frustum) = frustum else {
            draw(root.indices.clone());
            return;
        };
        let mut at = 0;
        let mut pending: Option<Range<u32>> = None;
        while let Some(node) = self.nodes.get(at) {
            match frustum.classify(node.bounds) {
                Relation::Outside => at = node.end,
                Relation::Crossing if node.end > at + 1 => at += 1,
                _ => {
                    if let Some(range) = &mut pending {
                        if range.end == node.indices.start {
                            range.end = node.indices.end;
                        } else {
                            draw(range.clone());
                            *range = node.indices.clone();
                        }
                    } else {
                        pending = Some(node.indices.clone());
                    }
                    at = node.end;
                }
            }
        }
        if let Some(range) = pending {
            draw(range);
        }
    }
}

#[cfg(test)]
mod tests;
