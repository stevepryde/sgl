//! Property tests for the `sgl-physics` contracts (physics.md). Every
//! property names the defect it exists to catch and takes its expectation
//! from the spec, plain geometry, a brute-force model, or a second run that
//! must agree, never from the code under test. Runs from a fixed seed
//! (testing.md 3); `PROPTEST_CASES` widens a local run. Native only:
//! proptest's `getrandom` does not build for `wasm32-unknown-unknown`.
#![cfg(not(target_arch = "wasm32"))]
#![allow(clippy::cast_precision_loss)]

use std::collections::HashMap;

use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, TestCaseError, TestRng, TestRunner};
use sgl_core::digest_of;
use sgl_core::math::{IVec3, Vec2, Vec3};
use sgl_physics::{
    ALL_LAYERS, CellColliders, CellWorld, Collider, ColliderKind, ColliderSet, Contacts,
    MoverConfig, MoverSettings, Shape, Vector, World, cast, move_and_slide, snap_to_ground,
};

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

const SEED: [u8; 32] = *b"sgl-physics property tests seed1";

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

/// A box by centre and half-extents, with the plain geometry the oracles
/// read: no crate code.
#[derive(Debug, Clone, Copy)]
struct Aabb<V> {
    center: V,
    half: V,
}

impl Aabb<Vec2> {
    fn min(&self) -> Vec2 {
        self.center - self.half
    }

    fn max(&self) -> Vec2 {
        self.center + self.half
    }

    fn translated(&self, by: Vec2) -> Self {
        Self {
            center: self.center + by,
            half: self.half,
        }
    }

    fn overlaps(&self, other: &Self) -> bool {
        let (a, b) = ((self.min(), self.max()), (other.min(), other.max()));
        a.0.x < b.1.x && a.1.x > b.0.x && a.0.y < b.1.y && a.1.y > b.0.y
    }
}

fn shape<V>(half: V) -> Shape<V> {
    Shape::Box { half }
}

fn aabb2() -> impl Strategy<Value = Aabb<Vec2>> {
    (vec2(-20.0..20.0), vec2(0.1..3.0)).prop_map(|(center, half)| Aabb { center, half })
}

/// The first contact of a box body with one solid box, through the public
/// cast over a one-collider world.
fn sweep(body: &Aabb<Vec2>, delta: Vec2, target: &Aabb<Vec2>) -> Option<sgl_physics::Hit<Vec2>> {
    let world = [Collider::new(0, shape(target.half), target.center)];
    cast(
        &shape(body.half),
        body.center,
        delta,
        &world[..],
        ALL_LAYERS,
    )
    .expect("a slice answers")
}

/// Separation of two boxes per axis (negative = penetration on that axis).
fn separation(a: &Aabb<Vec2>, b: &Aabb<Vec2>) -> Vec2 {
    (a.center - b.center).abs() - (a.half + b.half)
}

// ------------------------------------------------------------ pair sweeps

/// Defect: a slab sign error or wrong entering axis, so a body stops short,
/// tunnels, or is pushed sideways off a floor. Oracle: geometry: a hit is
/// a touching contact on the reported face, reached inside the motion, and
/// a miss has no penetration anywhere along the sampled path.
#[test]
fn swept_boxes_touch_the_reported_face_and_misses_never_penetrate() {
    check(
        (aabb2(), aabb2(), vec2(-30.0..30.0)),
        |(body, target, delta)| {
            prop_assume!(delta != Vec2::ZERO);
            match sweep(&body, delta, &target) {
                Some(hit) => {
                    prop_assert!((0.0..=1.0).contains(&hit.t), "t = {}", hit.t);
                    prop_assert!(
                        [Vec2::X, Vec2::NEG_X, Vec2::Y, Vec2::NEG_Y].contains(&hit.normal),
                        "normal {}",
                        hit.normal
                    );
                    prop_assert!(hit.normal.dot(delta) < 0.0, "normal must oppose the motion");
                    let sep = separation(&body.translated(delta * hit.t), &target);
                    let along = sep.dot(hit.normal.abs());
                    prop_assert!(along.abs() < 1e-3, "contact face separation {along}");
                }
                None => {
                    if !body.overlaps(&target) {
                        for i in 0..=64 {
                            let at = body.translated(delta * (i as f32 / 64.0));
                            let sep = separation(&at, &target);
                            prop_assert!(
                                sep.x > -1e-3 || sep.y > -1e-3,
                                "missed sweep penetrates at step {i}: {sep}"
                            );
                        }
                    }
                }
            }
            Ok(())
        },
    );
}

