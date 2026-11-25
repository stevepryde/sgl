//! Game scenarios the audit adds (#924 item 3) to those in `scenarios.rs`:
//! a voxel walker at the edge of loaded space, across a seam between two
//! chunk worlds, against a sideways ramp and under a ceiling slope; a
//! top-down beam and projectile; and a bridge over a road. Each expected
//! outcome comes from the requirement and the level's geometry, worked in
//! the comment beside it; "never inside" reads the `f64` shape oracle.
//! Runs natively and in a wasm runtime.
// Results are compared bit for bit on purpose; casts are of small counts.
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::float_cmp,
    clippy::too_many_lines
)]

mod audit_support;

use audit_support::{SKIN, config, solids_inside};
use sgl_core::math::{IVec2, IVec3, Vec2, Vec3};
use sgl_physics::{
    ALL_LAYERS, CellColliders, CellWorld, Collider, ColliderKind, ColliderSet, Contacts, Cut,
    Floor, MoverOptions, MoverState, Shape, World, move_and_collide, move_and_slide, ray,
};
use wasm_bindgen_test::wasm_bindgen_test;

const DT: f32 = 1.0 / 60.0;
const GRAVITY: f32 = 20.0;

/// The walker: 0.6 wide and deep, 1.8 tall.
const WALKER: Vec3 = Vec3::new(0.3, 0.9, 0.3);

fn walker() -> Shape<Vec3> {
    Shape::Box { half: WALKER }
}

fn full(id: u64) -> Collider<Vec3> {
    Collider::new(
        id,
        Shape::Box {
            half: Vec3::splat(0.5),
        },
        Vec3::splat(0.5),
    )
}

/// Standing on ground whose top is at y = 0, a skin above it.
fn on_ground(x: f32, z: f32) -> MoverState<Vec3> {
    MoverState {
        position: Vec3::new(x, 0.9 + SKIN, z),
        velocity: Vec3::ZERO,
        floor: Some(Floor {
            normal: Vec3::Y,
            id: 1,
        }),
    }
}

/// One step of a walker holding `walk` (units a second across), as a game
/// runs it: gravity, then the mover.
fn walk_step<W: World<Vec3> + ?Sized>(
    state: &mut MoverState<Vec3>,
    world: &W,
    walk: Vec3,
) -> Contacts<Vec3> {
    state.velocity.x = walk.x;
    state.velocity.z = walk.z;
    state.velocity.y -= GRAVITY * DT;
    move_and_slide(
        state,
        &walker(),
        DT,
        world,
        &config(Vec3::Y),
        &MoverOptions::default(),
    )
    .expect("a walker's reach is far under the cell cap")
}

/// Whether a body whose bottom is at `bottom` rests on a floor whose top is
/// at `top`: on it or above it by no more than the skin.
fn rests(bottom: f32, top: f32) -> bool {
    (0.0..=SKIN + 1e-5).contains(&(bottom - top))
}

// ------------------------------------------------------------ voxel walker

/// Defect: unloaded space that blocks or holds up a body when the game's
/// policy is to leave it empty (a hidden wall at the edge of what is
/// loaded, a floor remembered from the last loaded cell). Level: ground
/// cells (y = -1) are loaded for x < 5; cells from x = 5 are unloaded, and
/// the game's policy pushes nothing for them. Walking +x at 4 units a
/// second from x = 0.5 for two seconds: the walker meets no wall; it
/// stands on the ground while any of its footprint (x ± 0.3) is over
/// x < 5; once its trailing edge passes x = 5 it has no floor; and, having
/// walked off at 1.2 s, it falls for the remaining 0.8 s, about 6.4 units
/// at 20 units/s², so ends well below y = -4. Oracle: the requirement and
/// geometry.
#[wasm_bindgen_test(unsupported = test)]
fn a_voxel_walker_falls_into_space_the_game_leaves_empty() {
    let world = CellWorld::new(1.0, |cell: IVec3, out: &mut CellColliders<'_, Vec3>| {
        if cell.y == -1 && cell.x < 5 && cell.z.abs() < 8 && cell.x > -8 {
            out.push(full(1));
        }
    });
    let mut state = on_ground(0.5, 0.5);
    for n in 0..120 {
        let contacts = walk_step(&mut state, &world, Vec3::new(4.0, 0.0, 0.0));
        assert!(
            !contacts.wall,
            "step {n}: a wall at the edge of loaded space"
        );
        let x = state.position.x;
        if x - WALKER.x < 5.0 - 1e-3 {
            assert!(state.floor.is_some(), "step {n}: lost the floor at {x}");
            assert!(rests(state.position.y - WALKER.y, 0.0), "step {n}");
        }
        if x - WALKER.x > 5.0 + 1e-3 {
            assert!(state.floor.is_none(), "step {n}: a floor in empty space");
        }
    }
    assert!(state.position.y < -4.0, "{state:?}");
}

