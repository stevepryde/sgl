//! The plane solver of soft contact: where a body ends when each collider it
//! overlaps pushes it out along a plane, by no more than that collider's
//! limit.
//!
//! Ported from `Box2D` v3.2.0 (MIT, Erin Catto; `PROVENANCE.md`), commit
//! `178495ba35695aefe9c321e569d2277f06db3b90`: `b2SolvePlanes` of
//! `src/mover.c`, with `b2CollisionPlane` and `b2PlaneSolverResult` of
//! `include/box2d/collision.h`, and `b2PlaneSeparation`, `b2Dot`,
//! `b2MulAdd`, `b2ClampFloat` and `b2AbsFloat` of
//! `include/box2d/math_functions.h`. Generic over the crate's two spaces;
//! the departures are listed in `PROVENANCE.md`.

use crate::space::Vector;

/// Most passes the solver makes over its planes: `b2SolvePlanes`' twenty.
/// It stops sooner when a pass pushes the body less than its tolerance in
/// all.
pub(crate) const MAX_PLANE_ITERATIONS: u32 = 20;

/// One collider a body overlaps, as the solver sees it: `b2CollisionPlane`,
/// the plane's normal and offset written out.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Plane<V> {
    /// Unit normal from the collider toward the body.
    pub normal: V,
    /// How deep the body is along `normal`: the plane lies this far along
    /// it from the body's place.
    pub offset: f32,
    /// Most the collider may push the body along `normal`, in all.
    pub limit: f32,
    /// How far the solve pushed the body along `normal`, in `0..=limit`.
    pub push: f32,
}

/// `b2Dot`: the components' products summed in axis order.
fn dot<V: Vector>(a: V, b: V) -> f32 {
    let mut sum = a[0] * b[0];
    for axis in 1..V::AXES {
        sum += a[axis] * b[axis];
    }
    sum
}

/// `b2ClampFloat`.
fn clamp(a: f32, lower: f32, upper: f32) -> f32 {
    if a < lower {
        lower
    } else if a > upper {
        upper
    } else {
        a
    }
}

/// `b2AbsFloat`.
fn abs(a: f32) -> f32 {
    if a < 0.0 { -a } else { a }
}

/// `b2SolvePlanes`: the translation, from `target`, that puts the body
/// `tolerance` clear of every plane, each pushing it along its normal by no
/// more than its limit in all. Sets each plane's `push`. Planes are solved
/// in order, one after another, pass by pass (Gauss-Seidel), for at most
/// [`MAX_PLANE_ITERATIONS`] passes or until a pass pushes less than
/// `tolerance` in all.
pub(crate) fn solve<V: Vector>(target: V, planes: &mut [Plane<V>], tolerance: f32) -> V {
    for plane in planes.iter_mut() {
        plane.push = 0.0;
    }

    let mut delta = target;
    for _ in 0..MAX_PLANE_ITERATIONS {
        let mut total_push = 0.0;
        for plane in planes.iter_mut() {
            // The plane's separation, kept `tolerance` clear of it.
            let separation = (dot(plane.normal, delta) - plane.offset) - tolerance;
            let push = -separation;

            // Clamp the accumulated push.
            let accumulated = plane.push;
            plane.push = clamp(plane.push + push, 0.0, plane.limit);
            let push = plane.push - accumulated;
            delta = delta + plane.normal * push;

            total_push += abs(push);
        }
        if total_push < tolerance {
            break;
        }
    }
    delta
}

#[cfg(test)]
mod tests {
    use super::*;
    use sgl_core::math::{Vec2, Vec3};
    use wasm_bindgen_test::wasm_bindgen_test;

    fn plane<V>(normal: V, offset: f32, limit: f32) -> Plane<V> {
        Plane {
            normal,
            offset,
            limit,
            push: 0.0,
        }
    }

    /// Defect: a solve that leaves the body inside, pushes past the plane
    /// or past the limit. Oracle: one plane is met head on, so the body ends
    /// exactly `tolerance` clear when the limit allows (depth 0.5 plus 0.01,
    /// along +x), and is pushed exactly the limit when it does not.
    #[wasm_bindgen_test(unsupported = test)]
    fn one_plane_clears_by_the_tolerance_or_pushes_its_limit() {
        let mut planes = [plane(Vec2::X, 0.5, 1.0)];
        let delta = solve(Vec2::ZERO, &mut planes, 0.01);
        assert!((delta.x - 0.51).abs() < 1e-6 && delta.y == 0.0, "{delta}");
        assert!((planes[0].push - 0.51).abs() < 1e-6);

        let mut planes = [plane(Vec3::Y, 0.5, 0.2)];
        let delta = solve(Vec3::ZERO, &mut planes, 0.01);
        assert_eq!(delta, Vec3::new(0.0, 0.2, 0.0));
        assert_eq!(planes[0].push.to_bits(), 0.2f32.to_bits());
    }

    /// Defect: planes solved independently, so two that share a direction
    /// push twice, or a corner is left inside one. Oracle: geometry. Two
    /// perpendicular planes (depths 0.3 along +x and 0.2 along +y) are
    /// cleared by 0.31 and 0.21; two parallel ones (depths 0.3 and 0.1 along
    /// +x) need only the deeper, 0.31, and the shallower ends with no push.
    #[wasm_bindgen_test(unsupported = test)]
    fn several_planes_share_the_push() {
        let mut corner = [plane(Vec2::X, 0.3, 5.0), plane(Vec2::Y, 0.2, 5.0)];
        let delta = solve(Vec2::ZERO, &mut corner, 0.01);
        assert!((delta - Vec2::new(0.31, 0.21)).length() < 1e-6, "{delta}");

        let mut parallel = [plane(Vec2::X, 0.3, 5.0), plane(Vec2::X, 0.1, 5.0)];
        let delta = solve(Vec2::ZERO, &mut parallel, 0.01);
        assert!((delta - Vec2::new(0.31, 0.0)).length() < 1e-6, "{delta}");
        assert_eq!(parallel[1].push.to_bits(), 0);
    }

    /// Defect: an accumulated push that goes negative and pulls the body
    /// into a plane. Oracle: the clamp to `0..=limit`. A body squeezed
    /// between opposite planes whose limits cannot both be met ends where
    /// neither push is negative or past its limit.
    #[wasm_bindgen_test(unsupported = test)]
    fn pushes_stay_between_zero_and_the_limit() {
        let mut squeezed = [plane(Vec2::X, 0.4, 0.1), plane(Vec2::NEG_X, 0.4, 0.3)];
        let delta = solve(Vec2::ZERO, &mut squeezed, 0.01);
        for p in &squeezed {
            assert!((0.0..=p.limit).contains(&p.push), "{p:?}");
        }
        // The net push is the difference of the two, each at most its limit.
        assert!(delta.x.abs() <= 0.3 + 1e-6, "{delta}");
    }
}
