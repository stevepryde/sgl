//! Moving colliders (#920, physics.md 21): riding a platform and leaving
//! it, a collider that pushes or crushes a body, bodies that block or nudge
//! each other, triggers, and the set a game updates by id. Every expected
//! value comes from a requirement of `specs/physics.md`, from Godot's
//! documented platform rule, from geometry worked in the test, or from a
//! brute-force model (a slice holding every collider). Runs natively and in
//! a wasm runtime.
#![allow(clippy::cast_precision_loss)]

mod audit_support;

use std::collections::BTreeMap;

use audit_support::{Refusing, Rng, depth, rounding};
use sgl_core::math::{IVec2, IVec3, Vec2, Vec3};
use sgl_core::{CanonicalWrite, StateHasher};
use sgl_physics::{
    ALL_LAYERS, CellColliders, CellWorld, Collider, ColliderKind, ColliderSet, Contacts,
    MoverConfig, MoverOptions, MoverSettings, MoverState, Push, Shape, Vector, World,
    move_and_slide, overlaps, sensor_events, sensors_at,
};
use wasm_bindgen_test::wasm_bindgen_test;

const SKIN: f32 = 0.01;
const DT: f32 = 1.0 / 60.0;
const GRAVITY: f32 = 20.0;
/// The rounding these tests' coordinates (a few units) can show after a
/// few dozen steps: far below the skin, so a wrong rule still shows.
const ROUND: f32 = 1e-4;

fn config<V: Vector>() -> MoverConfig<V> {
    MoverConfig::new(MoverSettings {
        up: vec(0.0, 1.0, 0.0),
        skin: SKIN,
        snap_distance: 0.1,
        normal_threshold: 0.7,
        block_epsilon: 1e-4,
    })
}

/// A vector from its x, y and z; 2D drops z.
fn vec<V: Vector>(x: f32, y: f32, z: f32) -> V {
    let mut out = V::ZERO;
    out[0] = x;
    out[1] = y;
    if V::AXES == 3 {
        out[2] = z;
    }
    out
}

fn cuboid<V>(half: V) -> Shape<V> {
    Shape::Box { half }
}

fn dot<V: Vector>(a: V, b: V) -> f32 {
    (0..V::AXES).map(|axis| a[axis] * b[axis]).sum()
}

/// The body every test steps: 0.8 wide (and deep), 1 tall.
fn half<V: Vector>() -> V {
    vec(0.4, 0.5, 0.4)
}

/// A floor whose top is at y = 0.
fn floor<V: Vector>(id: u64) -> Collider<V> {
    Collider::new(id, cuboid(vec(20.0, 0.5, 20.0)), vec(0.0, -0.5, 0.0))
}

/// The game's gravity for one step.
fn fall<V: Vector>(state: &mut MoverState<V>) {
    state.velocity[1] -= GRAVITY * DT;
}

fn step<V: Vector, W: World<V> + ?Sized>(state: &mut MoverState<V>, world: &W) -> Contacts<V> {
    let body = cuboid(half::<V>());
    let options = MoverOptions::default();
    move_and_slide(state, &body, DT, world, &config(), &options).expect("the world answers")
}

/// A body at `x, z` resting on whatever is below it at height `y`, after a
/// few steps to land.
fn settled<V: Vector, W: World<V> + ?Sized>(x: f32, y: f32, z: f32, world: &W) -> MoverState<V> {
    let mut state = MoverState {
        position: vec(x, y + 0.5 + 0.5 * SKIN, z),
        ..MoverState::default()
    };
    for _ in 0..5 {
        fall(&mut state);
        step(&mut state, world);
    }
    assert!(state.floor.is_some(), "the body lands");
    state
}

/// How deep the body at `at` is inside `c`, less a rounding step: positive
/// only when it is inside by more than rounding. The oracle reads the
/// shapes' own definitions in `f64`, no crate query.
fn inside<V: Vector>(at: V, c: &Collider<V>) -> f64 {
    depth(at, half::<V>(), c) - rounding(at, half::<V>(), c)
}

/// A collider moved by its velocity over one step, the game's way.
fn advance<V: Vector>(collider: &mut Collider<V>, set: &mut ColliderSet<V>) {
    collider.position = collider.position + collider.velocity * DT;
    assert!(set.set_position(collider.id, collider.position));
    assert!(set.set_velocity(collider.id, collider.velocity));
}

// ------------------------------------------------------------------ riding

/// Defect: a rider left behind by its platform, falling a step behind a
/// descending one, or drifting on it. Oracle: physics.md 21, a body on a
/// moving collider rides it: its place relative to the platform stays the
/// same to within the skin, step after step, the platform stays its floor
/// and the step names it in `carried_by`, whichever way it moves, a lift
/// dropping faster than the ground probe reaches included.
fn a_rider_keeps_its_place<V: Vector>() {
    let mut platform = Collider::<V>::new(7, cuboid(vec(1.5, 0.25, 1.5)), vec(0.0, 2.0, 0.0));
    let mut set = ColliderSet::new(2.0);
    set.insert(platform);
    let mut state = settled(0.2, 2.25, 0.1, &set);
    // The last drops a quarter of a unit a step, more than the ground probe
    // reaches.
    let motions: [V; 6] = [
        vec(3.0, 0.0, 0.0),
        vec(0.0, 2.0, 0.0),
        vec(0.0, -2.0, 0.0),
        vec(-2.0, 1.5, 2.5),
        vec(0.0, -4.0, -3.0),
        vec(0.0, -15.0, 0.0),
    ];
    for motion in motions {
        let held = state.position - platform.position;
        platform.velocity = motion;
        for n in 0..30 {
            advance(&mut platform, &mut set);
            fall(&mut state);
            let contacts = step(&mut state, &set);
            let relative = state.position - platform.position;
            for axis in 0..V::AXES {
                assert!(
                    (relative[axis] - held[axis]).abs() <= SKIN,
                    "{motion:?} step {n}: {relative:?} from {held:?}"
                );
            }
            assert_eq!(state.floor.map(|f| f.id), Some(7), "{motion:?} step {n}");
            assert_eq!(contacts.carried_by, Some(7), "{motion:?} step {n}");
        }
    }
}

#[wasm_bindgen_test(unsupported = test)]
fn a_rider_keeps_its_place_on_a_moving_platform_in_2d() {
    a_rider_keeps_its_place::<Vec2>();
}

#[wasm_bindgen_test(unsupported = test)]
fn a_rider_keeps_its_place_on_a_moving_platform_in_3d() {
    a_rider_keeps_its_place::<Vec3>();
}