/// Defect: a seam between two worlds that snags a body (a candidate one
/// world drops at its border, a duplicate face between them) or changes
/// how it moves. Level: two chunks of ground, each a cell world that
/// answers only its own cells (x < 16 and x >= 16), given as a pair.
/// Walking +x at 4 units a second from x = 14 for a second: every step
/// covers exactly the velocity times the step along x and nothing along z,
/// the walker rests on the ground and keeps its floor, and it meets no
/// wall; and the whole walk is bit for bit the walk through one cell world
/// holding both chunks. Oracle: the requirement and a second world.
#[wasm_bindgen_test(unsupported = test)]
fn a_voxel_walker_crosses_a_seam_between_two_chunk_worlds() {
    let ground = |cell: IVec3| cell.y == -1 && cell.z.abs() < 4 && (0..32).contains(&cell.x);
    let west = CellWorld::new(
        1.0,
        move |cell: IVec3, out: &mut CellColliders<'_, Vec3>| {
            if ground(cell) && cell.x < 16 {
                out.push(full(1));
            }
        },
    );
    let east = CellWorld::new(
        1.0,
        move |cell: IVec3, out: &mut CellColliders<'_, Vec3>| {
            if ground(cell) && cell.x >= 16 {
                out.push(full(1));
            }
        },
    );
    let whole = CellWorld::new(
        1.0,
        move |cell: IVec3, out: &mut CellColliders<'_, Vec3>| {
            if ground(cell) {
                out.push(full(1));
            }
        },
    );
    let walk = Vec3::new(4.0, 0.0, 0.0);
    let mut chunked = on_ground(14.0, 0.5);
    let mut single = chunked;
    for n in 0..60 {
        let before = chunked.position;
        let contacts = walk_step(&mut chunked, &(&west, &east), walk);
        walk_step(&mut single, &whole, walk);
        let moved = chunked.position - before;
        assert!((moved.x - walk.x * DT).abs() <= 1e-5, "step {n}: {moved:?}");
        assert_eq!(moved.z, 0.0, "step {n}");
        assert!(
            !contacts.wall,
            "step {n}: snagged at {:?}",
            chunked.position
        );
        assert!(chunked.floor.is_some(), "step {n}");
        assert!(rests(chunked.position.y - WALKER.y, 0.0), "step {n}");
        assert_eq!(chunked, single, "step {n}: the seam showed");
    }
    assert!(chunked.position.x > 17.5);
}

