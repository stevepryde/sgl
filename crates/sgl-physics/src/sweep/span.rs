//! The stretch of a ray that lies in a collider's closed solid
//! ([`crate::ray_spans`]): its surface counts as part of it, where a contact
//! ([`super::sweep`]) asks for an approach.
//!
//! A solid here is convex, so a ray is in it over one stretch, from where it
//! enters to where it leaves. Every decision is a comparison of the values
//! the contact ray computes, with nothing grown: a ray lying in a face, one
//! touching an edge or a corner, a tangent to a ball and a ray that starts
//! inside all have a stretch, and a ray beside the solid by any amount `f32`
//! can hold has none.
//!
//! The kernels are the contact ray's. A box, whole or cut, is clipped by the
//! slabs and the cut's plane the box sweep reads ([`super::slab`],
//! [`super::below`], [`super::CutBox`]); a ball by the line crossings of the
//! `b3RayCastSphere` port ([`ray::ball_line`]); a polygon by its edges'
//! half-planes ([`ray::polygon_edge`]), as `b2CastRayPolygon` clips it. A
//! capsule is its two end balls and the side between them: the side is this
//! file's own closed form, since `b3RayCastCapsule` finds only where a ray
//! comes in. It keeps that port's short names (`s`, `d`, `u`) so the two
//! read against each other.

#![allow(clippy::many_single_char_names)]

use super::{CutBox, TIE_ORDER, below, ray, slab};
use crate::shape::{Cut, Shape};
use crate::space::{self, Vector};

/// The stretch of a ray in a closed solid, as fractions of its motion.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Span<V> {
    /// Where the ray is first in the solid, in `[0, 1]`.
    pub enter: f32,
    /// Where it is last in it, in `[enter, 1]`.
    pub exit: f32,
    /// The outward unit normal of the surface crossed at `enter`, or `None`
    /// when the ray starts in the solid without entering it there.
    pub enter_normal: Option<V>,
    /// The outward unit normal of the surface crossed at `exit`, or `None`
    /// when the ray ends in the solid.
    pub exit_normal: Option<V>,
}

/// One end of a stretch before it is held to the ray: a fraction of the
/// motion, which may lie outside `[0, 1]`, and the normal crossed there.
type End<V> = (f32, Option<V>);

/// The stretch of a ray from the origin by `delta` in the closed solid of
/// `target`, cut by `cut`, at `rel`, or `None` when the ray never touches
/// it, the motion is zero or not finite, or the shape has no closed form
/// here: a 3D convex shape, and a ball or a capsule of no radius.
pub(crate) fn span<V: Vector>(
    target: &Shape<V>,
    cut: Option<&Cut<V>>,
    delta: V,
    rel: V,
) -> Option<Span<V>> {
    if delta == V::ZERO || !space::is_finite(delta) {
        return None;
    }
    match (target, cut) {
        (Shape::Box { half }, None) => planes(rel - *half, rel + *half, None, delta),
        (Shape::Box { half }, Some(cut)) => {
            let solid = CutBox::new(*half, cut, rel);
            planes(solid.lo, solid.hi, Some((solid.normal, solid.top)), delta)
        }
        (Shape::Ball { radius }, None) if *radius > 0.0 => ball(rel, *radius, delta),
        (Shape::Capsule { half_axis, radius }, None) if *radius > 0.0 => {
            capsule(rel, *half_axis, *radius, delta)
        }
        (Shape::Convex(polygon), None) if V::AXES == 2 => {
            polygon_span(polygon.points(), rel, delta)
        }
        _ => None,
    }
}

/// Holds a stretch to the ray: `None` when it is empty, behind the start or
/// beyond the end. Entering and leaving at one instant is a touch, and a
/// stretch. An end outside the ray is the ray's own end, with no surface
/// crossed there. `+ 0.0` turns a `-0.0` into `0.0`, as the sweep does.
fn held<V: Vector>(enter: End<V>, exit: End<V>) -> Option<Span<V>> {
    let touches = enter.0 <= exit.0 && enter.0 <= 1.0 && exit.0 >= 0.0;
    if !touches {
        return None;
    }
    let (enter, enter_normal) = if enter.0 >= 0.0 {
        (enter.0 + 0.0, enter.1)
    } else {
        (0.0, None)
    };
    let (exit, exit_normal) = if exit.0 <= 1.0 {
        (exit.0 + 0.0, exit.1)
    } else {
        (1.0, None)
    };
    Some(Span {
        enter,
        exit,
        enter_normal,
        exit_normal,
    })
}

/// A box with edges `lo..=hi`, cut by the plane with `normal` at `top` when
/// `cut` gives one, all relative to the ray's start: the slabs and the plane
/// the box sweep reads, closed. The ray enters by the last of them to let it
/// in and leaves by the first to let it out; a tie goes to the first axis
/// in [`TIE_ORDER`], and the cut's plane decides only when strictly last in
/// or first out, as it does for a contact.
fn planes<V: Vector>(lo: V, hi: V, cut: Option<(V, f32)>, delta: V) -> Option<Span<V>> {
    let mut enter: End<V> = (f32::NEG_INFINITY, None);
    let mut exit: End<V> = (f32::INFINITY, None);
    for &axis in &TIE_ORDER[..V::AXES] {
        let d = delta[axis];
        if d == 0.0 {
            // Parallel: in the slab, its faces included, or never.
            if lo[axis] > 0.0 || hi[axis] < 0.0 {
                return None;
            }
            continue;
        }
        let (entry, leave) = slab(0.0, 0.0, d, lo[axis], hi[axis])?;
        if entry > enter.0 {
            enter = (entry, Some(space::unit(axis, d < 0.0)));
        }
        if leave < exit.0 {
            exit = (leave, Some(space::unit(axis, d > 0.0)));
        }
    }
    if let Some((normal, top)) = cut {
        let d = space::dot(delta, normal);
        if d == 0.0 {
            // Parallel to the plane: under it, or in it, or never.
            if top < 0.0 {
                return None;
            }
        } else {
            let (entry, leave) = below(0.0, d, top)?;
            if entry > enter.0 {
                enter = (entry, Some(normal));
            }
            if leave < exit.0 {
                exit = (leave, Some(normal));
            }
        }
    }
    held(enter, exit)
}

