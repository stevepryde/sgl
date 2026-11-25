//! Defects the audit (#924) found in the mover and the directions a game
//! gives, and the contract sentences it found missing (physics.md 3, 12, 13,
//! 15, 16, 17). Each test names the defect it catches and its oracle, which
//! comes from the contract, never from the code. Each runs in well under a
//! second, natively and in a wasm runtime.
// Results are compared bit for bit on purpose.
#![allow(clippy::float_cmp)]

mod audit_support;

use audit_support::{bits, config, set_of, up};
use sgl_core::math::{Quat, Vec2, Vec3};
use sgl_physics::{
    ALL_LAYERS, CellColliders, CellWorld, Collider, Contacts, Convex, Cut, Floor, MoverConfig,
    MoverOptions, MoverSettings, MoverState, Shape, Vector, World, distance, find_ledge,
    move_and_collide, move_and_slide, slide, snap_to_ground,
};
use wasm_bindgen_test::wasm_bindgen_test;

/// The skin of [`config`], the brief's settings.
const SKIN: f32 = audit_support::SKIN;

/// The vector with `x` and `y` on the first two axes and zero beyond.
fn xy<V: Vector>(x: f32, y: f32) -> V {
    let mut out = V::ZERO;
    out[0] = x;
    out[1] = y;
    out
}

/// A box body of half-extents 0.4 across and 0.5 along y (and 0.4 along z).
fn body<V: Vector>() -> Shape<V> {
    let mut half = audit_support::splat::<V>(0.4);
    half[1] = 0.5;
    Shape::Box { half }
}