/// Defect: a y-up/y-down asymmetry in the sweep. Oracle: mirroring every
/// input across the x axis mirrors the answer, bit for bit.
#[test]
fn swept_boxes_mirror_across_the_x_axis() {
    check(
        (aabb2(), aabb2(), vec2(-30.0..30.0)),
        |(body, target, delta)| {
            let flip = |v: Vec2| Vec2::new(v.x, -v.y);
            let mirror = |a: &Aabb<Vec2>| Aabb {
                center: flip(a.center),
                half: a.half,
            };
            let hit = sweep(&body, delta, &target);
            let mirrored = sweep(&mirror(&body), flip(delta), &mirror(&target));
            match (hit, mirrored) {
                (None, None) => {}
                (Some(h), Some(m)) => {
                    prop_assert_eq!(h.t.to_bits(), m.t.to_bits());
                    prop_assert_eq!(flip(h.normal), m.normal);
                }
                other => prop_assert!(false, "asymmetric result {other:?}"),
            }
            Ok(())
        },
    );
}

/// Defect (#427): the sweep reads edges differently from the overlap test,
/// so a body exactly touching a face (not overlapping) misses it and moves
/// straight through. Oracle: the edges themselves, in the body's frame (the
/// body at the origin, where the contract computes): on every face, a body
/// whose edge equals the face's moves toward it with contact at `t = 0`,
/// and one a rounding gap away still makes contact, on that face, before it
/// penetrates.
#[test]
fn a_touched_face_is_hit_at_once_on_every_side() {
    let case = (
        vec2(0.1..3.0),
        vec2(0.1..3.0),
        finite(-0.9..0.9),
        finite(0.1..5.0),
        finite(-5.0..5.0),
    );
    check(case, |(half, target_half, along, toward, lateral)| {
        // The target sits under the body, its top face on the body's bottom
        // edge up to rounding, then everything turns exactly (swaps and
        // negations) onto each face in turn.
        let body = Aabb {
            center: Vec2::ZERO,
            half,
        };
        let target = Aabb {
            center: Vec2::new(-along * target_half.x, -(half.y + target_half.y)),
            half: target_half,
        };
        let delta = Vec2::new(lateral, -toward);
        let quarter = |v: Vec2| Vec2::new(-v.y, v.x);
        let turn = |a: &Aabb<Vec2>| Aabb {
            center: quarter(a.center),
            half: Vec2::new(a.half.y, a.half.x),
        };
        let (mut body, mut target, mut delta, mut face) = (body, target, delta, Vec2::Y);
        for _ in 0..4 {
            if !body.overlaps(&target) {
                let gap = (body.min() - target.max()).max(target.min() - body.max());
                let gap = gap.dot(face.abs());
                let hit = sweep(&body, delta, &target);
                prop_assert!(hit.is_some(), "missed face {face} at gap {gap}");
                let hit = hit.unwrap();
                prop_assert_eq!(hit.normal, face);
                if gap == 0.0 {
                    prop_assert_eq!(hit.t.to_bits(), 0.0f32.to_bits());
                } else {
                    prop_assert!(hit.t * delta.dot(-face) <= gap * 1.0001, "t = {}", hit.t);
                }
            }
            body = turn(&body);
            target = turn(&target);
            delta = quarter(delta);
            face = quarter(face);
        }
        Ok(())
    });
}

// ------------------------------------------------------------- 2D mover

fn solids() -> impl Strategy<Value = Vec<Aabb<Vec2>>> {
    prop::collection::vec(
        (-8i32..8, -8i32..8, 1u8..3, 1u8..3).prop_map(|(x, y, hw, hh)| Aabb {
            center: Vec2::new(x as f32, y as f32),
            half: Vec2::new(f32::from(hw) * 0.5, f32::from(hh) * 0.5),
        }),
        0..8,
    )
}

fn config2(up: Vec2) -> MoverConfig<Vec2> {
    mover(up, 0.01, 0.1, 0.7, 1e-4)
}

/// Whether `moved` ends inside `b` by more than one representable step past
/// one of `b`'s edges, rather than a world-unit tolerance that would hide a
/// skin applied with the wrong sign.
fn inside2(moved: &Aabb<Vec2>, b: &Aabb<Vec2>) -> bool {
    let (min, max) = (moved.min(), moved.max());
    let (b_min, b_max) = (b.min(), b.max());
    !(min.x >= b_max.x.next_down()
        || max.x <= b_min.x.next_up()
        || min.y >= b_max.y.next_down()
        || max.y <= b_min.y.next_up())
}

