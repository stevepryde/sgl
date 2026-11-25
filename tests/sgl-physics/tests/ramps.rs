//! Ramps: boxes cut by one plane (physics.md, "Colliders", "Sweep" and
//! "Move and slide"), in 2D and 3D. Every test names the defect it exists to
//! catch and takes its expectation from geometry worked by hand, from the
//! cut box's own definition (inside the box and on the solid side of its
//! plane) evaluated in `f64` by the test, or from a requirement, never from
//! the crate's sweep. The hand-worked and walking tests run natively and in
//! a wasm runtime; the property tests are native only.
#![allow(clippy::cast_precision_loss)]

use sgl_core::math::{Vec2, Vec3};
use sgl_physics::{
    ALL_LAYERS, Collider, ColliderKind, Contacts, Cut, Hit, MoverConfig, MoverSettings, Shape,
    Vector, cast, overlaps, ray, slide, snap_to_ground,
};
use wasm_bindgen_test::wasm_bindgen_test;

/// Mover settings from positional values, in `MoverSettings`' order.
fn mover<V: Vector>(
    up: V,
    skin: f32,
    snap_distance: f32,
    normal_threshold: f32,
    block_epsilon: f32,
) -> MoverConfig<V> {
    MoverConfig::new(MoverSettings {
        up,
        skin,
        snap_distance,
        normal_threshold,
        block_epsilon,
    })
}

fn shape<V>(half: V) -> Shape<V> {
    Shape::Box { half }
}

/// A collider as the test describes it: a box, and the plane that cuts it
/// as given (any length), which the oracle reads in `f64`.
#[derive(Debug, Clone, Copy)]
struct Block<V> {
    id: u64,
    center: V,
    half: V,
    cut: Option<(V, f32)>,
}

impl<V: Vector> Block<V> {
    fn collider(&self) -> Collider<V> {
        let collider = Collider::new(self.id, shape(self.half), self.center);
        match self.cut {
            Some((normal, offset)) => collider.with_cut(Cut::new(normal, offset)),
            None => collider,
        }
    }

    /// How far `p` lies inside the solid part, by the block's own
    /// definition, in `f64`: the least of its depth inside the box on each
    /// axis and its depth below the plane. Positive inside.
    fn depth(&self, p: [f64; 3]) -> f64 {
        let mut depth = f64::INFINITY;
        for (axis, &at) in p.iter().enumerate().take(V::AXES) {
            let off = at - f64::from(self.center[axis]);
            depth = depth.min(f64::from(self.half[axis]) - off.abs());
        }
        if self.cut.is_some() {
            depth = depth.min(-self.plane(p));
        }
        depth
    }

    /// Points of the solid part: the box's corners and a grid through it on
    /// the solid side of the plane, and where the plane crosses the box's
    /// edges.
    fn points(&self, per_axis: usize) -> Vec<[f64; 3]> {
        let mut out: Vec<_> = grid(self.center, self.half, per_axis)
            .into_iter()
            .filter(|&p| self.depth(p) >= -1e-9)
            .collect();
        if self.cut.is_some() {
            let corners = grid(self.center, self.half, 2);
            // Corners are listed by axis bits, so an edge joins two whose
            // indices differ in one bit.
            for (i, &a) in corners.iter().enumerate() {
                for (j, &b) in corners.iter().enumerate().skip(i + 1) {
                    let (da, db) = (self.plane(a), self.plane(b));
                    if (i ^ j).is_power_of_two() && (da < 0.0) != (db < 0.0) {
                        let s = da / (da - db);
                        out.push([0, 1, 2].map(|k| a[k] + (b[k] - a[k]) * s));
                    }
                }
            }
        }
        out
    }

    /// `normal · (p - center) - offset` with the unit normal, in `f64`.
    fn plane(&self, p: [f64; 3]) -> f64 {
        let (normal, offset) = self.cut.expect("cut");
        let (mut along, mut length) = (0.0, 0.0);
        for (axis, &at) in p.iter().enumerate().take(V::AXES) {
            let n = f64::from(normal[axis]);
            along += n * (at - f64::from(self.center[axis]));
            length += n * n;
        }
        along / length.sqrt() - f64::from(offset)
    }
}

/// `per_axis` points along each axis of the box, ends included.
fn grid<V: Vector>(center: V, half: V, per_axis: usize) -> Vec<[f64; 3]> {
    let step = |i: usize| 2.0 * i as f64 / (per_axis - 1) as f64 - 1.0;
    let mut out = Vec::new();
    let zs = if V::AXES == 3 { per_axis } else { 1 };
    for i in 0..per_axis {
        for j in 0..per_axis {
            for k in 0..zs {
                let mut p = [0.0; 3];
                for (axis, s) in [(0, step(i)), (1, step(j)), (2, step(k))]
                    .into_iter()
                    .take(V::AXES)
                {
                    p[axis] = f64::from(center[axis]) + f64::from(half[axis]) * s;
                }
                out.push(p);
            }
        }
    }
    out
}

/// How deep the box body at `center` reaches into `block`, sampled both
/// ways: points through the body against the block's definition, and the
/// block's points (its corners, its plane's crossings of its edges, a grid)
/// against the body's box.
fn penetration<V: Vector>(center: V, half: V, block: &Block<V>) -> f64 {
    let into_block = grid(center, half, if V::AXES == 2 { 9 } else { 5 })
        .into_iter()
        .map(|p| block.depth(p))
        .fold(f64::NEG_INFINITY, f64::max);
    let body = Block {
        id: 0,
        center,
        half,
        cut: None,
    };
    let into_body = block
        .points(if V::AXES == 2 { 7 } else { 5 })
        .into_iter()
        .map(|p| body.depth(p))
        .fold(f64::NEG_INFINITY, f64::max);
    into_block.max(into_body)
}

/// Deeper than this is inside: a rounding step at these coordinates is a
/// few millionths, the skin a hundredth.
const INSIDE: f64 = 1e-4;

// ------------------------------------------------------------ hand-worked

/// The ramp the 2D and upright 3D cases share: the box from (-2, -1.5) to
/// (2, 1.5) at the origin, cut by the plane through its centre with normal
/// (-3, 4)/5, so the solid part is `y <= 0.75 x`, rising to the right.
fn ramp_2d() -> Block<Vec2> {
    Block {
        id: 1,
        center: Vec2::ZERO,
        half: Vec2::new(2.0, 1.5),
        cut: Some((Vec2::new(-3.0, 4.0), 0.0)),
    }
}