/// A cell world of `cell_size` whose cells hold nothing.
fn empty_cells<V: Vector>(
    cell_size: f32,
) -> CellWorld<V, impl Fn(V::Cell, &mut CellColliders<'_, V>)> {
    CellWorld::new(cell_size, |_: V::Cell, _: &mut CellColliders<'_, V>| {})
}

/// Runs `run` through an empty slice, an empty set and an empty cell world
/// of `cell_size` (large enough that the places tested lie on its grid).
fn empty_worlds<V: Vector>(cell_size: f32, run: &mut dyn FnMut(&str, &dyn World<V>)) {
    let none: [Collider<V>; 0] = [];
    run("slice", &&none[..]);
    run("set", &set_of::<V>(&[], 1.0));
    run("cells", &empty_cells::<V>(cell_size));
}

/// Whether `contacts` reports nothing but the motion asked for: no
/// contact, no flag, nothing applied.
fn reports_nothing<V: Vector>(contacts: &Contacts<V>) -> bool {
    Contacts {
        desired: V::ZERO,
        ..*contacts
    } == Contacts::default()
}

/// A state's bits, so non-finite states compare exactly.
fn state_bits<V: Vector>(state: &MoverState<V>) -> (Vec<u32>, Vec<u32>, String) {
    (
        bits(state.position),
        bits(state.velocity),
        format!("{:?}", state.floor),
    )
}

/// Whether `v` is a unit vector to within a few units in the last place,
/// measured in `f64`.
fn is_unit<V: Vector>(v: V) -> bool {
    let length = (0..V::AXES)
        .map(|axis| f64::from(v[axis]) * f64::from(v[axis]))
        .sum::<f64>()
        .sqrt();
    (length - 1.0).abs() <= 1e-6
}

// ------------------------------------------------------------------- D1

/// Defect: a step from a finite state left the position infinite (a body
/// at x = 3e38 moving 3e38 ended at infinity, through every world).
/// Oracle: physics.md 17: finite inputs whose sum `f32` cannot hold move
/// nothing and change nothing, as a non-finite velocity does.
fn a_step_from_a_finite_state_stays_finite<V: Vector>() {
    let huge: V = audit_support::splat(f32::MAX);
    let cases = [
        (xy::<V>(3.0e38, 0.0), xy::<V>(3.0e38, 0.0)),
        (huge, audit_support::splat(1.0e38)),
        (-huge, audit_support::splat(-1.0e38)),
    ];
    // 1e33-unit cells keep these places on the cell world's i32 grid, and
    // the rounding pad there within a few cells.
    empty_worlds::<V>(1.0e33, &mut |name, world| {
        for (position, velocity) in cases {
            for floor in [
                None,
                Some(Floor {
                    normal: up(),
                    id: 0,
                }),
            ] {
                let before = MoverState {
                    position,
                    velocity,
                    floor,
                };
                let mut state = before;
                let answer = move_and_slide(
                    &mut state,
                    &body(),
                    1.0,
                    world,
                    &config(up()),
                    &MoverOptions::default(),
                );
                assert_eq!(state, before, "{name}: {answer:?}");
                match answer {
                    Ok(contacts) => assert!(reports_nothing(&contacts), "{name}: {contacts:?}"),
                    // A cell world grows a region by a rounding allowance,
                    // which at f32::MAX reaches past f32: it may refuse
                    // (physics.md 10), and a refusal changes nothing.
                    Err(_) => assert!(
                        name == "cells" && position[0].abs() == f32::MAX,
                        "{name}: {position:?} refused"
                    ),
                }
            }
        }
    });
}

#[wasm_bindgen_test(unsupported = test)]
fn a_step_from_a_finite_state_stays_finite_2d() {
    a_step_from_a_finite_state_stays_finite::<Vec2>();
}

#[wasm_bindgen_test(unsupported = test)]
fn a_step_from_a_finite_state_stays_finite_3d() {
    a_step_from_a_finite_state_stays_finite::<Vec3>();
}

/// Defect: `slide` and `move_and_collide` returned a translation that,
/// added to the body's finite position, is infinite; `snap_to_ground`'s
/// probe past the end of `f32` was refused by a cell world. Oracle:
/// physics.md 13 and 15: a move whose end `f32` cannot hold moves nothing
/// and reports no contact, through every world.
fn the_other_moves_move_nothing_past_the_end_of_f32<V: Vector>() {
    let position: V = xy(3.0e38, 0.0);
    let delta: V = xy(3.0e38, 0.0);
    let config = config(up());
    let deep = MoverConfig::new(MoverSettings {
        up: up(),
        skin: SKIN,
        snap_distance: f32::MAX,
        normal_threshold: 0.7,
        block_epsilon: 1e-4,
    });
    let low: V = xy(0.0, -3.0e38);
    empty_worlds::<V>(1.0e33, &mut |name, world| {
        let (moved, contacts) = slide(&body(), position, delta, world, ALL_LAYERS, &config)
            .expect("an empty world answers");
        assert_eq!(moved, V::ZERO, "{name}: slide");
        assert!(reports_nothing(&contacts), "{name}: slide {contacts:?}");

        let collided = move_and_collide(&body(), position, delta, world, ALL_LAYERS, &config)
            .expect("an empty world answers");
        assert_eq!(collided, (V::ZERO, None), "{name}: move_and_collide");

        let (snapped, contacts) = snap_to_ground(&body(), low, world, ALL_LAYERS, &deep)
            .unwrap_or_else(|_| panic!("{name}: snap_to_ground refused"));
        assert_eq!(snapped, V::ZERO, "{name}: snap");
        assert!(reports_nothing(&contacts), "{name}: snap {contacts:?}");
    });
}

#[wasm_bindgen_test(unsupported = test)]
fn the_other_moves_move_nothing_past_the_end_of_f32_2d() {
    the_other_moves_move_nothing_past_the_end_of_f32::<Vec2>();
}

#[wasm_bindgen_test(unsupported = test)]
fn the_other_moves_move_nothing_past_the_end_of_f32_3d() {
    the_other_moves_move_nothing_past_the_end_of_f32::<Vec3>();
}

/// A floor whose top is at y = 0, wide enough for every test here.
fn floor<V: Vector>() -> Collider<V> {
    let mut half = audit_support::splat::<V>(50.0);
    half[1] = 0.5;
    Collider::new(1, Shape::Box { half }, xy(0.0, -0.5))
}

/// Defect: a body at a NaN x over a floor "landed" and reported the floor
/// it cannot be over. Oracle: physics.md 13, 15 and 17: a position or a
/// body size that is not finite changes nothing and reports no contact,
/// in every public move.
fn a_body_that_is_not_finite_moves_nothing_and_meets_nothing<V: Vector>() {
    let level = [floor::<V>()];
    let config = config(up());
    let sound = body::<V>();
    let mut nan_half = sound.half_extents();
    nan_half[0] = f32::NAN;
    let bodies = [
        (sound, xy::<V>(f32::NAN, 1.0)),
        (sound, xy::<V>(f32::INFINITY, 1.0)),
        (Shape::Box { half: nan_half }, xy::<V>(0.0, 1.0)),
        (Shape::Ball { radius: f32::NAN }, xy::<V>(0.0, 1.0)),
    ];
    let fall: V = xy(0.0, -2.0);
    for (shape, position) in bodies {
        let name = format!("{shape:?} at {position:?}");
        for floor in [
            None,
            Some(Floor {
                normal: up(),
                id: 1,
            }),
        ] {
            let before = MoverState {
                position,
                velocity: fall,
                floor,
            };
            let mut state = before;
            let contacts = move_and_slide(
                &mut state,
                &shape,
                1.0,
                &level[..],
                &config,
                &MoverOptions::default(),
            )
            .expect("a slice answers");
            assert_eq!(state_bits(&state), state_bits(&before), "{name}");
            assert!(reports_nothing(&contacts), "{name}: {contacts:?}");
        }
        let (moved, contacts) =
            slide(&shape, position, fall, &level[..], ALL_LAYERS, &config).expect("answers");
        assert_eq!(moved, V::ZERO, "{name}: slide");
        assert!(reports_nothing(&contacts), "{name}: slide {contacts:?}");
        let (snapped, contacts) =
            snap_to_ground(&shape, position, &level[..], ALL_LAYERS, &config).expect("answers");
        assert_eq!(snapped, V::ZERO, "{name}: snap");
        assert!(reports_nothing(&contacts), "{name}: snap {contacts:?}");
        let collided = move_and_collide(&shape, position, fall, &level[..], ALL_LAYERS, &config)
            .expect("answers");
        assert_eq!(collided, (V::ZERO, None), "{name}: move_and_collide");
    }
}

#[wasm_bindgen_test(unsupported = test)]
fn a_body_that_is_not_finite_moves_nothing_and_meets_nothing_2d() {
    a_body_that_is_not_finite_moves_nothing_and_meets_nothing::<Vec2>();
}

#[wasm_bindgen_test(unsupported = test)]
fn a_body_that_is_not_finite_moves_nothing_and_meets_nothing_3d() {
    a_body_that_is_not_finite_moves_nothing_and_meets_nothing::<Vec3>();
}

/// Defect: a non-finite velocity moving the body, touching its velocity or
/// floor, or reporting a contact. Oracle: physics.md 17: a non-finite
/// velocity changes nothing.
fn a_non_finite_velocity_changes_nothing<V: Vector>() {
    let level = [floor::<V>()];
    // Resting a skin above the floor, so any move would meet it.
    let position: V = xy(0.0, 0.5 + SKIN);
    for velocity in [
        xy::<V>(f32::NAN, -1.0),
        xy::<V>(1.0, f32::NEG_INFINITY),
        xy::<V>(f32::INFINITY, 0.0),
    ] {
        for floor in [
            None,
            Some(Floor {
                normal: up(),
                id: 1,
            }),
        ] {
            let before = MoverState {
                position,
                velocity,
                floor,
            };
            let mut state = before;
            let contacts = move_and_slide(
                &mut state,
                &body(),
                1.0,
                &level[..],
                &config(up()),
                &MoverOptions::default(),
            )
            .expect("a slice answers");
            assert_eq!(state_bits(&state), state_bits(&before), "{velocity:?}");
            assert!(reports_nothing(&contacts), "{velocity:?}: {contacts:?}");
        }
    }
}

#[wasm_bindgen_test(unsupported = test)]
fn a_non_finite_velocity_changes_nothing_2d() {
    a_non_finite_velocity_changes_nothing::<Vec2>();
}

#[wasm_bindgen_test(unsupported = test)]
fn a_non_finite_velocity_changes_nothing_3d() {
    a_non_finite_velocity_changes_nothing::<Vec3>();
}

// ------------------------------------------------------------------- D3

/// Defect: the blocked flags read the motion applied as a difference of
/// rounded absolute positions, so a free fall 4096 units up reported
/// `down_blocked` (and a rise `up_blocked`) where the same fall at the
/// origin did not. Oracle: physics.md 3 and 14: in an empty world nothing
/// blocks, and the motion applied, measured relative to the body, is
/// exactly the motion asked for, near the origin or far from it.
fn a_free_fall_far_from_the_origin_is_not_blocked<V: Vector>() {
    // 409/8192: half a unit in the last place past a representable step
    // below 4096, and a quarter of one above it.
    let speed = 409.0 / 8192.0;
    empty_worlds::<V>(1.0, &mut |name, world| {
        for y in [0.0, 4096.0, -4096.0, 65_536.0] {
            for sign in [-1.0, 1.0] {
                let velocity = xy::<V>(0.0, sign * speed);
                let mut state = MoverState {
                    position: xy(0.0, y),
                    velocity,
                    floor: None,
                };
                let contacts = move_and_slide(
                    &mut state,
                    &body(),
                    1.0,
                    world,
                    &config(up()),
                    &MoverOptions::default(),
                )
                .expect("an empty world answers");
                let at = format!("{name}: y = {y}, speed {}", sign * speed);
                assert!(
                    !contacts.down_blocked && !contacts.up_blocked,
                    "{at}: {contacts:?}"
                );
                assert_eq!(contacts.effective, velocity, "{at}");
            }
        }
    });
}

#[wasm_bindgen_test(unsupported = test)]
fn a_free_fall_far_from_the_origin_is_not_blocked_2d() {
    a_free_fall_far_from_the_origin_is_not_blocked::<Vec2>();
}

#[wasm_bindgen_test(unsupported = test)]
fn a_free_fall_far_from_the_origin_is_not_blocked_3d() {
    a_free_fall_far_from_the_origin_is_not_blocked::<Vec3>();
}

/// A cell world whose row of cells `row` (along y) is solid.
fn floor_cells<V: Vector>(row: i32) -> CellWorld<V, impl Fn(V::Cell, &mut CellColliders<'_, V>)> {
    CellWorld::new(1.0, move |cell: V::Cell, out: &mut CellColliders<'_, V>| {
        if cell[1] == row {
            out.push(Collider::new(
                1,
                Shape::Box {
                    half: audit_support::splat(0.5),
                },
                audit_support::splat(0.5),
            ));
        }
    })
}