/// Defect: a slide pass that pushes the body into the next solid, or a skin
/// applied with the wrong sign. Oracle: the body never overlaps a solid
/// after the move (a direct overlap test against every box); mirroring the
/// world across the x axis (and `up`) mirrors the move and keeps the
/// floor/ceiling/wall classification.
#[test]
fn a_2d_move_never_ends_inside_a_solid_and_mirrors_with_up() {
    let body = (vec2(-10.0..10.0), vec2(0.2..1.0)).prop_map(|(center, half)| Aabb { center, half });
    check((solids(), body, vec2(-6.0..6.0)), |(boxes, body, delta)| {
        prop_assume!(boxes.iter().all(|b| !b.overlaps(&body)));
        let build = |flip: bool| {
            let mut set = ColliderSet::new(2.0);
            for (id, b) in (0u64..).zip(&boxes) {
                let y = if flip { -b.center.y } else { b.center.y };
                set.insert(Collider::new(id, shape(b.half), Vec2::new(b.center.x, y)));
            }
            set
        };
        let (effective, contacts) = move_and_slide(
            &shape(body.half),
            body.center,
            delta,
            &build(false),
            ALL_LAYERS,
            &config2(Vec2::Y),
        )
        .unwrap();
        let moved = body.translated(effective);
        for b in &boxes {
            prop_assert!(!inside2(&moved, b), "ended inside {b:?}: {moved:?}");
        }
        prop_assert!(
            effective.length() <= delta.length() + 1e-4,
            "moved further than asked"
        );

        let (mirrored, mirrored_contacts) = move_and_slide(
            &shape(body.half),
            Vec2::new(body.center.x, -body.center.y),
            Vec2::new(delta.x, -delta.y),
            &build(true),
            ALL_LAYERS,
            &config2(Vec2::NEG_Y),
        )
        .unwrap();
        prop_assert!(
            (mirrored.x - effective.x).abs() < 1e-4 && (mirrored.y + effective.y).abs() < 1e-4,
            "{effective} vs {mirrored}"
        );
        prop_assert_eq!(
            (contacts.floor, contacts.ceiling, contacts.wall),
            (
                mirrored_contacts.floor,
                mirrored_contacts.ceiling,
                mirrored_contacts.wall
            )
        );
        Ok(())
    });
}

/// Defect: a broadphase cell rounding that drops a candidate, which the
/// sweep then tunnels through. Oracle: an O(n) overlap scan.
#[test]
fn a_set_offers_every_collider_overlapping_a_region() {
    check(
        (finite(0.5..4.0), solids(), aabb2()),
        |(cell, boxes, region)| {
            let mut set = ColliderSet::new(cell);
            for (id, b) in (0u64..).zip(&boxes) {
                set.insert(Collider::new(id, shape(b.half), b.center));
            }
            let mut offered = Vec::new();
            set.colliders_near(region.min(), region.max(), &mut |c| offered.push(c.id))
                .unwrap();
            for (id, b) in (0u64..).zip(&boxes) {
                if b.overlaps(&region) {
                    prop_assert!(
                        offered.contains(&id),
                        "collider {id} {b:?} missing for {region:?}"
                    );
                }
            }
            Ok(())
        },
    );
}

// ------------------------------------------------------------- 3D contains 2D

/// Defect: a 3D path that differs from the 2D one where it should not (an
/// extra rounding, a tie broken on z, a dot product that picks up the sign
/// of a zero from an unused axis), so a 2D game and the same game in 3D
/// disagree. Oracle: the 2D move itself. A 3D move with no motion on z,
/// among colliders that span the body on z, gives bit-equal x and y, the
/// same contacts and the same ids, for moves and ground snaps, in y-up and
/// y-down worlds.
#[test]
fn a_3d_move_with_no_z_motion_matches_the_2d_move() {
    let boxes = prop::collection::vec((-8i32..8, -8i32..8, 1u8..3, 1u8..3, any::<bool>()), 0..10);
    // Exactly zero motion along an axis is common (standing still while
    // falling) and is where a stray zero term could change a sign.
    let component = prop_oneof![Just(0.0f32), Just(-0.0f32), finite(-6.0..6.0)];
    let delta = (component.clone(), component).prop_map(|(x, y)| Vec2::new(x, y));
    let case = (
        boxes,
        vec2(-10.0..10.0),
        vec2(0.2..1.0),
        delta,
        any::<bool>(),
    );
    check(case, |(boxes, center, half, delta, y_down)| {
        let up = if y_down { -1.0 } else { 1.0 };
        let mut flat = ColliderSet::new(2.0);
        let mut deep = ColliderSet::new(2.0);
        for (id, &(x, y, hw, hh, one_way)) in (0u64..).zip(&boxes) {
            let flags = if one_way {
                ColliderKind::OneWay
            } else {
                ColliderKind::Solid
            };
            let (c, h) = (
                Vec2::new(x as f32, y as f32),
                Vec2::new(f32::from(hw) * 0.5, f32::from(hh) * 0.5),
            );
            flat.insert(Collider::new(id, shape(h), c).with_kind(flags));
            let spanning = Vec3::new(h.x, h.y, 16.0);
            deep.insert(Collider::new(id, shape(spanning), c.extend(0.0)).with_kind(flags));
        }
        let config2 = config2(Vec2::new(0.0, up));
        let config3 = mover(Vec3::new(0.0, up, 0.0), 0.01, 0.1, 0.7, 1e-4);
        let body2 = shape(half);
        let body3 = shape(half.extend(0.5));
        let (moved2, contacts2) =
            move_and_slide(&body2, center, delta, &flat, ALL_LAYERS, &config2).unwrap();
        let (moved3, contacts3) = move_and_slide(
            &body3,
            center.extend(0.0),
            delta.extend(0.0),
            &deep,
            ALL_LAYERS,
            &config3,
        )
        .unwrap();
        prop_assert_eq!(bits2(moved2), bits3(moved3));
        prop_assert_eq!(flatten(&contacts2), flatten3(&contacts3));

        let (snap2, snapped2) =
            snap_to_ground(&body2, center + moved2, &flat, ALL_LAYERS, &config2).unwrap();
        let (snap3, snapped3) = snap_to_ground(
            &body3,
            (center + moved2).extend(0.0),
            &deep,
            ALL_LAYERS,
            &config3,
        )
        .unwrap();
        prop_assert_eq!(bits2(snap2), bits3(snap3));
        prop_assert_eq!(flatten(&snapped2), flatten3(&snapped3));
        Ok(())
    });
}

