//! Audit of moving colliders (#924, physics.md 21), written from the
//! contract: a one-way lift, lifts faster than a body is tall, colliders
//! with velocities that are not finite or that `f32` cannot add, a body
//! carried and pushed in one step, soft colliders on every side, and
//! `Unanswered` at every stage with a round rider. Expected values come from
//! the shapes' definitions in `f64` and the contract's sentences, never
//! from the crate. Runs natively and in a wasm runtime.
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::float_cmp,
    clippy::many_single_char_names,
    clippy::too_many_lines
)]

mod audit_support;

use audit_support::{Refusing, Rng, SKIN, config, up};
use sgl_core::math::{Vec2, Vec3};
use sgl_physics::{
    Collider, ColliderKind, Contacts, Floor, MoverOptions, MoverState, Push, Shape, Vector,
    move_and_slide,
};
use wasm_bindgen_test::wasm_bindgen_test;

const DT: f32 = 1.0 / 60.0;

/// A vector with `x` and `y`, and `z` in 3D.
fn vec<V: Vector>(x: f32, y: f32, z: f32) -> V {
    let mut out = V::ZERO;
    out[0] = x;
    out[1] = y;
    if V::AXES == 3 {
        out[2] = z;
    }
    out
}

fn cuboid<V: Vector>(x: f32, y: f32, z: f32) -> Shape<V> {
    Shape::Box { half: vec(x, y, z) }
}

/// The rider: a box 0.8 wide and 1 tall.
fn rider<V: Vector>() -> Shape<V> {
    cuboid(0.4, 0.5, 0.4)
}

/// How far a box body at `at` overlaps a box collider, by their
/// definitions: the least overlap over the axes (positive inside).
fn box_depth<V: Vector>(at: V, half: V, c: &Collider<V>) -> f64 {
    let ch = c.shape.half_extents();
    (0..V::AXES)
        .map(|axis| {
            let (a, b) = (f64::from(at[axis]), f64::from(c.position[axis]));
            let (h, k) = (f64::from(half[axis]), f64::from(ch[axis]));
            (a + h).min(b + k) - (a - h).max(b - k)
        })
        .fold(f64::INFINITY, f64::min)
}

/// A lift `half` wide and 0.5 thick whose top is at `top`, moving at
/// `speed` along up: placed where the game moved it this step, with that
/// velocity.
fn lift<V: Vector>(top: f32, speed: f32, kind: ColliderKind) -> Collider<V> {
    Collider::new(5, cuboid(2.0, 0.25, 2.0), vec(0.0, top - 0.25, 0.0))
        .with_velocity(vec(0.0, speed, 0.0))
        .with_kind(kind)
}

/// A rider standing on the lift, a skin above its top at `top`.
fn standing<V: Vector>(top: f32) -> MoverState<V> {
    MoverState {
        position: vec(0.0, top + 0.5 + SKIN, 0.0),
        velocity: V::ZERO,
        floor: Some(Floor {
            normal: up::<V>(),
            id: 5,
        }),
    }
}

fn one_way_and_fast_lifts_carry_their_rider<V: Vector>() {
    let config = config(up::<V>());
    // Units a second: 1 a step is the rider's height, 1.4 more than it
    // (and less than it and the deck together, so the deck still meets
    // it); down at 0.9 a step is within the body's height below the probe.
    for (speed, kind) in [
        (3.0, ColliderKind::OneWay),
        (-3.0, ColliderKind::OneWay),
        (60.0, ColliderKind::OneWay),
        (-54.0, ColliderKind::OneWay),
        (84.0, ColliderKind::Solid),
        (-54.0, ColliderKind::Solid),
    ] {
        let mut top = 0.0f32;
        let mut state = standing::<V>(top);
        for step in 0..40 {
            top += speed * DT;
            let world = [lift::<V>(top, speed, kind)];
            state.velocity[1] -= 20.0 * DT;
            let contacts = move_and_slide(
                &mut state,
                &rider::<V>(),
                DT,
                &world[..],
                &config,
                &MoverOptions::default(),
            )
            .unwrap();
            let bottom = f64::from(state.position[1]) - 0.5;
            let gap = bottom - f64::from(top);
            let why = format!("{speed} {kind:?} step {step}: gap {gap}, {state:?} {contacts:?}");
            assert_eq!(contacts.carried_by, Some(5), "{why}");
            assert_eq!(state.floor.map(|f| f.id), Some(5), "{why}");
            // Never inside the lift; resting within the skin of its top
            // (physics.md 17: within the skin), and no further off it.
            let rounding = 64.0 * f64::from(f32::EPSILON) * (1.0 + f64::from(top.abs()));
            assert!(
                gap >= -rounding && gap <= f64::from(SKIN) + rounding,
                "{why}"
            );
            assert_eq!(state.position[0], 0.0, "{why}");
        }
    }
}