/// A ball of positive `radius` about `center`: where the ray's line crosses
/// it, a tangent at one point, with the radius's direction at each end.
fn ball<V: Vector>(center: V, radius: f32, delta: V) -> Option<Span<V>> {
    let line = ray::ball_line(center, radius, delta)?;
    let normal = |along: f32| space::direction(line.s + line.d * along).map(|(normal, _)| normal);
    held(
        (line.near / line.length, normal(line.near)),
        (line.far / line.length, normal(line.far)),
    )
}

/// A capsule of positive `radius` about `rel`: the union of its two end
/// balls and the side between them, each a stretch of the ray's line, which
/// join into one because the capsule is convex. The normal at an end is the
/// direction from the nearest point of the capsule's segment.
fn capsule<V: Vector>(rel: V, half_axis: V, radius: f32, delta: V) -> Option<Span<V>> {
    // A capsule with no axis is a ball.
    let Some((axis, half_length)) = space::direction(half_axis) else {
        return ball(rel, radius, delta);
    };
    let (d, length) = space::direction(delta)?;
    let (c1, c2) = (rel - half_axis, rel + half_axis);
    let axis_length = half_length * 2.0;
    // From the first centre to the ray's start, and the start along the axis.
    let s = -c1;
    let u = space::dot(s, axis);
    let a12 = space::dot(axis, d);
    let mut reach: Option<(f32, f32)> = None;
    let mut join = |near: f32, far: f32| {
        reach = Some(reach.map_or((near, far), |(first, last)| {
            (space::min(first, near), space::max(last, far))
        }));
    };
    for cap in [c1, c2] {
        if let Some(line) = ray::ball_line(cap, radius, delta) {
            join(line.near, line.far);
        }
    }
    if let Some((near, far)) = side(s, u, a12, axis, axis_length, radius, d) {
        join(near, far);
    }
    let (near, far) = reach?;
    let normal = |along: f32| {
        let p = s + d * along;
        let on_axis = axis * ray::clamp(space::dot(p, axis), axis_length);
        space::direction(p - on_axis).map(|(normal, _)| normal)
    };
    held((near / length, normal(near)), (far / length, normal(far)))
}

/// The stretch of a line in a capsule's side: within `radius` of its axis,
/// between the planes through its two centres. The line starts at `s` from
/// the first centre, `u` along the unit `axis`, and runs along the unit `d`,
/// which makes `a12` with the axis. Distances along `d`, either of which may
/// be infinite; `None` when the line is never in the side.
fn side<V: Vector>(
    s: V,
    u: f32,
    a12: f32,
    axis: V,
    axis_length: f32,
    radius: f32,
    d: V,
) -> Option<(f32, f32)> {
    // Across the axis the line is `sc + perp t`: within the radius where
    // `a t² + 2 b t + c <= 0`.
    let sc = s - axis * u;
    let perp = d - axis * a12;
    let a = space::dot(perp, perp);
    let b = space::dot(sc, perp);
    let c = space::dot(sc, sc) - radius * radius;
    let across = if a == 0.0 {
        // Along the axis: within the radius all the way, or never.
        if c > 0.0 {
            return None;
        }
        (f32::NEG_INFINITY, f32::INFINITY)
    } else {
        let disc = b * b - a * c;
        if disc < 0.0 {
            return None;
        }
        // The roots, each from the sum that does not cancel.
        let root = disc.sqrt();
        if b <= 0.0 {
            let larger = root - b;
            if larger == 0.0 {
                (0.0, 0.0)
            } else {
                (c / larger, larger / a)
            }
        } else {
            let smaller = -b - root;
            (smaller / a, c / smaller)
        }
    };
    // Along the axis the line is at `u + a12 t`: between the centres.
    let along = if a12 == 0.0 {
        if u < 0.0 || u > axis_length {
            return None;
        }
        (f32::NEG_INFINITY, f32::INFINITY)
    } else {
        let (first, second) = ((0.0 - u) / a12, (axis_length - u) / a12);
        if a12 > 0.0 {
            (first, second)
        } else {
            (second, first)
        }
    };
    let (near, far) = (space::max(across.0, along.0), space::min(across.1, along.1));
    (near <= far).then_some((near, far))
}

/// A 2D convex polygon about `rel`: the ray clipped against its edges'
/// half-planes, closed. It enters by the last edge to let it in and leaves
/// by the first to let it out, the first such edge on a tie.
fn polygon_span<V: Vector>(points: &[V], rel: V, delta: V) -> Option<Span<V>> {
    let mut enter: End<V> = (f32::NEG_INFINITY, None);
    let mut exit: End<V> = (f32::INFINITY, None);
    for i in 0..points.len() {
        // The ray is inside the edge while `numerator - a denominator >= 0`.
        let (normal, numerator) = ray::polygon_edge(points, i, rel);
        let denominator = space::dot(normal, delta);
        if denominator == 0.0 {
            // Parallel to the edge: inside it, or along its line, or never.
            if numerator < 0.0 {
                return None;
            }
        } else {
            let at = numerator / denominator;
            if denominator < 0.0 {
                if at > enter.0 {
                    enter = (at, Some(normal));
                }
            } else if at < exit.0 {
                exit = (at, Some(normal));
            }
        }
    }
    held(enter, exit)
}
