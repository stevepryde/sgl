//! Audit of how the slices of `sgl-physics` work together (#924): round
//! bodies on moving colliders and ramps, a door pushing into a one-way
//! platform, a soft crowd on a lift, `find_ledge` with round bodies and
//! moving colliders, triggers and `move_and_collide` with every shape, a
//! cell world paired with a moving set, and a mover predicted with
//! `sgl_core::prediction` across a ride and a push. Expected values come
//! from the shapes' definitions in `f64`, the contract's sentences, or an
//! independent run, never from reading the crate. Runs natively and in a
//! wasm runtime.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::float_cmp,
    clippy::too_many_lines,
    clippy::trivially_copy_pass_by_ref
)]

mod audit_support;

use audit_support::flat::{self, separation};
use audit_support::solid;
use audit_support::{Rng, SKIN, config};
use sgl_core::math::{Vec2, Vec3};
use sgl_core::prediction::Predictor;
use sgl_physics::{
    ALL_LAYERS, CellColliders, CellWorld, Collider, ColliderKind, ColliderSet, Contacts, Convex,
    Cut, Floor, MoverOptions, MoverState, Push, Shape, World, find_ledge, move_and_collide,
    move_and_slide, sensors_at,
};
use wasm_bindgen_test::wasm_bindgen_test;

const DT: f32 = 1.0 / 60.0;

fn v2(x: f32, y: f32) -> Vec2 {
    Vec2::new(x, y)
}

fn v3(x: f32, y: f32, z: f32) -> Vec3 {
    Vec3::new(x, y, z)
}

/// The capsule walker: 0.6 wide, 1.8 tall, its centre 0.9 above its
/// lowest point.
fn walker() -> Shape<Vec3> {
    Shape::Capsule {
        half_axis: v3(0.0, 0.6, 0.0),
        radius: 0.3,
    }
}

/// Defect: a round rider that a lift does not carry, that sinks into the
/// lift or the ramp it walks onto, or that is slowed or stopped where the
/// lift meets the ramp. Oracle: physics.md 21 (a body standing on a moving
/// collider is carried as it moved), 16 (a walkable slope keeps a walk's
/// motion across `up`), 17 (a body standing under gravity stays within the
/// skin of its floor) and the shapes' definitions. A capsule rides a lift
/// from the ground up to the foot of a 45 degree ramp, walks off it up the
/// ramp onto a plateau: carried every step the lift moves, inside nothing
/// at any step, on the ground at every step once walking, and standing
/// within the skin of the plateau at the end.
#[wasm_bindgen_test(unsupported = test)]
fn a_capsule_rides_a_lift_onto_a_ramp() {
    let config = config(Vec3::Y);
    let ground = Collider::new(
        1,
        Shape::Box {
            half: v3(20.0, 0.5, 20.0),
        },
        v3(0.0, -0.5, 0.0),
    );
    // The ramp rises from y = 1 at x = 2 to y = 2 at x = 3 (its plane
    // y = x - 1 is half a unit along its normal (-1, 1) / sqrt 2 from its
    // centre); the plateau stays at 2 to x = 8.
    let ramp = Collider::new(
        2,
        Shape::Box {
            half: v3(0.5, 1.0, 2.0),
        },
        v3(2.5, 1.0, 0.0),
    )
    .with_cut(Cut::new(
        v3(-1.0, 1.0, 0.0),
        0.5 * core::f32::consts::FRAC_1_SQRT_2,
    ));
    let plateau = Collider::new(
        4,
        Shape::Box {
            half: v3(2.5, 1.0, 2.0),
        },
        v3(5.5, 1.0, 0.0),
    );
    let mut state = MoverState {
        position: v3(0.0, 0.9 + SKIN, 0.0),
        velocity: Vec3::ZERO,
        floor: Some(Floor {
            normal: Vec3::Y,
            id: 5,
        }),
    };
    let mut lift_top = 0.0f32;
    let mut carried = 0;
    for tick in 0..200 {
        // The lift (x -2..2) rises at 1.5 a second until its top is level
        // with the ramp's foot, y = 1.
        let speed = if lift_top < 1.0 { 1.5 } else { 0.0 };
        lift_top = (lift_top + speed * DT).min(1.0);
        let lift = Collider::new(
            5,
            Shape::Box {
                half: v3(2.0, 0.25, 2.0),
            },
            v3(0.0, lift_top - 0.25, 0.0),
        )
        .with_velocity(v3(0.0, speed, 0.0));
        let world = [ground, ramp, plateau, lift];
        let walk = if speed == 0.0 { 2.0 } else { 0.0 };
        state.velocity.x = walk;
        state.velocity.y -= 20.0 * DT;
        let contacts = move_and_slide(
            &mut state,
            &walker(),
            DT,
            &world[..],
            &config,
            &MoverOptions::default(),
        )
        .unwrap();
        carried += usize::from(contacts.carried_by == Some(5));
        for c in &world {
            let into = solid::depth(&walker(), state.position, |p| solid::to_collider(p, c));
            assert!(
                into <= 1e-5,
                "tick {tick}: {into} inside {}: {state:?}",
                c.id
            );
        }
        if tick > 100 && state.position.x < 7.0 {
            assert!(
                state.floor.is_some(),
                "tick {tick}: off the ground: {state:?}"
            );
        }
    }
    assert!(carried >= 35, "carried {carried} steps");
    let gap = f64::from(state.position.y) - 0.9 - 2.0;
    assert!(
        state.position.x > 4.0 && gap >= -1e-5 && gap <= f64::from(SKIN) * 65.0 / 64.0 + 1e-5,
        "{state:?}: {gap} above the plateau"
    );
}