/// Defect: a one-way lift that drops its rider or lets it sink into its
/// deck, a lift moving a body's height or more a step that leaves its
/// rider behind or inside it, or a descending lift that loses its rider.
/// Oracle: physics.md 21, Carry (a one-way platform carries; a body whose
/// floor moves is moved as it moved; the floor is found where the ground
/// probe looks or as far again below as the body is tall, so a descending
/// lift keeps its rider) and 17 (a resting body stays within the skin),
/// with the boxes' definitions. Riders on one-way and solid lifts going up
/// at 3, 60 and 84 units a second (one and 1.4 body heights a step) and
/// down at 3 and 54 (0.9 of a height a step), under gravity, are carried
/// every step and stand on the lift within the skin of its top.
#[wasm_bindgen_test(unsupported = test)]
fn one_way_and_fast_lifts_carry_their_rider_in_2d() {
    one_way_and_fast_lifts_carry_their_rider::<Vec2>();
}

#[wasm_bindgen_test(unsupported = test)]
fn one_way_and_fast_lifts_carry_their_rider_in_3d() {
    one_way_and_fast_lifts_carry_their_rider::<Vec3>();
}

fn a_lift_that_drops_away_faster_than_the_search_leaves_its_rider<V: Vector>() {
    let config = config(up::<V>());
    // Three body heights a step: beyond the probe and the body's height.
    let speed = -180.0;
    let top = speed * DT;
    let world = [lift::<V>(top, speed, ColliderKind::Solid)];
    let mut state = standing::<V>(0.0);
    state.velocity[1] = -1.0;
    let start = state;
    let contacts = move_and_slide(
        &mut state,
        &rider::<V>(),
        DT,
        &world[..],
        &config,
        &MoverOptions::default(),
    )
    .unwrap();
    assert_eq!(contacts.carried_by, None, "{contacts:?}");
    // It falls by its own velocity only.
    let mut expected = start.position;
    expected[1] -= DT;
    assert_eq!(state.position, expected, "{state:?}");
}

/// Defect: a carry that reaches further below than the contract's search
/// and pulls a body down after a lift that fell away from it. Oracle:
/// physics.md 21, Carry (the floor is found where the ground probe looks
/// or as far again below as the body is tall; when the floor is not there,
/// nothing is carried). A lift that drops three body heights in one step
/// leaves its rider, which moves by its own velocity alone.
#[wasm_bindgen_test(unsupported = test)]
fn a_lift_that_drops_away_beyond_the_search_leaves_its_rider_in_2d() {
    a_lift_that_drops_away_faster_than_the_search_leaves_its_rider::<Vec2>();
}

#[wasm_bindgen_test(unsupported = test)]
fn a_lift_that_drops_away_beyond_the_search_leaves_its_rider_in_3d() {
    a_lift_that_drops_away_faster_than_the_search_leaves_its_rider::<Vec3>();
}

/// One step of the rider in `world`, from `state`.
fn step<V: Vector>(state: MoverState<V>, world: &[Collider<V>]) -> (MoverState<V>, Contacts<V>) {
    let mut state = state;
    let contacts = move_and_slide(
        &mut state,
        &rider::<V>(),
        DT,
        world,
        &config(up::<V>()),
        &MoverOptions::default(),
    )
    .unwrap();
    (state, contacts)
}

