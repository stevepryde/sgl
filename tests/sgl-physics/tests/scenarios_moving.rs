//! Game scenarios with moving colliders (#920, #924 item 3): a platformer's
//! lift and crusher, a door, a crowd, boats and a pickup, each driven the
//! way a game drives it, a fixed step in which the game first moves its
//! colliders and then steps its bodies. Every expectation comes from the
//! requirement (physics.md 21) or from the level's geometry, worked in the
//! comment beside it. Runs natively and in a wasm runtime.
#![allow(clippy::cast_precision_loss)]

use sgl_core::math::{Vec2, Vec3};
use sgl_physics::{
    ALL_LAYERS, Collider, ColliderKind, ColliderSet, Contacts, MoverConfig, MoverOptions,
    MoverSettings, MoverState, Push, Shape, Vector, World, move_and_slide, sensor_events,
    sensors_at,
};
use wasm_bindgen_test::wasm_bindgen_test;

const SKIN: f32 = 0.01;
const DT: f32 = 1.0 / 60.0;
const GRAVITY: f32 = 20.0;
/// Rounding at these coordinates over a few hundred steps: far below the
/// skin.
const ROUND: f32 = 1e-4;

fn config<V: Vector>(up: V) -> MoverConfig<V> {
    MoverConfig::new(MoverSettings {
        up,
        skin: SKIN,
        snap_distance: 0.1,
        normal_threshold: 0.7,
        block_epsilon: 1e-4,
    })
}

fn cuboid<V>(half: V) -> Shape<V> {
    Shape::Box { half }
}

/// A slab whose top face is at `top`, spanning `left..right` in x.
fn slab(id: u64, left: f32, right: f32, top: f32) -> Collider<Vec2> {
    let half = Vec2::new((right - left) * 0.5, 0.5);
    Collider::new(id, cuboid(half), Vec2::new((left + right) * 0.5, top - 0.5))
}

fn step<V: Vector, W: World<V> + ?Sized>(
    state: &mut MoverState<V>,
    body: &Shape<V>,
    world: &W,
    up: V,
) -> Contacts<V> {
    let options = MoverOptions::default();
    move_and_slide(state, body, DT, world, &config(up), &options).expect("the world answers")
}

/// Whether two boxes share interior by more than `allowance`: plain
/// geometry, no crate code.
fn overlap<V: Vector>(a: V, a_half: V, b: V, b_half: V) -> f32 {
    (0..V::AXES)
        .map(|axis| a_half[axis] + b_half[axis] - (a[axis] - b[axis]).abs())
        .fold(f32::INFINITY, f32::min)
}

// ---------------------------------------------------------------- the lift

/// The platformer's player: 0.8 wide, 1 tall.
const PLAYER: Vec2 = Vec2::new(0.4, 0.5);