/// Defect: a rider that leaves without walking or jumping, keeps the
/// platform's velocity while still on it, or loses it on leaving. Oracle:
/// physics.md 21 and Godot's `CharacterBody` default
/// (`PLATFORM_ON_LEAVE_ADD_VELOCITY`): a body standing still stays on its
/// platform, and one that leaves it in a step, by a jump or by walking off
/// its edge, ends the step with the platform's velocity added to its own.
/// A jump meets nothing, so its velocity is exactly the jump plus the
/// platform's.
fn a_rider_leaves_only_by_walking_or_jumping<V: Vector>() {
    let platform_velocity: V = vec(3.0, 1.0, -2.0);
    let mut platform = Collider::<V>::new(7, cuboid(vec(1.5, 0.25, 1.5)), vec(0.0, 2.0, 0.0));
    let mut set = ColliderSet::new(2.0);
    set.insert(platform);
    let start = settled(0.0, 2.25, 0.0, &set);
    platform.velocity = platform_velocity;

    // Standing still: on it every step, its own velocity untouched.
    let mut state = start;
    let mut moving = platform;
    let mut moving_set = set.clone();
    for n in 0..40 {
        advance(&mut moving, &mut moving_set);
        state.velocity = vec(0.0, state.velocity[1], 0.0);
        fall(&mut state);
        step(&mut state, &moving_set);
        assert_eq!(state.floor.map(|f| f.id), Some(7), "step {n}");
        assert!(state.velocity[0] == 0.0 && dot(state.velocity, vec(0.0, 0.0, 1.0)) == 0.0);
    }

    // A jump: the platform's velocity joins it.
    let jump: V = vec(0.0, 6.0, 0.0);
    state.velocity = jump;
    advance(&mut moving, &mut moving_set);
    step(&mut state, &moving_set);
    assert_eq!(state.floor, None);
    assert_eq!(state.velocity, jump + platform_velocity);

    // Walking off its edge (+x): on it until the step it leaves, which adds
    // the platform's velocity to the walk.
    let mut state = start;
    let (mut moving, mut moving_set) = (platform, set);
    let mut left = false;
    for _ in 0..120 {
        advance(&mut moving, &mut moving_set);
        state.velocity[0] = 5.0;
        fall(&mut state);
        step(&mut state, &moving_set);
        if state.floor.is_none() {
            assert_eq!(state.velocity[0].to_bits(), (5.0f32 + 3.0).to_bits());
            left = true;
            break;
        }
        assert_eq!(
            state.velocity[0].to_bits(),
            5.0f32.to_bits(),
            "no platform velocity while on it"
        );
    }
    assert!(left, "the walker reaches the edge");
}

/// Defect: a platform found wherever a world offers it, so one that has
/// left its rider still carries it through a slice (which offers every
/// collider) and not through a set. Oracle: physics.md 6 and 21, the floor
/// is found under the body and results never depend on the world. A
/// platform 2 wide slides 3 units in one step, out from under its rider:
/// nothing carries the rider, it is left standing on nothing, and a slice
/// and a set give bit-equal results.
fn a_platform_that_left_its_rider_does_not_carry_it<V: Vector>() {
    let platform = Collider::<V>::new(7, cuboid(vec(1.0, 0.25, 1.0)), vec(0.0, 2.0, 0.0));
    let start = settled(0.0, 2.25, 0.0, &[platform][..]);
    let gone = Collider {
        position: platform.position + vec(3.0, 0.0, 0.0),
        ..platform.with_velocity(vec(180.0, 0.0, 0.0))
    };
    let mut set = ColliderSet::new(2.0);
    set.insert(gone);
    let mut through_slice = start;
    let mut through_set = start;
    for state in [&mut through_slice, &mut through_set] {
        fall(state);
    }
    let from_slice = step(&mut through_slice, &[gone][..]);
    let from_set = step(&mut through_set, &set);
    assert_eq!(from_slice.carried_by, None);
    assert_eq!(through_slice.floor, None);
    assert_eq!(
        format!("{through_slice:?} {from_slice:?}"),
        format!("{through_set:?} {from_set:?}")
    );
}

#[wasm_bindgen_test(unsupported = test)]
fn a_platform_that_left_its_rider_does_not_carry_it_in_2d() {
    a_platform_that_left_its_rider_does_not_carry_it::<Vec2>();
}

#[wasm_bindgen_test(unsupported = test)]
fn a_platform_that_left_its_rider_does_not_carry_it_in_3d() {
    a_platform_that_left_its_rider_does_not_carry_it::<Vec3>();
}

#[wasm_bindgen_test(unsupported = test)]
fn a_rider_leaves_only_by_walking_or_jumping_in_2d() {
    a_rider_leaves_only_by_walking_or_jumping::<Vec2>();
}

#[wasm_bindgen_test(unsupported = test)]
fn a_rider_leaves_only_by_walking_or_jumping_in_3d() {
    a_rider_leaves_only_by_walking_or_jumping::<Vec3>();
}

/// Defect: a carry moved without a sweep, so a fast platform takes its
/// rider through a thin wall it cannot pass. Oracle: physics.md 21, a carry
/// that would press the body into a solid is stopped there. A platform 6
/// wide slides at 60 units a second (a unit a step) under a wall 0.1 thick
/// hanging from above, whose bottom clears the platform by 0.3, less than
/// the rider's height: every step the rider stays on the near side of the
/// wall, its face at most a skin from it, while the platform slides on
/// beneath.
fn a_fast_carry_stops_at_a_thin_wall<V: Vector>() {
    let mut platform = Collider::<V>::new(7, cuboid(vec(3.0, 0.25, 1.5)), vec(0.0, 1.0, 0.0));
    let wall = Collider::new(2, cuboid(vec(0.05, 2.0, 2.0)), vec(0.45 + SKIN, 3.55, 0.0));
    let still = [wall];
    let mut set = ColliderSet::new(2.0);
    set.insert(platform);
    let mut state = settled(0.0, 1.25, 0.0, &(&still[..], &set));
    platform.velocity = vec(60.0, 0.0, 0.0);
    for n in 0..3 {
        advance(&mut platform, &mut set);
        fall(&mut state);
        let contacts = step(&mut state, &(&still[..], &set));
        assert_eq!(contacts.carried_by, Some(7), "step {n}");
        let face = state.position[0] + 0.4;
        assert!(
            face <= 0.4 + SKIN + ROUND,
            "step {n}: passed the wall to {face}"
        );
        assert!(inside(state.position, &wall) <= 0.0, "step {n}");
    }
}

#[wasm_bindgen_test(unsupported = test)]
fn a_fast_carry_stops_at_a_thin_wall_in_2d() {
    a_fast_carry_stops_at_a_thin_wall::<Vec2>();
}

#[wasm_bindgen_test(unsupported = test)]
fn a_fast_carry_stops_at_a_thin_wall_in_3d() {
    a_fast_carry_stops_at_a_thin_wall::<Vec3>();
}

// ------------------------------------------------------------------ pushing

/// A door 0.5 thick along `direction` (a unit vector across up), 2 tall and
/// 2 across, standing on the floor `gap` behind a body at the origin, and
/// moving along `direction` at 4 units a second.
fn door<V: Vector>(direction: V, gap: f32) -> Collider<V> {
    let mut half = vec::<V>(1.0, 1.0, 1.0);
    let axis = (0..V::AXES).find(|&axis| direction[axis] != 0.0).unwrap();
    half[axis] = 0.25;
    let position = vec::<V>(0.0, 1.0, 0.0) - direction * (0.4 + 0.25 + gap);
    Collider::new(5, cuboid(half), position).with_velocity(direction * 4.0)
}