/// Defect: a door that crushes a ball against a one-way platform's side
/// (as if the side were a wall), or a one-way platform that pushes the
/// ball out. Oracle: physics.md 9 (a one-way platform blocks only motion
/// into a floor-classified face), 17 (one-way colliders never push) and 21
/// (a body is `crushed` only when held between a moving collider and
/// something else that blocks it), with the shapes' definitions. A ball on
/// the ground, a door sliding into it from the left, a one-way deck to its
/// right at its height: the ball is pressed into the deck's side and on
/// with the door, never crushed or stuck, never inside the door.
#[wasm_bindgen_test(unsupported = test)]
fn a_door_pushes_a_ball_into_a_one_way_platform_side() {
    let config = config(Vec2::Y);
    let ball = Shape::Ball { radius: 0.4 };
    let ground = Collider::new(
        1,
        Shape::Box {
            half: v2(20.0, 0.5),
        },
        v2(0.0, -0.5),
    );
    let deck = Collider::new(2, Shape::Box { half: v2(2.0, 0.3) }, v2(3.0, 0.5))
        .with_kind(ColliderKind::OneWay);
    let mut state = MoverState {
        position: v2(0.3, 0.4 + SKIN),
        velocity: Vec2::ZERO,
        floor: Some(Floor {
            normal: Vec2::Y,
            id: 1,
        }),
    };
    let mut door_x = -0.8f32;
    for tick in 0..60 {
        door_x += 3.0 * DT;
        let door = Collider::new(
            3,
            Shape::Box {
                half: v2(0.25, 2.0),
            },
            v2(door_x, 2.0),
        )
        .with_velocity(v2(3.0, 0.0));
        let world = [ground, deck, door];
        state.velocity.y -= 20.0 * DT;
        let contacts = move_and_slide(
            &mut state,
            &ball,
            DT,
            &world[..],
            &config,
            &MoverOptions::default(),
        )
        .unwrap();
        let why = format!("tick {tick}: {state:?} {contacts:?}");
        assert!(!contacts.crushed && !contacts.stuck, "{why}");
        let placed = flat::place(&ball, flat::p(state.position), (1.0, 0.0));
        for solid in [ground, door] {
            assert!(
                separation(&placed, &flat::collider(&solid)) > -1e-5,
                "{why}"
            );
        }
    }
    // The ball went with the door, past the deck's side at x = 1.
    assert!(state.position.x > 2.0, "{state:?}");
}

