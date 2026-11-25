//! The mover (physics.md, "Mover"): `move_and_slide`, `move_and_collide`
//! and `find_ledge`, each in 2D and 3D. Every expectation comes from the
//! requirement or from geometry worked by hand in the comment beside it,
//! never from the code under test. Runs natively and in a wasm runtime.
#![allow(clippy::cast_precision_loss)]

use sgl_core::math::{IVec2, Vec2, Vec3};
use sgl_core::{CanonicalWrite, SplitMix64, StateHasher};
use sgl_physics::{
    CellColliders, CellWorld, Collider, ColliderKind, Contacts, Cut, Floor, MoverConfig,
    MoverOptions, MoverSettings, MoverState, Shape, Unanswered, Vector, find_ledge,
    move_and_collide, move_and_slide,
};
use wasm_bindgen_test::wasm_bindgen_test;

const SKIN: f32 = 0.01;

/// The two spaces the scenes are built in: x across, y up, and in 3D a z
/// every collider spans.
trait Space: Vector {
    fn at(x: f32, y: f32) -> Self;
    /// Half-extents of a collider, deep along z in 3D.
    fn deep(hx: f32, hy: f32) -> Self;
    /// Half-extents of a body, as wide along z as along x in 3D.
    fn body(hx: f32, hy: f32) -> Self;
}

impl Space for Vec2 {
    fn at(x: f32, y: f32) -> Self {
        Vec2::new(x, y)
    }
    fn deep(hx: f32, hy: f32) -> Self {
        Vec2::new(hx, hy)
    }
    fn body(hx: f32, hy: f32) -> Self {
        Vec2::new(hx, hy)
    }
}

impl Space for Vec3 {
    fn at(x: f32, y: f32) -> Self {
        Vec3::new(x, y, 0.0)
    }
    fn deep(hx: f32, hy: f32) -> Self {
        Vec3::new(hx, hy, 20.0)
    }
    fn body(hx: f32, hy: f32) -> Self {
        Vec3::new(hx, hy, hx)
    }
}

fn config<V: Space>() -> MoverConfig<V> {
    MoverConfig::new(MoverSettings {
        up: V::at(0.0, 1.0),
        skin: SKIN,
        snap_distance: 0.1,
        normal_threshold: 0.7,
        block_epsilon: 1e-4,
    })
}

/// A solid box from its lower and upper corners in x and y.
fn block<V: Space>(id: u64, (x0, y0): (f32, f32), (x1, y1): (f32, f32)) -> Collider<V> {
    Collider::new(
        id,
        Shape::Box {
            half: V::deep((x1 - x0) * 0.5, (y1 - y0) * 0.5),
        },
        V::at((x0 + x1) * 0.5, (y0 + y1) * 0.5),
    )
}

/// The body: 0.8 wide and 1 tall.
fn half<V: Space>() -> V {
    V::body(0.4, 0.5)
}

fn body<V: Space>() -> Shape<V> {
    Shape::Box { half: half() }
}

/// A body standing at rest on a floor whose top is at y = 0: a skin above
/// it.
fn standing<V: Space>(x: f32, floor: u64) -> MoverState<V> {
    MoverState {
        position: V::at(x, 0.5 + SKIN),
        velocity: V::ZERO,
        floor: Some(Floor {
            normal: V::at(0.0, 1.0),
            id: floor,
        }),
    }
}

fn step<V: Space>(
    state: &mut MoverState<V>,
    world: &[Collider<V>],
    dt: f32,
    options: &MoverOptions,
) -> Contacts<V> {
    move_and_slide(state, &body(), dt, world, &config(), options).expect("a slice always answers")
}

/// Whether two boxes share interior: plain geometry, no crate code.
fn overlap<V: Vector>(a: V, a_half: V, b: V, b_half: V) -> bool {
    (0..V::AXES).all(|axis| (a[axis] - b[axis]).abs() < a_half[axis] + b_half[axis])
}

/// Whether the body at `at` is inside any solid of `world`.
fn inside_any<V: Space>(at: V, world: &[Collider<V>]) -> bool {
    world.iter().any(|c| {
        c.kind == ColliderKind::Solid && overlap(at, half(), c.position, c.shape.half_extents())
    })
}

fn close(value: f32, expected: f32, tolerance: f32) -> bool {
    (value - expected).abs() <= tolerance
}

/// Runs a scene's 2D and 3D versions.
fn both(two: fn(&str), three: fn(&str)) {
    two("2D");
    three("3D");
}

// ------------------------------------------------------------ velocity

/// A body falling at (1, -10) for half a second onto a floor whose top is at
/// y = 0. Oracle: the requirement (landing zeroes the falling part and
/// keeps the part across, and sets `floor` to that floor) and geometry (it
/// rests a skin above the top).
fn landing<V: Space>(name: &str) {
    let world = [block::<V>(1, (-10.0, -1.0), (10.0, 0.0))];
    let mut state = MoverState {
        position: V::at(0.0, 2.0),
        velocity: V::at(1.0, -10.0),
        floor: None,
    };
    let contacts = step(&mut state, &world, 0.5, &MoverOptions::default());
    assert!(contacts.floor, "{name}");
    assert!(
        close(state.position[1], 0.5 + SKIN, 1e-4),
        "{name}: {state:?}"
    );
    assert_eq!(state.velocity, V::at(1.0, 0.0), "{name}");
    assert_eq!(
        state.floor,
        Some(Floor {
            normal: V::at(0.0, 1.0),
            id: 1
        }),
        "{name}"
    );
}

#[wasm_bindgen_test(unsupported = test)]
fn landing_zeroes_the_fall_and_sets_the_floor() {
    both(landing::<Vec2>, landing::<Vec3>);
}

/// A body rising at (1, 10) into a ceiling whose underside is at y = 3.
/// Oracle: the requirement (the rising part goes, the part across stays,
/// no floor) and geometry (its top stops a skin below the ceiling).
fn head_bump<V: Space>(name: &str) {
    let world = [block::<V>(1, (-10.0, 3.0), (10.0, 4.0))];
    let mut state = MoverState {
        position: V::at(0.0, 1.0),
        velocity: V::at(1.0, 10.0),
        floor: None,
    };
    let contacts = step(&mut state, &world, 0.5, &MoverOptions::default());
    assert!(contacts.ceiling && contacts.up_blocked, "{name}");
    assert!(
        close(state.position[1], 2.5 - SKIN, 1e-4),
        "{name}: {state:?}"
    );
    assert_eq!(state.velocity, V::at(1.0, 0.0), "{name}");
    assert_eq!(state.floor, None, "{name}");
}

#[wasm_bindgen_test(unsupported = test)]
fn a_head_bump_zeroes_the_rise() {
    both(head_bump::<Vec2>, head_bump::<Vec3>);
}