/// Defect: a body a collider moves into left inside it, pushed out the
/// wrong way (back through the door, or up), or pushed further than the
/// door moved. Oracle: physics.md 21, a collider that has moved into a body
/// pushes it out the way it moved: after every step the body's trailing
/// face is clear of the door's leading face by no more than the skin, the
/// body has not moved across the door's motion, and the step names the
/// door in `pushed_by`.
fn a_collider_moved_into_a_body_pushes_it_clear<V: Vector>(direction: V) {
    let ground = [floor::<V>(1)];
    let mut door = door(direction, 0.05);
    let mut set = ColliderSet::new(2.0);
    set.insert(door);
    let world = (&ground[..], &set);
    let mut state = settled(0.0, 0.0, 0.0, &world);
    let across = state.position - direction * dot(state.position, direction);
    let mut pushed = 0;
    for n in 0..40 {
        advance(&mut door, &mut set);
        let caught = dot(state.position - door.position, direction) < 0.65;
        fall(&mut state);
        let world = (&ground[..], &set);
        let contacts = step(&mut state, &world);
        let clear = dot(state.position - door.position, direction) - 0.65;
        assert!(clear >= -ROUND, "step {n}: inside the door by {clear}");
        assert!(inside(state.position, &ground[0]) <= 0.0, "step {n}");
        let now = state.position - direction * dot(state.position, direction);
        for axis in 0..V::AXES {
            if axis != 1 {
                assert_eq!(now[axis].to_bits(), across[axis].to_bits(), "step {n}");
            }
        }
        if caught {
            pushed += 1;
            assert!(clear <= SKIN + ROUND, "step {n}: pushed {clear} clear");
            assert_eq!(contacts.pushed_by, Some(5), "step {n}");
            assert!(contacts.pushed_out && !contacts.crushed);
        }
    }
    assert!(pushed > 30, "{pushed}");
}

#[wasm_bindgen_test(unsupported = test)]
fn a_collider_moved_into_a_body_pushes_it_clear_in_2d() {
    a_collider_moved_into_a_body_pushes_it_clear(Vec2::X);
    a_collider_moved_into_a_body_pushes_it_clear(Vec2::NEG_X);
}

#[wasm_bindgen_test(unsupported = test)]
fn a_collider_moved_into_a_body_pushes_it_clear_in_3d() {
    a_collider_moved_into_a_body_pushes_it_clear(Vec3::X);
    a_collider_moved_into_a_body_pushes_it_clear(Vec3::NEG_Z);
}

/// Defect: a pusher that frees a body the shortest way (along one axis)
/// rather than the way it moved. Oracle: physics.md 21, the push prefers
/// the collider's motion when that way is free. A block moving up and to
/// the right (and along z in 3D) at an angle into a body floating in open
/// space (no gravity, nothing else near) leaves it moved only along that
/// motion: its displacement from where it started is parallel to the
/// motion, and it is clear of the block.
fn a_pusher_at_an_angle_pushes_the_way_it_moved<V: Vector>() {
    let motion: V = vec(3.0, 2.0, 1.0);
    let length = dot(motion, motion).sqrt();
    let start: V = vec(0.0, 5.0, 0.0);
    // Two units back along the motion: clear of the body on x.
    let mut block = Collider::new(
        6,
        cuboid(vec(0.5, 0.5, 0.5)),
        start - motion * (2.0 / length),
    )
    .with_velocity(motion);
    let mut set = ColliderSet::new(2.0);
    set.insert(block);
    let mut state = MoverState {
        position: start,
        ..MoverState::default()
    };
    let mut pushed = 0;
    for n in 0..45 {
        advance(&mut block, &mut set);
        let contacts = step(&mut state, &set);
        assert!(inside(state.position, &block) <= 0.0, "step {n}");
        if contacts.pushed_by == Some(6) {
            pushed += 1;
            let moved = state.position - start;
            let along = dot(moved, motion) / length;
            let off = moved - motion * (along / length);
            assert!(
                along > 0.0 && dot(off, off).sqrt() <= ROUND,
                "step {n}: {moved:?}"
            );
        }
    }
    assert!(pushed > 20, "{pushed}");
}

#[wasm_bindgen_test(unsupported = test)]
fn a_pusher_at_an_angle_pushes_the_way_it_moved_in_2d() {
    a_pusher_at_an_angle_pushes_the_way_it_moved::<Vec2>();
}

#[wasm_bindgen_test(unsupported = test)]
fn a_pusher_at_an_angle_pushes_the_way_it_moved_in_3d() {
    a_pusher_at_an_angle_pushes_the_way_it_moved::<Vec3>();
}

/// Defect: a body crushed against a wall pushed into the wall, through the
/// door, or not reported. Oracle: physics.md 21 and geometry. The body is
/// 0.8 wide: once the door's leading face is less than that from the wall
/// it cannot fit and the step reports `crushed` (by the door), and while
/// the room is more than its width and two skins it is not crushed. It is
/// never inside the wall or the floor. The door moves on until its trailing
/// face is about to pass the wall's face.
fn a_crushed_body_never_enters_the_wall<V: Vector>() {
    let wall = Collider::new(2, cuboid(vec(0.5, 2.0, 2.0)), vec(1.5, 2.0, 0.0));
    let ground = [floor::<V>(1), wall];
    let mut door = door(vec(1.0, 0.0, 0.0), 0.05);
    let mut set = ColliderSet::new(2.0);
    set.insert(door);
    let mut state = settled(0.0, 0.0, 0.0, &(&ground[..], &set));
    let mut crushed = 0;
    for n in 0..29 {
        advance(&mut door, &mut set);
        assert!(door.position[0] - 0.25 < 1.0);
        fall(&mut state);
        let contacts = step(&mut state, &(&ground[..], &set));
        let room = 1.0 - (door.position[0] + 0.25);
        for solid in &ground {
            assert!(
                inside(state.position, solid) <= 0.0,
                "step {n}: in {}",
                solid.id
            );
        }
        if room < 0.8 {
            assert!(contacts.crushed, "step {n}: room {room}");
            assert_eq!(contacts.pushed_by, Some(5));
            crushed += 1;
        }
        if room > 0.8 + 2.0 * SKIN + ROUND {
            assert!(!contacts.crushed, "step {n}: room {room}");
        }
    }
    assert!(crushed > 3, "{crushed}");
}

#[wasm_bindgen_test(unsupported = test)]
fn a_crushed_body_never_enters_the_wall_in_2d() {
    a_crushed_body_never_enters_the_wall::<Vec2>();
}

#[wasm_bindgen_test(unsupported = test)]
fn a_crushed_body_never_enters_the_wall_in_3d() {
    a_crushed_body_never_enters_the_wall::<Vec3>();
}

// ------------------------------------------------------- bodies and bodies