/// Defect: a lift that loses its rider on the way up or down, lets it sink
/// into the deck or float above it, or keeps it when it jumps. Oracle:
/// physics.md 21 and the level. A lift 2 wide (id 5) runs from its top at
/// y = 0 to y = 5 and back at 2 units a second, turning at each end, as
/// the game moves it. The player standing on it rides the whole travel up
/// and down: every step its floor is the lift and its feet are on the deck
/// or above it by no more than the skin. At the top of a second ride it
/// jumps: it leaves the lift (no floor), rises above the deck by more than
/// the skin, and comes down onto the ground beside the shaft or the lift.
/// While it rides, each step names the lift as what carried it.
#[wasm_bindgen_test(unsupported = test)]
fn a_player_rides_a_lift_through_its_travel_and_leaves_by_jumping() {
    let up = Vec2::Y;
    let body = cuboid(PLAYER);
    let ground = [slab(1, -10.0, -1.0, 0.0), slab(2, 1.0, 10.0, 0.0)];
    let mut lift = Collider::new(5, cuboid(Vec2::new(1.0, 0.25)), Vec2::new(0.0, -0.25));
    let mut set = ColliderSet::new(2.0);
    set.insert(lift);
    let mut state = MoverState {
        position: Vec2::new(0.0, 0.5 + 0.5 * SKIN),
        ..MoverState::default()
    };
    for _ in 0..3 {
        state.velocity.y -= GRAVITY * DT;
        step(&mut state, &body, &(&ground[..], &set), up);
    }
    let mut speed = 2.0f32;
    let mut turns = 0;
    let mut rode = 0;
    while turns < 3 {
        // The game moves its lift first, turning it at each end.
        let top = lift.position.y + 0.25;
        if (speed > 0.0 && top >= 5.0) || (speed < 0.0 && top <= 0.0) {
            speed = -speed;
            turns += 1;
        }
        lift.position.y += speed * DT;
        lift.velocity = Vec2::new(0.0, speed);
        set.insert(lift);
        state.velocity.y -= GRAVITY * DT;
        let contacts = step(&mut state, &body, &(&ground[..], &set), up);
        let deck = lift.position.y + 0.25;
        let feet = state.position.y - 0.5;
        assert_eq!(state.floor.map(|f| f.id), Some(5), "deck at {deck}");
        assert_eq!(contacts.carried_by, Some(5), "deck at {deck}");
        assert!(
            (-ROUND..=SKIN + ROUND).contains(&(feet - deck)),
            "feet {feet}, deck {deck}"
        );
        rode += 1;
    }
    // Up 5, down 5 and up 5 again at a thirtieth of a unit a step.
    assert!(rode >= 440, "{rode}");

    // At the top it jumps toward the ground on the right.
    state.velocity = Vec2::new(3.0, 8.0);
    let mut airborne = 0;
    let mut highest = f32::MIN;
    for _ in 0..120 {
        let top = lift.position.y + 0.25;
        if top <= 0.0 || top >= 5.0 {
            speed = -speed;
        }
        lift.position.y += speed * DT;
        lift.velocity = Vec2::new(0.0, speed);
        set.insert(lift);
        state.velocity.y -= GRAVITY * DT;
        step(&mut state, &body, &(&ground[..], &set), up);
        if state.floor.is_none() {
            airborne += 1;
            let above = (state.position.y - 0.5) - (lift.position.y + 0.25);
            highest = highest.max(above);
        } else if airborne > 0 {
            break;
        }
    }
    assert!(airborne > 10 && highest > SKIN, "{airborne} {highest}");
    assert!(state.floor.is_some(), "it lands");
}

/// Defect: a descending lift that drags a rider standing on the shaft's rim
/// down through the rim. Oracle: physics.md 21 (a carry that would press
/// the body into a solid is stopped there) and the level. The player stands
/// across the rim at x = 1, half on the lift (id 3, the lower id, so the
/// rider's floor is the lift) and half on the rim, a ledge 0.2 thick (id
/// 4). The lift drops away at 24 units a second, 0.4 a step, twice the
/// ledge's thickness: the player never enters the ledge or passes it,
/// stays standing on it (its feet within a skin of y = 0), and ends with
/// the ledge as its floor.
#[wasm_bindgen_test(unsupported = test)]
fn a_descending_lift_does_not_pull_its_rider_through_the_rim() {
    let up = Vec2::Y;
    let body = cuboid(PLAYER);
    let ledge = Collider::new(4, cuboid(Vec2::new(4.5, 0.1)), Vec2::new(5.5, -0.1));
    let ground = [ledge, slab(6, -10.0, -1.0, 0.0)];
    let mut lift = Collider::new(3, cuboid(Vec2::new(1.0, 0.25)), Vec2::new(0.0, -0.25));
    let mut set = ColliderSet::new(2.0);
    set.insert(lift);
    let mut state = MoverState {
        position: Vec2::new(1.0, 0.5 + 0.5 * SKIN),
        ..MoverState::default()
    };
    for _ in 0..3 {
        state.velocity.y -= GRAVITY * DT;
        step(&mut state, &body, &(&ground[..], &set), up);
    }
    assert_eq!(state.floor.map(|f| f.id), Some(3));
    lift.velocity = Vec2::new(0.0, -24.0);
    for n in 0..30 {
        lift.position.y += lift.velocity.y * DT;
        set.insert(lift);
        state.velocity.y -= GRAVITY * DT;
        step(&mut state, &body, &(&ground[..], &set), up);
        let feet = state.position.y - 0.5;
        assert!(
            (-ROUND..=SKIN + ROUND).contains(&feet),
            "step {n}: feet at {feet}"
        );
        assert!(overlap(state.position, PLAYER, ledge.position, Vec2::new(4.5, 0.1)) <= ROUND);
    }
    assert_eq!(state.floor.map(|f| f.id), Some(4));
}