/// Defect: the mover's later stages (the ground snap, the step-up, the
/// ledge guard) started from the body's rounded absolute position, so a
/// walking step 4096 units from the origin applied a different translation
/// and read different contacts from the same step at the origin. Oracle:
/// physics.md 3: a scene translated by a power of two whose coordinates
/// stay exact moves by bit-equal translations, with bit-equal contacts,
/// velocity and floor.
fn a_walking_step_far_from_the_origin_moves_as_near_it<V: Vector>() {
    let shift = 4096.0;
    // The floor, a step 0.1875 high from x = 0.75 to 2.0 to climb, and a
    // ceiling at y = 1.5 over x <= 0: every coordinate on a 1/32 grid, so
    // the scene translates exactly.
    let mut step_half = audit_support::splat::<V>(0.625);
    step_half[1] = 0.093_75;
    let step = Collider::new(2, Shape::Box { half: step_half }, xy::<V>(1.375, 0.093_75));
    let mut ceiling_half = audit_support::splat::<V>(2.0);
    ceiling_half[1] = 0.5;
    let ceiling = Collider::new(3, Shape::Box { half: ceiling_half }, xy::<V>(-2.0, 2.0));
    let level = |by: f32| {
        [floor::<V>(), step, ceiling].map(|mut c| {
            for axis in 0..V::AXES {
                c.position[axis] += by;
            }
            c
        })
    };
    let (near, far) = (level(0.0), level(shift));
    let (near_set, far_set) = (set_of(&near, 1.0), set_of(&far, 1.0));
    // The cell world holds the floor alone, its top at 0 and at 4096.
    // A body pushed out of the floor and rising into the ceiling, which no
    // ground snap levels afterwards, shows a slide that starts from a
    // rounded place; a walk that ends hovering shows the snap.
    let (near_cells, far_cells) = (floor_cells::<V>(-1), floor_cells::<V>(4095));
    let worlds: [(&str, &dyn World<V>, &dyn World<V>); 3] = [
        ("slice", &&near[..], &&far[..]),
        ("set", &near_set, &far_set),
        ("cells", &near_cells, &far_cells),
    ];
    // Starts on a 1/64 grid: resting, hovering within the snap, and 1/64
    // inside the floor, which the push-out lifts by a length with every
    // bit set before the step-up settles from there. Speeds with every bit
    // set, so where the body ends rounds differently far away, falling
    // further than the skin and less.
    let starts = [
        xy::<V>(0.25, 0.5 + 1.0 / 64.0),
        xy::<V>(0.25, 0.5),
        xy::<V>(0.25, 0.5 + 3.0 / 64.0),
        xy::<V>(0.25, 0.5 - 1.0 / 64.0),
    ];
    let velocities = [
        xy::<V>(2863.0 / 8192.0, -1.0 / 6.0),
        xy::<V>(2863.0 / 8192.0, -1.0 / 60.0),
        xy::<V>(-0.3, -0.1),
        xy::<V>(-0.2, 0.7),
    ];
    let options = MoverOptions {
        step_height: 0.25,
        ledge_drop: Some(0.5),
        ..MoverOptions::default()
    };
    for (name, near_world, far_world) in worlds {
        for start in starts {
            for velocity in velocities {
                let mut moved = Vec::new();
                for (by, world) in [(0.0, near_world), (shift, far_world)] {
                    let mut state = MoverState {
                        position: start + audit_support::splat::<V>(by),
                        velocity,
                        floor: Some(Floor {
                            normal: up(),
                            id: 1,
                        }),
                    };
                    let mut steps = Vec::new();
                    for _ in 0..4 {
                        let contacts = move_and_slide(
                            &mut state,
                            &body(),
                            1.0,
                            world,
                            &config(up()),
                            &options,
                        )
                        .expect("these worlds answer");
                        steps.push((contacts, state.velocity, state.floor));
                        state.velocity = velocity;
                    }
                    moved.push(steps);
                }
                assert_eq!(moved[0], moved[1], "{name}: {start:?} {velocity:?}");
            }
        }
    }
}