/// Two bodies, ids 1 and 2, on a floor, walking at each other at `speed`
/// for `walk` steps and then standing, stepped in ascending id against the
/// other as it stands (physics.md 21's convention): each step removes the
/// body's own collider from the set, steps it, and puts it back where it
/// ended. Calls `check` after each body's step with its id, where it
/// started, the state and the contacts, and `pair` after both.
fn two_bodies<V: Vector>(
    push: Push,
    speed: f32,
    walk: u32,
    steps: u32,
    mut check: impl FnMut(u64, V, &MoverState<V>, &Contacts<V>),
    mut pair: impl FnMut(u32, &[MoverState<V>; 2]),
) {
    let ground = [floor::<V>(100)];
    let body = cuboid(half::<V>());
    let collider = |id: u64, state: &MoverState<V>| {
        Collider::new(id, body, state.position)
            .with_velocity(state.velocity)
            .with_push(push)
    };
    let mut states = [
        settled(-0.9, 0.0, 0.0, &ground[..]),
        settled(0.9, 0.0, 0.0, &ground[..]),
    ];
    let mut set = ColliderSet::new(2.0);
    for (id, state) in (1u64..).zip(&states) {
        set.insert(collider(id, state));
    }
    for n in 0..steps {
        for (index, id) in [(0, 1u64), (1, 2)] {
            let state = &mut states[index];
            assert!(set.remove(id));
            let toward = if index == 0 { speed } else { -speed };
            state.velocity[0] = if n < walk { toward } else { 0.0 };
            fall(state);
            let before = state.position;
            let contacts = step(state, &(&ground[..], &set));
            check(id, before, state, &contacts);
            assert!(!set.insert(collider(id, state)));
        }
        pair(n, &states);
    }
}

/// How far two bodies of [`half`] at `a` and `b` overlap along x, the only
/// axis they meet on: positive when they do, the gap's negation when not.
fn overlap_x<V: Vector>(a: V, b: V) -> f32 {
    0.8 - (a[0] - b[0]).abs()
}

/// Defect: bodies that walk into each other end overlapping, or never come
/// to rest. Oracle: physics.md 21, a rigid body blocks: two bodies walked
/// at each other never overlap, by the boxes' own geometry, and come to
/// rest facing each other no more than two skins apart.
fn rigid_bodies_block_each_other<V: Vector>() {
    let mut last = [V::ZERO; 2];
    two_bodies::<V>(
        Push::Rigid,
        3.0,
        60,
        60,
        |_, _, _, _| {},
        |n, states| {
            let overlap = overlap_x(states[0].position, states[1].position);
            assert!(overlap <= ROUND, "step {n}: overlap {overlap}");
            last = [states[0].position, states[1].position];
        },
    );
    let gap = -overlap_x(last[0], last[1]);
    assert!((0.0..=2.0 * SKIN + ROUND).contains(&gap), "gap {gap}");
}

#[wasm_bindgen_test(unsupported = test)]
fn rigid_bodies_block_each_other_in_2d() {
    rigid_bodies_block_each_other::<Vec2>();
}

#[wasm_bindgen_test(unsupported = test)]
fn rigid_bodies_block_each_other_in_3d() {
    rigid_bodies_block_each_other::<Vec3>();
}

/// Defect: a soft contact that stops a body, nudges it by more than its
/// limit, or holds two bodies together once they stop pushing. Oracle:
/// physics.md 21. Walking at 2 units a second (a thirtieth of a unit a
/// step) against a limit of 0.02 a step, each body's x moves by its own
/// walk plus a nudge of at most the limit, and the nudge names the other
/// body; they overlap, since the walk beats the limit, and once they stand
/// still they separate.
fn soft_bodies_nudge_each_other<V: Vector>() {
    let limit = 0.02;
    let mut nudged = 0;
    let mut overlapped = false;
    let mut apart_after_release = false;
    two_bodies::<V>(
        Push::Soft(limit),
        2.0,
        25,
        60,
        |id, before, state, contacts| {
            let walked = state.velocity[0] * DT;
            let nudge = (state.position[0] - before[0]) - walked;
            assert!(nudge.abs() <= limit + ROUND, "body {id}: nudge {nudge}");
            if nudge.abs() > ROUND {
                nudged += 1;
                assert_eq!(contacts.pushed_by, Some(3 - id));
            }
        },
        |n, states| {
            let overlap = overlap_x(states[0].position, states[1].position);
            overlapped |= overlap > 0.05;
            if n >= 50 {
                assert!(overlap <= 0.0, "step {n}: still overlapping by {overlap}");
                apart_after_release = true;
            }
        },
    );
    assert!(nudged > 10 && overlapped && apart_after_release, "{nudged}");
}

#[wasm_bindgen_test(unsupported = test)]
fn soft_bodies_nudge_each_other_in_2d() {
    soft_bodies_nudge_each_other::<Vec2>();
}

#[wasm_bindgen_test(unsupported = test)]
fn soft_bodies_nudge_each_other_in_3d() {
    soft_bodies_nudge_each_other::<Vec3>();
}

/// Defect: a soft nudge applied without a sweep, pressing a body into a
/// rigid wall. Oracle: physics.md 21, the nudge never puts the body into a
/// rigid solid. A body standing against a wall (its face a skin away) with
/// a soft collider of a generous limit (0.5) overlapping it from the other
/// side by 0.3 is nudged toward the wall and stops a skin short of it,
/// never inside it.
fn a_soft_nudge_never_enters_a_wall<V: Vector>() {
    let wall = Collider::new(2, cuboid(vec(0.5, 2.0, 2.0)), vec(0.9 + SKIN, 2.0, 0.0));
    let crowd = Collider::new(3, cuboid(half::<V>()), vec(-0.5, 0.5 + 0.5 * SKIN, 0.0))
        .with_push(Push::Soft(0.5));
    let world = [floor::<V>(1), wall, crowd];
    let mut state = settled(0.0, 0.0, 0.0, &world[..2]);
    for n in 0..5 {
        fall(&mut state);
        let contacts = step(&mut state, &world[..]);
        assert!(inside(state.position, &wall) <= 0.0, "step {n}");
        assert!(0.4 + SKIN + ROUND >= wall.position[0] - 0.5 - state.position[0]);
        assert!(contacts.pushed_by.is_none() || contacts.pushed_by == Some(3));
    }
}

#[wasm_bindgen_test(unsupported = test)]
fn a_soft_nudge_never_enters_a_wall_in_2d() {
    a_soft_nudge_never_enters_a_wall::<Vec2>();
}

#[wasm_bindgen_test(unsupported = test)]
fn a_soft_nudge_never_enters_a_wall_in_3d() {
    a_soft_nudge_never_enters_a_wall::<Vec3>();
}

// ------------------------------------------------------------------ sensors

/// Defect: an entry or exit reported twice, missed, or reported for a
/// sensor the body stays in. Oracle: the set differences worked by hand,
/// over a body's sensor lists step by step, including a sensor entered and
/// left in consecutive steps (5) and lists given out of order with repeats.
#[wasm_bindgen_test(unsupported = test)]
fn sensor_events_report_each_entry_and_exit_once() {
    let steps: [&[u64]; 6] = [&[], &[3], &[3, 5], &[3], &[], &[9, 2, 9]];
    let expected: [(&[u64], &[u64]); 5] = [
        (&[3], &[]),
        (&[5], &[]),
        (&[], &[5]),
        (&[], &[3]),
        (&[2, 9], &[]),
    ];
    let mut entered = BTreeMap::<u64, u32>::new();
    let mut left = BTreeMap::<u64, u32>::new();
    for (pair, want) in steps.windows(2).zip(expected) {
        let (into, out) = sensor_events(pair[0], pair[1]);
        assert_eq!((into.as_slice(), out.as_slice()), want, "{pair:?}");
        for id in into {
            *entered.entry(id).or_default() += 1;
        }
        for id in out {
            *left.entry(id).or_default() += 1;
        }
    }
    assert_eq!(entered.get(&5), Some(&1));
    assert_eq!(left.get(&5), Some(&1));
    assert_eq!(sensor_events(&[4, 1, 4], &[1, 4]), (vec![], vec![]));
}