/// Defect: a ramp turned sideways (its normal across `up`, a slanted wall
/// in plan) that a walker climbs, enters, or stops dead against instead of
/// sliding along. Level: ground, and a box spanning x 1 to 5, z -2 to 2,
/// y 0 to 4, cut by the plane of normal (-1, 0, 1)/√2 through its centre:
/// solid where z <= x - 3. Walking +x at 4 units a second from x = 0,
/// z = 0, its footprint corner (x + 0.3, -0.3) meets the plane at
/// x = 2.4; from there the face, a wall, takes the part of each step's
/// motion into it and leaves the part along it, (1, 0, 1)/2 of the step:
/// the walker slides toward +z. It never enters the cut box (oracle),
/// never rises off the ground, and after the 84 steps left in two seconds,
/// at about 0.033 units of z each, has slid well past z = 1.5. Oracle: the
/// requirement and geometry.
#[wasm_bindgen_test(unsupported = test)]
fn a_voxel_walker_slides_along_a_sideways_ramp() {
    let ground = Collider::new(
        1,
        Shape::Box {
            half: Vec3::new(20.0, 0.5, 20.0),
        },
        Vec3::new(0.0, -0.5, 0.0),
    );
    let ramp = Collider::new(
        2,
        Shape::Box {
            half: Vec3::new(2.0, 2.0, 2.0),
        },
        Vec3::new(3.0, 2.0, 0.0),
    )
    .with_cut(Cut::new(Vec3::new(-1.0, 0.0, 1.0), 0.0));
    let mut world = ColliderSet::new(2.0);
    world.insert(ground);
    world.insert(ramp);
    let level = [ground, ramp];
    let mut state = on_ground(0.0, 0.0);
    let mut touched = false;
    for n in 0..120 {
        let contacts = walk_step(&mut state, &world, Vec3::new(4.0, 0.0, 0.0));
        touched |= contacts.wall && contacts.last_wall_id == Some(2);
        let inside = solids_inside(state.position, WALKER, &level, ALL_LAYERS);
        assert!(inside.is_empty(), "step {n}: {state:?} inside {inside:?}");
        assert!(
            rests(state.position.y - WALKER.y, 0.0),
            "step {n}: {state:?}"
        );
        assert!(state.floor.is_some(), "step {n}");
    }
    assert!(touched);
    assert!(state.position.z > 1.5, "{state:?}");
}

/// Defect: a ceiling slope (an inverted ramp) that a walker walks into, is
/// pushed through the floor by, or keeps pressing past. Level: ground, and
/// a box spanning x 3 to 7, y 0.5 to 4.5, cut by the plane of normal
/// (-1, -1, 0)/√2 through its centre (5, 2.5): solid where
/// x + y >= 7.5, a face descending toward +x, a ceiling to the walker
/// (`normal · up` is -0.707, past -0.7). Walking +x at 3 units a second,
/// the walker's top front edge (x + 0.3, 1.81) meets it at x = 5.39. It
/// stops there: never inside the slope (oracle), still resting on the
/// ground with its floor, its x settled no further than 5.39 and within a
/// few skins of it. Oracle: the requirement and geometry.
#[wasm_bindgen_test(unsupported = test)]
fn a_voxel_walker_stops_under_a_ceiling_slope() {
    let ground = Collider::new(
        1,
        Shape::Box {
            half: Vec3::new(20.0, 0.5, 20.0),
        },
        Vec3::new(0.0, -0.5, 0.0),
    );
    let slope = Collider::new(
        2,
        Shape::Box {
            half: Vec3::new(2.0, 2.0, 10.0),
        },
        Vec3::new(5.0, 2.5, 0.0),
    )
    .with_cut(Cut::new(Vec3::new(-1.0, -1.0, 0.0), 0.0));
    let level = [ground, slope];
    let mut state = on_ground(2.0, 0.0);
    let mut last = state.position.x;
    for n in 0..120 {
        walk_step(&mut state, &level[..], Vec3::new(3.0, 0.0, 0.0));
        let inside = solids_inside(state.position, WALKER, &level, ALL_LAYERS);
        assert!(inside.is_empty(), "step {n}: {state:?} inside {inside:?}");
        assert!(
            rests(state.position.y - WALKER.y, 0.0),
            "step {n}: {state:?}"
        );
        assert!(state.floor.is_some(), "step {n}");
        if n >= 100 {
            assert!(
                (state.position.x - last).abs() <= 1e-4,
                "step {n}: still moving"
            );
        }
        last = state.position.x;
    }
    let stop = 7.5 - (0.9 + SKIN + 0.9) - 0.3;
    assert!(
        state.position.x <= stop && state.position.x >= stop - 4.0 * SKIN,
        "{state:?}, stop {stop}"
    );
}