/// A body in the air moving at (4, 1) into a wall whose face is at x = 2.
/// Oracle: the requirement (the part into the wall goes, the part along it
/// stays) and geometry (its right edge stops a skin short of the face).
fn wall_slide<V: Space>(name: &str) {
    let world = [block::<V>(1, (2.0, -10.0), (3.0, 10.0))];
    let mut state = MoverState {
        position: V::ZERO,
        velocity: V::at(4.0, 1.0),
        floor: None,
    };
    let contacts = step(&mut state, &world, 0.5, &MoverOptions::default());
    assert!(contacts.wall, "{name}");
    assert!(
        close(state.position[0], 1.6 - SKIN, 1e-4),
        "{name}: {state:?}"
    );
    assert_eq!(state.velocity, V::at(0.0, 1.0), "{name}");
}

#[wasm_bindgen_test(unsupported = test)]
fn a_wall_slide_keeps_the_part_along_the_wall() {
    both(wall_slide::<Vec2>, wall_slide::<Vec3>);
}

/// In 3D the part along a wall has two directions: moving at (4, 1, 3)
/// into the wall at x = 2 keeps (0, 1, 3). Oracle: the requirement, and
/// geometry for the position: the half-second motion (2, 0.5, 1.5) meets
/// the wall after 0.8 of it and backs off 0.01 / 2 of it; the slide keeps
/// the part along the wall of everything not applied, so z reaches the
/// whole 1.5.
#[wasm_bindgen_test(unsupported = test)]
fn a_3d_wall_slide_keeps_both_directions_along_the_wall() {
    let world = [block::<Vec3>(1, (2.0, -10.0), (3.0, 10.0))];
    let mut state = MoverState {
        position: Vec3::ZERO,
        velocity: Vec3::new(4.0, 1.0, 3.0),
        floor: None,
    };
    step(&mut state, &world, 0.5, &MoverOptions::default());
    assert_eq!(state.velocity, Vec3::new(0.0, 1.0, 3.0));
    assert!(close(state.position.z, 1.5, 1e-5), "{state:?}");
}

// ------------------------------------------------------------ step-up

fn climbing(height: f32) -> MoverOptions {
    MoverOptions {
        step_height: height,
        ..MoverOptions::default()
    }
}

/// A body standing on a floor at y = 0 walks 1 unit (2 per second for half
/// a second) toward a ledge whose face is at x = 1. With a step height of
/// 0.3 it climbs a ledge 0.3 high, standing a skin above it at x = 1; one
/// 0.36 high (above the step by more than the skin it lifts clear by) stops
/// it a skin short of the face, on the floor. Oracle: the requirement.
fn step_height<V: Space>(name: &str) {
    for (height, climbs) in [(0.3, true), (0.36, false)] {
        let world = [
            block::<V>(1, (-10.0, -1.0), (10.0, 0.0)),
            block::<V>(2, (1.0, -1.0), (10.0, height)),
        ];
        let mut state = standing::<V>(0.0, 1);
        state.velocity = V::at(2.0, 0.0);
        let contacts = step(&mut state, &world, 0.5, &climbing(0.3));
        assert_eq!(contacts.stepped, climbs, "{name} {height}: {contacts:?}");
        if climbs {
            assert!(close(state.position[0], 1.0, 1e-5), "{name}: {state:?}");
            let rest = height + 0.5 + SKIN;
            assert!(close(state.position[1], rest, 1e-4), "{name}: {state:?}");
            assert_eq!(state.floor.map(|f| f.id), Some(2), "{name}");
        } else {
            assert!(
                close(state.position[0], 0.6 - SKIN, 1e-4),
                "{name}: {state:?}"
            );
            assert!(
                close(state.position[1], 0.5 + SKIN, 1e-4),
                "{name}: {state:?}"
            );
        }
    }
}

#[wasm_bindgen_test(unsupported = test)]
fn a_ledge_at_the_step_height_is_climbed_and_one_above_it_is_not() {
    both(step_height::<Vec2>, step_height::<Vec3>);
}

/// Two steps that cannot be climbed: a 0.3 ledge under a ceiling 0.1 above
/// the body's head (the lift is cut short), and a 0.3 fence with nothing
/// beyond it (the climb would land on no floor). Oracle: the requirement
/// says the plain move's result stands, so the step with a step height of
/// 0.3 must equal the same step with none.
fn step_refused<V: Space>(name: &str) {
    let ceiling = [
        block::<V>(1, (-10.0, -1.0), (10.0, 0.0)),
        block::<V>(2, (1.0, -1.0), (10.0, 0.3)),
        block::<V>(3, (-10.0, 1.11), (10.0, 2.0)),
    ];
    let fence = [
        block::<V>(1, (-10.0, -1.0), (1.0, 0.0)),
        block::<V>(2, (1.0, -1.0), (1.05, 0.3)),
    ];
    for (scene, world, speed) in [("ceiling", &ceiling[..], 2.0), ("fence", &fence[..], 4.0)] {
        let mut start = standing::<V>(0.0, 1);
        start.velocity = V::at(speed, 0.0);
        let mut climbing_state = start;
        let contacts = step(&mut climbing_state, world, 0.5, &climbing(0.3));
        let mut plain = start;
        step(&mut plain, world, 0.5, &MoverOptions::default());
        assert!(!contacts.stepped, "{name} {scene}");
        assert_eq!(climbing_state, plain, "{name} {scene}");
        assert!(close(plain.position[0], 0.6 - SKIN, 1e-4), "{name} {scene}");
    }
}

#[wasm_bindgen_test(unsupported = test)]
fn a_step_under_a_low_ceiling_or_onto_no_floor_leaves_the_plain_move() {
    both(step_refused::<Vec2>, step_refused::<Vec3>);
}

/// Seeded scenes of a floor, a ledge of random height, a random ceiling
/// and a random gap beyond the ledge, walked into at random speeds with a
/// step height of 0.3. Oracle: geometry (#914's): no step, climbed or
/// refused, ends inside a solid. Both outcomes must occur, so the property
/// covers each.
fn steps_stay_clear<V: Space>(name: &str) {
    let mut rng = SplitMix64::new(0x0916);
    let mut unit = || (rng.next_u64() >> 40) as f32 / 16_777_216.0;
    let (mut climbed, mut refused) = (0, 0);
    for _ in 0..120 {
        let height = unit() * 0.6;
        let width = 0.05 + unit() * 2.0;
        let ceiling = 1.02 + unit() * 1.5;
        let world = [
            block::<V>(1, (-10.0, -1.0), (1.0, 0.0)),
            block::<V>(2, (1.0, -1.0), (1.0 + width, height)),
            block::<V>(3, (-10.0, ceiling), (10.0, ceiling + 1.0)),
            block::<V>(4, (1.0 + width + unit(), -2.0), (10.0, -0.2)),
        ];
        let mut state = standing::<V>(unit() * 0.5, 1);
        state.velocity = V::at(1.0 + unit() * 5.0, -unit());
        let contacts = step(&mut state, &world, 0.5, &climbing(0.3));
        assert!(!inside_any(state.position, &world), "{name}: {state:?}");
        if contacts.stepped {
            climbed += 1;
        } else if contacts.wall {
            refused += 1;
        }
    }
    assert!(climbed > 10 && refused > 10, "{name}: {climbed} {refused}");
}