fn bits2(v: Vec2) -> [u32; 2] {
    [v.x.to_bits(), v.y.to_bits()]
}

fn bits3(v: Vec3) -> [u32; 2] {
    [v.x.to_bits(), v.y.to_bits()]
}

type Flat = ([bool; 5], [[u32; 2]; 4], [Option<u64>; 3]);

fn flatten(c: &Contacts<Vec2>) -> Flat {
    (
        [c.floor, c.ceiling, c.wall, c.down_blocked, c.up_blocked],
        [
            bits2(c.last_floor_normal),
            bits2(c.last_ceiling_normal),
            bits2(c.desired),
            bits2(c.effective),
        ],
        [c.last_floor_id, c.last_wall_id, c.last_ceiling_id],
    )
}

fn flatten3(c: &Contacts<Vec3>) -> Flat {
    (
        [c.floor, c.ceiling, c.wall, c.down_blocked, c.up_blocked],
        [
            bits3(c.last_floor_normal),
            bits3(c.last_ceiling_normal),
            bits3(c.desired),
            bits3(c.effective),
        ],
        [c.last_floor_id, c.last_wall_id, c.last_ceiling_id],
    )
}

// ------------------------------------------------------------- 3D worlds

/// What one occupied cell holds, in cell-local units: a full block, a
/// bottom slab or a thin pillar, each inside its cell.
const KINDS: [(Vec3, Vec3); 3] = [
    (Vec3::splat(0.5), Vec3::splat(0.5)),
    (Vec3::new(0.5, 0.25, 0.5), Vec3::new(0.5, 0.25, 0.5)),
    (Vec3::new(0.5, 0.5, 0.5), Vec3::new(0.25, 0.5, 0.25)),
];

/// A voxel level: occupied cells and what each holds, a cell to a kind.
type Level = HashMap<IVec3, usize>;

fn level() -> impl Strategy<Value = Level> {
    prop::collection::vec(((-4i32..4, -4i32..4, -4i32..4), 0usize..3), 0..40).prop_map(|cells| {
        cells
            .into_iter()
            .map(|((x, y, z), kind)| (IVec3::new(x, y, z), kind))
            .collect()
    })
}

/// The game's id for a cell: its coordinates packed, so the same box has
/// the same id from every source.
fn cell_id(cell: IVec3) -> u64 {
    let pack = |v: i32| u64::from((v + 128).unsigned_abs());
    pack(cell.x) | pack(cell.y) << 16 | pack(cell.z) << 32
}

/// The level's boxes in world units, one-unit cells, ascending by cell.
fn boxes(level: &Level) -> Vec<Collider<Vec3>> {
    let mut cells: Vec<_> = level.iter().collect();
    cells.sort_by_key(|(c, _)| (c.x, c.y, c.z));
    cells
        .into_iter()
        .map(|(&cell, &kind)| {
            let (local, half) = KINDS[kind];
            Collider::new(cell_id(cell), shape(half), cell.as_vec3() + local)
        })
        .collect()
}