// ---------------------------------------------------------------- top-down

/// Defect: a beam that stops at something outside its layers, at a sensor,
/// at the second thing in its path, or at the wrong place on the first.
/// Level (2D, top-down): along +x from the origin, a sensor on layer 1 at
/// x 1.5 to 2.5, a solid on layer 2 at 3.5 to 4.5, a one-way box on
/// layer 1 at 5.5 to 6.5 (a ray knows no up: it meets one as a box) and a
/// solid on layer 1 at 7.5 to 8.5. A beam of length 10: on layer 1 it
/// stops at 5.5 (t = 0.55) on the one-way box, on layer 2 at 3.5
/// (t = 0.35), on both at 3.5, on layer 4 nowhere; each facing -x. Oracle:
/// the requirement and geometry.
#[wasm_bindgen_test(unsupported = test)]
fn a_beam_stops_at_the_first_thing_in_its_layers() {
    let block = |id, x: f32, layers, kind| {
        Collider::new(
            id,
            Shape::Box {
                half: Vec2::splat(0.5),
            },
            Vec2::new(x, 0.0),
        )
        .with_layers(layers)
        .with_kind(kind)
    };
    let level = [
        block(1, 2.0, 1, ColliderKind::Sensor),
        block(2, 4.0, 2, ColliderKind::Solid),
        block(3, 6.0, 1, ColliderKind::OneWay),
        block(4, 8.0, 1, ColliderKind::Solid),
    ];
    let set = audit_support::set_of(&level, 1.0);
    for world in [&level.as_slice() as &dyn World<Vec2>, &set] {
        for (mask, expected) in [
            (1, Some((0.55, 3))),
            (2, Some((0.35, 2))),
            (3, Some((0.35, 2))),
            (4, None),
        ] {
            let hit = ray(Vec2::new(0.0, 0.1), Vec2::new(10.0, 0.0), world, mask).unwrap();
            assert_eq!(
                hit.map(|h| (h.t, h.id, h.normal)),
                expected.map(|(t, id)| (t, id, Vec2::NEG_X)),
                "mask {mask}"
            );
        }
    }
}

/// Defect: a fast projectile that tunnels through a thin wall, or is
/// stopped beyond its face, when stepped with `move_and_collide` the way a
/// game steps one (move by the translation, stop at a hit). Level: a wall
/// 0.1 thick at x 0 to 0.1, as a set and as one-cell-thin cells of a cell
/// world (each cell 1 unit; the wall a tenth of it). A projectile 0.1
/// across fired at 50 units a step from 20 starting places between x = -100
/// and x = -93: it never ends with its front past x = 0, and it ends
/// stopped at the wall, a skin short (the `skin` it is moved back by along
/// its path). Oracle: the requirement and geometry.
#[wasm_bindgen_test(unsupported = test)]
fn a_projectile_never_ends_beyond_a_wall() {
    let wall = Collider::new(
        1,
        Shape::Box {
            half: Vec2::new(0.05, 5.0),
        },
        Vec2::new(0.05, 0.0),
    );
    let set = audit_support::set_of(&[wall], 4.0);
    let cells = CellWorld::new(1.0, |cell: IVec2, out: &mut CellColliders<'_, Vec2>| {
        if cell.x == 0 && (-5..5).contains(&cell.y) {
            out.push(Collider::new(
                1,
                Shape::Box {
                    half: Vec2::new(0.05, 0.5),
                },
                Vec2::new(0.05, 0.5),
            ));
        }
    });
    let config = config(Vec2::Y);
    let bullet = Shape::Box {
        half: Vec2::splat(0.05),
    };
    for (name, world) in [("set", &set as &dyn World<Vec2>), ("cells", &cells)] {
        for k in 0..20 {
            let mut at = Vec2::new(-100.0 + k as f32 * 0.37, 0.3 + k as f32 * 0.01);
            let mut stopped = false;
            for _ in 0..4 {
                let (moved, hit) = move_and_collide(
                    &bullet,
                    at,
                    Vec2::new(50.0, 0.0),
                    world,
                    ALL_LAYERS,
                    &config,
                )
                .expect("fifty cells is under the cap");
                at += moved;
                assert!(at.x + 0.05 <= 0.0, "{name} {k}: front at {}", at.x + 0.05);
                if hit.is_some() {
                    stopped = true;
                    break;
                }
            }
            assert!(stopped, "{name} {k}: never met the wall");
            assert!(
                at.x + 0.05 >= -SKIN - 1e-4,
                "{name} {k}: stopped short at {}",
                at.x + 0.05
            );
        }
    }
}