#[wasm_bindgen_test(unsupported = test)]
fn a_walking_step_far_from_the_origin_moves_as_near_it_2d() {
    a_walking_step_far_from_the_origin_moves_as_near_it::<Vec2>();
}

#[wasm_bindgen_test(unsupported = test)]
fn a_walking_step_far_from_the_origin_moves_as_near_it_3d() {
    a_walking_step_far_from_the_origin_moves_as_near_it::<Vec3>();
}

// ------------------------------------------------------------- resting

/// Defect: a body standing under gravity creeping into its floor, or
/// jittering, step after step. Oracle: physics.md 17: a resting body stays
/// where it is to within the skin: on its first steps it may close up to
/// `skin` of its gap along `-up`, and once a step leaves it where it was
/// its position is bit-equal step to step, near the origin and far from
/// it. Here it falls 9.8 · dt² (0.0027) a step, so it settles within four
/// steps of a skin's gap.
fn a_resting_body_stays_where_it_is<V: Vector>() {
    let dt = 1.0 / 60.0;
    for (x, top) in [(0.0, 0.0), (1000.3, 4096.0), (-7.7, -300.0)] {
        let mut floor = floor::<V>();
        floor.position[0] = x;
        floor.position[1] = top - 0.5;
        let level = [floor];
        for gap in [SKIN, 0.5 * SKIN, 0.0] {
            let start: V = xy(x, top + 0.5 + gap);
            let mut state = MoverState {
                position: start,
                velocity: V::ZERO,
                floor: Some(Floor {
                    normal: up(),
                    id: 1,
                }),
            };
            let mut last = start;
            let mut settled = None;
            for step in 0..40 {
                state.velocity = state.velocity - up::<V>() * (9.8 * dt);
                move_and_slide(
                    &mut state,
                    &body(),
                    dt,
                    &level[..],
                    &config(up()),
                    &MoverOptions::default(),
                )
                .expect("a slice answers");
                let at = format!("top {top}, gap {gap}, step {step}: {state:?}");
                let p = state.position;
                assert_eq!(p[0], x, "{at}");
                let closed = f64::from(start[1]) - f64::from(p[1]);
                assert!((0.0..=f64::from(SKIN)).contains(&closed), "{at}");
                if settled.is_some() || bits(p) == bits(last) {
                    settled.get_or_insert(step);
                    assert_eq!(bits(p), bits(last), "{at}");
                }
                assert!(state.floor.is_some(), "{at}");
                last = p;
            }
            assert!(
                settled.is_some_and(|step| step <= 4),
                "top {top}, gap {gap}: {settled:?}"
            );
        }
    }
}