/// Defect: a soft crowd on a moving lift that the lift fails to carry, that
/// sinks into it, or whose members are nudged by more than their limits or
/// never part. Oracle: physics.md 21 (a body on a moving collider is
/// carried; soft colliders nudge by at most their limit a step; bodies
/// steps in ascending id, each against the others as they stand) and the
/// boxes' definitions. Three riders overlapping one another stand on a
/// lift moving up and sideways, each kept in the set as a soft collider:
/// every step each is carried and stands within the skin of the lift's
/// top, moves across by the lift's motion and no more than the limits of
/// the others besides, and at the end they overlap less than at the start.
#[wasm_bindgen_test(unsupported = test)]
fn a_soft_crowd_rides_a_moving_lift() {
    let config = config(Vec2::Y);
    let body = Shape::Box { half: v2(0.3, 0.5) };
    let limit = 0.02;
    let lift_velocity = v2(1.5, 1.0);
    let mut lift_at = v2(0.0, -0.25);
    let mut riders = [(10u64, -0.4f32), (11, 0.0), (12, 0.45)].map(|(id, x)| {
        (
            id,
            MoverState {
                position: v2(x, 0.5 + SKIN),
                velocity: Vec2::ZERO,
                floor: Some(Floor {
                    normal: Vec2::Y,
                    id: 5,
                }),
            },
        )
    });
    let overlap = |riders: &[(u64, MoverState<Vec2>)]| {
        let mut most = 0.0f32;
        for i in 0..riders.len() {
            for j in i + 1..riders.len() {
                most = most.max(0.6 - (riders[i].1.position.x - riders[j].1.position.x).abs());
            }
        }
        most
    };
    let at_start = overlap(&riders);
    let mut set = ColliderSet::new(2.0);
    for (id, state) in &riders {
        set.insert(Collider::new(*id, body, state.position).with_push(Push::Soft(limit)));
    }
    for tick in 0..40 {
        lift_at += lift_velocity * DT;
        let lift = Collider::new(
            5,
            Shape::Box {
                half: v2(3.0, 0.25),
            },
            lift_at,
        )
        .with_velocity(lift_velocity);
        set.insert(lift);
        for (id, state) in &mut riders {
            set.remove(*id);
            let before = state.position;
            state.velocity.y -= 20.0 * DT;
            let contacts =
                move_and_slide(state, &body, DT, &set, &config, &MoverOptions::default()).unwrap();
            let why = format!("tick {tick}, rider {id}: {state:?} {contacts:?}");
            assert_eq!(contacts.carried_by, Some(5), "{why}");
            let gap = state.position.y - 0.5 - (lift_at.y + 0.25);
            assert!((-1e-5..=SKIN + 1e-5).contains(&gap), "{why}: {gap}");
            let across = (state.position.x - before.x) - lift_velocity.x * DT;
            assert!(across.abs() <= 2.0 * limit + 1e-5, "{why}: nudged {across}");
            set.insert(
                Collider::new(*id, body, state.position)
                    .with_velocity(state.velocity)
                    .with_push(Push::Soft(limit)),
            );
        }
    }
    assert!(
        overlap(&riders) < at_start - 0.05,
        "{} then {}",
        at_start,
        overlap(&riders)
    );
}