// ---------------------------------------------------------------- vehicles

/// Defect: a road under a bridge that a car on the bridge meets (a floor
/// found through the deck, a contact with what lies far below), or a deck
/// that a car on the road passing under it bumps. Level (3D): a road whose
/// top is at y = 0 (id 1), and a bridge deck 0.5 thick whose top is at
/// y = 4.5 over x -3 to 3, running along z (id 2). A car 2 wide, 1.5 tall
/// and 4 long. On the road, driving +x at 10 units a second for four
/// seconds from x = -20, it passes under the deck (its top at 1.51, the
/// deck's underside at 4): it meets no wall or ceiling, covers exactly its
/// speed times the step each step and stands on the road throughout. On
/// the deck, driving +z for six seconds from z = -40, it stands on the
/// deck every step, never on the road, and stays at the deck's height.
/// Oracle: the requirement and geometry.
#[wasm_bindgen_test(unsupported = test)]
fn a_car_on_a_bridge_never_meets_the_road_beneath() {
    let road = Collider::new(
        1,
        Shape::Box {
            half: Vec3::new(60.0, 0.5, 60.0),
        },
        Vec3::new(0.0, -0.5, 0.0),
    );
    let deck = Collider::new(
        2,
        Shape::Box {
            half: Vec3::new(3.0, 0.25, 60.0),
        },
        Vec3::new(0.0, 4.25, 0.0),
    );
    let mut world = ColliderSet::new(4.0);
    world.insert(road);
    world.insert(deck);
    let car = Shape::Box {
        half: Vec3::new(1.0, 0.75, 2.0),
    };
    let config = config(Vec3::Y);
    let drive = |start: Vec3, floor: u64, velocity: Vec3, top: f32, steps: usize| {
        let mut state = MoverState {
            position: start,
            velocity: Vec3::ZERO,
            floor: Some(Floor {
                normal: Vec3::Y,
                id: floor,
            }),
        };
        for n in 0..steps {
            let before = state.position;
            state.velocity.x = velocity.x;
            state.velocity.z = velocity.z;
            state.velocity.y -= GRAVITY * DT;
            let contacts = move_and_slide(
                &mut state,
                &car,
                DT,
                &world,
                &config,
                &MoverOptions::default(),
            )
            .unwrap();
            let moved = state.position - before;
            assert!(
                !contacts.wall && !contacts.ceiling,
                "step {n}: {contacts:?}"
            );
            assert!((moved.x - velocity.x * DT).abs() <= 1e-4, "step {n}");
            assert!((moved.z - velocity.z * DT).abs() <= 1e-4, "step {n}");
            assert_eq!(state.floor.map(|f| f.id), Some(floor), "step {n}");
            assert!(rests(state.position.y - 0.75, top), "step {n}: {state:?}");
        }
    };
    drive(
        Vec3::new(-20.0, 0.75 + SKIN, 0.0),
        1,
        Vec3::new(10.0, 0.0, 0.0),
        0.0,
        240,
    );
    drive(
        Vec3::new(0.0, 4.5 + 0.75 + SKIN, -40.0),
        2,
        Vec3::new(0.0, 0.0, 10.0),
        4.5,
        360,
    );
}