/// Defect: a crusher that pushes its victim into the floor, lets it through,
/// or never says so. Oracle: physics.md 21 and the level: a block (id 8,
/// 2 wide) comes down at 3 units a second onto the player standing on the
/// ground. Once its underside is lower than the player is tall (1 unit) the
/// player cannot fit and every step reports `crushed` by it; while it is
/// more than a unit and two skins up, none does. The player never enters
/// the ground.
#[wasm_bindgen_test(unsupported = test)]
fn a_crusher_reports_crushed() {
    let up = Vec2::Y;
    let body = cuboid(PLAYER);
    let ground = [slab(1, -10.0, 10.0, 0.0)];
    let mut crusher = Collider::new(8, cuboid(Vec2::new(1.0, 0.5)), Vec2::new(0.0, 3.0))
        .with_velocity(Vec2::new(0.0, -3.0));
    let mut set = ColliderSet::new(2.0);
    set.insert(crusher);
    let mut state = MoverState {
        position: Vec2::new(0.0, 0.5 + 0.5 * SKIN),
        ..MoverState::default()
    };
    let mut held = 0;
    for n in 0..60 {
        crusher.position.y += crusher.velocity.y * DT;
        set.insert(crusher);
        state.velocity.y -= GRAVITY * DT;
        let contacts = step(&mut state, &body, &(&ground[..], &set), up);
        let underside = crusher.position.y - 0.5;
        assert!(state.position.y - 0.5 >= -ROUND, "step {n}: in the ground");
        if underside < 1.0 {
            assert!(contacts.crushed, "step {n}: underside {underside}");
            assert_eq!(contacts.pushed_by, Some(8));
            held += 1;
        } else if underside > 1.0 + 2.0 * SKIN + ROUND {
            assert!(!contacts.crushed, "step {n}: underside {underside}");
        }
    }
    assert!(held > 10, "{held}");
}

// ---------------------------------------------------------------- the door

/// Defect: a sliding door that passes through a body standing in its way,
/// knocks it off the floor or into the wall behind it. Oracle: physics.md
/// 21 and the room: a door (id 9) 0.2 thick slides along +z at 2 units a
/// second across a corridor where a player (0.6 wide and deep, 1.8 tall)
/// stands. Every step after it reaches the player, the player is ahead of
/// it, clear of its face by no more than the skin, still standing on the
/// floor, and moved only along z.
#[wasm_bindgen_test(unsupported = test)]
fn a_sliding_door_shoves_a_standing_player_aside() {
    let up = Vec3::Y;
    let half = Vec3::new(0.3, 0.9, 0.3);
    let body = cuboid(half);
    let floor = [Collider::new(
        1,
        cuboid(Vec3::new(10.0, 0.5, 10.0)),
        Vec3::new(0.0, -0.5, 0.0),
    )];
    let mut door = Collider::new(
        9,
        cuboid(Vec3::new(1.0, 1.25, 0.1)),
        Vec3::new(0.0, 1.25, -1.0),
    )
    .with_velocity(Vec3::new(0.0, 0.0, 2.0));
    let mut set = ColliderSet::new(2.0);
    set.insert(door);
    let mut state = MoverState {
        position: Vec3::new(0.25, 0.9 + 0.5 * SKIN, 0.0),
        ..MoverState::default()
    };
    let mut shoved = 0;
    for n in 0..90 {
        door.position.z += door.velocity.z * DT;
        set.insert(door);
        let reached = door.position.z + 0.1 > state.position.z - 0.3;
        state.velocity.y -= GRAVITY * DT;
        step(&mut state, &body, &(&floor[..], &set), up);
        let clear = (state.position.z - 0.3) - (door.position.z + 0.1);
        assert!(clear >= -ROUND, "step {n}: inside the door by {clear}");
        assert_eq!(state.position.x.to_bits(), 0.25f32.to_bits());
        assert_eq!(state.floor.map(|f| f.id), Some(1), "step {n}");
        if reached {
            assert!(clear <= SKIN + ROUND, "step {n}: {clear}");
            shoved += 1;
        }
    }
    assert!(shoved > 60, "{shoved}");
}