fn assert_hit<V: Vector + core::fmt::Debug>(hit: Option<Hit<V>>, t: f32, normal: V) {
    let hit = hit.expect("the sweep meets the ramp");
    assert!(
        (hit.t - t).abs() < 1e-6,
        "t was {}, geometry says {t}",
        hit.t
    );
    for axis in 0..V::AXES {
        assert!(
            (hit.normal[axis] - normal[axis]).abs() < 1e-6,
            "normal {:?}, geometry says {normal:?}",
            hit.normal
        );
    }
}

/// Defect: a cut box swept as its whole box, or the plane's slab read with
/// a sign slip, so a body lands on air above a ramp or sinks into it.
/// Oracle: geometry. A unit body centred at (-1, 3) falls by 4. Its
/// x-extent is -1.5..-0.5, where the ramp `y = 0.75 x` is highest at the
/// right edge, y = -0.375; its bottom starts at 2.5, so it falls 2.875 of
/// the 4: t = 0.71875, on the plane, normal (-0.6, 0.8). Its bottom passes
/// the box's top (y = 1.5) at t = 0.25, so the whole box would stop it
/// there. A body moving right at height y = -1..0 from x = -3.5 (right
/// edge) by 4 first meets the box's left face at t = 0.375 and the ramp
/// where `0.75 x = -1`, x = -4/3, after 13/6 of travel: t = 13/24.
#[wasm_bindgen_test(unsupported = test)]
fn a_box_meets_a_2d_ramp_where_geometry_puts_it() {
    let world = [ramp_2d().collider()];
    let body = shape(Vec2::splat(0.5));
    let fall = cast(
        &body,
        Vec2::new(-1.0, 3.0),
        Vec2::new(0.0, -4.0),
        &world[..],
        ALL_LAYERS,
    );
    assert_hit(fall.unwrap(), 0.718_75, Vec2::new(-0.6, 0.8));
    let walk = cast(
        &body,
        Vec2::new(-4.0, -0.5),
        Vec2::new(4.0, 0.0),
        &world[..],
        ALL_LAYERS,
    );
    assert_hit(walk.unwrap(), 13.0 / 24.0, Vec2::new(-0.6, 0.8));
}

/// Defect: a cut box whose extent along an axis is read as the whole
/// box's, so a body stops on air short of a face the cut planed away
/// entirely. Oracle: geometry. The 2D ramp's box with its plane lowered by
/// 0.5, `-0.6 x + 0.8 y <= -0.5`, is solid below `y = 0.75 x - 0.625`:
/// its left face is gone but for the corner (-7/6, -1.5), and its top but
/// for the corner (2, 0.875). A unit body spanning y -2.4..-1.4, its right
/// edge at x = -4, moving right by 4 meets that left corner with its face
/// after 17/6 of 4 (t = 17/24, normal -x), not the box's left face at
/// t = 0.5. One spanning x 1.5..2.5 with its bottom at y = 3, falling by
/// 4, meets the top corner after 2.125 (t = 0.53125, normal +y), not the
/// box's top at t = 0.375.
#[wasm_bindgen_test(unsupported = test)]
fn a_box_meets_the_corners_a_cut_leaves_standing() {
    let lowered = Block {
        cut: Some((Vec2::new(-3.0, 4.0), -0.5)),
        ..ramp_2d()
    };
    let world = [lowered.collider()];
    let body = shape(Vec2::splat(0.5));
    let walk = cast(
        &body,
        Vec2::new(-4.5, -1.9),
        Vec2::new(4.0, 0.0),
        &world[..],
        ALL_LAYERS,
    );
    assert_hit(walk.unwrap(), 17.0 / 24.0, Vec2::NEG_X);
    let fall = cast(
        &body,
        Vec2::new(2.0, 3.5),
        Vec2::new(0.0, -4.0),
        &world[..],
        ALL_LAYERS,
    );
    assert_hit(fall.unwrap(), 0.531_25, Vec2::Y);
}

/// Defect: a 3D cut sweep that is right only for one orientation (a slab
/// on the wrong axis, a sign taken from y alone). Oracle: geometry, for one
/// ramp of each kind, every body a unit cube.
/// - Upright: the 2D ramp extruded on z (half 1): a body walking along +x
///   at height -1..0 meets the plane at t = 13/24, as in 2D.
/// - Inverted: normal (-3, -4, 0)/5, solid `y >= -0.75 x`, a ceiling
///   descending to the right. A body at (1, -3) rising by 4 spans x
///   0.5..1.5, where the ceiling is lowest at the right edge, y = -1.125;
///   its top starts at -2.5 and rises 1.375: t = 0.34375.
/// - Sideways: the box (half 2, 1, 1.5) cut by normal (3, 0, 4)/5, solid
///   `z <= -0.75 x`, a vertical face across a corner. A body at (-1, 0, 4)
///   moving by -4 on z spans x -1.5..-0.5, where the face is highest at
///   the left edge, z = 1.125; its near face starts at 3.5 and travels
///   2.375: t = 0.59375.
#[wasm_bindgen_test(unsupported = test)]
fn a_box_meets_each_kind_of_3d_ramp_where_geometry_puts_it() {
    let body = shape(Vec3::splat(0.5));
    let block = |half: Vec3, normal: Vec3| {
        [Block {
            id: 1,
            center: Vec3::ZERO,
            half,
            cut: Some((normal, 0.0)),
        }
        .collider()]
    };
    let upright = block(Vec3::new(2.0, 1.5, 1.0), Vec3::new(-3.0, 4.0, 0.0));
    let hit = cast(
        &body,
        Vec3::new(-4.0, -0.5, 0.0),
        Vec3::new(4.0, 0.0, 0.0),
        &upright[..],
        ALL_LAYERS,
    );
    assert_hit(hit.unwrap(), 13.0 / 24.0, Vec3::new(-0.6, 0.8, 0.0));

    let inverted = block(Vec3::new(2.0, 1.5, 1.0), Vec3::new(-3.0, -4.0, 0.0));
    let hit = cast(
        &body,
        Vec3::new(1.0, -3.0, 0.0),
        Vec3::new(0.0, 4.0, 0.0),
        &inverted[..],
        ALL_LAYERS,
    );
    assert_hit(hit.unwrap(), 0.343_75, Vec3::new(-0.6, -0.8, 0.0));

    let sideways = block(Vec3::new(2.0, 1.0, 1.5), Vec3::new(3.0, 0.0, 4.0));
    let hit = cast(
        &body,
        Vec3::new(-1.0, 0.0, 4.0),
        Vec3::new(0.0, 0.0, -4.0),
        &sideways[..],
        ALL_LAYERS,
    );
    assert_hit(hit.unwrap(), 0.593_75, Vec3::new(0.6, 0.0, 0.8));
}