fn a_velocity_that_is_not_finite_moves_nothing<V: Vector>() {
    let still = lift::<V>(0.0, 0.0, ColliderKind::Solid);
    let mut state = standing::<V>(0.0);
    state.velocity = vec(1.0, -0.5, 0.0);
    // A rider on it, and a body it was moved into.
    let inside = MoverState {
        position: vec(1.5, 0.3, 0.0),
        velocity: V::ZERO,
        floor: None,
    };
    for velocity in [
        vec::<V>(f32::NAN, 0.0, 0.0),
        vec(0.0, f32::INFINITY, 0.0),
        vec(f32::NEG_INFINITY, 1.0, 0.0),
    ] {
        let moving = [Collider { velocity, ..still }];
        for start in [state, inside] {
            assert_eq!(
                format!("{:?}", step(start, &moving)),
                format!("{:?}", step(start, &[still])),
                "{velocity:?} from {start:?}"
            );
        }
    }
}

/// Defect: a collider whose velocity is not finite that carries or presses
/// a body (a NaN position, or a push the wrong way). Oracle: physics.md 21
/// (a velocity that is not finite moves nothing). A rider on a lift, and a
/// body inside it, step bit for bit as with the lift still.
#[wasm_bindgen_test(unsupported = test)]
fn a_velocity_that_is_not_finite_moves_nothing_in_2d() {
    a_velocity_that_is_not_finite_moves_nothing::<Vec2>();
}

#[wasm_bindgen_test(unsupported = test)]
fn a_velocity_that_is_not_finite_moves_nothing_in_3d() {
    a_velocity_that_is_not_finite_moves_nothing::<Vec3>();
}

fn a_carry_past_the_end_of_f32_changes_nothing<V: Vector>() {
    // A step's carry, f32::MAX / 60, takes 3.39e38 past f32::MAX.
    let far = 3.39e38;
    let world = [
        Collider::new(5, cuboid(2.0, 0.25, 2.0), vec(far, -0.25, 0.0)).with_velocity(vec(
            f32::MAX,
            0.0,
            0.0,
        )),
    ];
    let start = MoverState {
        position: vec(far, 0.5 + SKIN, 0.0),
        velocity: V::ZERO,
        floor: Some(Floor {
            normal: up::<V>(),
            id: 5,
        }),
    };
    let (state, contacts) = step(start, &world);
    assert_eq!(format!("{state:?}"), format!("{start:?}"));
    let nothing = Contacts {
        desired: contacts.desired,
        ..Contacts::default()
    };
    assert_eq!(contacts, nothing);
}

/// Defect: a carry that moves a finite body to an infinite place. Oracle:
/// physics.md 17 (finite inputs whose sum `f32` cannot hold, a position
/// plus the motion a stage would apply, change nothing in the state and
/// report no contact). A rider at 3.39e38 on a lift moving `f32::MAX` a
/// second: the carry would end past `f32`, so the state is unchanged and
/// nothing is reported.
#[wasm_bindgen_test(unsupported = test)]
fn a_carry_past_the_end_of_f32_changes_nothing_in_2d() {
    a_carry_past_the_end_of_f32_changes_nothing::<Vec2>();
}

#[wasm_bindgen_test(unsupported = test)]
fn a_carry_past_the_end_of_f32_changes_nothing_in_3d() {
    a_carry_past_the_end_of_f32_changes_nothing::<Vec3>();
}