#[wasm_bindgen_test(unsupported = test)]
fn no_step_up_ends_inside_a_solid() {
    both(steps_stay_clear::<Vec2>, steps_stay_clear::<Vec3>);
}

// ------------------------------------------------------------ ledge guard

fn guarded(drop: f32) -> MoverOptions {
    MoverOptions {
        ledge_drop: Some(drop),
        ..MoverOptions::default()
    }
}

/// A body standing at x = 4 on a platform that ends at x = 5, over a deep
/// pit, walks 3 units right. Guarded, it stops still over the platform with
/// its trailing (left) edge within a skin of the platform's edge, and stays
/// on the floor; unguarded, it walks off to x = 7 with no floor. Oracle:
/// the requirement and geometry.
fn guard<V: Space>(name: &str) {
    let world = [block::<V>(1, (-10.0, -5.0), (5.0, 0.0))];
    let mut kept = standing::<V>(4.0, 1);
    kept.velocity = V::at(3.0, 0.0);
    let mut free = kept;
    let contacts = step(&mut kept, &world, 1.0, &guarded(1.0));
    let trailing = kept.position[0] - 0.4;
    assert!(contacts.ledge_guarded, "{name}");
    assert!((5.0 - SKIN..5.0).contains(&trailing), "{name}: {kept:?}");
    assert_eq!(kept.floor.map(|f| f.id), Some(1), "{name}");

    let contacts = step(&mut free, &world, 1.0, &MoverOptions::default());
    assert!(!contacts.ledge_guarded, "{name}");
    assert_eq!(free.position[0].to_bits(), 7.0f32.to_bits(), "{name}");
    assert_eq!(free.floor, None, "{name}");
}

#[wasm_bindgen_test(unsupported = test)]
fn a_guarded_walk_off_a_platform_stops_supported_and_an_unguarded_one_leaves() {
    both(guard::<Vec2>, guard::<Vec3>);
}

/// Walking diagonally by (3, 0, 2) off the x = 5 edge of a platform that
/// runs on along z: the guard stops the motion across the edge and keeps
/// all of the motion along it. Oracle: the requirement. (In 2D there is no
/// direction along an edge.)
#[wasm_bindgen_test(unsupported = test)]
fn a_guarded_diagonal_walk_keeps_the_motion_along_the_edge() {
    let world = [block::<Vec3>(1, (-10.0, -5.0), (5.0, 0.0))];
    let mut state = standing::<Vec3>(4.0, 1);
    state.velocity = Vec3::new(3.0, 0.0, 2.0);
    let contacts = step(&mut state, &world, 1.0, &guarded(1.0));
    assert!(contacts.ledge_guarded);
    let trailing = state.position.x - 0.4;
    assert!((5.0 - SKIN..5.0).contains(&trailing), "{state:?}");
    assert!(close(state.position.z, 2.0, 1e-6), "{state:?}");
}

/// Off the same edge onto a lower floor: with a guard of 0.5, a drop of 0.3
/// is taken (the body walks the whole 3 units) and a drop of 0.8 is not.
/// Oracle: the requirement.
fn drop_taken<V: Space>(name: &str) {
    for (depth, taken) in [(0.3, true), (0.8, false)] {
        let world = [
            block::<V>(1, (-10.0, -5.0), (5.0, 0.0)),
            block::<V>(2, (5.0, -5.0), (20.0, -depth)),
        ];
        let mut state = standing::<V>(4.0, 1);
        state.velocity = V::at(3.0, 0.0);
        let contacts = step(&mut state, &world, 1.0, &guarded(0.5));
        assert_eq!(contacts.ledge_guarded, !taken, "{name} {depth}");
        if taken {
            assert_eq!(state.position[0].to_bits(), 7.0f32.to_bits(), "{name}");
        } else {
            assert!(state.position[0] - 0.4 < 5.0, "{name}: {state:?}");
        }
    }
}

#[wasm_bindgen_test(unsupported = test)]
fn a_drop_no_deeper_than_the_ledge_drop_is_taken() {
    both(drop_taken::<Vec2>, drop_taken::<Vec3>);
}

// ------------------------------------------------------------ one-way

/// A solid floor at y = 0 and a one-way deck whose top is at y = 2. Rising
/// from under the deck passes through it; falling from above lands on it;
/// standing on it with `drop_through` falls through to the floor; standing
/// on the solid floor with `drop_through` stays. Oracle: the requirement
/// and geometry.
fn one_way<V: Space>(name: &str) {
    let world = [
        block::<V>(1, (-10.0, -1.0), (10.0, 0.0)),
        block::<V>(2, (-10.0, 1.75), (10.0, 2.0)).with_kind(ColliderKind::OneWay),
    ];
    let dropping = MoverOptions {
        drop_through: true,
        ..MoverOptions::default()
    };
    let mut rising = MoverState {
        position: V::at(0.0, 1.0),
        velocity: V::at(0.0, 10.0),
        floor: None,
    };
    let contacts = step(&mut rising, &world, 0.5, &MoverOptions::default());
    assert!(!contacts.ceiling, "{name}");
    assert_eq!(rising.position, V::at(0.0, 6.0), "{name}");

    let mut falling = MoverState {
        position: V::at(0.0, 4.0),
        velocity: V::at(0.0, -10.0),
        floor: None,
    };
    step(&mut falling, &world, 0.5, &MoverOptions::default());
    assert!(close(falling.position[1], 2.5 + SKIN, 1e-4), "{name}");
    assert_eq!(falling.floor.map(|f| f.id), Some(2), "{name}");

    falling.velocity = V::at(0.0, -10.0);
    step(&mut falling, &world, 0.5, &dropping);
    assert!(close(falling.position[1], 0.5 + SKIN, 1e-4), "{name}");
    assert_eq!(falling.floor.map(|f| f.id), Some(1), "{name}");

    falling.velocity = V::at(0.0, -10.0);
    step(&mut falling, &world, 0.5, &dropping);
    assert!(close(falling.position[1], 0.5 + SKIN, 1e-4), "{name}");
    assert_eq!(falling.floor.map(|f| f.id), Some(1), "{name}");
}

#[wasm_bindgen_test(unsupported = test)]
fn one_way_platforms_pass_risers_land_fallers_and_drop_on_request() {
    both(one_way::<Vec2>, one_way::<Vec3>);
}

// ------------------------------------------------------------ push-out