/// Defect: a ray that meets the whole box, so a shot or a line of sight
/// over a ramp stops on air. Oracle: geometry. The 2D ramp's cut-away part
/// is above `y = 0.75 x`. A ray from (-3, 1) along +x by 6 crosses the box
/// at y = 1, where the ramp is solid only from x = 4/3: it meets the plane
/// after 13/3 of 6 (t = 13/18), not the box's left face at t = 1/6. A ray
/// from (-3, 1.4) by (2.5, 0) ends at x = -0.5, inside the box but in the
/// cut-away part, and meets nothing. A ray along `y = 0.75 x + 0.5`, from
/// (-3, -1.75) by (8, 6), crosses the box above the plane, through the
/// corner the cut removed, and meets the wall at x = 3 after 6 of 8.
#[wasm_bindgen_test(unsupported = test)]
fn a_ray_passes_through_the_cut_away_part() {
    let wall = Block {
        id: 2,
        center: Vec2::new(3.5, 0.0),
        half: Vec2::new(0.5, 5.0),
        cut: None,
    };
    let world = [ramp_2d().collider(), wall.collider()];
    let hit = ray(
        Vec2::new(-3.0, 1.0),
        Vec2::new(6.0, 0.0),
        &world[..],
        ALL_LAYERS,
    )
    .unwrap();
    assert_eq!(hit.map(|h| h.id), Some(1));
    assert_hit(hit, 13.0 / 18.0, Vec2::new(-0.6, 0.8));
    let short = ray(
        Vec2::new(-3.0, 1.4),
        Vec2::new(2.5, 0.0),
        &world[..],
        ALL_LAYERS,
    );
    assert_eq!(short, Ok(None));
    let over = ray(
        Vec2::new(-3.0, -1.75),
        Vec2::new(8.0, 6.0),
        &world[..],
        ALL_LAYERS,
    )
    .unwrap();
    assert_eq!(over.map(|h| h.id), Some(2));
    assert_hit(over, 0.75, Vec2::NEG_X);
}

/// Defect: `overlaps` that tests the whole box, so a trigger reports a body
/// standing in the cut-away part of a ramp. Oracle: the ramp's definition:
/// a quarter-unit body at (-1, 1) lies wholly above `y = 0.75 x` (its
/// lowest right corner, (-0.75, 0.75), is above the line's -0.5625), and
/// one at (1, -0.5) lies below it (its highest left corner, (0.75,
/// -0.25), is under 0.5625).
#[wasm_bindgen_test(unsupported = test)]
fn overlaps_meets_a_cut_box_as_the_shape_it_is() {
    let world = [ramp_2d().collider()];
    let body = shape(Vec2::splat(0.25));
    let above = overlaps(&body, Vec2::new(-1.0, 1.0), &world[..], ALL_LAYERS).unwrap();
    assert!(above.is_empty(), "{above:?}");
    let below = overlaps(&body, Vec2::new(1.0, -0.5), &world[..], ALL_LAYERS).unwrap();
    assert_eq!(below.iter().map(|c| c.id).collect::<Vec<_>>(), [1]);
}

/// Defect: an embedded body read against the box's faces only, so a body
/// a little below a ramp's plane (a spawn, a moved ramp) sinks further into
/// it. Oracle: the ramp's definition, sampled. A quarter-unit body at
/// (1, 0.5) has its bottom-right corner (1.25, 0.25) 0.55 below the plane
/// along its normal (0.6 * 1.25 - 0.8 * 0.25), less than its depth past any
/// face of the box (1.25), so the plane is its axis of least penetration:
/// motion into the plane leaves it no deeper, motion out of it is free.
#[wasm_bindgen_test(unsupported = test)]
fn a_body_inside_a_ramp_may_leave_it_but_not_go_deeper() {
    let ramp = ramp_2d();
    let world = [ramp.collider()];
    let config = mover(Vec2::Y, 0.01, 0.1, 0.7, 1e-4);
    let half = Vec2::splat(0.25);
    let start = Vec2::new(1.0, 0.5);
    let before = penetration(start, half, &ramp);
    assert!(before > INSIDE, "the body starts inside: {before}");
    let (moved, _) = slide(
        &shape(half),
        start,
        Vec2::new(0.3, -0.4),
        &world[..],
        ALL_LAYERS,
        &config,
    )
    .unwrap();
    let after = penetration(start + moved, half, &ramp);
    assert!(after <= before + 1e-6, "went deeper: {before} to {after}");
    let out = Vec2::new(-0.3, 0.4);
    let (moved, _) = slide(&shape(half), start, out, &world[..], ALL_LAYERS, &config).unwrap();
    assert_eq!(moved, out);
}

// ------------------------------------------------------------ the cut

/// Defect: a cut the sweep cannot handle exactly getting in (a 3D normal
/// with no zero component needs edge-pair axes the sweep does not test), a
/// normal that is no direction, or a one-way platform with a cut, whose
/// one-way rule reads a whole box. Oracle: the requirements: each is
/// refused where it is built.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn a_cut_the_sweep_cannot_meet_exactly_is_refused() {
    use std::panic::catch_unwind;
    assert!(catch_unwind(|| Cut::new(Vec3::new(1.0, 1.0, 1.0), 0.0)).is_err());
    assert!(catch_unwind(|| Cut::new(Vec3::new(1.0, 2.0, 1e-3), 0.0)).is_err());
    assert!(catch_unwind(|| Cut::new(Vec2::ZERO, 0.0)).is_err());
    assert!(catch_unwind(|| Cut::new(Vec2::new(f32::NAN, 1.0), 0.0)).is_err());
    assert!(catch_unwind(|| Cut::new(Vec2::new(1e30, 1e30), 0.0)).is_err());
    assert!(catch_unwind(|| Cut::new(Vec2::Y, f32::INFINITY)).is_err());
    // Any 2D direction, and a 3D one turning about one axis, are taken.
    let _ = Cut::new(Vec2::new(0.3, -7.0), 0.25);
    let _ = Cut::new(Vec3::new(0.0, 1.0, -2.0), -0.5);
    let ramp = Collider::new(1, shape(Vec2::splat(0.5)), Vec2::ZERO);
    let cut = Cut::new(Vec2::new(-1.0, 1.0), 0.0);
    assert!(catch_unwind(|| ramp.with_cut(cut).with_kind(ColliderKind::OneWay)).is_err());
    assert!(catch_unwind(|| ramp.with_kind(ColliderKind::OneWay).with_cut(cut)).is_err());
}