fn a_body_carried_and_pushed_in_one_step_takes_both<V: Vector>() {
    // The lift rose 0.05 this step and a door slid 0.05 into the rider.
    let lift = Collider::new(5, cuboid(2.0, 0.25, 2.0), vec(0.0, -0.25 + 0.05, 0.0))
        .with_velocity(vec(0.0, 3.0, 0.0));
    let door = Collider::new(6, cuboid(0.25, 2.0, 2.0), vec(-0.6 + 0.05, 2.0, 0.0))
        .with_velocity(vec(3.0, 0.0, 0.0));
    let start = standing::<V>(0.0);
    let (state, contacts) = step(start, &[lift, door]);
    let half = rider::<V>().half_extents();
    let why = format!("{state:?} {contacts:?}");
    assert_eq!(
        (contacts.carried_by, contacts.pushed_by, contacts.pushed_out),
        (Some(5), Some(6), true),
        "{why}"
    );
    // Clear of both, by the boxes' definitions.
    for c in [lift, door] {
        assert!(box_depth(state.position, half, &c) <= 1e-6, "{why}");
    }
    // Carried up by the lift's 0.05; pressed by the door to a skin off its
    // face, which moved to x = -0.3.
    let expected_x = -0.3 + f64::from(SKIN) + 0.4;
    assert!(
        (f64::from(state.position[0]) - expected_x).abs() < 1e-5,
        "{why}"
    );
    let expected_y = f64::from(start.position[1]) + 0.05;
    assert!(
        (f64::from(state.position[1]) - expected_y).abs() < 1e-5,
        "{why}"
    );
}

/// Defect: a body carried by a lift and pushed by a door in the same step
/// that loses one motion, is pressed back into the lift, or ends in the
/// door. Oracle: physics.md 21 (the carry comes first; the push-out reads
/// the body where the carry left it and presses it the way the door moved
/// to a skin clear of it) and the boxes' definitions. The rider ends a
/// skin off the door's new face, raised with the lift, inside neither,
/// carried by the lift and pushed by the door.
#[wasm_bindgen_test(unsupported = test)]
fn a_body_carried_and_pushed_in_one_step_takes_both_in_2d() {
    a_body_carried_and_pushed_in_one_step_takes_both::<Vec2>();
}

#[wasm_bindgen_test(unsupported = test)]
fn a_body_carried_and_pushed_in_one_step_takes_both_in_3d() {
    a_body_carried_and_pushed_in_one_step_takes_both::<Vec3>();
}

fn soft_colliders_on_every_side_nudge_within_their_limits<V: Vector>(seed: u64) {
    let mut rng = Rng::new(seed);
    let config = config(up::<V>());
    let half = rider::<V>().half_extents();
    let wall = Collider::new(1, cuboid(0.5, 3.0, 3.0), vec(2.0, 0.0, 0.0));
    let mut nudged = 0;
    for case in 0..120 {
        let start = MoverState {
            position: vec(0.0, 0.0, 0.0),
            velocity: rng.vector(-1.0, 1.0),
            floor: None,
        };
        let count = rng.int(1, 4);
        let mut world = Vec::new();
        let mut total = 0.0f64;
        for id in 0..count {
            let limit = rng.range(0.0, 0.08);
            total += f64::from(limit);
            let offset: V = rng.vector(-0.9, 0.9);
            let shape = if rng.chance(50) {
                cuboid(
                    rng.range(0.2, 0.5),
                    rng.range(0.2, 0.5),
                    rng.range(0.2, 0.5),
                )
            } else {
                Shape::Ball {
                    radius: rng.range(0.2, 0.5),
                }
            };
            world.push(Collider::new(10 + id as u64, shape, offset).with_push(Push::Soft(limit)));
        }
        let with_wall = rng.chance(40);
        if with_wall {
            world.push(wall);
        }
        let mut state = start;
        let contacts = move_and_slide(
            &mut state,
            &rider::<V>(),
            DT,
            &world[..],
            &config,
            &MoverOptions::default(),
        )
        .unwrap();
        let why = format!("case {case}: {world:?} {state:?} {contacts:?}");
        // The body's own move, and then the nudge.
        let moved = state.position - start.position;
        let own = start.velocity * DT;
        let nudge: f64 = (0..V::AXES)
            .map(|axis| f64::from(moved[axis] - own[axis]).powi(2))
            .sum::<f64>()
            .sqrt();
        if with_wall {
            assert!(box_depth(state.position, half, &wall) <= 1e-6, "{why}");
        } else {
            assert!(
                nudge <= total * (1.0 + 1e-5) + 1e-6,
                "{why}: nudged {nudge} of {total}"
            );
            // Soft contact takes nothing from the velocity.
            assert_eq!(state.velocity, start.velocity, "{why}");
            nudged += usize::from(nudge > 1e-4);
            if let Some(id) = contacts.pushed_by {
                assert!(id >= 10, "{why}");
            }
        }
    }
    assert!(nudged >= 30, "{nudged} bodies nudged");
}