/// A crate 0.4 wide spanning y 0.8 to 1.5 appears in the head of a body
/// standing on a floor. Down is the floor, up is 1.5 away (more than the
/// body's height), and either side is 0.2 + 0.4 + skin = 0.61 away. Oracle:
/// geometry (#914's: one step later it overlaps nothing) and the
/// requirement (it moved no further than its own size on any axis, and by
/// the shortest way, 0.61).
fn push_out<V: Space>(name: &str) {
    let world = [
        block::<V>(1, (-10.0, -1.0), (10.0, 0.0)),
        block::<V>(2, (-0.2, 0.8), (0.2, 1.5)),
    ];
    let start = standing::<V>(0.0, 1);
    let mut state = start;
    let contacts = step(&mut state, &world, 1.0 / 60.0, &MoverOptions::default());
    assert!(contacts.pushed_out && !contacts.stuck, "{name}");
    assert!(!inside_any(state.position, &world), "{name}: {state:?}");
    let moved = state.position - start.position;
    let size = half::<V>() * 2.0;
    let mut length = 0.0;
    for axis in 0..V::AXES {
        assert!(moved[axis].abs() <= size[axis], "{name}: {moved:?}");
        length += moved[axis] * moved[axis];
    }
    assert!(close(length.sqrt(), 0.6 + SKIN, 1e-4), "{name}: {moved:?}");
}

#[wasm_bindgen_test(unsupported = test)]
fn a_collider_placed_over_a_body_frees_it_within_its_size() {
    both(push_out::<Vec2>, push_out::<Vec3>);
}

/// Bodies falling and running across a ramp a thousand units out, with no
/// skin, land on its slope and run along it, ending each move touching it:
/// a rounding step of the body's own coordinates can put such a body a hair
/// inside the ramp when taken relative to it. That is touching, and no step
/// pushes one out. Oracle: the requirement (only a body inside a solid is
/// pushed out) and the #914 contract (a body a move leaves touching a face
/// is not inside it).
#[wasm_bindgen_test(unsupported = test)]
fn a_body_resting_on_a_ramp_far_out_is_not_pushed_out() {
    let config = MoverConfig::new(MoverSettings {
        up: Vec2::Y,
        skin: 0.0,
        snap_distance: 0.1,
        normal_threshold: 0.7,
        block_epsilon: 1e-4,
    });
    let shape = Shape::Box {
        half: half::<Vec2>(),
    };
    for i in 0..16u8 {
        let x = 1000.0 + f32::from(i) * 0.0137;
        let ramp = Collider::new(
            1,
            Shape::Box {
                half: Vec2::splat(2.0),
            },
            Vec2::new(x + 0.3, 500.7),
        )
        .with_cut(Cut::new(Vec2::new(-1.0, 2.0), 0.0));
        let world = [ramp];
        let mut state = MoverState {
            position: Vec2::new(x, 503.5),
            ..MoverState::default()
        };
        for run in -10..20i8 {
            state.velocity = Vec2::new(0.8 * f32::from(run), -18.0);
            let contacts = move_and_slide(
                &mut state,
                &shape,
                1.0 / 60.0,
                &world[..],
                &config,
                &MoverOptions::default(),
            )
            .unwrap();
            assert!(!contacts.pushed_out, "{i} {run}: {state:?}");
        }
        assert!(state.floor.is_some(), "{i}: {state:?}");
    }
}

/// A body inside a box ten units across cannot get out within its own
/// size. Oracle: the requirement: it reports `stuck` and its state does not
/// change at all.
fn enclosed<V: Space>(name: &str) {
    let world = [block::<V>(1, (-5.0, -5.0), (5.0, 5.0))];
    let start = MoverState {
        position: V::ZERO,
        velocity: V::at(1.0, -1.0),
        floor: None,
    };
    let mut state = start;
    let contacts = step(&mut state, &world, 0.5, &MoverOptions::default());
    assert!(contacts.stuck && !contacts.pushed_out, "{name}");
    assert_eq!(state, start, "{name}");
}

#[wasm_bindgen_test(unsupported = test)]
fn a_body_enclosed_on_all_sides_is_stuck_and_does_not_move() {
    both(enclosed::<Vec2>, enclosed::<Vec3>);
}

// ------------------------------------------------------------ move_and_collide

/// A body at the origin moving by (5, 1) toward a wall whose face is at
/// x = 2 touches it after (2 - 0.4) / 5 = 0.32 of the motion and stops a
/// skin short along its path, at 0.32 - 0.01 / 5 = 0.318 of it: (1.59,
/// 0.318). Away from the wall it moves the whole motion; from where it
/// stopped, moving along the face is not blocked. Oracle: geometry.
fn collide<V: Space>(name: &str) {
    let world = [block::<V>(7, (2.0, -10.0), (3.0, 10.0))];
    let config = config::<V>();
    let shape = body::<V>();
    let (moved, hit) = move_and_collide(
        &shape,
        V::ZERO,
        V::at(5.0, 1.0),
        &world[..],
        u32::MAX,
        &config,
    )
    .unwrap();
    let hit = hit.expect("the wall is in the way");
    assert_eq!((hit.id, hit.normal), (7, V::at(-1.0, 0.0)), "{name}");
    assert!(
        close(moved[0], 5.0 * (0.32 - 0.002), 1e-5) && close(moved[1], 0.32 - 0.002, 1e-5),
        "{name}: {moved:?}"
    );

    let away = V::at(-3.0, 1.0);
    let clear = move_and_collide(&shape, V::ZERO, away, &world[..], u32::MAX, &config).unwrap();
    assert_eq!(clear, (away, None), "{name}");

    let along = V::at(0.0, 3.0);
    let slid = move_and_collide(&shape, moved, along, &world[..], u32::MAX, &config).unwrap();
    assert_eq!(slid, (along, None), "{name}");
}

#[wasm_bindgen_test(unsupported = test)]
fn move_and_collide_stops_a_skin_short_of_the_first_contact() {
    both(collide::<Vec2>, collide::<Vec3>);
}

// ------------------------------------------------------------ find_ledge

/// A body pressed (a skin away) against a wall whose face is at x = 1,
/// its top at y = 0.5, reaching 0.5. The geometry of a ledge whose top is
/// at `top`: hanging, the body's top is a skin above it beside the wall;
/// standing, its bottom is a skin above it and its trailing edge a skin
/// past the face.
fn ledge_world<V: Space>(top: f32, kind: ColliderKind) -> Vec<Collider<V>> {
    vec![block::<V>(1, (1.0, -5.0), (3.0, top)).with_kind(kind)]
}

fn ledge<V: Space>(world: &[Collider<V>]) -> Option<sgl_physics::Ledge<V>> {
    let pressed = V::at(0.6 - SKIN, 0.0);
    find_ledge(
        &body(),
        pressed,
        V::at(1.0, 0.0),
        world,
        &config(),
        0.5,
        u32::MAX,
    )
    .unwrap()
}