/// Defect: a one-way platform with a cut, written as a struct literal past
/// the builders, blocking as some shape neither rule describes. Oracle:
/// the contract: it occupies nothing, so a ray down through it meets the
/// floor beneath.
#[wasm_bindgen_test(unsupported = test)]
fn a_one_way_platform_with_a_cut_occupies_nothing() {
    let platform = Collider {
        kind: ColliderKind::OneWay,
        cut: Some(Cut::new(Vec2::new(-1.0, 1.0), 0.0)),
        ..Collider::new(1, shape(Vec2::splat(0.5)), Vec2::new(0.0, 2.0))
    };
    let floor = Collider::new(2, shape(Vec2::new(5.0, 0.5)), Vec2::ZERO);
    let world = [platform, floor];
    let hit = ray(
        Vec2::new(0.25, 4.0),
        Vec2::new(0.0, -8.0),
        &world[..],
        ALL_LAYERS,
    );
    assert_eq!(hit.unwrap().map(|h| h.id), Some(2));
}

/// Defect: a cut that comes back from the wire with a different normal
/// (normalised twice, so rounded twice), so a client's ramp is a rounding
/// step from its server's. Oracle: the round trip, by the bits of the unit
/// normal and the canonical digest, over normals whose normalisation
/// rounds.
#[wasm_bindgen_test(unsupported = test)]
fn a_cut_round_trips_to_the_same_bits() {
    for (x, y) in [
        (0.3f32, 0.7f32),
        (-1.0, 3.0),
        (0.123, -0.987),
        (7.0, 0.1),
        (1.0, 1.0),
    ] {
        let cut = Cut::new(Vec3::new(x, y, 0.0), 0.375);
        let bytes = postcard::to_allocvec(&cut).unwrap();
        let restored: Cut<Vec3> = postcard::from_bytes(&bytes).unwrap();
        let bits = |v: Vec3| [v.x.to_bits(), v.y.to_bits(), v.z.to_bits()];
        assert_eq!(bits(restored.normal()), bits(cut.normal()), "({x}, {y})");
        assert_eq!(sgl_core::digest_of(&restored), sgl_core::digest_of(&cut));
    }
}

/// A cut on the wire is validated as `new` validates it: a 3D normal with
/// no zero component is refused, not built.
#[wasm_bindgen_test(unsupported = test)]
fn a_cut_from_the_wire_is_validated() {
    let mut bytes = postcard::to_allocvec(&Cut::new(Vec3::new(1.0, 1.0, 0.0), 0.0)).unwrap();
    // The normal's z, after its x and y.
    bytes[8..12].copy_from_slice(&1.0f32.to_le_bytes());
    assert!(postcard::from_bytes::<Cut<Vec3>>(&bytes).is_err());
}

// ------------------------------------------------------------ walking

fn colliders<V: Vector>(blocks: &[Block<V>]) -> Vec<Collider<V>> {
    blocks.iter().map(Block::collider).collect()
}

/// The deepest any block of `blocks` reaches into the body at `center`.
fn deepest<V: Vector>(center: V, half: V, blocks: &[Block<V>]) -> f64 {
    blocks
        .iter()
        .map(|b| penetration(center, half, b))
        .fold(f64::NEG_INFINITY, f64::max)
}

/// A side-on profile along x, in `V` (extruded on z in 3D): a floor whose
/// top is y = 0, a ramp up from x = 4 to x = 8 rising 2 (normal (-1, 2):
/// `normal · up` is 0.89, a floor at the 0.7 threshold), a plateau to
/// x = 12 and a ramp down to x = 16. Ids: 1 floor, 2 up, 3 plateau, 4 down.
fn hill<V: Vector>() -> Vec<Block<V>> {
    let at = |x: f32, y: f32, hx: f32, hy: f32| {
        let (mut center, mut half) = (V::ZERO, V::ZERO);
        center[0] = x;
        center[1] = y;
        half[0] = hx;
        half[1] = hy;
        if V::AXES == 3 {
            half[2] = 50.0;
        }
        (center, half)
    };
    let normal = |x: f32, y: f32| {
        let mut n = V::ZERO;
        n[0] = x;
        n[1] = y;
        n
    };
    let block = |id, (center, half), cut| Block {
        id,
        center,
        half,
        cut,
    };
    vec![
        block(1, at(10.0, -0.5, 30.0, 0.5), None),
        block(2, at(6.0, 1.0, 2.0, 1.0), Some((normal(-1.0, 2.0), 0.0))),
        block(3, at(10.0, 1.0, 2.0, 1.0), None),
        block(4, at(14.0, 1.0, 2.0, 1.0), Some((normal(1.0, 2.0), 0.0))),
    ]
}

/// Defect: a walkable slope that slows a body (the motion left after a
/// ramp contact projected onto the face, or the skin back-off taking its
/// share of the run), or a body that leaves the ground going down. Oracle:
/// the requirement, a body walking at a constant `step` across `up` covers
/// exactly `step` across `up` every step, on flat ground, up the ramp,
/// across the plateau and down the other side, transitions included; it
/// is on a floor after every step; and it is never inside the hill (the
/// blocks' own definition, sampled). In 3D the walk is diagonal, so the
/// direction across `up` must be kept too. The game's step is the usual
/// one: gravity's share of a step down, then a ground snap.
fn walk_the_hill<V: Vector>(step: V) {
    let blocks = hill::<V>();
    let world = colliders(&blocks);
    let config = mover(
        {
            let mut up = V::ZERO;
            up[1] = 1.0;
            up
        },
        0.01,
        0.1,
        0.7,
        1e-4,
    );
    let mut half = V::ZERO;
    for axis in 0..V::AXES {
        half[axis] = if axis == 1 { 0.5 } else { 0.25 };
    }
    let body = shape(half);
    let mut position = V::ZERO;
    position[1] = 0.51;
    let mut gravity = V::ZERO;
    gravity[1] = -0.02;
    let mut met = [false; 4];
    while position[0] < 19.0 {
        let (moved, contacts) = slide(
            &body,
            position,
            step + gravity,
            &world[..],
            ALL_LAYERS,
            &config,
        )
        .unwrap();
        position = position + moved;
        let (snap, snapped) =
            snap_to_ground(&body, position, &world[..], ALL_LAYERS, &config).unwrap();
        position = position + snap;
        for axis in (0..V::AXES).filter(|&axis| axis != 1) {
            let across = moved[axis] + snap[axis];
            assert!(
                (across - step[axis]).abs() < 1e-5,
                "at {position:?}: moved {across} on axis {axis}, asked {}",
                step[axis]
            );
        }
        assert!(
            contacts.floor || snapped.floor,
            "left the ground at {position:?}"
        );
        for id in [contacts.last_floor_id, snapped.last_floor_id]
            .into_iter()
            .flatten()
        {
            met[usize::try_from(id).unwrap() - 1] = true;
        }
        let depth = deepest(position, half, &blocks);
        assert!(depth < INSIDE, "inside the hill by {depth} at {position:?}");
    }
    assert_eq!(met, [true; 4], "the walk stood on every block");
}