/// Defect: `sensors_at` listing a solid, a sensor on another layer, one the
/// body only touches, or one id twice. Oracle: physics.md 21 and the
/// geometry, every value exact in `f32`: the body (half-extents 0.5 about
/// the origin) overlaps sensors 4 and 9 (9 twice, in a slice where ids may
/// repeat) and the solid 1; it touches sensor 7 exactly at x = 0.5; sensor
/// 8 overlaps it on a layer the mask leaves out.
#[wasm_bindgen_test(unsupported = test)]
fn sensors_at_lists_the_sensors_overlapped_once_each() {
    let sensor = |id, x: f32| {
        Collider::new(id, cuboid(Vec2::splat(0.5)), Vec2::new(x, 0.0))
            .with_kind(ColliderKind::Sensor)
    };
    let world = [
        sensor(9, 0.75),
        Collider::new(1, cuboid(Vec2::splat(0.5)), Vec2::new(0.25, 0.0)),
        sensor(7, 1.0),
        sensor(4, -0.5),
        sensor(9, -0.75),
        sensor(8, 0.0).with_layers(2),
    ];
    let found = sensors_at(&cuboid(Vec2::splat(0.5)), Vec2::ZERO, &world[..], 1).unwrap();
    assert_eq!(found, vec![4, 9]);
}

// ------------------------------------------------------------ the set by id

/// Defect: a set updated by id that loses, duplicates or misplaces a
/// collider (a removal that leaves a stale bucket, a moved collider still
/// found where it was), or reports the wrong presence. Oracle: a brute
/// force, every collider in a slice, kept by the test in a map by id; after
/// each of 400 random inserts, moves, velocity changes and removals the set
/// answers `overlaps` for random regions exactly as the slice does, holds
/// as many colliders, and each update says whether the id was there. Some
/// colliders span more cells than a set buckets, so they are kept apart.
#[wasm_bindgen_test(unsupported = test)]
fn a_set_updated_by_id_answers_as_every_collider_does() {
    let mut rng = Rng::new(0x5eed_0920);
    let mut set = ColliderSet::<Vec2>::new(0.25);
    let mut truth = BTreeMap::<u64, Collider<Vec2>>::new();
    let random = |rng: &mut Rng, id: u64| {
        let big = rng.chance(10);
        let half = if big {
            Vec2::new(rng.range(6.0, 9.0), rng.range(0.2, 6.0))
        } else {
            Vec2::new(rng.range(0.1, 1.0), rng.range(0.1, 1.0))
        };
        Collider::new(id, cuboid(half), rng.vector(-10.0, 10.0))
    };
    for n in 0..400 {
        let id = u64::try_from(rng.int(0, 30)).unwrap();
        let present = truth.contains_key(&id);
        match rng.below(4) {
            0 => {
                let collider = random(&mut rng, id);
                assert_eq!(set.insert(collider), present, "op {n}");
                truth.insert(id, collider);
            }
            1 => {
                let to: Vec2 = rng.vector(-10.0, 10.0);
                assert_eq!(set.set_position(id, to), present, "op {n}");
                if let Some(c) = truth.get_mut(&id) {
                    c.position = to;
                }
            }
            2 => {
                let velocity: Vec2 = rng.vector(-3.0, 3.0);
                assert_eq!(set.set_velocity(id, velocity), present, "op {n}");
                if let Some(c) = truth.get_mut(&id) {
                    c.velocity = velocity;
                }
            }
            _ => {
                assert_eq!(set.remove(id), present, "op {n}");
                truth.remove(&id);
            }
        }
        assert_eq!(set.len(), truth.len());
        let all: Vec<Collider<Vec2>> = truth.values().copied().collect();
        for _ in 0..3 {
            let probe = cuboid(Vec2::new(rng.range(0.1, 3.0), rng.range(0.1, 3.0)));
            let at = rng.vector(-12.0, 12.0);
            let through_set = overlaps(&probe, at, &set, ALL_LAYERS).unwrap();
            let through_all = overlaps(&probe, at, &all[..], ALL_LAYERS).unwrap();
            assert_eq!(through_set, through_all, "op {n}");
        }
    }
}

/// A scene of moving things around two bodies: a lift (10) going up and
/// down, a door (11) sliding to and fro, a soft neighbour (12) wandering
/// through the walkers, and a sensor (13) and a crate (14) that come and
/// go.
/// `colliders(n)` is the game's state at step `n`, as it would hold it.
fn scene<V: Vector>(n: u32) -> Vec<Collider<V>> {
    let phase = |period: u32| {
        if (n / period).is_multiple_of(2) {
            1.0
        } else {
            -1.0
        }
    };
    let swing = |period: u32, speed: f32| {
        // Where something moving at `speed` and turning every `period`
        // steps is after `n` steps, and its velocity now.
        let mut at = 0.0f32;
        for k in 0..n {
            at += if (k / period).is_multiple_of(2) {
                speed
            } else {
                -speed
            } * DT;
        }
        (at, phase(period) * speed)
    };
    let (lift, lift_speed) = swing(40, 1.5);
    let (door, door_speed) = swing(30, 2.0);
    let (wander, wander_speed) = swing(25, 1.0);
    let mut colliders = vec![
        floor::<V>(1),
        Collider::new(10, cuboid(vec(1.0, 0.25, 1.0)), vec(-3.0, 1.0 + lift, 0.0))
            .with_velocity(vec(0.0, lift_speed, 0.0)),
        Collider::new(11, cuboid(vec(0.25, 1.0, 1.0)), vec(1.0 + door, 1.0, 0.0))
            .with_velocity(vec(door_speed, 0.0, 0.0)),
        Collider::new(12, cuboid(half::<V>()), vec(2.5 + wander, 0.5, 0.0))
            .with_velocity(vec(wander_speed, 0.0, 0.0))
            .with_push(Push::Soft(0.03)),
    ];
    if (n / 15).is_multiple_of(2) {
        colliders.push(
            Collider::new(13, cuboid(vec(0.5, 0.5, 0.5)), vec(3.0, 0.5, 0.0))
                .with_kind(ColliderKind::Sensor),
        );
    }
    if (n / 10).is_multiple_of(2) {
        colliders.push(Collider::new(
            14,
            cuboid(vec(0.3, 0.3, 0.3)),
            vec(5.0, 0.3, 0.0),
        ));
    }
    colliders
}