fn cells(level: &Level) -> CellWorld<Vec3, impl Fn(IVec3, &mut CellColliders<'_, Vec3>) + '_> {
    CellWorld::new(
        1.0,
        move |cell: IVec3, out: &mut CellColliders<'_, Vec3>| {
            if let Some(&kind) = level.get(&cell) {
                let (local, half) = KINDS[kind];
                out.push(Collider::new(cell_id(cell), shape(half), local));
            }
        },
    )
}

fn config3() -> MoverConfig<Vec3> {
    mover(Vec3::Y, 0.01, 0.1, 0.7, 1e-4)
}

fn body3() -> impl Strategy<Value = Aabb<Vec3>> {
    (vec3(-6.0..6.0), vec3(0.2..0.9)).prop_map(|(center, half)| Aabb { center, half })
}

/// Strict interior overlap of two 3D boxes, by their edges.
fn overlap3(a: &Aabb<Vec3>, b: &Aabb<Vec3>) -> bool {
    let (a_min, a_max) = (a.center - a.half, a.center + a.half);
    let (b_min, b_max) = (b.center - b.half, b.center + b.half);
    (0..3).all(|i| a_min[i] < b_max[i] && a_max[i] > b_min[i])
}

/// [`inside2`] in 3D: inside by more than one representable step on every
/// axis.
fn inside3(moved: &Aabb<Vec3>, b: &Aabb<Vec3>) -> bool {
    let (min, max) = (moved.center - moved.half, moved.center + moved.half);
    let (b_min, b_max) = (b.center - b.half, b.center + b.half);
    (0..3).all(|i| min[i] < b_max[i].next_down() && max[i] > b_min[i].next_up())
}

fn as_aabb(c: &Collider<Vec3>) -> Aabb<Vec3> {
    Aabb {
        center: c.position,
        half: c.shape.half_extents(),
    }
}

fn move3<W: World<Vec3> + ?Sized>(
    body: &Aabb<Vec3>,
    delta: Vec3,
    world: &W,
) -> (Vec3, Contacts<Vec3>) {
    move_and_slide(
        &shape(body.half),
        body.center,
        delta,
        world,
        ALL_LAYERS,
        &config3(),
    )
    .expect("every world here answers")
}

/// Defect: a 3D slide pass that pushes the body into a solid, or a world
/// that leaves a solid out of a query, so the body ends inside it. Oracle:
/// a direct overlap test against every box of the level, with no
/// broadphase and no sweep maths, after the same move through a slice, a
/// set, a cell world and a pair of a set and a slice.
#[test]
fn a_3d_move_never_ends_inside_a_solid_through_any_world() {
    check(
        (level(), body3(), vec3(-4.0..4.0), finite(0.5..4.0)),
        |(level, body, delta, cell)| {
            let all = boxes(&level);
            prop_assume!(all.iter().all(|c| !overlap3(&as_aabb(c), &body)));
            let mut set = ColliderSet::new(cell);
            let mut half_set = ColliderSet::new(cell);
            let mut rest = Vec::new();
            for (i, c) in all.iter().enumerate() {
                set.insert(*c);
                if i % 2 == 0 {
                    half_set.insert(*c);
                } else {
                    rest.push(*c);
                }
            }
            let grid = cells(&level);
            let pair = (&half_set, rest.as_slice());
            let results = [
                ("slice", move3(&body, delta, all.as_slice())),
                ("set", move3(&body, delta, &set)),
                ("cells", move3(&body, delta, &grid)),
                ("pair", move3(&body, delta, &pair)),
            ];
            for (world, (effective, _)) in results {
                let moved = Aabb {
                    center: body.center + effective,
                    half: body.half,
                };
                for c in &all {
                    prop_assert!(
                        !inside3(&moved, &as_aabb(c)),
                        "{world}: ended inside {c:?}: {moved:?}"
                    );
                }
            }
            Ok(())
        },
    );
}

/// A collider far larger than a cell, in world units: each axis has a face
/// on a whole coordinate `face` near the origin and runs `2 * half` away
/// from it, so the box lies within one 1024-unit cell (0 or -1 on that
/// axis) and its centre is exact.
#[derive(Debug, Clone, Copy)]
struct Big {
    face: [i8; 3],
    half: [f32; 3],
}

impl Big {
    fn collider(&self, index: usize) -> Collider<Vec3> {
        let mut center = Vec3::ZERO;
        for axis in 0..3 {
            let face = f32::from(self.face[axis]);
            let away = if self.face[axis] >= 0 { 1.0 } else { -1.0 };
            center[axis] = face + away * self.half[axis];
        }
        Collider::new(
            1 << 48 | index as u64,
            shape(Vec3::from_array(self.half)),
            center,
        )
    }
}