#[wasm_bindgen_test(unsupported = test)]
fn a_2d_walk_over_ramps_keeps_flat_ground_speed() {
    walk_the_hill(Vec2::new(0.1, 0.0));
}

#[wasm_bindgen_test(unsupported = test)]
fn a_3d_walk_over_ramps_keeps_flat_ground_speed() {
    walk_the_hill(Vec3::new(0.1, 0.0, 0.07));
}

/// A game's fixed step, as the parity walk has it: gravity accumulates in
/// the air, a landing or a head-bump stops the vertical speed, and a body
/// that was grounded snaps down. Returns the new position, vertical speed
/// and contacts.
fn game_step<V: Vector>(
    body: &Shape<V>,
    position: V,
    across: V,
    vertical: f32,
    grounded: bool,
    world: &[Collider<V>],
    config: &MoverConfig<V>,
) -> (V, f32, Contacts<V>) {
    let mut vertical = if grounded {
        -0.05
    } else {
        (vertical - 0.05).max(-0.8)
    };
    let mut delta = across;
    delta[1] = vertical;
    let (moved, mut contacts) = slide(body, position, delta, world, ALL_LAYERS, config).unwrap();
    let mut position = position + moved;
    if (contacts.floor && contacts.down_blocked) || (contacts.ceiling && contacts.up_blocked) {
        vertical = 0.0;
    }
    if grounded && vertical <= 0.0 {
        // The ground probe also says whether the body is still on a floor,
        // as a game's controller reads it: a move that met a wall first
        // and spent itself there never reached the floor it stands on.
        let (snap, probe) = snap_to_ground(body, position, world, ALL_LAYERS, config).unwrap();
        position = position + snap;
        if probe.floor {
            contacts.floor = true;
            contacts.last_floor_id = probe.last_floor_id;
        }
    }
    (position, vertical, contacts)
}

/// Defect: a slope steeper than the threshold treated as a floor (turned
/// along like a walkable one, so a body walks up a cliff), or met as its
/// whole box. Oracle: the requirement, against the same walk as on the
/// hill: a ramp rising 2 over 1 (`normal · up` is 0.45, under 0.7) is never
/// a floor; the body never gets onto the plateau behind it, and is held at
/// its foot, as a wall holds it; it is never inside either (the blocks'
/// definition, sampled).
#[wasm_bindgen_test(unsupported = test)]
fn a_ramp_steeper_than_the_threshold_stops_a_body_as_a_wall_does() {
    let blocks = [
        Block {
            id: 1,
            center: Vec2::new(0.0, -0.5),
            half: Vec2::new(20.0, 0.5),
            cut: None,
        },
        // From (4, 0) up to (5, 2).
        Block {
            id: 2,
            center: Vec2::new(4.5, 1.0),
            half: Vec2::new(0.5, 1.0),
            cut: Some((Vec2::new(-2.0, 1.0), 0.0)),
        },
        Block {
            id: 3,
            center: Vec2::new(7.0, 1.0),
            half: Vec2::new(2.0, 1.0),
            cut: None,
        },
    ];
    let world = colliders(&blocks);
    let config = mover(Vec2::Y, 0.01, 0.1, 0.7, 1e-4);
    let half = Vec2::new(0.25, 0.5);
    let body = shape(half);
    let (mut position, mut vertical, mut grounded) = (Vec2::new(1.0, 0.51), 0.0, true);
    let mut trail = Vec::new();
    for _ in 0..200 {
        let contacts;
        (position, vertical, contacts) = game_step(
            &body,
            position,
            Vec2::new(0.1, 0.0),
            vertical,
            grounded,
            &world,
            &config,
        );
        grounded = contacts.floor;
        assert_ne!(
            contacts.last_floor_id,
            Some(2),
            "stood on the cliff at {position}"
        );
        assert!(position.y - half.y < 0.5, "climbed the cliff to {position}");
        let depth = deepest(position, half, &blocks);
        assert!(depth < INSIDE, "inside by {depth} at {position}");
        trail.push(position);
    }
    // Held at the foot (x = 4) from the second hundred steps on, and held
    // still: a slope too steep to stand on gives a run no height, so the
    // body neither creeps up it nor bobs. It rests a skin above the floor
    // and within a step's run of the foot, at one place.
    for at in &trail[100..] {
        let (front, bottom) = (at.x + half.x, at.y - half.y);
        assert!((3.9..=4.0).contains(&front), "front at {front}");
        assert!(bottom <= 0.01 + 1e-4, "lifted to {bottom}");
        assert_eq!(*at, trail[100], "the body moved while held");
    }
}

/// Defect: a run pushed at a slope too steep to stand on is projected onto
/// it and turned into height, so a body creeps up a cliff a little every
/// step and bobs as gravity takes it back. Oracle: the requirement (a face
/// that is not a floor gives a push no height). The slope rises 2 over 1
/// from (4, 0); a body beside it is pushed level. It may close the gap to
/// the slope, and it must not rise at all. In 3D the same push with a part
/// along the slope keeps that part and still does not rise.
#[wasm_bindgen_test(unsupported = test)]
fn a_level_push_at_a_slope_too_steep_to_stand_on_gains_no_height() {
    let config = mover(Vec2::Y, 0.01, 0.1, 0.7, 1e-4);
    let cliff = [Block {
        id: 1,
        center: Vec2::new(4.5, 1.0),
        half: Vec2::new(0.5, 1.0),
        cut: Some((Vec2::new(-2.0, 1.0), 0.0)),
    }
    .collider()];
    let body = shape(Vec2::new(0.25, 0.5));
    let start = Vec2::new(3.6, 0.51);
    let (moved, contacts) = slide(
        &body,
        start,
        Vec2::new(0.5, 0.0),
        &cliff[..],
        ALL_LAYERS,
        &config,
    )
    .unwrap();
    assert!(contacts.wall && !contacts.floor);
    assert!(moved.y.abs() <= 0.0, "the push gained height: {moved}");
    assert!(moved.x > 0.0 && moved.x < 0.5, "moved {moved}");

    let config = mover(Vec3::Y, 0.01, 0.1, 0.7, 1e-4);
    let cliff = [Block {
        id: 1,
        center: Vec3::new(4.5, 1.0, 0.0),
        half: Vec3::new(0.5, 1.0, 8.0),
        cut: Some((Vec3::new(-2.0, 1.0, 0.0), 0.0)),
    }
    .collider()];
    let body = shape(Vec3::new(0.25, 0.5, 0.25));
    let (moved, contacts) = slide(
        &body,
        Vec3::new(3.6, 0.51, 0.0),
        Vec3::new(0.5, 0.0, 0.5),
        &cliff[..],
        ALL_LAYERS,
        &config,
    )
    .unwrap();
    assert!(contacts.wall && !contacts.floor);
    assert!(moved.y.abs() <= 0.0, "the push gained height: {moved}");
    assert!(moved.x > 0.0 && moved.x < 0.5, "moved {moved}");
    // The part along the slope stays level and is kept, less only the
    // share the skin back-off discards at any wall.
    assert!(moved.z > 0.45 && moved.z <= 0.5, "moved {moved}");
}