/// A top 0.45 above the body's (inside its reach) is found, with the
/// geometry above; one 0.55 above (beyond it) is not. Oracle: the
/// requirement and geometry.
fn ledge_reach<V: Space>(name: &str) {
    let found = ledge::<V>(&ledge_world(0.95, ColliderKind::Solid)).expect("within reach");
    assert_eq!(found.id, 1, "{name}");
    assert!(close(found.hang[0], 0.6 - SKIN, 1e-5), "{name}: {found:?}");
    assert!(
        close(found.hang[1] + 0.5, 0.95 + SKIN, 1e-4),
        "{name}: {found:?}"
    );
    assert!(
        close(found.stand[0] - 0.4, 1.0 + SKIN, 1e-4),
        "{name}: {found:?}"
    );
    assert!(
        close(found.stand[1] - 0.5, 0.95 + SKIN, 1e-4),
        "{name}: {found:?}"
    );
    assert_eq!(
        ledge::<V>(&ledge_world(1.05, ColliderKind::Solid)),
        None,
        "{name}"
    );
}

#[wasm_bindgen_test(unsupported = test)]
fn find_ledge_finds_a_ledge_at_reach_and_not_one_beyond() {
    both(ledge_reach::<Vec2>, ledge_reach::<Vec3>);
}

/// The same reachable ledge with an overhang from y = 1.5, starting 0.2
/// past the face, over where the body would stand (y 0.96 to 1.96), is
/// refused; and the same ledge as a one-way platform is never a wall to
/// climb. Oracle: the requirement.
fn ledge_refused<V: Space>(name: &str) {
    let mut covered = ledge_world::<V>(0.95, ColliderKind::Solid);
    covered.push(block(2, (1.2, 1.5), (3.0, 3.0)));
    assert_eq!(ledge(&covered), None, "{name}");
    assert_eq!(
        ledge::<V>(&ledge_world(0.95, ColliderKind::OneWay)),
        None,
        "{name}"
    );
}

#[wasm_bindgen_test(unsupported = test)]
fn find_ledge_refuses_an_overhang_and_a_one_way_edge() {
    both(ledge_refused::<Vec2>, ledge_refused::<Vec3>);
}

// ------------------------------------------------------------ review fixes

/// Gravity for the falling scenes, in units per second squared.
const GRAVITY: f32 = 30.0;
const DT: f32 = 1.0 / 60.0;

/// A ramp from its box's lower and upper corners in x and y, cut by the
/// plane through the box's centre of normal `(nx, ny)`.
fn ramp<V: Space>(id: u64, lower: (f32, f32), upper: (f32, f32), nx: f32, ny: f32) -> Collider<V> {
    block::<V>(id, lower, upper).with_cut(Cut::new(V::at(nx, ny), 0.0))
}

fn length<V: Vector>(v: V) -> f32 {
    (0..V::AXES)
        .map(|axis| v[axis] * v[axis])
        .sum::<f32>()
        .sqrt()
}

/// One step of a game holding no input: gravity, then the mover.
fn fall_step<V: Space>(state: &mut MoverState<V>, world: &[Collider<V>]) -> Contacts<V> {
    state.velocity[1] -= GRAVITY * DT;
    step(state, world, DT, &MoverOptions::default())
}

/// A wall whose face is at x = 0 and, beside it, a slope too steep to stand
/// on (its box x 0 to 2 and y 0 to 4, cut along y = 2x: normal (-2, 1)),
/// which leaves a wedge between them. A body dropped into the wedge from
/// x = 0.45 and y = 3 comes to rest in it, wedged against both. Every step
/// gravity adds 0.5 to its fall, and the step must remove the part pointing
/// into what it touched, so once at rest its velocity is never more than
/// one step's gravity; when the wall goes, it starts down the slope from
/// rest, not at a speed it gathered while held. Oracle: the requirement.
fn wedge<V: Space>(name: &str) {
    let wall = block::<V>(1, (-2.0, 0.0), (0.0, 6.0));
    let slope = ramp::<V>(2, (0.0, 0.0), (2.0, 4.0), -2.0, 1.0);
    let mut state = MoverState {
        position: V::at(0.45, 3.0),
        ..MoverState::default()
    };
    let world = [wall, slope];
    let one_step = GRAVITY * DT;
    for frame in 0..300 {
        fall_step(&mut state, &world);
        if frame >= 120 {
            assert!(
                length(state.velocity) <= one_step,
                "{name} {frame}: {state:?}"
            );
        }
    }
    assert!(state.floor.is_none(), "{name}: {state:?}");
    fall_step(&mut state, &[slope]);
    assert!(
        length(state.velocity) <= 2.0 * one_step,
        "{name}: {state:?}"
    );
}

#[wasm_bindgen_test(unsupported = test)]
fn a_body_wedged_between_a_wall_and_a_steep_slope_gathers_no_speed() {
    both(wedge::<Vec2>, wedge::<Vec3>);
}

/// Walls whose faces are at x = 1 (id 1) and z = 1 (id 2), and a body
/// flying into their corner at (6, 0, 6) every step: both walls are met at
/// the same instant. Once pinned in the corner its velocity after each step
/// has no part into either wall. Oracle: the requirement (the step removes
/// the part of the velocity that points into anything touched).
#[wasm_bindgen_test(unsupported = test)]
fn a_body_pinned_in_a_corner_keeps_no_velocity_into_either_wall() {
    let world = [
        block::<Vec3>(1, (1.0, -10.0), (3.0, 10.0)),
        Collider::new(
            2,
            Shape::Box {
                half: Vec3::new(20.0, 10.0, 1.0),
            },
            Vec3::new(0.0, 0.0, 2.0),
        ),
    ];
    let mut state = MoverState::<Vec3>::default();
    for frame in 0..30 {
        state.velocity = Vec3::new(6.0, 0.0, 6.0);
        step(&mut state, &world, DT, &MoverOptions::default());
        if frame >= 10 {
            assert!(
                state.velocity.x <= 0.0 && state.velocity.z <= 0.0,
                "{frame}: {state:?}"
            );
        }
    }
}

/// A body dropped beside a slope too steep to stand on (y = 2x, normal
/// (-2, 1)) lands on it and slides down it under gravity. Oracle: energy:
/// a body that only slides can never be faster than free fall over the
/// height it has lost, `v² <= 2 g h` (the game's step, gravity then the
/// move, keeps that for free fall itself).
fn steep_slide<V: Space>(name: &str) {
    let world = [ramp::<V>(1, (0.0, 0.0), (20.0, 40.0), -2.0, 1.0)];
    let start = V::at(14.0, 30.0);
    let mut state = MoverState {
        position: start,
        ..MoverState::default()
    };
    let mut slid = false;
    for frame in 0..90 {
        let contacts = fall_step(&mut state, &world);
        slid |= contacts.wall;
        let height = start[1] - state.position[1];
        let speed = length(state.velocity);
        assert!(
            speed * speed <= 2.0 * GRAVITY * height * (1.0 + 1e-4) + 1e-6,
            "{name} {frame}: {speed} after {height}: {state:?}"
        );
    }
    assert!(slid, "{name}: never reached the slope");
}

#[wasm_bindgen_test(unsupported = test)]
fn a_body_sliding_down_a_steep_slope_is_never_faster_than_free_fall() {
    both(steep_slide::<Vec2>, steep_slide::<Vec3>);
}