/// The game's input for body `index` at step `n`: a function of the step
/// alone, so a replay is fed what the forward run was.
fn input<V: Vector>(index: usize, n: u32, state: &mut MoverState<V>) {
    let mut rng = Rng::new(u64::from(n) * 2 + index as u64);
    state.velocity[0] = rng.range(-3.0, 3.0);
    if V::AXES == 3 {
        state.velocity[2] = rng.range(-0.5, 0.5);
    }
    if state.floor.is_some() && rng.chance(4) {
        state.velocity[1] = 6.0;
    }
    fall(state);
}

/// Everything one step of the two scene bodies gives, as exact bits.
fn step_digest<V: Vector>(states: &[MoverState<V>], contacts: &[Contacts<V>]) -> String {
    let mut hasher = StateHasher::new();
    for (state, contacts) in states.iter().zip(contacts) {
        state.canonical_write(&mut hasher);
        contacts.canonical_write(&mut hasher);
    }
    hasher.finish().to_hex()
}

/// Defect: a result that depends on the order a set was updated or built
/// in (physics.md 6 and 21). Oracle: the same game run twice, once with one
/// set kept across steps and updated by id (moves, velocities, a sensor and
/// a crate removed and inserted again), once with a set rebuilt every step
/// from the
/// game's colliders in a shuffled order: every step's states and contacts
/// are bit-equal.
fn a_kept_set_and_a_rebuilt_one_step_alike<V: Vector>() {
    let run = |kept: bool| {
        let mut rng = Rng::new(0x05ee_d5e7);
        let mut states = [
            settled(-3.0, 1.25, 0.0, &scene::<V>(0)[..]),
            settled(0.0, 0.0, 0.0, &scene::<V>(0)[..]),
        ];
        let mut set = ColliderSet::new(1.5);
        for c in scene::<V>(0) {
            set.insert(c);
        }
        let mut digests = Vec::new();
        for n in 1..120 {
            let colliders = scene::<V>(n);
            if kept {
                for c in &colliders {
                    if !set.set_position(c.id, c.position) {
                        set.insert(*c);
                    }
                    set.set_velocity(c.id, c.velocity);
                }
                for gone in [13, 14] {
                    if !colliders.iter().any(|c| c.id == gone) {
                        set.remove(gone);
                    }
                }
            } else {
                let mut shuffled = colliders.clone();
                rng.shuffle(&mut shuffled);
                set = ColliderSet::new(1.5);
                for c in shuffled {
                    set.insert(c);
                }
            }
            let mut contacts = Vec::new();
            for (index, state) in states.iter_mut().enumerate() {
                input(index, n, state);
                contacts.push(step(state, &set));
            }
            digests.push(step_digest(&states, &contacts));
        }
        digests
    };
    assert_eq!(run(true), run(false));
}

#[wasm_bindgen_test(unsupported = test)]
fn a_kept_set_and_a_rebuilt_one_step_alike_in_2d() {
    a_kept_set_and_a_rebuilt_one_step_alike::<Vec2>();
}

#[wasm_bindgen_test(unsupported = test)]
fn a_kept_set_and_a_rebuilt_one_step_alike_in_3d() {
    a_kept_set_and_a_rebuilt_one_step_alike::<Vec3>();
}

// ------------------------------------------------------------- atomicity

/// A world that answers every query but its `refuse`-th, which it refuses:
/// what any world may do (physics.md 10), here to reach each stage alone.
struct RefusingOne<'a, V: Vector> {
    inner: &'a [Collider<V>],
    refuse: usize,
    asked: std::cell::Cell<usize>,
}

impl<V: Vector> RefusingOne<'_, V> {
    fn answer(&self, visit: &mut dyn FnMut(&Collider<V>)) -> Result<(), sgl_physics::Unanswered> {
        let n = self.asked.get();
        self.asked.set(n + 1);
        if n == self.refuse {
            return Err(sgl_physics::Unanswered);
        }
        self.inner.iter().for_each(visit);
        Ok(())
    }
}

impl<V: Vector> World<V> for RefusingOne<'_, V> {
    fn colliders_near(
        &self,
        _min: V,
        _max: V,
        visit: &mut dyn FnMut(&Collider<V>),
    ) -> Result<(), sgl_physics::Unanswered> {
        self.answer(visit)
    }
}

/// Defect: a step that writes part of its work before a later query is
/// refused, or carries on past a refusal. Oracle: physics.md 17 and 21, an
/// `Unanswered` query at any stage leaves the state exactly as it was and
/// the step answers `Unanswered`. A body riding a lift, with a door moved
/// into it and a soft neighbour overlapping it, so every stage asks: a
/// world that refuses from its k-th query on, and one that refuses only its
/// k-th, for every k the step asks.
fn unanswered_moves_nothing<V: Vector>() {
    let ground = [floor::<V>(1)];
    let lift = Collider::new(10, cuboid(vec(2.0, 0.25, 2.0)), vec(0.0, 1.0, 0.0));
    let start = settled(0.0, 1.25, 0.0, &(&ground[..], &[lift][..]));
    let moving = [
        lift.with_velocity(vec(1.0, 1.0, 0.0)),
        Collider::new(
            11,
            cuboid(vec(0.25, 1.0, 1.0)),
            start.position - vec(0.5, 0.0, 0.0),
        )
        .with_velocity(vec(3.0, 0.0, 0.0)),
        Collider::new(12, cuboid(half::<V>()), start.position + vec(0.6, 0.0, 0.0))
            .with_push(Push::Soft(0.05)),
    ];
    let mut answered = None;
    for k in 0..200 {
        let refusing = Refusing {
            inner: &moving,
            refuse_at: k,
            asked: std::cell::Cell::new(0),
        };
        let world = (&ground[..], &refusing);
        let mut state = start;
        let body = cuboid(half::<V>());
        let result = move_and_slide(
            &mut state,
            &body,
            DT,
            &world,
            &config(),
            &MoverOptions::default(),
        );
        match result {
            Err(_) => assert_eq!(format!("{state:?}"), format!("{start:?}"), "refused at {k}"),
            Ok(contacts) => {
                answered = Some((k, contacts));
                break;
            }
        }
    }
    let (asked, contacts) = answered.expect("answered once the world stops refusing");
    assert!(asked > 5, "{asked}");
    assert_eq!(contacts.carried_by, Some(10));
    assert_eq!(contacts.pushed_by, Some(11));
    for k in 0..asked {
        let refusing = RefusingOne {
            inner: &moving,
            refuse: k,
            asked: std::cell::Cell::new(0),
        };
        let world = (&ground[..], &refusing);
        let mut state = start;
        let body = cuboid(half::<V>());
        let result = move_and_slide(
            &mut state,
            &body,
            DT,
            &world,
            &config(),
            &MoverOptions::default(),
        );
        assert!(result.is_err(), "query {k} refused, yet answered");
        assert_eq!(format!("{state:?}"), format!("{start:?}"), "refused at {k}");
    }
}

#[wasm_bindgen_test(unsupported = test)]
fn unanswered_moves_nothing_with_moving_colliders_in_2d() {
    unanswered_moves_nothing::<Vec2>();
}

#[wasm_bindgen_test(unsupported = test)]
fn unanswered_moves_nothing_with_moving_colliders_in_3d() {
    unanswered_moves_nothing::<Vec3>();
}

// ------------------------------------------------------------- the details