/// Defect: a slide out of one face that moves the body into another it met
/// in the same move, so a body pushed into the crease of a ramp and a wall
/// ends inside one of them. Oracle: the blocks' definition, sampled, after
/// every step of a walk into the crease: in 2D up a ramp into the wall at
/// its top, in 3D diagonally up a ramp along a wall beside it (the body
/// keeps climbing, its run along the wall intact). The body touches both.
#[wasm_bindgen_test(unsupported = test)]
fn a_body_pushed_into_the_crease_of_a_ramp_and_a_wall_penetrates_neither() {
    // 2D: the hill's up ramp, from (4, 0) to (8, 2), and a wall at x = 7.
    let mut blocks = hill::<Vec2>()[..2].to_vec();
    blocks.push(Block {
        id: 5,
        center: Vec2::new(7.5, 3.0),
        half: Vec2::new(0.5, 3.0),
        cut: None,
    });
    let world = colliders(&blocks);
    let config = mover(Vec2::Y, 0.01, 0.1, 0.7, 1e-4);
    let half = Vec2::new(0.25, 0.5);
    let body = shape(half);
    let (mut position, mut vertical, mut grounded) = (Vec2::new(2.0, 0.51), 0.0, true);
    let (mut floor, mut wall) = (false, false);
    for _ in 0..100 {
        let contacts;
        (position, vertical, contacts) = game_step(
            &body,
            position,
            Vec2::new(0.1, 0.0),
            vertical,
            grounded,
            &world,
            &config,
        );
        grounded = contacts.floor;
        floor |= contacts.last_floor_id == Some(2);
        wall |= contacts.last_wall_id == Some(5);
        let depth = deepest(position, half, &blocks);
        assert!(depth < INSIDE, "2D: inside by {depth} at {position}");
    }
    assert!(
        floor && wall,
        "2D: met the ramp {floor} and the wall {wall}"
    );
    assert!(
        position.x + half.x > 6.9,
        "2D: stopped short of the wall at {position}"
    );

    // 3D: the hill's up ramp, extruded on z, and a wall along it whose
    // face is z = 1. The body walks up and into the wall.
    let mut blocks = hill::<Vec3>()[..2].to_vec();
    blocks.push(Block {
        id: 5,
        center: Vec3::new(6.0, 3.0, 1.5),
        half: Vec3::new(10.0, 3.0, 0.5),
        cut: None,
    });
    let world = colliders(&blocks);
    let config = mover(Vec3::Y, 0.01, 0.1, 0.7, 1e-4);
    let half = Vec3::new(0.25, 0.5, 0.25);
    let body = shape(half);
    let (mut position, mut vertical, mut grounded) = (Vec3::new(2.0, 0.51, 0.0), 0.0, true);
    let (mut floor, mut wall) = (false, false);
    let step = Vec3::new(0.1, 0.0, 0.1);
    while position.x < 7.5 {
        let contacts;
        (position, vertical, contacts) =
            game_step(&body, position, step, vertical, grounded, &world, &config);
        grounded = contacts.floor;
        floor |= contacts.last_floor_id == Some(2);
        wall |= contacts.last_wall_id == Some(5);
        let depth = deepest(position, half, &blocks);
        assert!(depth < INSIDE, "3D: inside by {depth} at {position}");
    }
    assert!(
        floor && wall,
        "3D: met the ramp {floor} and the wall {wall}"
    );
    // On the ramp, along the wall: its top is x / 2 - 2 under the body's
    // front edge.
    let front = position.x + half.x;
    assert!(
        position.y - half.y > (front - 4.0) * 0.5 - 0.05,
        "at {position}"
    );
}

/// Defect: a slide that moves a body back into a face it met earlier in
/// the same move, so a body wedged where a floor meets a ceiling sloping
/// down to it loses its run along the crease, every step, to a contact it
/// already had. Oracle: the crease rule. A floor (top y = 0) and an
/// inverted ramp above it descending from y = 2 at x = 0 to the floor at
/// x = 4 (normal (-1, -2, 0)) meet in a crease along z. A body pushed by
/// (0.2, 0, 0.2) each step wedges into it, and then the slide out of each
/// face turns into the other: the motion left follows their crease along z.
/// Once wedged, each step's run along z is all but the share of it the
/// first contact's back-off takes, at most `skin` along the ramp's normal
/// over the step's 0.2 * 1/√5 into it, so over 0.75 of 0.2. Losing that
/// run to the second contact leaves almost none.
#[wasm_bindgen_test(unsupported = test)]
fn a_body_wedged_in_a_crease_follows_it() {
    let blocks = [
        Block {
            id: 1,
            center: Vec3::new(0.0, -0.5, 0.0),
            half: Vec3::new(10.0, 0.5, 50.0),
            cut: None,
        },
        Block {
            id: 2,
            center: Vec3::new(2.0, 1.0, 0.0),
            half: Vec3::new(2.0, 1.0, 50.0),
            cut: Some((Vec3::new(-1.0, -2.0, 0.0), 0.0)),
        },
    ];
    let world = colliders(&blocks);
    let config = mover(Vec3::Y, 0.01, 0.1, 0.7, 1e-4);
    let half = Vec3::splat(0.25);
    let body = shape(half);
    let mut position = Vec3::new(1.0, 0.26, 0.0);
    let step = Vec3::new(0.2, 0.0, 0.2);
    for n in 0..40 {
        let (moved, contacts) =
            slide(&body, position, step, &world[..], ALL_LAYERS, &config).unwrap();
        position += moved;
        let depth = deepest(position, half, &blocks);
        assert!(depth < INSIDE, "inside by {depth} at {position}");
        if n >= 20 {
            assert!(contacts.ceiling, "step {n}: not wedged at {position}");
            assert!(
                moved.z > 0.75 * 0.2,
                "step {n}: ran {} along the crease",
                moved.z
            );
        }
    }
}

// ------------------------------------------------------------ properties

#[cfg(not(target_arch = "wasm32"))]
mod properties {
    use super::*;
    use proptest::prelude::*;
    use proptest::test_runner::{Config, RngAlgorithm, TestCaseError, TestRng, TestRunner};
    use sgl_core::digest_of;
    use sgl_core::math::IVec3;
    use sgl_physics::{CellColliders, CellWorld, World};

    const SEED: [u8; 32] = *b"sgl-physics ramp property seed 1";

