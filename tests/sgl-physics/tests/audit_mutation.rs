//! Tests the mutation run of #924 showed missing: each kills mutants that
//! survived every other test, with an oracle from the contract or geometry
//! worked here, never from the crate. Runs natively and in a wasm runtime.
#![allow(clippy::cast_precision_loss, clippy::float_cmp)]

mod audit_support;

use audit_support::{Rng, SKIN, config, dot};
use sgl_core::math::{Quat, Vec2, Vec3};
use sgl_physics::{
    Collider, Convex, Cut, Floor, MoverConfig, MoverOptions, MoverSettings, MoverState, Shape,
    Vector, move_and_slide, slide, snap_to_ground,
};
use wasm_bindgen_test::wasm_bindgen_test;

/// Defect: a ground probe that reports it asked for a motion along `up`
/// (or none) when it finds no ground. Oracle: physics.md 13
/// (`snap_to_ground` probes `snap_distance` along `-up`) and 14 (`Contacts`
/// records the motion asked for). With nothing below, the probe asks for
/// `-up * snap_distance`, for y-up and y-down worlds, in 2D and 3D.
#[wasm_bindgen_test(unsupported = test)]
fn a_ground_probe_that_finds_nothing_reports_the_probe_it_asked_for() {
    let empty2: [Collider<Vec2>; 0] = [];
    let empty3: [Collider<Vec3>; 0] = [];
    for up in [Vec2::Y, Vec2::NEG_Y] {
        let (moved, contacts) = snap_to_ground(
            &Shape::Ball { radius: 0.5 },
            Vec2::new(3.0, 4.0),
            &empty2[..],
            u32::MAX,
            &config(up),
        )
        .unwrap();
        assert_eq!((moved, contacts.desired), (Vec2::ZERO, -up * 0.1), "{up}");
        assert!(!contacts.floor, "{contacts:?}");
    }
    for up in [Vec3::Y, Vec3::NEG_Y] {
        let (_, contacts) = snap_to_ground(
            &Shape::Box {
                half: Vec3::splat(0.5),
            },
            Vec3::new(3.0, 4.0, -2.0),
            &empty3[..],
            u32::MAX,
            &config(up),
        )
        .unwrap();
        assert_eq!(contacts.desired, -up * 0.1, "{up}");
    }
    // A probe from a place that is not finite moves nothing and reports
    // no contact, but still the probe it asked for.
    let (moved, contacts) = snap_to_ground(
        &Shape::Ball { radius: 0.5 },
        Vec2::new(f32::NAN, 4.0),
        &empty2[..],
        u32::MAX,
        &config(Vec2::Y),
    )
    .unwrap();
    assert_eq!(
        (moved, contacts.desired),
        (Vec2::ZERO, Vec2::new(0.0, -0.1))
    );
    assert!(!contacts.floor && !contacts.wall && !contacts.ceiling);
}

/// A slab 6 by 6 by 1 whose broad face, of unit normal `normal`, lies
/// `reach` from the origin, facing it: the slab is beyond that face.
fn wall(id: u64, normal: Vec3, reach: f32) -> Collider<Vec3> {
    let half = Vec3::new(3.0, 3.0, 0.5);
    let corners: Vec<Vec3> = (0u8..8)
        .map(|i| {
            let sign = |bit: u8, h: f32| if i >> bit & 1 == 0 { -h } else { h };
            Vec3::new(sign(0, half.x), sign(1, half.y), sign(2, half.z))
        })
        .collect();
    let turn = Quat::from_rotation_arc(Vec3::Z, -normal);
    Collider::new(
        id,
        Shape::Convex(Convex::hull(&corners, turn)),
        -normal * (reach + 0.5),
    )
}

fn unit(x: f32, y: f32, z: f32) -> Vec3 {
    Vec3::new(x, y, z).normalize()
}

/// The faces of the tests below: walls leaning over the body (their
/// normals point a little down, so neither is a floor, a ceiling, or a
/// slope too steep to stand on), each a skin and a sixty-fourth from a
/// ball of radius 0.3 at the origin.
fn corner_faces() -> [Vec3; 3] {
    [
        unit(0.8, -0.3, 0.5),
        unit(-0.6, -0.2, 0.7),
        unit(0.1, -0.25, -0.95),
    ]
}