/// A step 0.3 high whose face is at x = 1, and a body standing a skin from
/// it with a step height of 0.5: walked into at 0.1, 0.59 and 2 units a
/// second, at 60 steps a second, it ends standing on the step (a skin above
/// y = 0.3, on its collider) within a second. A slow walk climbs as a fast
/// one does. Oracle: the requirement.
fn slow_climb<V: Space>(name: &str) {
    let world = [
        block::<V>(1, (-10.0, -1.0), (10.0, 0.0)),
        block::<V>(2, (1.0, -1.0), (10.0, 0.3)),
    ];
    for speed in [0.1, 0.59, 2.0] {
        let mut state = standing::<V>(0.6 - SKIN, 1);
        for _ in 0..60 {
            state.velocity = V::at(speed, 0.0);
            step(&mut state, &world, DT, &climbing(0.5));
        }
        assert_eq!(
            state.floor.map(|f| f.id),
            Some(2),
            "{name} {speed}: {state:?}"
        );
        assert!(
            close(state.position[1], 0.3 + 0.5 + SKIN, 1e-4),
            "{name} {speed}: {state:?}"
        );
    }
}

#[wasm_bindgen_test(unsupported = test)]
fn a_slow_walker_climbs_a_step_as_a_fast_one_does() {
    both(slow_climb::<Vec2>, slow_climb::<Vec3>);
}

/// Whether a box at `at` of half-extents `half` shares interior with the
/// solid part of `collider`: the two boxes' common part has room on every
/// axis, and its lowest point along the cut's normal lies below the cut's
/// plane. Plain geometry from the cut's definition (solid where
/// `normal · (p - centre) <= offset`), no crate code.
fn shares_interior<V: Vector>(at: V, half: V, collider: &Collider<V>) -> bool {
    let size = collider.shape.half_extents();
    let mut centre = V::ZERO;
    let mut common = V::ZERO;
    for axis in 0..V::AXES {
        let low = f32::max(at[axis] - half[axis], collider.position[axis] - size[axis]);
        let high = f32::min(at[axis] + half[axis], collider.position[axis] + size[axis]);
        if low >= high {
            return false;
        }
        centre[axis] = (low + high) * 0.5;
        common[axis] = (high - low) * 0.5;
    }
    let Some(cut) = collider.cut else {
        return true;
    };
    let n = cut.normal();
    let lowest = (0..V::AXES).fold(0.0, |sum, axis| {
        sum + n[axis] * (centre[axis] - collider.position[axis]) - (n[axis] * common[axis]).abs()
    });
    lowest < cut.offset()
}

/// A body of half-extents 0.25 at (0.1035, -0.1035) inside a ramp box of
/// half-extent 3 at the origin, cut along y = x (normal (-1, 1) / √2,
/// offset 0). Its deepest corner, (-0.1465, 0.1465) from its centre... lies
/// (0.207 + 0.5) / √2 inside the plane, so the shortest way out moves it
/// along the normal by D = 0.707 / √2 + 0.01: by (-D, D) / √2, 0.36057 on
/// each axis, within its size (0.5). Along x or y alone it would take
/// 0.721, more than its size. Oracle: geometry.
fn ramp_push_out<V: Space>(name: &str) {
    let half = V::body(0.25, 0.25);
    let shape = Shape::Box { half };
    let world = [Collider::new(
        1,
        Shape::Box {
            half: V::deep(3.0, 3.0),
        },
        V::ZERO,
    )
    .with_cut(Cut::new(V::at(-1.0, 1.0), 0.0))];
    let start = V::at(0.1035, -0.1035);
    assert!(shares_interior(start, half, &world[0]), "{name}");
    let mut state = MoverState {
        position: start,
        ..MoverState::default()
    };
    let contacts = move_and_slide(
        &mut state,
        &shape,
        DT,
        &world[..],
        &config(),
        &MoverOptions::default(),
    )
    .unwrap();
    assert!(
        contacts.pushed_out && !contacts.stuck,
        "{name}: {contacts:?}"
    );
    let depth = 0.707 / 2f32.sqrt() + SKIN;
    let along = depth / 2f32.sqrt();
    let moved = state.position - start;
    assert!(close(moved[0], -along, 1e-5), "{name}: {moved:?}");
    assert!(close(moved[1], along, 1e-5), "{name}: {moved:?}");
    assert!(!shares_interior(state.position, half, &world[0]), "{name}");
}

#[wasm_bindgen_test(unsupported = test)]
fn a_body_deep_in_a_ramp_is_pushed_out_along_its_normal() {
    both(ramp_push_out::<Vec2>, ramp_push_out::<Vec3>);
}

/// The same ramp, cut along y = x, and a wall beside it whose face is at
/// x = -0.13, with a body of half-extents 0.25 at (-0.09, 0) inside both.
/// Out of the wall it must move left by 0.16 + 0.13 + 0.01 = 0.3; out of
/// the ramp, along its normal (-1, 1) / √2, by 0.01 + (0.5 - 0.09) / √2,
/// so with x = -0.3 it must rise by √2 times that less 0.3: 0.11 + 0.01·√2.
/// That is the shortest way out of both: along the normal alone it stays
/// in the wall, and along x alone it takes 0.424. Oracle: geometry.
fn ramp_and_wall_push_out<V: Space>(name: &str) {
    let half = V::body(0.25, 0.25);
    let shape = Shape::Box { half };
    let world = [
        Collider::new(
            1,
            Shape::Box {
                half: V::deep(3.0, 3.0),
            },
            V::ZERO,
        )
        .with_cut(Cut::new(V::at(-1.0, 1.0), 0.0)),
        block::<V>(2, (-0.13, -3.0), (3.0, 3.0)),
    ];
    let start = V::at(-0.09, 0.0);
    assert!(
        world.iter().all(|c| shares_interior(start, half, c)),
        "{name}"
    );
    let mut state = MoverState {
        position: start,
        ..MoverState::default()
    };
    let contacts = move_and_slide(
        &mut state,
        &shape,
        DT,
        &world[..],
        &config(),
        &MoverOptions::default(),
    )
    .unwrap();
    assert!(contacts.pushed_out, "{name}: {contacts:?}");
    let moved = state.position - start;
    assert!(close(moved[0], -0.3, 1e-5), "{name}: {moved:?}");
    assert!(
        close(moved[1], 0.11 + SKIN * 2f32.sqrt(), 1e-5),
        "{name}: {moved:?}"
    );
    assert!(
        world
            .iter()
            .all(|c| !shares_interior(state.position, half, c)),
        "{name}"
    );
}

#[wasm_bindgen_test(unsupported = test)]
fn a_body_in_a_ramp_and_a_wall_takes_the_shortest_way_out_of_both() {
    both(
        ramp_and_wall_push_out::<Vec2>,
        ramp_and_wall_push_out::<Vec3>,
    );
}