/// Defect: soft colliders that together nudge a body by more than their
/// limits, that stop its velocity, or that push it into a rigid wall.
/// Oracle: physics.md 21, Soft contact (each plane's accumulated push is
/// held within `0..=limit`, so the nudge is at most the sum of the limits;
/// soft contact takes nothing from the velocity; the displacement stops a
/// skin short of any rigid solid) and the boxes' definitions. A body in
/// open space overlapping one to four soft boxes and balls on every side,
/// some with a wall beside them, moving slowly.
#[wasm_bindgen_test(unsupported = test)]
fn soft_colliders_on_every_side_nudge_within_their_limits_in_2d() {
    soft_colliders_on_every_side_nudge_within_their_limits::<Vec2>(0x0924_2101);
}

#[wasm_bindgen_test(unsupported = test)]
fn soft_colliders_on_every_side_nudge_within_their_limits_in_3d() {
    soft_colliders_on_every_side_nudge_within_their_limits::<Vec3>(0x0924_2102);
}

/// Defect: a step with a round rider that changes its state before a world
/// refuses (a carry or a press kept when a later query is `Unanswered`).
/// Oracle: physics.md 17 (any `Unanswered` query leaves the state exactly
/// as it was) and 10 (any world may refuse any query). A capsule riding a
/// rising lift, a door sliding into it and a soft crowd member beside it,
/// in a world that refuses its k-th query for every k until a step
/// completes: every refused step leaves the state bit-equal, and the step
/// that completes answers as the world that never refuses.
#[wasm_bindgen_test(unsupported = test)]
fn unanswered_at_any_stage_leaves_a_round_rider_as_it_was() {
    let capsule = Shape::Capsule {
        half_axis: Vec3::new(0.0, 0.6, 0.0),
        radius: 0.3,
    };
    let lift = Collider::new(5, cuboid(2.0, 0.25, 2.0), Vec3::new(0.0, -0.25 + 0.05, 0.0))
        .with_velocity(Vec3::new(0.0, 3.0, 0.0));
    let door = Collider::new(6, cuboid(0.25, 2.0, 2.0), Vec3::new(-0.5 + 0.05, 1.0, 0.0))
        .with_velocity(Vec3::new(3.0, 0.0, 0.0));
    let soft = Collider::new(7, Shape::Ball { radius: 0.4 }, Vec3::new(0.6, 0.9, 0.0))
        .with_push(Push::Soft(0.05));
    let world = [lift, door, soft];
    let start = MoverState {
        position: Vec3::new(0.0, 0.9 + SKIN, 0.0),
        velocity: Vec3::new(1.0, -0.3, 0.5),
        floor: Some(Floor {
            normal: Vec3::Y,
            id: 5,
        }),
    };
    let options = MoverOptions {
        step_height: 0.3,
        ledge_drop: Some(0.5),
        ..MoverOptions::default()
    };
    let config = config(Vec3::Y);
    let mut plain = start;
    let expected = move_and_slide(&mut plain, &capsule, DT, &world[..], &config, &options).unwrap();
    assert!(
        expected.carried_by == Some(5) && expected.pushed_by.is_some(),
        "{expected:?}"
    );
    for k in 0..400 {
        let refusing = Refusing {
            inner: &world,
            refuse_at: k,
            asked: std::cell::Cell::new(0),
        };
        let mut state = start;
        match move_and_slide(&mut state, &capsule, DT, &refusing, &config, &options) {
            Err(_) => assert_eq!(format!("{state:?}"), format!("{start:?}"), "refused at {k}"),
            Ok(contacts) => {
                assert!(k > 3, "answered after {k} queries");
                assert_eq!((state, contacts), (plain, expected));
                return;
            }
        }
    }
    panic!("no step completed within 400 queries");
}