    fn check<S: Strategy>(strategy: S, test: impl Fn(S::Value) -> Result<(), TestCaseError>) {
        let config = Config {
            failure_persistence: None,
            ..Config::default()
        };
        let mut runner =
            TestRunner::new_with_rng(config, TestRng::from_seed(RngAlgorithm::ChaCha, &SEED));
        if let Err(failure) = runner.run(&strategy, test) {
            panic!("{failure}");
        }
    }

    fn finite(range: std::ops::Range<f32>) -> impl Strategy<Value = f32> {
        range.prop_filter("finite", |v| v.is_finite())
    }

    fn vec2(range: std::ops::Range<f32>) -> impl Strategy<Value = Vec2> {
        (finite(range.clone()), finite(range)).prop_map(|(x, y)| Vec2::new(x, y))
    }

    fn vec3(range: std::ops::Range<f32>) -> impl Strategy<Value = Vec3> {
        (finite(range.clone()), finite(range.clone()), finite(range))
            .prop_map(|(x, y, z)| Vec3::new(x, y, z))
    }

    /// A small level of boxes on a half-unit grid, most of them cut by a
    /// plane whose normal has small whole components (in 3D, one of them
    /// zero) and whose offset is a quarter-unit step within a unit of the
    /// centre: some cut nothing, some leave nothing.
    fn blocks<V: Vector>() -> impl Strategy<Value = Vec<Block<V>>> {
        let block = (
            [-8i8..8, -8i8..8, -8i8..8],
            [1u8..4, 1u8..4, 1u8..4],
            [-3i8..=3, -3i8..=3, -3i8..=3],
            0usize..3,
            -4i8..=4,
            0u8..4,
        );
        prop::collection::vec(block, 0..7).prop_map(|list| {
            (1u64..)
                .zip(list)
                .map(|(id, (at, size, mut normal, flat, offset, uncut))| {
                    let (mut center, mut half, mut n) = (V::ZERO, V::ZERO, V::ZERO);
                    if V::AXES == 3 {
                        normal[flat] = 0;
                    }
                    if normal[..V::AXES].iter().all(|&c| c == 0) {
                        normal[1] = 1;
                    }
                    for axis in 0..V::AXES {
                        center[axis] = f32::from(at[axis]) * 0.5;
                        half[axis] = f32::from(size[axis]) * 0.5;
                        n[axis] = f32::from(normal[axis]);
                    }
                    let cut = (uncut != 0).then_some((n, f32::from(offset) * 0.25));
                    Block {
                        id,
                        center,
                        half,
                        cut,
                    }
                })
                .collect()
        })
    }

    /// Defect: a cut-box sweep that lets a body through the plane, past a
    /// corner the cut leaves standing (the solid part's edges read as the
    /// whole box's), or a slide pass or snap that pushes it in. Oracle: the
    /// blocks' own definition, sampled through the body and over the
    /// blocks (`penetration`), with no sweep maths: a body that starts
    /// clear of every block ends clear of every one after a slide and a
    /// ground snap, in 2D and in 3D.
    fn never_inside<V: Vector>(
        body: Block<V>,
        blocks: &[Block<V>],
        delta: V,
        up: V,
    ) -> Result<(), TestCaseError> {
        let world = colliders(blocks);
        let clear = deepest(body.center, body.half, blocks) < -INSIDE
            && overlaps(&shape(body.half), body.center, &world[..], ALL_LAYERS)
                .unwrap()
                .is_empty();
        prop_assume!(clear);
        let config = mover(up, 0.01, 0.1, 0.7, 1e-4);
        let shape = shape(body.half);
        let (moved, _) =
            slide(&shape, body.center, delta, &world[..], ALL_LAYERS, &config).unwrap();
        let at = body.center + moved;
        let depth = deepest(at, body.half, blocks);
        prop_assert!(depth < INSIDE, "slid inside by {depth} to {at:?}");
        let (snap, _) = snap_to_ground(&shape, at, &world[..], ALL_LAYERS, &config).unwrap();
        let depth = deepest(at + snap, body.half, blocks);
        prop_assert!(
            depth < INSIDE,
            "snapped inside by {depth} to {:?}",
            at + snap
        );
        Ok(())
    }

    #[test]
    fn a_2d_move_never_ends_inside_a_cut_box() {
        let body = (vec2(-5.0..5.0), vec2(0.2..1.0));
        check(
            (blocks::<Vec2>(), body, vec2(-5.0..5.0), any::<bool>()),
            |(blocks, (center, half), delta, down)| {
                let body = Block {
                    id: 0,
                    center,
                    half,
                    cut: None,
                };
                never_inside(
                    body,
                    &blocks,
                    delta,
                    if down { Vec2::NEG_Y } else { Vec2::Y },
                )
            },
        );
    }

    #[test]
    fn a_3d_move_never_ends_inside_a_cut_box() {
        let body = (vec3(-5.0..5.0), vec3(0.2..1.0));
        check(
            (blocks::<Vec3>(), body, vec3(-5.0..5.0)),
            |(blocks, (center, half), delta)| {
                let body = Block {
                    id: 0,
                    center,
                    half,
                    cut: None,
                };
                never_inside(body, &blocks, delta, Vec3::Y)
            },
        );
    }

    fn mirrored<V: Vector>(v: V, axis: usize) -> V {
        let mut out = v;
        out[axis] = -v[axis];
        out
    }

    fn bits<V: Vector>(v: V) -> Vec<u32> {
        (0..V::AXES).map(|axis| v[axis].to_bits()).collect()
    }

    /// Everything a cast, a slide and a snap answer, as bits.
    fn answers<V: Vector>(
        blocks: &[Block<V>],
        center: V,
        half: V,
        delta: V,
        up: V,
    ) -> Vec<Vec<u32>> {
        let world = colliders(blocks);
        let body = shape(half);
        let config = mover(up, 0.01, 0.1, 0.7, 1e-4);
        let hit = cast(&body, center, delta, &world[..], ALL_LAYERS).unwrap();
        let (moved, contacts) =
            slide(&body, center, delta, &world[..], ALL_LAYERS, &config).unwrap();
        let (snap, snapped) =
            snap_to_ground(&body, center, &world[..], ALL_LAYERS, &config).unwrap();
        let flags = |c: &Contacts<V>| {
            vec![
                u32::from(c.floor),
                u32::from(c.ceiling),
                u32::from(c.wall),
                u32::from(c.down_blocked),
                u32::from(c.up_blocked),
            ]
        };
        let ids = |c: &Contacts<V>| {
            [c.last_floor_id, c.last_wall_id, c.last_ceiling_id]
                .map(|id| id.map_or(u32::MAX, |id| u32::try_from(id).unwrap()))
                .to_vec()
        };
        vec![
            hit.map_or(vec![], |h| {
                [
                    vec![h.t.to_bits(), u32::try_from(h.id).unwrap()],
                    bits(h.normal),
                ]
                .concat()
            }),
            bits(moved),
            flags(&contacts),
            ids(&contacts),
            bits(contacts.last_floor_normal),
            bits(snap),
            flags(&snapped),
            ids(&snapped),
        ]
    }