/// Defect: a cell world that passes a moving collider's velocity or a soft
/// limit through in cell units. Oracle: physics.md 10 and 21, a cell
/// world's colliders are given in cell-local units, lengths and speeds
/// alike. At a cell size of 2, a platform moving one and a half cells a
/// second carries its rider 3 units a second, and a soft limit of 0.01
/// cells nudges a body deep inside it by 0.02 units.
#[wasm_bindgen_test(unsupported = test)]
fn a_cell_world_scales_velocity_and_soft_limit_by_its_cell() {
    let platform_at = |x: f32| {
        CellWorld::new(
            2.0,
            move |cell: IVec2, out: &mut CellColliders<'_, Vec2>| {
                if cell == IVec2::ZERO {
                    out.push(
                        Collider::new(7, cuboid(Vec2::new(0.4, 0.1)), Vec2::new(x, 0.5))
                            .with_velocity(Vec2::new(1.5, 0.0)),
                    );
                }
            },
        )
    };
    // The platform's top is at y = 1.2, its centre 1 + 2x across.
    let mut state = settled(1.0, 1.2, 0.0, &platform_at(0.5));
    let mut x = 0.5;
    for _ in 0..10 {
        let before = state.position;
        x += 1.5 * DT;
        fall(&mut state);
        let contacts = step(&mut state, &platform_at(x));
        assert_eq!(contacts.carried_by, Some(7));
        let carried = state.position.x - before.x;
        assert!((carried - 3.0 * DT).abs() <= ROUND, "{carried}");
    }

    let soft = CellWorld::new(2.0, |cell: IVec2, out: &mut CellColliders<'_, Vec2>| {
        if cell == IVec2::ZERO {
            out.push(
                Collider::new(8, cuboid(Vec2::splat(0.25)), Vec2::new(0.5, 0.5))
                    .with_push(Push::Soft(0.01)),
            );
        }
    });
    let mut state = MoverState {
        position: Vec2::new(1.5, 1.0),
        ..MoverState::default()
    };
    let contacts = step(&mut state, &soft);
    assert_eq!(contacts.pushed_by, Some(8));
    assert!(
        (state.position.x - 1.52).abs() <= ROUND,
        "{}",
        state.position.x
    );
}

/// Defect: finding a moving floor by asking a still voxel world about a
/// region far larger than the step asked about before, so a large body
/// standing still gets `Unanswered` where it was answered. Oracle:
/// physics.md 10 and 21, a cell world walks at most 4096 cells and the
/// first search for a floor is the ground probe's own query. A body 10
/// cells across standing on a floor of one-unit voxels asks about some
/// 1300 cells a query; three times its size each way would be some 30 000.
/// Every step is answered and the body stays on the floor.
#[wasm_bindgen_test(unsupported = test)]
fn a_large_body_on_a_still_voxel_floor_is_answered() {
    let voxels = CellWorld::new(1.0, |cell: IVec3, out: &mut CellColliders<'_, Vec3>| {
        if cell.y == -1 {
            out.push(Collider::new(1, cuboid(Vec3::splat(0.5)), Vec3::splat(0.5)));
        }
    });
    let body = cuboid(Vec3::splat(5.0));
    let mut state = MoverState {
        position: Vec3::new(0.5, 5.0 + 0.5 * SKIN, 0.5),
        ..MoverState::default()
    };
    for n in 0..5 {
        fall(&mut state);
        let options = MoverOptions::default();
        let answered = move_and_slide(&mut state, &body, DT, &voxels, &config(), &options);
        assert!(answered.is_ok(), "step {n}");
        assert_eq!(state.floor.map(|f| f.id), Some(1), "step {n}");
    }
}

/// Defect: a collider's velocity or push left out of its digest or its
/// order, so a desync in them goes unseen or two colliders differing only
/// in them are listed as one; or a contact's new fields left out of its
/// digest. Oracle: physics.md 5 and 6. Changing any of them changes the
/// digest, and `overlaps` lists both of two colliders that differ only in
/// one, in the same order however a slice holds them.
#[wasm_bindgen_test(unsupported = test)]
fn velocity_push_and_the_new_contacts_reach_the_digest_and_the_order() {
    let digest = |value: &dyn CanonicalWrite| {
        let mut hasher = StateHasher::new();
        value.canonical_write(&mut hasher);
        hasher.finish().to_hex()
    };
    let base = Collider::new(3, cuboid(Vec3::ONE), Vec3::ZERO);
    let variants = [
        base.with_velocity(Vec3::new(0.0, 0.0, 1.0)),
        base.with_push(Push::Soft(0.0)),
        base.with_push(Push::Soft(0.5)),
    ];
    for variant in &variants {
        assert_ne!(digest(&base), digest(variant), "{variant:?}");
    }
    for pair in [[base, variants[0]], [base, variants[1]]] {
        let swapped = [pair[1], pair[0]];
        let body = cuboid(Vec3::splat(0.5));
        let listed = overlaps(&body, Vec3::ZERO, &pair[..], ALL_LAYERS).unwrap();
        let again = overlaps(&body, Vec3::ZERO, &swapped[..], ALL_LAYERS).unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed, again);
    }
    let contacts = Contacts::<Vec3>::default();
    for changed in [
        Contacts {
            crushed: true,
            ..contacts
        },
        Contacts {
            carried_by: Some(0),
            ..contacts
        },
        Contacts {
            pushed_by: Some(0),
            ..contacts
        },
    ] {
        assert_ne!(digest(&contacts), digest(&changed));
    }
}

/// Defect: a soft push whose limit is negative or not finite accepted, so
/// the solver pulls a body into a collider. Oracle: physics.md 21, an
/// authoring error.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn with_push_refuses_a_negative_or_non_finite_limit() {
    for bad in [-0.01, f32::NAN, f32::INFINITY] {
        let built = std::panic::catch_unwind(|| {
            Collider::new(1, cuboid(Vec2::ONE), Vec2::ZERO).with_push(Push::Soft(bad))
        });
        assert!(built.is_err(), "accepted {bad}");
    }
}

// ---------------------------------------------------------------- properties

/// A random box on a quarter-unit grid within `extent` of the origin.
fn random_box<V: Vector>(rng: &mut Rng, id: u64, extent: i32) -> Collider<V> {
    let mut half = V::ZERO;
    let mut at = V::ZERO;
    for axis in 0..V::AXES {
        half[axis] = rng.grid(1, 6, 0.25);
        at[axis] = rng.grid(-extent, extent, 0.25);
    }
    Collider::new(id, cuboid(half), at)
}