/// Seeded bodies placed inside ramps of random size, slope and offset.
/// Whenever a step reports `stuck`, no place along any axis or along the
/// ramp's normal, either way, within the body's size on each axis, is free
/// of the ramp with a skin to spare; whenever it pushes the body out, the
/// body ends free. Oracle: the ramp's definition ([`shares_interior`], for
/// the body grown by a skin), which the push-out does not use. Both
/// outcomes must occur, so the property covers each.
fn ramp_stuck<V: Space>(name: &str, normal: fn(&mut dyn FnMut() -> f32) -> V) {
    let mut rng = SplitMix64::new(0x0916_0003);
    let mut unit = || (rng.next_u64() >> 40) as f32 / 16_777_216.0;
    let (mut stuck, mut freed) = (0, 0);
    for _ in 0..200 {
        let mut size = V::ZERO;
        let mut half = V::ZERO;
        for axis in 0..V::AXES {
            size[axis] = 0.5 + unit() * 2.5;
            half[axis] = 0.1 + unit() * 0.5;
        }
        let n = normal(&mut unit);
        let cut = Cut::new(n, (unit() - 0.5) * 2.0);
        let ramp = Collider::new(1, Shape::Box { half: size }, V::ZERO).with_cut(cut);
        let mut at = V::ZERO;
        for axis in 0..V::AXES {
            at[axis] = (unit() * 2.0 - 1.0) * size[axis];
        }
        if !shares_interior(at, half, &ramp) {
            continue;
        }
        let world = [ramp];
        let mut state = MoverState {
            position: at,
            ..MoverState::default()
        };
        let shape = Shape::Box { half };
        let contacts = move_and_slide(
            &mut state,
            &shape,
            DT,
            &world[..],
            &config(),
            &MoverOptions::default(),
        )
        .unwrap();
        if contacts.pushed_out {
            freed += 1;
            assert!(
                !shares_interior(state.position, half, &ramp),
                "{name}: {state:?}"
            );
        }
        if !contacts.stuck {
            continue;
        }
        stuck += 1;
        let mut grown = half;
        for axis in 0..V::AXES {
            grown[axis] += SKIN;
        }
        let mut directions = vec![cut.normal(), -cut.normal()];
        for axis in 0..V::AXES {
            let mut d = V::ZERO;
            d[axis] = 1.0;
            directions.extend([d, -d]);
        }
        for d in directions {
            let limit = (0..V::AXES)
                .filter(|&axis| d[axis] != 0.0)
                .map(|axis| 2.0 * half[axis] / d[axis].abs())
                .fold(f32::INFINITY, f32::min);
            for k in 1..=64u8 {
                let place = at + d * (limit * f32::from(k) / 64.0);
                assert!(
                    shares_interior(place, grown, &ramp),
                    "{name}: stuck at {at:?} in {ramp:?}, but {place:?} is free"
                );
            }
        }
    }
    assert!(stuck > 10 && freed > 10, "{name}: {stuck} {freed}");
}

#[wasm_bindgen_test(unsupported = test)]
fn a_body_stuck_in_a_ramp_has_no_free_place_within_its_size() {
    ramp_stuck::<Vec2>("2D", |unit| {
        Vec2::new(unit() * 2.0 - 1.0, unit() * 2.0 - 1.0 + 0.01)
    });
    ramp_stuck::<Vec3>("3D", |unit| {
        let mut n = Vec3::new(
            unit() * 2.0 - 1.0,
            unit() * 2.0 - 1.0 + 0.01,
            unit() * 2.0 - 1.0 + 0.01,
        );
        let pick = unit();
        let zero = if pick < 1.0 / 3.0 {
            0
        } else if pick < 2.0 / 3.0 {
            1
        } else {
            2
        };
        n[zero] = 0.0;
        if n == Vec3::ZERO {
            n.y = 1.0;
        }
        n
    });
}

/// A floor x <= 0 whose top is at y = 0, and ground 5 below. A walking body
/// at (0.35, 0.501), moving at (6, -0.5), moves 0.1 across and ends with
/// its left edge 0.05 past the floor's edge: it is over nothing within the
/// snap distance, so it stands on nothing. Oracle: the requirement.
fn walk_off<V: Space>(name: &str) {
    let world = [
        block::<V>(1, (-10.0, -1.0), (0.0, 0.0)),
        block::<V>(2, (-10.0, -6.0), (10.0, -5.0)),
    ];
    let mut state = standing::<V>(0.35, 1);
    state.position[1] = 0.501;
    state.velocity = V::at(6.0, -0.5);
    step(&mut state, &world, DT, &MoverOptions::default());
    assert!(state.position[0] - 0.4 > 0.0, "{name}: {state:?}");
    assert_eq!(state.floor, None, "{name}: {state:?}");
}

#[wasm_bindgen_test(unsupported = test)]
fn a_body_that_walks_off_an_edge_stands_on_nothing_that_step() {
    both(walk_off::<Vec2>, walk_off::<Vec3>);
}

/// A floor x <= 0 whose top is at y = 0 and, past it, one 0.05 lower: less
/// than the snap distance (0.1). A body walking right at 4.3 units a second
/// under gravity is on a floor every step, and on the first step it is
/// wholly past the edge it is on the lower floor, resting on it (no more
/// than a skin above it). Oracle: the requirement.
fn step_down<V: Space>(name: &str) {
    let world = [
        block::<V>(1, (-10.0, -1.0), (0.0, 0.0)),
        block::<V>(2, (0.0, -1.0), (10.0, -0.05)),
    ];
    let mut state = standing::<V>(-1.0, 1);
    let mut crossed = false;
    for frame in 0..40 {
        state.velocity[0] = 4.3;
        state.velocity[1] -= GRAVITY * DT;
        step(&mut state, &world, DT, &MoverOptions::default());
        assert!(state.floor.is_some(), "{name} {frame}: {state:?}");
        if !crossed && state.position[0] - 0.4 > 0.0 {
            crossed = true;
            assert_eq!(state.floor.map(|f| f.id), Some(2), "{name}: {state:?}");
            let gap = state.position[1] - 0.5 + 0.05;
            assert!((0.0..=SKIN + 1e-5).contains(&gap), "{name}: {state:?}");
        }
    }
    assert!(crossed, "{name}");
}

#[wasm_bindgen_test(unsupported = test)]
fn a_body_walking_down_a_small_step_lands_on_it_that_step() {
    both(step_down::<Vec2>, step_down::<Vec3>);
}