#[wasm_bindgen_test(unsupported = test)]
fn a_resting_body_stays_where_it_is_2d() {
    a_resting_body_stays_where_it_is::<Vec2>();
}

#[wasm_bindgen_test(unsupported = test)]
fn a_resting_body_stays_where_it_is_3d() {
    a_resting_body_stays_where_it_is::<Vec3>();
}

// ------------------------------------------------------------ push-out

/// One still step of a body of half-extents `half` at the origin among
/// `level`.
fn still_step(
    half: Vec2,
    level: &[Collider<Vec2>],
    up: Vec2,
) -> (MoverState<Vec2>, Contacts<Vec2>) {
    let mut state = MoverState {
        position: Vec2::ZERO,
        velocity: Vec2::ZERO,
        floor: None,
    };
    let contacts = move_and_slide(
        &mut state,
        &Shape::Box { half },
        1.0 / 60.0,
        level,
        &config(up),
        &MoverOptions::default(),
    )
    .expect("a slice answers");
    (state, contacts)
}

fn slab(half: Vec2) -> Collider<Vec2> {
    Collider::new(1, Shape::Box { half }, Vec2::ZERO)
}

/// Defect: the push-out's reach read as half the body, or as its largest
/// size on every axis. Oracle: physics.md 17: it moves a body at most its
/// own size along each axis, the body's full extent on that axis: a body
/// 0.5 wide and 1.0 tall is freed 0.96 up out of a slab and 0.46 across
/// out of a wall, and is stuck when the way out is 1.06 up, or 0.56 across
/// (more than its width, less than its height).
#[wasm_bindgen_test(unsupported = test)]
fn push_out_reaches_the_body_s_full_extent_along_each_axis() {
    let half = Vec2::new(0.25, 0.5);
    let up_by = |h: f32| h + 0.5 + SKIN;
    let across_by = |w: f32| w + 0.25 + SKIN;

    let (state, contacts) = still_step(half, &[slab(Vec2::new(50.0, 0.45))], Vec2::Y);
    assert!(contacts.pushed_out && !contacts.stuck, "{contacts:?}");
    assert!((state.position.y - up_by(0.45)).abs() < 1e-6, "{state:?}");
    let (state, contacts) = still_step(half, &[slab(Vec2::new(50.0, 0.55))], Vec2::Y);
    assert!(contacts.stuck && !contacts.pushed_out, "{contacts:?}");
    assert_eq!(state.position, Vec2::ZERO);

    let (state, contacts) = still_step(half, &[slab(Vec2::new(0.2, 50.0))], Vec2::Y);
    assert!(contacts.pushed_out && !contacts.stuck, "{contacts:?}");
    assert!(
        (state.position.x.abs() - across_by(0.2)).abs() < 1e-6,
        "{state:?}"
    );
    let (state, contacts) = still_step(half, &[slab(Vec2::new(0.3, 50.0))], Vec2::Y);
    assert!(contacts.stuck && !contacts.pushed_out, "{contacts:?}");
    assert_eq!(state.position, Vec2::ZERO);
}