fn bigs() -> impl Strategy<Value = Vec<Big>> {
    let half = prop_oneof![Just(0.5f32), Just(100.0), Just(300.0), Just(450.0)];
    let big = (
        [-6i8..7, -6i8..7, -6i8..7],
        [half.clone(), half.clone(), half],
    )
        .prop_map(|(face, half)| Big { face, half });
    prop::collection::vec(big, 0..4)
}

/// The large colliders as a cell world of 1024-unit cells, each in
/// cell-local units (exact: the cell size is a power of two).
fn big_cells(
    bigs: &[Collider<Vec3>],
) -> CellWorld<Vec3, impl Fn(IVec3, &mut CellColliders<'_, Vec3>) + '_> {
    CellWorld::new(
        1024.0,
        move |cell: IVec3, out: &mut CellColliders<'_, Vec3>| {
            for c in bigs {
                let home = (c.position / 1024.0).floor().as_ivec3();
                if home == cell {
                    out.push(Collider {
                        position: c.position / 1024.0 - cell.as_vec3(),
                        shape: shape(c.shape.half_extents() / 1024.0),
                        ..*c
                    });
                }
            }
        },
    )
}

/// One step of a walk: a free motion, or one that ends with the body's
/// edge exactly on a large collider's face along one axis.
#[derive(Debug, Clone, Copy)]
enum Step {
    Free(Vec3),
    ToFace(usize, usize, Vec3),
}

fn steps() -> impl Strategy<Value = Vec<Step>> {
    let step = prop_oneof![
        vec3(-1.5..1.5).prop_map(Step::Free),
        (0usize..4, 0usize..3, vec3(-1.5..1.5)).prop_map(|(i, a, v)| Step::ToFace(i, a, v)),
    ];
    prop::collection::vec(step, 1..6)
}

/// Defect: a result that depends on where the colliders came from or the
/// order they were put in (a tie broken by visit order, a candidate one
/// source offers and the other drops, a set that buckets a large collider
/// by world coordinates a rounding step from where the body meets it).
/// Oracle: every source of the same level must agree bit for bit: the boxes
/// as a slice, as sets built in ascending and in descending order, and as a
/// cell world (voxels at one unit, large colliders at 1024), give the same
/// moves and contacts over a short walk, including motions that end exactly
/// on a large collider's face.
#[test]
fn one_level_from_every_source_or_order_moves_bit_equal() {
    check(
        (level(), bigs(), body3(), steps()),
        |(level, bigs, body, steps)| {
            let large: Vec<_> = bigs
                .iter()
                .enumerate()
                .map(|(i, b)| b.collider(i))
                .collect();
            let mut all = boxes(&level);
            all.extend(&large);
            let mut forward = ColliderSet::new(1.0);
            let mut backward = ColliderSet::new(1.0);
            for c in &all {
                forward.insert(*c);
            }
            for c in all.iter().rev() {
                backward.insert(*c);
            }
            let grid = (cells(&level), big_cells(&large));
            let walk = |world: &dyn World<Vec3>| {
                let mut at = body.center;
                let mut trace = Vec::new();
                for &step in &steps {
                    let delta = match step {
                        Step::Free(delta) => delta,
                        Step::ToFace(_, _, delta) if large.is_empty() => delta,
                        Step::ToFace(i, axis, mut delta) => {
                            let target = &large[i % large.len()];
                            let (c, h) = (target.position[axis], target.shape.half_extents()[axis]);
                            // The body's leading edge ends on the near face,
                            // reckoned from the body as a game aiming at the
                            // face would (and as the crate does).
                            let rel = c - at[axis];
                            delta[axis] = if rel > 0.0 {
                                (rel - h) - body.half[axis]
                            } else {
                                (rel + h) + body.half[axis]
                            };
                            delta
                        }
                    };
                    let here = Aabb {
                        center: at,
                        half: body.half,
                    };
                    let (moved, contacts) = move3(&here, delta, world);
                    at += moved;
                    trace.push((bits3full(at), digest_of(&contacts)));
                }
                trace
            };
            let reference = walk(&all.as_slice());
            prop_assert_eq!(
                &reference,
                &walk(&forward),
                "the set disagreed with the slice"
            );
            prop_assert_eq!(&reference, &walk(&backward), "insertion order showed");
            prop_assert_eq!(
                &reference,
                &walk(&grid),
                "the cell world disagreed with the slice"
            );
            Ok(())
        },
    );
}

fn bits3full(v: Vec3) -> [u32; 3] {
    [v.x.to_bits(), v.y.to_bits(), v.z.to_bits()]
}