/// A body standing on a 30° ramp (normal (-1, √3) / 2), near the origin and
/// two thousand units out, under gravity: once it has landed and come to
/// rest it stays, its position bit-equal from one step to the next. A
/// walkable slope holds a body that is not walking. Oracle: the
/// requirement.
fn ramp_rest<V: Space>(name: &str) {
    for far in [0.0, 2048.0] {
        let world = [ramp::<V>(
            1,
            (far - 4.0, far - 4.0),
            (far + 4.0, far + 4.0),
            -1.0,
            3f32.sqrt(),
        )];
        let mut state = MoverState {
            position: V::at(far, far + 2.0),
            ..MoverState::default()
        };
        for _ in 0..30 {
            fall_step(&mut state, &world);
        }
        assert!(state.floor.is_some(), "{name} {far}: {state:?}");
        for frame in 0..60 {
            let before = state.position;
            fall_step(&mut state, &world);
            for axis in 0..V::AXES {
                assert_eq!(
                    state.position[axis].to_bits(),
                    before[axis].to_bits(),
                    "{name} {far} {frame}: {state:?}"
                );
            }
        }
    }
}

#[wasm_bindgen_test(unsupported = test)]
fn a_body_standing_on_a_walkable_ramp_stays_put() {
    both(ramp_rest::<Vec2>, ramp_rest::<Vec3>);
}

/// Something solid where the body would hang beside a reachable ledge, and
/// something solid it would rise into: each refuses the ledge, and only its
/// own check can. The hanging body spans x 0.19 to 0.99 and y -0.04 to
/// 0.96, the risen one y 0.96 to 1.96. The first crate, x 0.15 to 0.25 and
/// y 0.55 to 0.95, is inside the hanging body by 0.06 along x, so the rise
/// beside it slides out along it unblocked, and is below the risen body.
/// The second, x 0.3 to 0.7 and y 1.2 to 1.5, is above the hanging body
/// and inside the risen one, which the move to stand starts inside and
/// passes. Neither touches the body where it starts. Oracle: the
/// requirement.
fn ledge_blocked<V: Space>(name: &str) {
    for (scene, crate_box) in [
        ("hang", block::<V>(2, (0.15, 0.55), (0.25, 0.95))),
        ("rise", block::<V>(2, (0.3, 1.2), (0.7, 1.5))),
    ] {
        let mut world = ledge_world::<V>(0.95, ColliderKind::Solid);
        world.push(crate_box);
        assert_eq!(ledge(&world), None, "{name} {scene}");
    }
}

#[wasm_bindgen_test(unsupported = test)]
fn find_ledge_refuses_a_ledge_with_no_room_to_hang_or_rise() {
    both(ledge_blocked::<Vec2>, ledge_blocked::<Vec3>);
}

/// A 2D grid of `cell`-sized cells, full where `full` says.
fn grid(
    cell: f32,
    full: impl Fn(IVec2) -> bool,
) -> CellWorld<Vec2, impl Fn(IVec2, &mut CellColliders<'_, Vec2>)> {
    CellWorld::new(cell, move |at: IVec2, out: &mut CellColliders<'_, Vec2>| {
        if full(at) {
            out.push(Collider::new(
                1,
                Shape::Box {
                    half: Vec2::splat(0.5),
                },
                Vec2::splat(0.5),
            ));
        }
    })
}

fn digest(state: &MoverState<Vec2>) -> String {
    let mut hasher = StateHasher::new();
    state.canonical_write(&mut hasher);
    hasher.finish().to_hex()
}

/// Each stage of a step that queries the world (the move, the step-up, the
/// ledge guard, the push-out and the snap), driven past a `CellWorld`'s
/// cell cap by a scene that differs from one it answers only in what that
/// stage asks: a longer move, a taller step, a deeper guard, a crate in the
/// body, a longer snap. Each answers `Unanswered` and leaves the state bit
/// for bit as it was; its answered twin shows the stages before it were
/// answered. Oracle: the requirement.
#[wasm_bindgen_test(unsupported = test)]
fn an_unanswered_query_at_any_stage_leaves_the_state_unchanged() {
    let fine = |snap: f32| {
        MoverConfig::new(MoverSettings {
            up: Vec2::Y,
            skin: SKIN,
            snap_distance: snap,
            normal_threshold: 0.7,
            block_epsilon: 1e-4,
        })
    };
    let floor = |at: IVec2| at.y == -1;
    let stepped = |at: IVec2| at.y == -1 || (at.x >= 10 && (0..3).contains(&at.y));
    let ledge = |at: IVec2| at.y == -1 && at.x < 0;
    let crated = |at: IVec2| at.y == -1 || ((-10..10).contains(&at.x) && (40..45).contains(&at.y));
    let walking = |x: f32| MoverState {
        velocity: Vec2::new(2.0, 0.0),
        ..standing::<Vec2>(x, 1)
    };
    let free = MoverState {
        position: Vec2::new(0.0, 0.5 + SKIN),
        ..MoverState::default()
    };
    let shape = body::<Vec2>();
    let check = |stage: &str,
                 world: &dyn sgl_physics::World<Vec2>,
                 start: MoverState<Vec2>,
                 config: &MoverConfig<Vec2>,
                 options: &MoverOptions|
     -> Result<Contacts<Vec2>, Unanswered> {
        let mut state = start;
        let result = move_and_slide(&mut state, &shape, DT, world, config, options);
        if result.is_err() {
            assert_eq!(digest(&state), digest(&start), "{stage}");
        }
        result
    };
    let plain = MoverOptions::default();

    // The move: two units a second, then six thousand.
    let world = grid(0.1, floor);
    check("move", &world, walking(0.0), &fine(0.1), &plain).expect("answered");
    let fast = MoverState {
        velocity: Vec2::new(6000.0, 0.0),
        ..walking(0.0)
    };
    assert_eq!(
        check("move", &world, fast, &fine(0.1), &plain),
        Err(Unanswered)
    );

    // The step-up: a 0.3 step climbed with a step height of 0.3, then 1000.
    let world = grid(0.1, stepped);
    let low = check(
        "step",
        &world,
        walking(0.6 - SKIN),
        &fine(0.1),
        &climbing(0.3),
    );
    assert!(low.expect("answered").stepped);
    let tall = check(
        "step",
        &world,
        walking(0.6 - SKIN),
        &fine(0.1),
        &climbing(1000.0),
    );
    assert_eq!(tall, Err(Unanswered));

    // The guard: a drop of 0.5, then 1000.
    let world = grid(0.1, ledge);
    check("guard", &world, walking(-0.5), &fine(0.1), &guarded(0.5)).expect("answered");
    let deep = check("guard", &world, walking(-0.5), &fine(0.1), &guarded(1000.0));
    assert_eq!(deep, Err(Unanswered));

    // The snap: a snap distance of 0.1, then 1000.
    let world = grid(0.1, floor);
    check("snap", &world, walking(0.0), &fine(0.1), &plain).expect("answered");
    let long = check("snap", &world, walking(0.0), &fine(1000.0), &plain);
    assert_eq!(long, Err(Unanswered));

    // The push-out: in cells a fiftieth of a unit, the body alone, then with
    // a crate in its head: the push-out looks twice the body's size around it.
    let world = grid(0.02, floor);
    check("push-out", &world, free, &fine(0.1), &plain).expect("answered");
    let world = grid(0.02, crated);
    let inside = check("push-out", &world, free, &fine(0.1), &plain);
    assert_eq!(inside, Err(Unanswered));
}