fn pressed(faces: &[Vec3], velocity: Vec3) -> MoverState<Vec3> {
    let config = MoverConfig::new(MoverSettings {
        up: Vec3::Y,
        skin: SKIN,
        snap_distance: 0.1,
        normal_threshold: 0.7,
        block_epsilon: 1e-4,
    });
    let world: Vec<Collider<Vec3>> = faces
        .iter()
        .enumerate()
        .map(|(i, &n)| wall(i as u64 + 1, n, 0.3 + SKIN * 65.0 / 64.0))
        .collect();
    let mut state = MoverState {
        position: Vec3::ZERO,
        velocity,
        floor: None,
    };
    move_and_slide(
        &mut state,
        &Shape::Ball { radius: 0.3 },
        0.05,
        &world[..],
        &config,
        &MoverOptions::default(),
    )
    .unwrap();
    state
}

/// Defect: a slide or velocity that leaves a body moving into one of two
/// faces it is pressed against, tilted on every axis. Oracle: physics.md
/// 16 (between two faces a slide has touched, the remaining motion follows
/// their crease) and 17 (the velocity keeps no part into a face met; a
/// later face never turns it into an earlier one, so it follows their
/// crease), with the crease worked here: the line of `a × b`. A ball
/// resting against two walls tilted on every axis, pushed into both and
/// along their crease, keeps exactly its velocity along the crease and
/// moves into neither wall past the skin.
#[wasm_bindgen_test(unsupported = test)]
fn a_body_pressed_into_two_tilted_walls_follows_their_crease() {
    let [a, b, _] = corner_faces();
    let line = a.cross(b).normalize();
    for along in [1.5, -1.0] {
        let velocity = -(a + b) * 2.0 + line * along;
        let state = pressed(&[a, b], velocity);
        let expected = line * along;
        assert!(
            (state.velocity - expected).length() < 1e-3,
            "{along}: {state:?}, expected {expected}"
        );
        // It started a sixty-fourth of the skin beyond the skin.
        let closer = -(SKIN / 64.0) - 1e-5;
        let moved = state.position;
        assert!(moved.dot(a) > closer && moved.dot(b) > closer, "{state:?}");
    }
}

/// Defect: a body in a corner of three faces that keeps moving along the
/// crease of two into the third. Oracle: physics.md 16 (among three faces
/// it has touched, the motion stops) and 17 (a body pinned in a corner
/// keeps no velocity into any of its faces). The ball in the corner of
/// three tilted walls, pushed into all three, keeps no velocity into any
/// and moves into none past the skin.
#[wasm_bindgen_test(unsupported = test)]
fn a_body_pressed_into_three_tilted_walls_keeps_no_velocity_into_them() {
    let faces = corner_faces();
    let velocity = -(faces[0] + faces[1] + faces[2]) * 2.0;
    let state = pressed(&faces, velocity);
    for n in faces {
        assert!(state.velocity.dot(n) > -1e-4, "{n}: {state:?}");
        assert!(
            state.position.dot(n) > -(SKIN / 64.0) - 1e-5,
            "{n}: {state:?}"
        );
    }
}

/// Defect: a mover that accepts a negative or non-finite step height or
/// ledge drop and carries on with it. Oracle: physics.md 17 (`MoverOptions`
/// lengths are finite and non-negative, or `move_and_slide` panics).
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn move_and_slide_refuses_a_negative_or_non_finite_length() {
    let floor = [Collider::new(
        1,
        Shape::Box {
            half: Vec2::new(5.0, 0.5),
        },
        Vec2::ZERO,
    )];
    let refused = |options: MoverOptions| {
        std::panic::catch_unwind(move || {
            let mut state = MoverState {
                position: Vec2::new(0.0, 1.01),
                ..MoverState::default()
            };
            move_and_slide(
                &mut state,
                &Shape::Box {
                    half: Vec2::new(0.4, 0.5),
                },
                0.1,
                &floor[..],
                &config(Vec2::Y),
                &options,
            )
        })
        .is_err()
    };
    assert!(!refused(MoverOptions::default()));
    for options in [
        MoverOptions {
            step_height: -0.1,
            ..MoverOptions::default()
        },
        MoverOptions {
            step_height: f32::NAN,
            ..MoverOptions::default()
        },
        MoverOptions {
            ledge_drop: Some(-1.0),
            ..MoverOptions::default()
        },
        MoverOptions {
            ledge_drop: Some(f32::INFINITY),
            ..MoverOptions::default()
        },
    ] {
        assert!(refused(options), "{options:?}");
    }
}

/// A rider of half-extents 0.4 by 0.5 standing a skin above `top`, its
/// floor id 5, at rest.
fn rider_on(top: f32) -> MoverState<Vec2> {
    MoverState {
        position: Vec2::new(0.0, top + 0.5 + SKIN),
        velocity: Vec2::ZERO,
        floor: Some(sgl_physics::Floor {
            normal: Vec2::Y,
            id: 5,
        }),
    }
}