/// Defect: a `find_ledge` that reads a collider's velocity, counts a soft
/// collider as solid, or places a round body inside the wall it hangs from.
/// Oracle: physics.md 18 (the top edge within reach; hang beside the wall,
/// the body's top level with the edge plus `skin`; stand a `skin` above the
/// top and past the edge; refuse a ledge with anything solid where the
/// body would stand) and 21 (`find_ledge` takes every collider where it
/// stands and does not count a soft one), with the shapes' definitions. A
/// capsule pressed against a wall whose top is 0.5 above its head finds
/// the ledge: hanging with its top a skin above the edge, standing clear
/// of the wall a skin above its top. The wall moving, or soft crates where
/// it would hang and stand, change nothing, to the bit; a rigid crate
/// where it would stand refuses it.
#[wasm_bindgen_test(unsupported = test)]
fn find_ledge_reads_round_bodies_and_moving_and_soft_colliders_as_written() {
    let config = config(Vec3::Y);
    let wall = Collider::new(
        7,
        Shape::Box {
            half: v3(1.0, 1.2, 2.0),
        },
        v3(1.31, 1.2, 0.0),
    );
    let at = v3(0.0, 0.9, 0.0);
    let toward = Vec3::X;
    let find = |world: &[Collider<Vec3>]| {
        find_ledge(&walker(), at, toward, world, &config, 1.0, ALL_LAYERS).unwrap()
    };
    let ledge = find(&[wall]).expect("the wall's top is within reach");
    assert_eq!(ledge.id, 7);
    // Hanging: its top (0.9 above its centre) a skin above the edge, 2.4.
    let top = f64::from(ledge.hang.y) + 0.9;
    assert!((top - 2.4 - f64::from(SKIN)).abs() < 1e-5, "{ledge:?}");
    // Standing: past the edge, clear of the wall, a skin above its top.
    let into = solid::depth(&walker(), ledge.stand, |p| solid::to_collider(p, &wall));
    let lowest = f64::from(ledge.stand.y) - 0.9;
    assert!(
        into <= 0.0 && ledge.stand.x > 0.31 && (lowest - 2.4 - f64::from(SKIN)).abs() < 1e-5,
        "{ledge:?}"
    );
    let moving = wall.with_velocity(v3(0.0, 5.0, 1.0));
    assert_eq!(find(&[moving]), Some(ledge));
    let crate_at = ledge.stand;
    let soft = |at| {
        Collider::new(
            8,
            Shape::Box {
                half: Vec3::splat(0.3),
            },
            at,
        )
        .with_push(Push::Soft(0.05))
    };
    assert_eq!(find(&[wall, soft(crate_at), soft(ledge.hang)]), Some(ledge));
    let rigid = Collider::new(
        8,
        Shape::Box {
            half: Vec3::splat(0.3),
        },
        crate_at,
    );
    assert_eq!(find(&[wall, rigid]), None);
}

/// A random 2D shape of every kind, of play size.
fn any_shape(rng: &mut Rng) -> Shape<Vec2> {
    match rng.below(4) {
        0 => Shape::Box {
            half: v2(rng.range(0.1, 0.6), rng.range(0.1, 0.6)),
        },
        1 => Shape::Ball {
            radius: rng.range(0.1, 0.6),
        },
        2 => Shape::Capsule {
            half_axis: rng.vector(-0.4, 0.4),
            radius: rng.range(0.05, 0.3),
        },
        _ => {
            let turn = rng.range(0.0, 6.2);
            Shape::Convex(Convex::polygon(
                &[v2(-0.4, -0.3), v2(0.5, -0.2), v2(0.2, 0.4), v2(-0.3, 0.3)],
                turn.cos(),
                turn.sin(),
            ))
        }
    }
}

/// A random 2D collider of every kind, a box cut into a ramp among them.
fn any_collider(rng: &mut Rng, id: u64) -> Collider<Vec2> {
    let at = v2(rng.range(-3.0, 3.0), rng.range(-3.0, 3.0));
    let shape = any_shape(rng);
    let collider = Collider::new(id, shape, at);
    if matches!(shape, Shape::Box { .. }) && rng.chance(40) {
        collider.with_cut(Cut::new(v2(rng.range(-1.0, 1.0), 1.0), 0.0))
    } else {
        collider
    }
}