/// Defect: a cell world that walks the cells of the region in world
/// coordinates only, so a collider filling a large cell, whose face on the
/// cell's boundary a body reaches exactly (reckoned from the body), is left
/// out when the region ends a rounding step short of that boundary. Oracle:
/// the same collider as a slice, which offers it always: the cast must
/// agree bit for bit.
#[test]
fn a_cell_world_offers_a_large_cell_reached_exactly_on_its_boundary() {
    let half = prop_oneof![Just(32.0f32), Just(64.0), Just(100.0)];
    check(
        (finite(-2.0..-0.5), finite(0.1..0.5), half, finite(0.5..1.5)),
        |(x, body_half, half, y)| {
            // The collider lies in cell 0 of a 256-unit grid, from x = 0 and
            // y = 0; the body is near the origin, so the query's own pad,
            // which scales with the body's coordinates, is small.
            let wall = Collider::new(1, shape(Vec2::new(half, 100.0)), Vec2::new(half, 100.0));
            let cells = CellWorld::new(
                256.0,
                |cell: sgl_core::math::IVec2, out: &mut CellColliders<'_, Vec2>| {
                    if cell == sgl_core::math::IVec2::ZERO {
                        out.push(Collider {
                            position: wall.position / 256.0,
                            shape: shape(Vec2::new(half, 100.0) / 256.0),
                            ..wall
                        });
                    }
                },
            );
            let at = Vec2::new(x, y);
            let reach = ((half - at.x) - half) - body_half;
            let body = shape(Vec2::splat(body_half));
            let motion = Vec2::new(reach, 0.0);
            let slice = [wall];
            let expected = cast(&body, at, motion, &slice[..], ALL_LAYERS).unwrap();
            let found = cast(&body, at, motion, &cells, ALL_LAYERS).unwrap();
            prop_assert_eq!(
                found.map(|h| h.t.to_bits()),
                expected.map(|h| h.t.to_bits())
            );
            Ok(())
        },
    );
}

/// Defect: a cell world that walks only part of the cells along a long
/// motion (a step that skips a cell, a range that drops one at a step's
/// joint), so a long cast passes a block. Oracle: the same blocks as a
/// slice, which offers every one: a long diagonal cast of a box body must
/// hit the same collider at the same bits, and is never unanswered here
/// (the walk along it stays under the cap, though its enclosing box does
/// not).
#[test]
fn a_long_cast_through_a_cell_world_matches_the_slice() {
    let scattered =
        prop::collection::vec(((-30i32..30, -30i32..30, -30i32..30), 0usize..3), 0..200).prop_map(
            |cells| {
                cells
                    .into_iter()
                    .map(|((x, y, z), kind)| (IVec3::new(x, y, z), kind))
                    .collect::<Level>()
            },
        );
    check(
        (
            scattered,
            vec3(-10.0..10.0),
            vec3(0.0..0.9),
            vec3(-60.0..60.0),
        ),
        |(level, center, half, motion)| {
            let all = boxes(&level);
            let grid = cells(&level);
            let body = shape(half);
            let expected = cast(&body, center, motion, all.as_slice(), ALL_LAYERS).unwrap();
            let found = cast(&body, center, motion, &grid, ALL_LAYERS);
            prop_assert!(found.is_ok(), "unanswered along {motion}");
            let bits = |h: sgl_physics::Hit<Vec3>| (h.t.to_bits(), bits3full(h.normal), h.id);
            prop_assert_eq!(found.unwrap().map(bits), expected.map(bits));
            Ok(())
        },
    );
}

/// Defect (#427): a body whose bottom edge equals a floor's top edge, bit
/// for bit in world coordinates, lies a rounding step inside the floor once
/// taken relative to the body, and a cast reading that as "started inside"
/// passes the floor. Oracle: the edges themselves, compared in world
/// coordinates: they touch, so a cast into the floor, away from the origin
/// where rounding is coarse, meets its face at once: at `t = 0`, or, where
/// the rounding opened a gap instead, after a travel no longer than the
/// rounding of the floor's own coordinates. A ray starting exactly on the
/// face and moving in is met the same way.
#[test]
fn a_body_or_ray_exactly_on_a_floor_is_met_at_once() {
    let case = (
        finite(-2000.0..2000.0),
        finite(100.0..10_000.0),
        vec2(0.05..3.0),
        vec2(0.1..2.0),
        finite(-0.9..0.9),
        (finite(-5.0..5.0), finite(-10.0..-0.01)),
    );
    check(case, |(cx, cy, floor_half, half, along, (dx, dy))| {
        let floor_half = Vec2::new(floor_half.x + 2.0, floor_half.y);
        let floor = [Collider::new(1, shape(floor_half), Vec2::new(cx, cy))];
        let top = cy + floor_half.y;
        let x = cx + along * (floor_half.x - half.x);
        let y = top + half.y;
        prop_assume!((y - half.y).to_bits() == top.to_bits());
        let motion = Vec2::new(dx, dy);
        let rounding = 4.0 * f32::EPSILON * (cy.abs() + floor_half.y);
        let starts = [(half, Vec2::new(x, y)), (Vec2::ZERO, Vec2::new(x, top))];
        for (half, at) in starts {
            let hit = cast(&shape(half), at, motion, &floor[..], ALL_LAYERS).unwrap();
            prop_assert!(hit.is_some(), "half {half}: passed the floor it touches");
            let hit = hit.unwrap();
            prop_assert_eq!(hit.normal, Vec2::Y);
            prop_assert!(hit.t * -dy <= rounding, "travelled {} first", hit.t * -dy);
        }
        Ok(())
    });
}