fn step_rider(state: &mut MoverState<Vec2>, world: &[Collider<Vec2>]) {
    move_and_slide(
        state,
        &Shape::Box {
            half: Vec2::new(0.4, 0.5),
        },
        0.1,
        world,
        &config(Vec2::Y),
        &MoverOptions::default(),
    )
    .unwrap();
}

/// Defect: a carry that takes a collider of the floor's id further below
/// when one lies where the ground probe looks, because the near search
/// looks the wrong way. Oracle: physics.md 21, Carry (the floor is found
/// among the colliders whose box meets the body's box swept down where the
/// ground probe looks, or, when none does there, as far again below as
/// the body is tall; the lowest in the fixed order where ids repeat).
/// Under the rider, a platform with its floor's id moving right, and 0.6
/// below it another with that id, earlier in the fixed order (its x is
/// lower), moving left: the rider is carried right, by the near one.
#[wasm_bindgen_test(unsupported = test)]
fn a_carry_takes_the_nearest_collider_of_the_floors_id() {
    let near = Collider::new(
        5,
        Shape::Box {
            half: Vec2::new(2.0, 0.25),
        },
        Vec2::new(0.0, -0.25),
    )
    .with_velocity(Vec2::new(1.0, 0.0));
    let far = Collider::new(
        5,
        Shape::Box {
            half: Vec2::new(2.0, 0.1),
        },
        Vec2::new(-1.0, -0.7),
    )
    .with_velocity(Vec2::new(-1.0, 0.0));
    let mut state = rider_on(0.0);
    step_rider(&mut state, &[far, near]);
    assert!((state.position.x - 0.1).abs() < 1e-6, "{state:?}");
}

/// Defect: a carry that finds a platform further below than the contract's
/// search, so a lift that dropped away still pulls its rider. Oracle:
/// physics.md 21, Carry (the second search reaches as far again below as
/// the body is tall, past the ground probe; beyond it nothing is carried).
/// Lifts that dropped 1.5 and 2 units in a step from under a rider 1 tall
/// (the probe reaching 0.1) are not found: the rider moves by its own
/// velocity alone. One that dropped 0.9 is, and carries it down.
#[wasm_bindgen_test(unsupported = test)]
fn a_lift_that_dropped_further_than_the_search_is_not_found() {
    for (drop, carried) in [(0.9, true), (1.5, false), (2.0, false)] {
        let lift = Collider::new(
            5,
            Shape::Box {
                half: Vec2::new(2.0, 0.25),
            },
            Vec2::new(0.0, -0.25 - drop),
        )
        .with_velocity(Vec2::new(0.0, -drop / 0.1));
        let start = rider_on(0.0);
        let mut state = start;
        step_rider(&mut state, &[lift]);
        let fell = start.position.y - state.position.y;
        if carried {
            assert!((fell - drop).abs() < 1e-5, "{drop}: {state:?}");
        } else {
            assert_eq!(fell, 0.0, "{drop}: {state:?}");
        }
    }
}

/// Defect: a slide that checks only its first pass for an end `f32` can
/// hold, so a later pass, turned along a face, carries a finite body to an
/// infinite place. Oracle: physics.md 13 (a finite body never moves to an
/// infinite place; a move whose end `f32` cannot hold moves nothing and
/// reports no contact) and the geometry of a leaning face: a box near the
/// bottom of `f32` moves along x, inside `f32`, into a ramp's face whose
/// normal is `(-1, -1) / √2`, and the rest of its motion turns along the
/// face, down, past `-f32::MAX`. Everything is scaled to `1e34` so the
/// skin is above the spacing of `f32` there (12).
#[wasm_bindgen_test(unsupported = test)]
fn a_slide_whose_later_pass_would_end_past_f32_moves_nothing() {
    let scale = 1e34f32;
    let config = MoverConfig::new(MoverSettings {
        up: Vec2::Y,
        skin: scale / 100.0,
        snap_distance: scale / 10.0,
        normal_threshold: 0.7,
        block_epsilon: scale / 1e4,
    });
    let low = -f32::MAX + 4.0 * scale;
    let body = Shape::Box {
        half: Vec2::splat(scale),
    };
    let ramp = Collider::new(
        1,
        Shape::Box {
            half: Vec2::splat(2.0 * scale),
        },
        Vec2::new(5.0 * scale, low),
    )
    .with_cut(Cut::new(Vec2::new(-1.0, -1.0), 0.0));
    let delta = Vec2::new(20.0 * scale, 0.0);
    let start = Vec2::new(0.0, low);
    assert!(
        (start + delta).is_finite(),
        "the first pass ends inside f32"
    );
    let (moved, contacts) = slide(&body, start, delta, &[ramp][..], u32::MAX, &config).unwrap();
    assert_eq!(moved, Vec2::ZERO, "{contacts:?}");
    assert!(
        !(contacts.floor || contacts.wall || contacts.ceiling),
        "{contacts:?}"
    );
    // The same move a long way above the bottom of `f32` meets the face
    // and slides down it.
    let high = Vec2::new(0.0, 0.0);
    let ramp_high = Collider {
        position: Vec2::new(5.0 * scale, 0.0),
        ..ramp
    };
    let (moved, contacts) = slide(&body, high, delta, &[ramp_high][..], u32::MAX, &config).unwrap();
    assert!(contacts.ceiling || contacts.wall, "{contacts:?}");
    assert!(moved.y < -5.0 * scale, "{moved}");
}