    /// `answers` of the scene mirrored across `axis`: its vectors mirrored
    /// back, so a mirror-exact crate gives the same bits.
    fn mirrored_answers<V: Vector>(
        blocks: &[Block<V>],
        center: V,
        half: V,
        delta: V,
        up: V,
        axis: usize,
    ) -> Vec<Vec<u32>> {
        let flipped: Vec<_> = blocks
            .iter()
            .map(|b| Block {
                center: mirrored(b.center, axis),
                cut: b.cut.map(|(n, o)| (mirrored(n, axis), o)),
                ..*b
            })
            .collect();
        let mut out = answers(
            &flipped,
            mirrored(center, axis),
            half,
            mirrored(delta, axis),
            mirrored(up, axis),
        );
        // Mirror back the vectors: the hit's normal, the moves, the floor
        // normal. A component's sign is its bit 31.
        let back = |v: &mut Vec<u32>, from: usize| {
            if v.len() > from + axis {
                v[from + axis] ^= 1 << 31;
            }
        };
        back(&mut out[0], 2);
        back(&mut out[1], 0);
        back(&mut out[4], 0);
        back(&mut out[5], 0);
        out
    }

    /// Defect: a cut sweep or a slide rule that treats one direction of an
    /// axis differently from the other (a sign read from one side, a tie
    /// broken by sign, a turn along a floor that rounds differently for
    /// y-down), so a level and its mirror image play differently. Oracle:
    /// the mirror: every input mirrored across an axis (`up` with it, so
    /// across y a y-up world becomes y-down) mirrors every answer, bit for
    /// bit, for casts, slides and snaps, on each axis in 2D and 3D. A
    /// zero's sign is not compared where mirroring flips it.
    fn mirrors<V: Vector>(
        blocks: &[Block<V>],
        center: V,
        half: V,
        delta: V,
    ) -> Result<(), TestCaseError> {
        let mut up = V::ZERO;
        up[1] = 1.0;
        let plain = answers(blocks, center, half, delta, up);
        for axis in 0..V::AXES {
            let mirror = mirrored_answers(blocks, center, half, delta, up, axis);
            let zeroless = |a: &Vec<Vec<u32>>| -> Vec<Vec<u32>> {
                a.iter()
                    .map(|v| {
                        v.iter()
                            .map(|&b| if b == 1 << 31 { 0 } else { b })
                            .collect()
                    })
                    .collect()
            };
            prop_assert_eq!(
                zeroless(&plain),
                zeroless(&mirror),
                "mirrored across axis {}",
                axis
            );
        }
        Ok(())
    }

    #[test]
    fn a_2d_level_of_ramps_mirrors_across_each_axis() {
        check(
            (
                blocks::<Vec2>(),
                vec2(-5.0..5.0),
                vec2(0.2..1.0),
                vec2(-5.0..5.0),
            ),
            |(blocks, center, half, delta)| mirrors(&blocks, center, half, delta),
        );
    }

    #[test]
    fn a_3d_level_of_ramps_mirrors_across_each_axis() {
        check(
            (
                blocks::<Vec3>(),
                vec3(-5.0..5.0),
                vec3(0.2..1.0),
                vec3(-5.0..5.0),
            ),
            |(blocks, center, half, delta)| mirrors(&blocks, center, half, delta),
        );
    }

    /// Defect: a cell world that scales a cut box's box to world units but
    /// not its plane's offset, so a slope block plays differently in a
    /// voxel grid. Oracle: the same blocks as a slice in world units: a
    /// walk through a grid of two-unit cells, each holding one cut block
    /// in cell-local units (exact: the cell size is a power of two), must
    /// move bit-equal to the slice.
    #[test]
    fn a_cell_world_of_ramps_moves_bit_equal_to_the_slice() {
        let cell = (
            (-3i32..3, -3i32..3, -3i32..3),
            [-3i8..=3, -3i8..=3],
            0usize..3,
            -2i8..=2,
        );
        let cells = prop::collection::vec(cell, 0..20);
        let steps = prop::collection::vec(vec3(-1.5..1.5), 1..6);
        check(
            (cells, vec3(-4.0..4.0), vec3(0.2..0.9), steps),
            |(cells, start, half, steps)| {
                let mut level = std::collections::HashMap::new();
                for ((x, y, z), [a, b], flat, offset) in cells {
                    let mut normal = [f32::from(a), f32::from(b), 0.0];
                    if a == 0 && b == 0 {
                        normal[1] = 1.0;
                    }
                    normal.rotate_left(flat);
                    level.insert(
                        IVec3::new(x, y, z),
                        (Vec3::from_array(normal), f32::from(offset) * 0.125),
                    );
                }
                let id = |c: IVec3| {
                    u64::from((c.x + 8).unsigned_abs())
                        | u64::from((c.y + 8).unsigned_abs()) << 8
                        | u64::from((c.z + 8).unsigned_abs()) << 16
                };
                let local = |c: IVec3, (normal, offset): (Vec3, f32)| {
                    Collider::new(id(c), shape(Vec3::splat(0.5)), Vec3::splat(0.5))
                        .with_cut(Cut::new(normal, offset))
                };
                let slice: Vec<_> = level
                    .iter()
                    .map(|(&c, &cut)| {
                        Collider::new(id(c), shape(Vec3::splat(1.0)), (c.as_vec3() + 0.5) * 2.0)
                            .with_cut(Cut::new(cut.0, cut.1 * 2.0))
                    })
                    .collect();
                let grid = CellWorld::new(2.0, |c: IVec3, out: &mut CellColliders<'_, Vec3>| {
                    if let Some(&cut) = level.get(&c) {
                        out.push(local(c, cut));
                    }
                });
                let config = mover(Vec3::Y, 0.01, 0.1, 0.7, 1e-4);
                let walk = |world: &dyn World<Vec3>| {
                    let mut at = start;
                    let mut trace = Vec::new();
                    for &step in &steps {
                        let (moved, contacts) =
                            slide(&shape(half), at, step, world, ALL_LAYERS, &config).unwrap();
                        at += moved;
                        let hit = ray(at, step * 4.0, world, ALL_LAYERS).unwrap();
                        trace.push((bits(at), digest_of(&contacts), hit.map(|h| digest_of(&h))));
                    }
                    trace
                };
                prop_assert_eq!(walk(&slice.as_slice()), walk(&grid));
                Ok(())
            },
        );
    }
}