/// Defect: a trigger list that misses a sensor a body overlaps, lists one
/// it only touches or is clear of, or lists one twice; or a
/// `move_and_collide` of some pair of shapes that ends inside a solid or
/// nearer than the skin. Oracle: physics.md 21, Triggers (`sensors_at`
/// lists the ids of the sensors the shape overlaps, ascending, each once;
/// touching is not overlap) and 15 (`move_and_collide` stops a `skin`
/// short along its path), with the shapes' definitions in `f64`. Random
/// bodies of every shape among random sensors and solids of every shape,
/// ramps included; cases within a ten-thousandth of touching are left out.
#[wasm_bindgen_test(unsupported = test)]
fn sensors_at_and_move_and_collide_read_every_shape() {
    let mut rng = Rng::new(0x0924_2301);
    let config = config(Vec2::Y);
    let (mut listed, mut hits) = (0, 0);
    for case in 0..150 {
        // Ids repeat, as they may in a slice: each is listed once.
        let sensors: Vec<Collider<Vec2>> = (0..8)
            .map(|i| any_collider(&mut rng, 100 + i % 3).with_kind(ColliderKind::Sensor))
            .collect();
        let body = any_shape(&mut rng);
        let at = v2(rng.range(-3.0, 3.0), rng.range(-3.0, 3.0));
        let placed = flat::place(&body, flat::p(at), (1.0, 0.0));
        let gaps: Vec<(u64, f64)> = sensors
            .iter()
            .map(|s| (s.id, separation(&placed, &flat::collider(s))))
            .collect();
        if gaps.iter().all(|&(_, gap)| gap.abs() > 1e-4) {
            let mut expected: Vec<u64> = gaps
                .iter()
                .filter(|&&(_, gap)| gap < 0.0)
                .map(|&(id, _)| id)
                .collect();
            expected.sort_unstable();
            expected.dedup();
            let found = sensors_at(&body, at, &sensors[..], ALL_LAYERS).unwrap();
            assert_eq!(
                found, expected,
                "case {case}: {body:?} at {at} among {sensors:?}"
            );
            listed += found.len();
        }
        let solids: Vec<Collider<Vec2>> = (0..5).map(|i| any_collider(&mut rng, i)).collect();
        let clear = solids
            .iter()
            .all(|s| separation(&placed, &flat::collider(s)) > f64::from(SKIN));
        if !clear {
            continue;
        }
        let delta: Vec2 = rng.vector(-4.0, 4.0);
        let (moved, hit) =
            move_and_collide(&body, at, delta, &solids[..], ALL_LAYERS, &config).unwrap();
        let end = flat::place(&body, flat::p(at + moved), (1.0, 0.0));
        let nearest = solids
            .iter()
            .map(|s| separation(&end, &flat::collider(s)))
            .fold(f64::INFINITY, f64::min);
        let why = format!("case {case}: {body:?} from {at} by {delta}: {moved:?} {hit:?}");
        assert!(nearest > -1e-5, "{why}: inside by {nearest}");
        if hit.is_some() {
            hits += 1;
            assert!(
                nearest >= f64::from(SKIN) - 1e-5,
                "{why}: {nearest} from the nearest"
            );
        } else {
            assert_eq!(moved, delta, "{why}");
        }
    }
    assert!(
        listed >= 25 && hits >= 25,
        "{listed} sensors listed, {hits} hits"
    );
}