/// Defect: a push-out between equally short ways out that depends on the
/// order it found them. Oracle: physics.md 17: of two equally short ways
/// out, the one most along `up` wins, then the lowest component by
/// component, x first: out of a square, up (and down in a y-down world);
/// out of a tall wall, where only left and right are in reach, left.
#[wasm_bindgen_test(unsupported = test)]
fn push_out_ties_go_up_then_to_the_lowest_component() {
    let half = Vec2::splat(0.5);
    let out = 0.3 + 0.5 + SKIN;
    for (up, expected) in [
        (Vec2::Y, Vec2::new(0.0, out)),
        (Vec2::NEG_Y, Vec2::new(0.0, -out)),
    ] {
        let (state, contacts) = still_step(half, &[slab(Vec2::splat(0.3))], up);
        assert!(contacts.pushed_out, "{contacts:?}");
        assert!(
            (state.position - expected).length() < 1e-6,
            "up {up}: {state:?}"
        );
    }
    let (state, contacts) = still_step(half, &[slab(Vec2::new(0.3, 50.0))], Vec2::Y);
    assert!(contacts.pushed_out, "{contacts:?}");
    assert!(
        (state.position - Vec2::new(-out, 0.0)).length() < 1e-6,
        "{state:?}"
    );
}

/// Thin walls, half 0.001 wide and 5 tall, at each x in `xs`.
fn slivers(xs: &[f32]) -> Vec<Collider<Vec2>> {
    (1u64..)
        .zip(xs)
        .map(|(id, &x)| {
            Collider::new(
                id,
                Shape::Box {
                    half: Vec2::new(0.001, 5.0),
                },
                Vec2::new(x, 0.0),
            )
        })
        .collect()
}