// --------------------------------------------------------------- the crowd

/// Defect: a crowd with soft contact that jams (a body stopped dead by
/// another, as rigid contact stops it), or whose bodies sink into one
/// another without bound. Oracle: physics.md 21. Twelve capsules (radius
/// 0.3) in a ring of radius 4 walk to its centre at 1.5 units a second, a
/// fortieth of a unit a step, stepped in ascending id against the others as
/// they stand, each a soft collider of limit 0.05. Soft contact never takes
/// a body's velocity, so after every step each keeps the walk the game gave
/// it. A round of steps closes two bodies by at most two walks, 0.05, which
/// the limit takes back, so no two overlap by more than that and a skin.
/// All of them reach the crowd at the centre: twelve discs of radius 0.3
/// pack within a circle of radius 1.25, and every body ends within 1.5.
#[wasm_bindgen_test(unsupported = test)]
fn a_soft_crowd_gathers_without_jamming_or_sinking() {
    let up = Vec3::Y;
    let body = Shape::Capsule {
        half_axis: Vec3::new(0.0, 0.5, 0.0),
        radius: 0.3,
    };
    let walk = 1.5;
    let limit = 0.05;
    let floor = [Collider::new(
        1000,
        cuboid(Vec3::new(20.0, 0.5, 20.0)),
        Vec3::new(0.0, -0.5, 0.0),
    )];
    // Ring positions from the twelfths of a turn, worked to f32.
    let ring: [(f32, f32); 12] = [
        (1.0, 0.0),
        (0.866_025_4, 0.5),
        (0.5, 0.866_025_4),
        (0.0, 1.0),
        (-0.5, 0.866_025_4),
        (-0.866_025_4, 0.5),
        (-1.0, 0.0),
        (-0.866_025_4, -0.5),
        (-0.5, -0.866_025_4),
        (0.0, -1.0),
        (0.5, -0.866_025_4),
        (0.866_025_4, -0.5),
    ];
    let mut crowd: Vec<MoverState<Vec3>> = ring
        .iter()
        .map(|&(c, s)| MoverState {
            position: Vec3::new(4.0 * c, 0.8 + 0.5 * SKIN, 4.0 * s),
            ..MoverState::default()
        })
        .collect();
    let collider = |id: u64, state: &MoverState<Vec3>| {
        Collider::new(id, body, state.position)
            .with_velocity(state.velocity)
            .with_push(Push::Soft(limit))
    };
    let mut set = ColliderSet::new(2.0);
    for (id, state) in (1u64..).zip(&crowd) {
        set.insert(collider(id, state));
    }
    for n in 0..300 {
        for (id, state) in (1u64..).zip(crowd.iter_mut()) {
            set.remove(id);
            let to = Vec3::new(-state.position.x, 0.0, -state.position.z);
            let toward = if to.length() > 0.05 {
                to.normalize() * walk
            } else {
                Vec3::ZERO
            };
            state.velocity = Vec3::new(toward.x, state.velocity.y - GRAVITY * DT, toward.z);
            step(state, &body, &(&floor[..], &set), up);
            assert_eq!(
                (state.velocity.x, state.velocity.z),
                (toward.x, toward.z),
                "{id} at {n}"
            );
            set.insert(collider(id, state));
        }
        for (i, a) in crowd.iter().enumerate() {
            for b in &crowd[i + 1..] {
                let apart = Vec2::new(a.position.x - b.position.x, a.position.z - b.position.z);
                let sunk = 0.6 - apart.length();
                assert!(sunk <= 2.0 * walk * DT + SKIN, "step {n}: {sunk}");
            }
        }
    }
    for state in &crowd {
        let from_centre = Vec2::new(state.position.x, state.position.z).length();
        assert!(from_centre < 1.5, "{from_centre}");
    }
}

// --------------------------------------------------------------- the boats