/// Defect: a pair of a cell world and a moving set that answers unlike the
/// same colliders in one slice: a carry or a ground probe that reads only
/// one world, or a cell world that misses the lift's neighbours. Oracle:
/// physics.md 10 (a pair offers both worlds' colliders) and 6 (results
/// never depend on the world or the visiting order). A capsule walker
/// crossing a voxel floor onto a lift kept in a set and moved by id each
/// step, riding it up and walking off onto a voxel ledge, steps bit-equal
/// through the pair, the pair the other way round, and one slice of every
/// collider.
#[wasm_bindgen_test(unsupported = test)]
fn a_cell_world_paired_with_a_moving_set_steps_as_one_slice() {
    let config = config(Vec3::Y);
    let floor_cell = |cell: sgl_core::math::IVec3| {
        // The ground under x < 3 and a ledge two cells up over x >= 6.
        (cell.y == -1 && cell.x < 3 || cell.y == 1 && cell.x >= 6)
            && (-4..8).contains(&cell.x)
            && (-2..2).contains(&cell.z)
    };
    let cells = CellWorld::new(
        1.0,
        move |cell: sgl_core::math::IVec3, out: &mut CellColliders<'_, Vec3>| {
            if floor_cell(cell) {
                out.push(Collider::new(
                    1,
                    Shape::Box {
                        half: Vec3::splat(0.5),
                    },
                    Vec3::splat(0.5),
                ));
            }
        },
    );
    let mut voxels = Vec::new();
    for x in -4..8 {
        for y in -1..2 {
            for z in -2..2 {
                let cell = sgl_core::math::IVec3::new(x, y, z);
                if floor_cell(cell) {
                    voxels.push(Collider::new(
                        1,
                        Shape::Box {
                            half: Vec3::splat(0.5),
                        },
                        cell.as_vec3() + Vec3::splat(0.5),
                    ));
                }
            }
        }
    }
    let mut set = ColliderSet::new(2.0);
    let mut lift = Collider::new(
        9,
        Shape::Box {
            half: v3(1.5, 0.25, 1.5),
        },
        v3(4.5, -0.25, 0.0),
    );
    set.insert(lift);
    let start = MoverState {
        position: v3(0.5, 0.9 + SKIN, 0.0),
        velocity: Vec3::ZERO,
        floor: Some(Floor {
            normal: Vec3::Y,
            id: 1,
        }),
    };
    let (mut a, mut b, mut c) = (start, start, start);
    let options = MoverOptions {
        step_height: 0.3,
        ..MoverOptions::default()
    };
    let mut rode = 0;
    for tick in 0..240 {
        // The lift rises two cells while the walker is on it, then stops.
        let speed = if (90..150).contains(&tick) { 2.0 } else { 0.0 };
        lift.position.y += speed * DT;
        lift.velocity = v3(0.0, speed, 0.0);
        assert!(set.set_position(9, lift.position) && set.set_velocity(9, lift.velocity));
        let walk = if (90..160).contains(&tick) { 0.0 } else { 2.0 };
        let mut slice = voxels.clone();
        slice.push(lift);
        let mut outs: Vec<(MoverState<Vec3>, Contacts<Vec3>)> = Vec::new();
        for (state, world) in [
            (&mut a, &(&cells, &set) as &dyn World<Vec3>),
            (&mut b, &(&set, &cells)),
            (&mut c, &&slice[..]),
        ] {
            state.velocity.x = walk;
            state.velocity.y -= 20.0 * DT;
            let contacts = move_and_slide(state, &walker(), DT, world, &config, &options).unwrap();
            outs.push((*state, contacts));
        }
        assert_eq!(outs[0], outs[2], "tick {tick}: the pair and the slice");
        assert_eq!(
            outs[1], outs[2],
            "tick {tick}: the pair reversed and the slice"
        );
        rode += usize::from(outs[2].1.carried_by == Some(9));
    }
    assert!(
        rode >= 50 && c.position.x > 6.0 && c.position.y > 2.5,
        "rode {rode}: {c:?}"
    );
}

/// What a player asks for each tick.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Input {
    walk: f32,
    jump: bool,
}

/// The world at `tick`: a floor, a lift that rises and falls with time,
/// and a door that slides across it, both moved by the game before the
/// step, with their velocities.
fn world_at(tick: u64) -> [Collider<Vec2>; 3] {
    let phase = (tick % 120) as f32;
    let lift_speed = if phase < 60.0 { 1.5 } else { -1.5 };
    let lift_y = if phase < 60.0 { phase } else { 120.0 - phase } * 1.5 * DT - 0.25;
    let door_speed = 2.0;
    let door_x = -3.0 + ((tick % 240) as f32) * door_speed * DT;
    [
        Collider::new(
            1,
            Shape::Box {
                half: v2(20.0, 0.5),
            },
            v2(0.0, -1.5),
        ),
        Collider::new(
            5,
            Shape::Box {
                half: v2(2.0, 0.25),
            },
            v2(0.0, lift_y),
        )
        .with_velocity(v2(0.0, lift_speed)),
        Collider::new(
            6,
            Shape::Box {
                half: v2(0.25, 1.0),
            },
            v2(door_x, 1.5),
        )
        .with_velocity(v2(door_speed, 0.0)),
    ]
}