/// Defect: a push-out cap that keeps the wrong distances, or none. Oracle:
/// physics.md 17: each axis tries the `MAX_PUSH_OUT_PLANES` (8) distances
/// nearest the body, staying put included. A body 1.0 wide inside a row
/// of thin walls whose only free place within its reach is 0.911 to the
/// right is freed there when fewer than 8 distances are nearer, and is
/// stuck when more are.
#[wasm_bindgen_test(unsupported = test)]
fn push_out_keeps_the_nearest_distances_on_each_axis() {
    let half = Vec2::splat(0.5);
    let free = 0.4 + 0.001 + 0.5 + SKIN;
    // Distances within reach: 0, 0.011, -0.111, -0.489, -0.511, 0.511, 0.911.
    let few = slivers(&[-1.0, -0.5, 0.0, 0.4]);
    let (state, contacts) = still_step(half, &few, Vec2::Y);
    assert!(contacts.pushed_out && !contacts.stuck, "{contacts:?}");
    assert!(
        (state.position - Vec2::new(free, 0.0)).length() < 1e-6,
        "{state:?}"
    );
    // A wall every 0.1 from -1.0 to 0.4: more than 8 distances nearer.
    let xs: Vec<f32> = (-10i16..=4).map(|i| f32::from(i) * 0.1).collect();
    let (state, contacts) = still_step(half, &slivers(&xs), Vec2::Y);
    assert!(contacts.stuck && !contacts.pushed_out, "{contacts:?}");
    assert_eq!(state.position, Vec2::ZERO);
}

// ------------------------------------------------------------------- D2

/// Defect: a direction whose squared length underflows (or overflows) was
/// accepted and "normalised" to a vector that is not unit, or refused
/// though it is a direction: a cut's normal and `up`, from `new` and from
/// the wire. Oracle: physics.md 12 and 16: a cut's normal and `up` are
/// unit vectors, and any finite non-zero direction is accepted, pointing
/// where it was given.
#[wasm_bindgen_test(unsupported = test)]
fn a_direction_of_any_finite_length_is_normalised_to_unit() {
    let half = core::f32::consts::FRAC_1_SQRT_2;
    let cases_2d = [
        (Vec2::new(1e-21, 1e-21), Vec2::splat(half)),
        (Vec2::new(1.5e-20, 0.0), Vec2::X),
        (Vec2::new(0.0, -1e-45), Vec2::NEG_Y),
        (Vec2::splat(f32::MAX), Vec2::splat(half)),
        (Vec2::new(-3e38, 3e38), Vec2::new(-half, half)),
    ];
    let near = |a: Vec2, b: Vec2| (a - b).length() <= 1e-6;
    for (given, expected) in cases_2d {
        let normal = Cut::new(given, 0.0).normal();
        assert!(
            is_unit(normal) && near(normal, expected),
            "cut {given}: {normal}"
        );
        let up = MoverConfig::new(MoverSettings {
            up: given,
            skin: SKIN,
            snap_distance: 0.1,
            normal_threshold: 0.7,
            block_epsilon: 1e-4,
        })
        .up();
        assert!(is_unit(up) && near(up, expected), "up {given}: {up}");
    }
    let cases_3d = [
        (Vec3::new(1e-21, 0.0, 1e-21), Vec3::new(half, 0.0, half)),
        (Vec3::new(0.0, 1.5e-20, 0.0), Vec3::Y),
        (
            Vec3::new(f32::MAX, -f32::MAX, 0.0),
            Vec3::new(half, -half, 0.0),
        ),
    ];
    for (given, expected) in cases_3d {
        let normal = Cut::new(given, 0.0).normal();
        assert!(is_unit(normal), "cut {given}: {normal}");
        assert!(
            (normal - expected).length() <= 1e-6,
            "cut {given}: {normal}"
        );
        let up = MoverConfig::new(MoverSettings {
            up: given,
            skin: SKIN,
            snap_distance: 0.1,
            normal_threshold: 0.7,
            block_epsilon: 1e-4,
        })
        .up();
        assert!(
            is_unit(up) && (up - expected).length() <= 1e-6,
            "up {given}: {up}"
        );
    }
}

/// Defect: deserialising took a different path from `new`, or the same
/// underflowing one: these 15 wire bytes decoded into a cut whose normal
/// was 12% short. Oracle: physics.md 5 and 16: a cut and a config are
/// validated again when deserialised, so their directions are unit.
#[wasm_bindgen_test(unsupported = test)]
fn a_direction_from_the_wire_is_normalised_to_unit() {
    let wire = [
        38u8, 227, 234, 20, 24, 100, 96, 154, 107, 57, 113, 145, 2, 98, 56,
    ];
    let cut: Cut<Vec2> = postcard::from_bytes(&wire).expect("a finite non-zero normal");
    assert!(is_unit(cut.normal()), "{cut:?}");

    let settings = MoverSettings {
        up: Vec2::new(1.5e-20, 0.0),
        skin: SKIN,
        snap_distance: 0.1,
        normal_threshold: 0.7,
        block_epsilon: 1e-4,
    };
    let bytes = postcard::to_allocvec(&settings).expect("encodes");
    let config: MoverConfig<Vec2> = postcard::from_bytes(&bytes).expect("a valid config");
    assert_eq!(config.up(), Vec2::X);
}