/// Defect: a projectile stepped to its hit (`p += v * t`) lands a rounding
/// step inside the wall, and the next cast, reading it as inside, passes
/// the wall. Oracle: the wall's near face in world coordinates: however
/// many times the projectile is stepped by its hit (or by its whole motion
/// when nothing is hit), it never ends past that face by more than the
/// rounding of the wall's own coordinates.
#[test]
fn a_projectile_stepped_to_its_hit_never_passes_the_wall() {
    let case = (
        finite(50.0..5000.0),
        finite(0.5..20.0),
        finite(1.0..200.0),
        finite(-100.0..100.0),
        (finite(1.0..400.0), finite(-50.0..50.0)),
    );
    check(case, |(wx, hx, back, y, (vx, vy))| {
        let wall = [Collider::new(
            1,
            shape(Vec2::new(hx, 1e4)),
            Vec2::new(wx, 0.0),
        )];
        let near = wx - hx;
        let limit = near + 4.0 * f32::EPSILON * (wx + hx);
        let (mut p, v) = (Vec2::new(near - back, y), Vec2::new(vx, vy));
        for _ in 0..4 {
            let hit = cast(&shape(Vec2::ZERO), p, v, &wall[..], ALL_LAYERS).unwrap();
            p += v * hit.map_or(1.0, |h| h.t);
            prop_assert!(p.x <= limit, "passed the face at {near}: {p}");
        }
        Ok(())
    });
}

/// Defect: a query whose result depends on how far the scene is from the
/// origin (positions subtracted late, a tolerance in absolute units).
/// Oracle: the same scene translated by 2^17 on every axis. Every
/// coordinate is on a half-unit grid, so the translation is exact and the
/// differences between positions are the same bits in both; moves, snaps
/// and casts must then give bit-equal translations, contacts and hits.
#[test]
fn a_scene_far_from_the_origin_moves_bit_equal_to_one_near_it() {
    let grid = |range: std::ops::Range<i32>| range.prop_map(|v| v as f32 * 0.5);
    let point =
        (grid(-32..32), grid(-32..32), grid(-32..32)).prop_map(|(x, y, z)| Vec3::new(x, y, z));
    let size = (1i32..5, 1i32..5, 1i32..5)
        .prop_map(|(x, y, z)| Vec3::new(x as f32, y as f32, z as f32) * 0.5);
    let scene = prop::collection::vec((point.clone(), size.clone()), 0..12);
    check(
        (scene, point, size, vec3(-6.0..6.0)),
        |(scene, start, half, delta)| {
            let far = Vec3::new(131_072.0, -131_072.0, 131_072.0);
            let colliders = |shift: Vec3| -> Vec<Collider<Vec3>> {
                (0u64..)
                    .zip(&scene)
                    .map(|(id, &(at, h))| Collider::new(id, shape(h), at + shift))
                    .collect()
            };
            let run = |shift: Vec3| {
                let list = colliders(shift);
                let mut set = ColliderSet::new(2.0);
                for c in &list {
                    set.insert(*c);
                }
                let body = shape(half);
                let at = start + shift;
                let mut out = Vec::new();
                for world in [&list.as_slice() as &dyn World<Vec3>, &set] {
                    let (moved, contacts) =
                        move_and_slide(&body, at, delta, world, ALL_LAYERS, &config3()).unwrap();
                    let (snap, snapped) =
                        snap_to_ground(&body, at, world, ALL_LAYERS, &config3()).unwrap();
                    let hit = cast(&body, at, delta, world, ALL_LAYERS).unwrap();
                    out.push((
                        bits3full(moved),
                        digest_of(&contacts),
                        bits3full(snap),
                        digest_of(&snapped),
                        hit.map(|h| digest_of(&h)),
                    ));
                }
                out
            };
            prop_assert_eq!(run(Vec3::ZERO), run(far));
            Ok(())
        },
    );
}