/// A random world around a body: a floor, still boxes, rigid boxes moving
/// at the body from a few units away along one axis each, and soft boxes;
/// and a start no rigid box holds, or `None` when none was found.
fn random_world<V: Vector>(rng: &mut Rng) -> Option<(V, Vec<Collider<V>>)> {
    let body_half = half::<V>();
    let mut colliders = vec![floor::<V>(1)];
    for id in 2..6 {
        colliders.push(random_box(rng, id, 12));
    }
    let start = (0..20)
        .map(|_| {
            let mut at = rng.vector::<V>(-2.0, 2.0);
            at[1] = rng.range(0.6, 2.0);
            at
        })
        .find(|&at| colliders.iter().all(|c| depth(at, body_half, c) <= 0.0))?;
    for id in 10..14 {
        let axis = rng.below(V::AXES);
        let sign = if rng.chance(50) { 1.0 } else { -1.0 };
        let mut collider: Collider<V> = random_box(rng, id, 4);
        collider.position[axis] = start[axis] - sign * rng.range(1.5, 3.5);
        collider.velocity[axis] = sign * rng.range(1.0, 6.0);
        colliders.push(collider);
    }
    for id in 20..22 {
        colliders.push(
            random_box(rng, id, 4)
                .with_velocity(rng.vector(-2.0, 2.0))
                .with_push(Push::Soft(0.05)),
        );
    }
    let clear = colliders
        .iter()
        .all(|c| c.push != Push::Rigid || depth(start, body_half, c) <= 0.0);
    clear.then_some((start, colliders))
}

/// Defect: a moving collider, a carry, a push or a soft nudge that leaves a
/// body inside a solid. Oracle: the shapes' own definitions at their
/// positions after the move, in `f64`, no crate query (physics.md 21 and
/// 13). Random worlds of still boxes, rigid boxes moving at up to 6 units a
/// second (turning now and then) and soft boxes, with a body walking and
/// jumping among them: after every step the body is inside no rigid solid
/// by more than rounding, unless the step reports it `crushed` and the
/// solid is one it was already inside when the step began (a collider that
/// moved into it and holds it against something else). A crushed body is
/// inside nothing still.
fn never_inside_a_solid<V: Vector>(seed: u64) {
    let mut rng = Rng::new(seed);
    let mut crushed = 0;
    let mut pushed = 0;
    for world in 0..24 {
        let Some((start, mut colliders)) = random_world::<V>(&mut rng) else {
            continue;
        };
        let rigid: Vec<usize> = (0..colliders.len())
            .filter(|&i| colliders[i].push == Push::Rigid)
            .collect();
        let mut state = MoverState {
            position: start,
            ..MoverState::default()
        };
        for n in 0..40 {
            for c in &mut colliders {
                if c.velocity != V::ZERO && c.push == Push::Rigid && rng.chance(3) {
                    c.velocity = -c.velocity;
                }
                c.position = c.position + c.velocity * DT;
            }
            let inside_before: Vec<u64> = rigid
                .iter()
                .map(|&i| &colliders[i])
                .filter(|c| inside(state.position, c) > 0.0)
                .map(|c| c.id)
                .collect();
            state.velocity[0] = rng.range(-4.0, 4.0);
            if V::AXES == 3 {
                state.velocity[2] = rng.range(-4.0, 4.0);
            }
            if state.floor.is_some() && rng.chance(10) {
                state.velocity[1] = 7.0;
            }
            fall(&mut state);
            let mut set = ColliderSet::new(2.0);
            for c in &colliders {
                set.insert(*c);
            }
            let contacts = step(&mut state, &set);
            crushed += u32::from(contacts.crushed);
            pushed += u32::from(contacts.pushed_out);
            for &i in &rigid {
                let solid = &colliders[i];
                let deep = inside(state.position, solid);
                if deep > 0.0 {
                    let held = contacts.crushed
                        && solid.velocity != V::ZERO
                        && inside_before.contains(&solid.id);
                    assert!(
                        held,
                        "world {world} step {n}: inside {} by {deep}\n{state:?}\n{contacts:?}\n{colliders:#?}",
                        solid.id
                    );
                }
            }
        }
    }
    // 88 crushed and 115 pushed in 2D, 30 and 112 in 3D.
    assert!(
        crushed > 10 && pushed > 50,
        "crushed {crushed}, pushed {pushed}"
    );
}

#[wasm_bindgen_test(unsupported = test)]
fn never_inside_a_solid_with_moving_colliders_in_2d() {
    never_inside_a_solid::<Vec2>(0x5eed_0920_0002);
}

#[wasm_bindgen_test(unsupported = test)]
fn never_inside_a_solid_with_moving_colliders_in_3d() {
    never_inside_a_solid::<Vec3>(0x5eed_0920_0003);
}

// ------------------------------------------------------------------- replay

/// Defect: a step that reads anything but its arguments and the world, so
/// a client restoring a saved body and the moving colliders' saved places
/// and velocities replays differently. Oracle: physics.md 2 and 21, the
/// forward run itself. The game keeps the scene's colliders and moves each
/// by its own velocity every step, turning the lift, the door and the
/// wanderer at fixed steps; its two bodies are run 120 steps. At step 60
/// the bodies' `MoverState`s and every collider (positions and velocities
/// included) go through postcard, the set is rebuilt from them in a
/// shuffled order, and the replay of steps 60 to 120 from what was restored
/// is bit-equal to the forward run.
fn a_restored_scene_replays_bit_equal<V>()
where
    V: Vector + serde::Serialize + serde::de::DeserializeOwned,
{
    let advance_all = |colliders: &mut Vec<Collider<V>>, n: u32| {
        for c in colliders.iter_mut() {
            let period = match c.id {
                10 => 40,
                11 => 30,
                12 => 25,
                _ => continue,
            };
            if n.is_multiple_of(period) {
                c.velocity = -c.velocity;
            }
            c.position = c.position + c.velocity * DT;
        }
    };
    let run_from = |mut states: [MoverState<V>; 2], mut colliders: Vec<Collider<V>>, from: u32| {
        let mut digests = Vec::new();
        let mut saved = None;
        for n in from..120 {
            if n == 60 {
                saved = Some((
                    postcard::to_allocvec(&states).unwrap(),
                    postcard::to_allocvec(&colliders).unwrap(),
                ));
            }
            advance_all(&mut colliders, n);
            let mut set = ColliderSet::new(1.5);
            for c in &colliders {
                set.insert(*c);
            }
            let mut contacts = Vec::new();
            for (index, state) in states.iter_mut().enumerate() {
                input(index, n, state);
                contacts.push(step(state, &set));
            }
            digests.push(step_digest(&states, &contacts));
        }
        (digests, saved)
    };
    let start = [
        settled(-3.0, 1.25, 0.0, &scene::<V>(0)[..]),
        settled(0.0, 0.0, 0.0, &scene::<V>(0)[..]),
    ];
    let (forward, saved) = run_from(start, scene::<V>(0), 1);
    let (states, colliders) = saved.expect("saved at step 60");
    let mut restored: Vec<Collider<V>> = postcard::from_bytes(&colliders).unwrap();
    Rng::new(7).shuffle(&mut restored);
    let states: [MoverState<V>; 2] = postcard::from_bytes(&states).unwrap();
    let (replayed, _) = run_from(states, restored, 60);
    assert_eq!(replayed, forward[59..]);
}

#[wasm_bindgen_test(unsupported = test)]
fn a_restored_scene_replays_bit_equal_in_2d() {
    a_restored_scene_replays_bit_equal::<Vec2>();
}

#[wasm_bindgen_test(unsupported = test)]
fn a_restored_scene_replays_bit_equal_in_3d() {
    a_restored_scene_replays_bit_equal::<Vec3>();
}