/// Defect: a convex shape's rotation, normalised the way the cut's normal
/// was, scaled the shape when the rotation's squared length underflowed.
/// Oracle: physics.md 19: the rotation is normalised first, so a square
/// turned by `(1e-21, 1e-21)` is turned 45 degrees, its corner (1, -1)
/// landing at (√2, 0), and a point turned by the quaternion
/// `(0, 1e-21, 0, 1e-21)` is turned a quarter turn about y, (1, 0, 0)
/// landing at (0, 0, -1).
#[wasm_bindgen_test(unsupported = test)]
fn a_convex_shape_s_tiny_rotation_turns_it_and_does_not_scale_it() {
    let square = [
        Vec2::new(-1.0, -1.0),
        Vec2::new(1.0, -1.0),
        Vec2::new(1.0, 1.0),
        Vec2::new(-1.0, 1.0),
    ];
    let turned = Convex::polygon(&square, 1e-21, 1e-21);
    let root2 = core::f32::consts::SQRT_2;
    let expected = [
        Vec2::new(0.0, -root2),
        Vec2::new(root2, 0.0),
        Vec2::new(0.0, root2),
        Vec2::new(-root2, 0.0),
    ];
    for (point, expected) in turned.points().iter().zip(expected) {
        assert!(
            (*point - expected).length() <= 1e-6,
            "{point} for {expected}"
        );
    }
    let hull = Convex::hull(&[Vec3::X], Quat::from_xyzw(0.0, 1e-21, 0.0, 1e-21));
    let point = hull.points()[0];
    assert!((point - Vec3::NEG_Z).length() <= 1e-6, "{point}");
}

/// Defect: `distance` normalised its normal the same way, so two points
/// 1.4e-20 apart were reported along a normal that is not unit. Oracle:
/// physics.md 15: the normal is a unit vector from the collider toward the
/// shape, here (-√½, -√½), and the gap is their distance, √2 · 1e-20.
#[wasm_bindgen_test(unsupported = test)]
fn a_tiny_separation_has_a_unit_normal() {
    let point = Shape::Box { half: Vec2::ZERO };
    let collider = Collider::new(1, point, Vec2::splat(1e-20));
    let found = distance(&point, Vec2::ZERO, &collider).expect("both are sound");
    let half = core::f32::consts::FRAC_1_SQRT_2;
    assert!(is_unit(found.normal), "{found:?}");
    assert!(
        (found.normal - Vec2::splat(-half)).length() <= 1e-6,
        "{found:?}"
    );
    let expected = 2f64.sqrt() * 1e-20;
    assert!(
        (f64::from(found.distance) - expected).abs() <= expected * 1e-6,
        "{found:?}"
    );
}

/// Defect: `find_ledge` read a very short `toward` as having no direction.
/// Oracle: physics.md 18: `None` only when `toward` has no direction across
/// `up`; a direction's length does not matter, so `(1e-30, 0)` finds the
/// ledge `(1, 0)` finds.
#[wasm_bindgen_test(unsupported = test)]
fn a_ledge_is_found_toward_a_very_short_direction() {
    let level = [
        floor::<Vec2>(),
        // A wall a skin to the right of the body, its top at 1.2.
        Collider::new(
            2,
            Shape::Box {
                half: Vec2::new(0.5, 0.6),
            },
            Vec2::new(0.4 + SKIN + 0.5, 0.6),
        ),
    ];
    let at = Vec2::new(0.0, 0.5 + SKIN);
    let find = |toward: Vec2| {
        find_ledge(
            &body(),
            at,
            toward,
            &level[..],
            &config(Vec2::Y),
            1.0,
            ALL_LAYERS,
        )
        .expect("a slice answers")
    };
    let ledge = find(Vec2::X);
    assert!(ledge.is_some_and(|ledge| ledge.id == 2), "{ledge:?}");
    assert_eq!(find(Vec2::new(1e-30, 0.0)), ledge);
}