fn a_walk_on_one_floor_under_a_tilted_up_keeps_its_run<V: Vector>(seed: u64) {
    let mut rng = Rng::new(seed);
    let mut walks = 0;
    for _ in 0..200 {
        // `up` tilted up to about 34° from y, so the box's top (+y) is a
        // floor (within `normal_threshold`, 0.7, of `up`).
        let mut up = V::ZERO;
        up[1] = 1.0;
        for axis in [0, 2] {
            if axis < V::AXES {
                up[axis] = rng.range(-0.45, 0.45);
            }
        }
        let config = MoverConfig::new(MoverSettings {
            up,
            skin: SKIN,
            snap_distance: 0.1,
            normal_threshold: 0.7,
            block_epsilon: 1e-4,
        });
        let up = config.up();
        let mut half = V::ZERO;
        half[1] = 1.0;
        let mut ground_at = V::ZERO;
        ground_at[1] = -1.0;
        for axis in [0, 2] {
            if axis < V::AXES {
                half[axis] = 1000.0;
            }
        }
        let ground = Collider::new(1, Shape::Box { half }, ground_at);
        let body = Shape::Box {
            half: audit_support::splat(0.5),
        };
        let pressing = rng.range(0.1, 3.0);
        let run: V = rng.vector(-5.0, 5.0);
        let across = run - up * dot(run, up);
        let velocity = across - up * pressing;
        let mut position = V::ZERO;
        position[1] = 0.5 + SKIN;
        let mut floor_normal = V::ZERO;
        floor_normal[1] = 1.0;
        let mut state = MoverState {
            position,
            velocity,
            floor: Some(Floor {
                normal: floor_normal,
                id: 1,
            }),
        };
        let contacts = move_and_slide(
            &mut state,
            &body,
            1.0 / 60.0,
            &[ground][..],
            &config,
            &MoverOptions::default(),
        )
        .unwrap();
        assert!(contacts.floor && !contacts.wall, "{contacts:?}");
        walks += 1;
        // The velocity less its part along `up`, in `f64`.
        let along: f64 = (0..V::AXES)
            .map(|axis| f64::from(velocity[axis]) * f64::from(up[axis]))
            .sum();
        for axis in 0..V::AXES {
            let expected = f64::from(velocity[axis]) - f64::from(up[axis]) * along;
            assert!(
                (f64::from(state.velocity[axis]) - expected).abs() <= 1e-5,
                "up {up:?}: {velocity:?} became {:?}, expected its run across up",
                state.velocity
            );
        }
    }
    assert_eq!(walks, 200);
}

/// Defect: a later face parallel to one already met (the same floor met by
/// the move and again by the ground probe) taken as a crease, so the
/// velocity a body walks with stops dead when rounding leaves a hair of it
/// pointing into the earlier face. Oracle: physics.md 16 (faces parallel
/// to one already met add no constraint) and 17 (a floor removes the part
/// of the velocity against `up` and keeps its part across `up`). Under a
/// tilted `up`, where removing the part along `up` leaves such a hair, a
/// body pressing into a wide floor while it walks keeps its velocity less
/// its part along `up`, worked in `f64`.
#[wasm_bindgen_test(unsupported = test)]
fn a_walk_on_one_floor_under_a_tilted_up_keeps_its_run_in_2d() {
    a_walk_on_one_floor_under_a_tilted_up_keeps_its_run::<Vec2>(0x0924_E301);
}

#[wasm_bindgen_test(unsupported = test)]
fn a_walk_on_one_floor_under_a_tilted_up_keeps_its_run_in_3d() {
    a_walk_on_one_floor_under_a_tilted_up_keeps_its_run::<Vec3>(0x0924_E302);
}