/// Defect: boats that sail through each other, or one stepped later that
/// ends inside one stepped earlier. Oracle: physics.md 21 and geometry: two
/// boats (rigid boxes 2 long and 0.8 wide, ids 1 and 2) on still water (no
/// gravity, nothing below) sail bow to bow along x, their keels 0.3 apart
/// across, stepped in ascending id against the other as it stands. They
/// never overlap; each is stopped by the other (a wall contact naming it);
/// and the helmsmen keep steering, so they stay pressed together, apart by
/// no more than two skins, as they were when they met: neither turns the
/// other along z.
#[wasm_bindgen_test(unsupported = test)]
fn two_boats_stepped_in_id_order_block_each_other() {
    let up = Vec3::Y;
    let half = Vec3::new(1.0, 0.3, 0.4);
    let hull = cuboid(half);
    let headings = [Vec3::new(3.0, 0.0, 0.0), Vec3::new(-3.0, 0.0, 0.0)];
    let mut boats = [
        MoverState {
            position: Vec3::new(-4.0, 0.3, 0.0),
            ..MoverState::default()
        },
        MoverState {
            position: Vec3::new(4.0, 0.3, 0.3),
            ..MoverState::default()
        },
    ];
    let collider = |id: u64, boat: &MoverState<Vec3>| {
        Collider::new(id, hull, boat.position).with_velocity(boat.velocity)
    };
    let mut set = ColliderSet::new(4.0);
    for (id, boat) in (1u64..).zip(&boats) {
        set.insert(collider(id, boat));
    }
    let mut blocked = [false; 2];
    for n in 0..120 {
        for (index, id) in [(0, 1u64), (1, 2)] {
            set.remove(id);
            boats[index].velocity = headings[index];
            let contacts = step(&mut boats[index], &hull, &set, up);
            if contacts.wall && contacts.last_wall_id == Some(3 - id) {
                blocked[index] = true;
            }
            set.insert(collider(id, &boats[index]));
        }
        let sunk = overlap(boats[0].position, half, boats[1].position, half);
        assert!(sunk <= ROUND, "step {n}: {sunk}");
    }
    assert_eq!(blocked, [true, true]);
    let gap = (boats[1].position.x - boats[0].position.x) - 2.0;
    assert!((0.0..=2.0 * SKIN + ROUND).contains(&gap), "{gap}");
    assert_eq!((boats[0].position.z, boats[1].position.z), (0.0, 0.3));
}

// -------------------------------------------------------------- the pickup

/// Defect: a pickup that fires twice, never, or on the wrong step. Oracle:
/// physics.md 21 and the level: a coin's sensor (id 40, half a unit
/// across, at x = 3) on the player's path; the player walks from x = 0 to
/// x = 6 at 4 units a second. Over the walk `sensor_events` reports the
/// coin entered exactly once, on the first step the player's box reaches
/// past x = 2.5, and left exactly once, on the first step its box is
/// wholly past x = 3.5.
#[wasm_bindgen_test(unsupported = test)]
fn a_walk_through_a_pickup_enters_and_leaves_it_once() {
    let up = Vec2::Y;
    let body = cuboid(PLAYER);
    let level = [
        slab(1, -10.0, 10.0, 0.0),
        Collider::new(40, cuboid(Vec2::new(0.5, 0.5)), Vec2::new(3.0, 1.0))
            .with_kind(ColliderKind::Sensor),
    ];
    let mut state = MoverState {
        position: Vec2::new(0.0, 0.5 + 0.5 * SKIN),
        ..MoverState::default()
    };
    let mut inside: Vec<u64> = Vec::new();
    let (mut entered, mut left) = (Vec::new(), Vec::new());
    for n in 0..90 {
        state.velocity = Vec2::new(4.0, state.velocity.y - GRAVITY * DT);
        step(&mut state, &body, &level[..], up);
        let now = sensors_at(&body, state.position, &level[..], ALL_LAYERS).unwrap();
        let (into, out) = sensor_events(&inside, &now);
        let (front, back) = (state.position.x + 0.4, state.position.x - 0.4);
        if into.contains(&40) {
            entered.push(n);
            assert!(front > 2.5, "{front}");
        }
        if out.contains(&40) {
            left.push(n);
            assert!(back >= 3.5, "{back}");
        }
        assert_eq!(now.contains(&40), front > 2.5 && back < 3.5, "step {n}");
        inside = now;
    }
    assert_eq!((entered.len(), left.len()), (1, 1));
    assert!(entered[0] < left[0]);
}