/// One fixed step and what it met: the function a server and its
/// client's predictor both call.
fn step_contacts(state: &mut MoverState<Vec2>, tick: u64, input: &Input) -> Contacts<Vec2> {
    let world = world_at(tick);
    state.velocity.x = input.walk;
    if input.jump && state.floor.is_some() {
        state.velocity.y = 6.0;
    }
    state.velocity.y -= 20.0 * DT;
    let body = Shape::Capsule {
        half_axis: v2(0.0, 0.3),
        radius: 0.3,
    };
    move_and_slide(
        state,
        &body,
        DT,
        &world[..],
        &config(Vec2::Y),
        &MoverOptions::default(),
    )
    .unwrap()
}

fn step(state: &mut MoverState<Vec2>, tick: u64, input: &Input) {
    step_contacts(state, tick, input);
}

/// Defect: a mover state that cannot be predicted and replayed (a step
/// that reads anything but its arguments, so the replay from an
/// acknowledged state drifts from the forward run), across a ride on a
/// lift and a push by a door. Oracle: physics.md 2 (restoring an earlier
/// value and repeating the same calls repeats the same results), core.md 9
/// (reconciling with the true state ends exactly where not reconciling
/// would) and an independent forward run. A client predicts 240 ticks of
/// walking, jumping, riding a lift and being pushed by a door, and is
/// acknowledged every seven ticks with the server's state three ticks
/// back: each reconcile corrects nothing and leaves the prediction at the
/// server's state for its tick, bit for bit. When the server ran a
/// different input at one tick, the reconcile after it ends where stepping
/// the client's later inputs from the server's state does, to the bit.
#[wasm_bindgen_test(unsupported = test)]
fn a_predicted_mover_replays_a_ride_and_a_push_bit_equal() {
    let start = MoverState {
        position: v2(-1.0, 0.6 + SKIN),
        velocity: Vec2::ZERO,
        floor: Some(Floor {
            normal: Vec2::Y,
            id: 5,
        }),
    };
    let input = |tick: u64| Input {
        walk: if (tick / 50).is_multiple_of(2) {
            1.0
        } else {
            -0.5
        },
        jump: tick % 70 == 69,
    };
    // The server's forward run, the state after each tick.
    let mut forward = vec![start];
    let (mut state, mut carried, mut pushed) = (start, false, false);
    for tick in 0..240 {
        let contacts = step_contacts(&mut state, tick, &input(tick));
        carried |= contacts.carried_by == Some(5);
        pushed |= contacts.pushed_by == Some(6);
        forward.push(state);
    }
    assert!(
        carried && pushed,
        "the run rides the lift ({carried}) and is pushed ({pushed})"
    );

    let mut client = Predictor::new(start, 0, 128);
    for tick in 0..240u64 {
        client.predict(input(tick), step).unwrap();
        if tick % 7 == 6 {
            let acked = tick - 3;
            let reconciled = client
                .reconcile(acked, forward[acked as usize + 1], step)
                .unwrap();
            assert!(
                !reconciled.corrected && reconciled.replayed == 3,
                "tick {tick}: {reconciled:?}"
            );
            assert_eq!(*client.state(), forward[tick as usize + 1], "tick {tick}");
        }
    }

    // The server ran a jump and a run at tick 100 that the client did not
    // predict; its state after tick 103 reaches the client at tick 110.
    let server_input = |tick: u64| {
        if tick == 100 {
            Input {
                walk: 2.0,
                jump: true,
            }
        } else {
            input(tick)
        }
    };
    let mut server = start;
    let mut truth = start;
    let mut client = Predictor::new(start, 0, 128);
    for tick in 0..=110u64 {
        step(&mut server, tick, &server_input(tick));
        if tick == 103 {
            truth = server;
        }
        client.predict(input(tick), step).unwrap();
    }
    let reconciled = client.reconcile(103, truth, step).unwrap();
    assert!(
        reconciled.corrected && reconciled.replayed == 7,
        "{reconciled:?}"
    );
    let mut expected = truth;
    for tick in 104..=110u64 {
        step(&mut expected, tick, &input(tick));
    }
    assert_eq!(*client.state(), expected);
}
