//! Surfaces (#923) beside moving colliders and triggers (#920): a game's
//! terrain or triangles in one world and its lifts, doors and sensors in a
//! `ColliderSet` beside it, asked as a pair, and the mover given a surface
//! world. Each test states its oracle: hand-worked geometry or a
//! requirement of `specs/physics.md` (21, "Moving colliders"; 22,
//! "Surfaces"). A ray and `ground_under` meet a box and a level triangle
//! exactly, to a few units in the last place of the coordinates involved.
#![allow(clippy::cast_precision_loss)]

use sgl_core::SplitMix64;
use sgl_core::math::Vec3;
use sgl_physics::{
    ALL_LAYERS, Collider, ColliderKind, ColliderSet, HeightField, MoverConfig, MoverOptions,
    MoverSettings, MoverState, Shape, TriangleSet, Unanswered, World, distance, ground_under,
    move_and_collide, move_and_slide, overlaps, ray, sensors_at, slide, snap_to_ground,
};
use wasm_bindgen_test::wasm_bindgen_test;

/// How far an exact answer may lie from the true one: four units in the
/// last place of `largest`, the largest coordinate the query involves.
fn rounding(largest: f32) -> f32 {
    4.0 * f32::EPSILON * largest
}

fn cuboid(half: Vec3) -> Shape<Vec3> {
    Shape::Box { half }
}

/// A level height field 16 by 16 cells of 1, its corner at (-8, 0, -8),
/// every sample at `height`, id 9 for every cell.
fn level_field(height: f32) -> HeightField {
    HeightField::new(
        &[height; 17 * 17],
        17,
        17,
        1.0,
        Vec3::new(-8.0, 0.0, -8.0),
        &[9],
    )
    .expect("a sound grid")
}

/// Defect: `ground_under` over terrain and a lift asked as a pair takes
/// the wrong one: the terrain while a lift stands above it within reach,
/// the lift once it has risen past `max_height` or been removed, or reads
/// the lift where it is heading rather than where it stands. Oracle:
/// physics.md 22 (the highest surface from `max_height` above the point
/// down) and 21 (only the mover reads a velocity; every other query takes
/// a collider where it stands), worked by hand: terrain level at y = 0.5,
/// a lift 2 by 0.2 by 2 (id 50) moved by `set_position`, its top 0.1 above
/// its centre; a wheel at y = 1, reaching 1 up and 3 down. Heights are the
/// surfaces' own, to rounding of the 8 the field's corners reach.
#[wasm_bindgen_test(unsupported = test)]
fn ground_under_terrain_and_a_lift_is_the_higher_one_within_reach() {
    let field = level_field(0.5);
    let mut lifts = ColliderSet::new(2.0);
    lifts.insert(Collider::new(
        50,
        cuboid(Vec3::new(1.0, 0.1, 1.0)),
        Vec3::new(0.0, 0.2, 0.0),
    ));
    let wheel = Vec3::new(0.25, 1.0, -0.25);
    let bound = rounding(8.0);
    let ground = |lifts: &ColliderSet<Vec3>| {
        ground_under(wheel, 1.0, 3.0, &(&field, lifts), ALL_LAYERS)
            .expect("answered")
            .expect("terrain at least")
    };

    // Sunk into the terrain (top at 0.3): the terrain is higher.
    let found = ground(&lifts);
    assert_eq!(found.id, 9, "{found:?}");
    assert!((found.height - 0.5).abs() <= bound, "{found:?}");

    // Risen above the terrain (top at 1.5), within reach of the wheel.
    // Heading up fast does not change where it stands.
    assert!(lifts.set_position(50, Vec3::new(0.0, 1.4, 0.0)));
    assert!(lifts.set_velocity(50, Vec3::new(0.0, 30.0, 0.0)));
    let found = ground(&lifts);
    assert_eq!(found.id, 50, "{found:?}");
    assert!((found.height - 1.5).abs() <= bound, "{found:?}");
    assert_eq!(found.normal, Vec3::Y);

    // Top exactly at `max_height` above the wheel (2.0): still met.
    assert!(lifts.set_position(50, Vec3::new(0.0, 1.9, 0.0)));
    let found = ground(&lifts);
    assert_eq!(found.id, 50, "{found:?}");
    assert!((found.height - 2.0).abs() <= bound, "{found:?}");

    // Risen past reach (top at 2.5): a deck above, the terrain below.
    assert!(lifts.set_position(50, Vec3::new(0.0, 2.4, 0.0)));
    assert_eq!(ground(&lifts).id, 9);

    // Back within reach, then removed.
    assert!(lifts.set_position(50, Vec3::new(0.0, 0.9, 0.0)));
    assert_eq!(ground(&lifts).id, 50);
    assert!(lifts.remove(50));
    assert_eq!(ground(&lifts).id, 9);
}

/// A wall of two triangles in the plane x = `x`, from y = 0 to 4 and z =
/// -2 to 2, id 4.
fn wall_at(x: f32) -> TriangleSet {
    let corners = [
        Vec3::new(x, 0.0, -2.0),
        Vec3::new(x, 4.0, -2.0),
        Vec3::new(x, 4.0, 2.0),
        Vec3::new(x, 0.0, 2.0),
    ];
    TriangleSet::new(&corners, &[0, 1, 2, 0, 2, 3], &[4]).expect("a sound wall")
}

/// Defect: a ray through a pair of a triangle world and a collider set
/// reports the first world's hit, or the farther one, or misses a door
/// the game moved into the way. Oracle: hand geometry. A ray from (0, 1,
/// 0.5) by (10, 0, 0) meets the wall at x = 5 at t = 0.5 and a door
/// 0.2 thick (id 60) centred at x = 3 at its face x = 2.9, t = 0.29; a
/// round door (a ball of radius 0.5, id 61) centred on the ray at x = 3
/// at x = 2.5, t = 0.25, since rays are exact against balls. Both pair
/// orders give the same hit; doors moved behind the wall leave the wall.
#[wasm_bindgen_test(unsupported = test)]
fn a_ray_through_triangles_and_moving_colliders_meets_the_nearer() {
    let wall = wall_at(5.0);
    let mut doors = ColliderSet::new(2.0);
    let (from, motion) = (Vec3::new(0.0, 1.0, 0.5), Vec3::new(10.0, 0.0, 0.0));
    let bound = rounding(10.0);
    let hit = |doors: &ColliderSet<Vec3>| {
        let ahead = ray(from, motion, &(&wall, doors), ALL_LAYERS).expect("answered");
        let behind = ray(from, motion, &(doors, &wall), ALL_LAYERS).expect("answered");
        assert_eq!(ahead, behind, "the pair's order shows");
        ahead.expect("the wall at least")
    };

    let met = hit(&doors);
    assert_eq!(met.id, 4, "{met:?}");
    assert!((met.t - 0.5).abs() <= bound / 10.0, "{met:?}");
    assert_eq!(met.normal, Vec3::NEG_X);

    doors.insert(Collider::new(
        60,
        cuboid(Vec3::new(0.1, 2.0, 1.0)),
        Vec3::new(3.0, 2.0, 0.0),
    ));
    let met = hit(&doors);
    assert_eq!(met.id, 60, "{met:?}");
    assert!((met.t - 0.29).abs() <= bound / 10.0, "{met:?}");

    assert!(doors.set_position(60, Vec3::new(7.0, 2.0, 0.0)));
    assert_eq!(hit(&doors).id, 4);

    doors.insert(Collider::new(
        61,
        Shape::Ball { radius: 0.5 },
        Vec3::new(3.0, 1.0, 0.5),
    ));
    let met = hit(&doors);
    assert_eq!(met.id, 61, "{met:?}");
    assert!((met.t - 0.25).abs() <= bound / 10.0, "{met:?}");
    assert_eq!(met.normal, Vec3::NEG_X);

    assert!(doors.set_position(61, Vec3::new(6.0, 1.0, 0.5)));
    assert_eq!(hit(&doors).id, 4);
}

/// Asks `surface`, and the pair of it and `coins`, about `ball` sunk into
/// a level floor at y = 0.5 and above it: see the test below.
fn read_beside<W: World<Vec3>>(
    surface: &W,
    id: u64,
    coins: &ColliderSet<Vec3>,
    ball: &Shape<Vec3>,
    (sunk, above): (Vec3, Vec3),
) {
    let pair = (surface, coins);
    for at in [sunk, above] {
        assert_eq!(sensors_at(ball, at, &pair, ALL_LAYERS), Ok(vec![40]));
        assert_eq!(sensors_at(ball, at, surface, ALL_LAYERS), Ok(vec![]));
    }
    let inside = overlaps(ball, sunk, surface, ALL_LAYERS).expect("answered");
    assert!(!inside.is_empty(), "the floor under a sunk ball");
    for triangle in &inside {
        assert_eq!(triangle.id, id);
        assert_eq!(triangle.kind, ColliderKind::Solid);
        assert_eq!(triangle.layers, ALL_LAYERS);
        let apart = distance(ball, sunk, triangle).expect("a sound pair");
        assert!(
            apart.distance <= 0.0 && apart.normal == Vec3::ZERO,
            "{apart:?}"
        );
    }
    assert_eq!(overlaps(ball, above, surface, ALL_LAYERS), Ok(vec![]));
    // Every triangle near the ball above, by a region that reaches the
    // floor: the nearest is 0.25 below, straight down.
    let mut near = Vec::new();
    surface
        .colliders_near(
            above - Vec3::splat(1.0),
            above + Vec3::splat(1.0),
            &mut |c| {
                near.push(*c);
            },
        )
        .expect("answered");
    assert!(!near.is_empty());
    let nearest = near
        .iter()
        .filter_map(|triangle| distance(ball, above, triangle))
        .min_by(|a, b| a.distance.total_cmp(&b.distance))
        .expect("a triangle below");
    assert!((nearest.distance - 0.25).abs() <= 1e-5, "{nearest:?}");
    assert!((nearest.normal - Vec3::Y).length() <= 1e-5, "{nearest:?}");
}

/// Defect: a triangle of a surface world reported as a trigger, a sensor
/// beside a surface missed or hidden by it, or `overlaps` and `distance`
/// misreading a triangle the world offers. Oracle: physics.md 22 (each
/// triangle is a solid collider on every layer, with the game's id) and 21
/// (`sensors_at` lists the sensors a shape overlaps), worked by hand. A
/// ball of radius 0.5 at y = 0.75 over a level floor at y = 0.5 (a
/// `TriangleSet` of two triangles, id 1, or a height field, id 9) overlaps
/// it by 0.25; one at y = 1.25 is 0.25 clear of it, the normal straight
/// up. A coin sensor (id 40, a box of half 0.5 at (0, 1, 0)) in a set
/// beside the floor is the only trigger either ball is in.
#[wasm_bindgen_test(unsupported = test)]
fn sensors_overlaps_and_distance_read_a_surface_world_and_a_set_beside_it() {
    let corners = [
        Vec3::new(-4.0, 0.5, -4.0),
        Vec3::new(4.0, 0.5, -4.0),
        Vec3::new(4.0, 0.5, 4.0),
        Vec3::new(-4.0, 0.5, 4.0),
    ];
    let floor = TriangleSet::new(&corners, &[0, 1, 2, 0, 2, 3], &[1]).expect("a sound floor");
    let field = level_field(0.5);
    let mut coins = ColliderSet::new(2.0);
    coins.insert(
        Collider::new(40, cuboid(Vec3::splat(0.5)), Vec3::new(0.0, 1.0, 0.0))
            .with_kind(ColliderKind::Sensor),
    );
    let ball = Shape::Ball { radius: 0.5 };
    let (sunk, above) = (Vec3::new(0.1, 0.75, 0.2), Vec3::new(0.1, 1.25, 0.2));
    read_beside(&floor, 1, &coins, &ball, (sunk, above));
    read_beside(&field, 9, &coins, &ball, (sunk, above));
}

/// Bumpy terrain: 24 by 24 cells of 1, corner at (-12, 0, -12), heights
/// drawn from `seed` between 0 and 0.6.
fn bumpy_field(seed: u64) -> HeightField {
    let mut rng = SplitMix64::new(seed);
    let heights: Vec<f32> = (0..25 * 25)
        .map(|_| (rng.next_u64() >> 40) as f32 / 16_777_216.0 * 0.6)
        .collect();
    HeightField::new(&heights, 25, 25, 1.0, Vec3::new(-12.0, 0.0, -12.0), &[9])
        .expect("a sound grid")
}

/// A 12 by 12 floor of 1 m quads at y = 0, a ramp rising along +x to 1.5
/// beside it and a wall at x = -6, as triangles.
fn floor_ramp_and_wall() -> TriangleSet {
    let mut positions = Vec::new();
    let mut indices = Vec::new();
    let mut quad = |corners: [Vec3; 4]| {
        let base = u32::try_from(positions.len()).expect("small");
        positions.extend(corners);
        indices.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
    };
    for i in -6..6 {
        for k in -6..6 {
            let (x, z) = (i as f32, k as f32);
            quad([
                Vec3::new(x, 0.0, z),
                Vec3::new(x + 1.0, 0.0, z),
                Vec3::new(x + 1.0, 0.0, z + 1.0),
                Vec3::new(x, 0.0, z + 1.0),
            ]);
        }
    }
    quad([
        Vec3::new(0.0, 0.0, 6.0),
        Vec3::new(4.0, 1.5, 6.0),
        Vec3::new(4.0, 1.5, 9.0),
        Vec3::new(0.0, 0.0, 9.0),
    ]);
    quad([
        Vec3::new(-6.0, 0.0, -6.0),
        Vec3::new(-6.0, 3.0, -6.0),
        Vec3::new(-6.0, 3.0, 6.0),
        Vec3::new(-6.0, 0.0, 6.0),
    ]);
    TriangleSet::new(&positions, &indices, &[3]).expect("a sound mesh")
}

fn finite(v: Vec3) -> bool {
    v.x.is_finite() && v.y.is_finite() && v.z.is_finite()
}

/// Seeded steps of `move_and_slide`, and calls of `slide`,
/// `snap_to_ground` and `move_and_collide` from each, with `body`
/// through `world`, from awkward starts: dropped onto the surface, inside
/// it and under it, at walking speeds, now and then at a thousand units a
/// second, and with every option. A body that leaves the scene starts
/// again. Returns how many steps ended on a floor.
fn drive<W: World<Vec3>>(world: &W, body: &Shape<Vec3>, seed: u64) -> u32 {
    let config = MoverConfig::new(MoverSettings {
        up: Vec3::Y,
        skin: 0.01,
        snap_distance: 0.1,
        normal_threshold: 0.7,
        block_epsilon: 1e-4,
    });
    let mut rng = SplitMix64::new(seed);
    let mut draw =
        |low: f32, high: f32| low + (high - low) * ((rng.next_u64() >> 40) as f32 / 16_777_216.0);
    let tall = body.half_extents().y;
    let mut state = MoverState {
        position: Vec3::ZERO,
        velocity: Vec3::ZERO,
        floor: None,
    };
    let mut on_floor = 0;
    for step in 0..240 {
        let away = state.position.x.abs() > 5.0
            || state.position.z.abs() > 5.0
            || !(-2.0..=6.0).contains(&state.position.y);
        if step == 0 || away {
            // One start in four inside or under the surface.
            let y = if draw(0.0, 1.0) < 0.25 {
                draw(-0.5, 0.5)
            } else {
                tall + draw(0.7, 1.5)
            };
            state = MoverState {
                position: Vec3::new(draw(-4.0, 4.0), y, draw(-4.0, 4.0)),
                velocity: Vec3::ZERO,
                floor: None,
            };
        }
        let fast = step % 37 == 36;
        let top_speed = if fast { 1000.0 } else { 4.0 };
        state.velocity.x = draw(-top_speed, top_speed);
        state.velocity.z = draw(-top_speed, top_speed);
        state.velocity.y = if fast {
            draw(-top_speed, top_speed)
        } else if state.floor.is_some() && draw(0.0, 1.0) < 0.02 {
            5.0
        } else {
            state.velocity.y - 20.0 / 60.0
        };
        let options = MoverOptions {
            step_height: draw(0.0, 0.4),
            ledge_drop: (step % 3 == 0).then_some(0.5),
            drop_through: step % 7 == 0,
            mask: ALL_LAYERS,
        };
        let before = state;
        match move_and_slide(&mut state, body, 1.0 / 60.0, world, &config, &options) {
            Ok(contacts) => assert!(finite(contacts.effective), "{contacts:?}"),
            Err(Unanswered) => assert_eq!(state, before),
        }
        assert!(
            finite(state.position) && finite(state.velocity),
            "{state:?}"
        );
        if let Some(floor) = state.floor {
            assert!(finite(floor.normal), "{floor:?}");
            on_floor += 1;
        }
        let delta = state.velocity / 60.0;
        if let Ok((moved, contacts)) =
            slide(body, state.position, delta, world, ALL_LAYERS, &config)
        {
            assert!(finite(moved) && finite(contacts.effective), "{moved:?}");
        }
        if let Ok((moved, contacts)) =
            snap_to_ground(body, state.position, world, ALL_LAYERS, &config)
        {
            assert!(finite(moved) && finite(contacts.effective), "{moved:?}");
        }
        if let Ok((moved, hit)) =
            move_and_collide(body, state.position, delta, world, ALL_LAYERS, &config)
        {
            assert!(finite(moved), "{moved:?}");
            if let Some(hit) = hit {
                assert!(hit.t.is_finite() && finite(hit.normal), "{hit:?}");
            }
        }
    }
    on_floor
}

/// Defect: the mover given a triangle world, which it does not walk on
/// (#922), panics, loops without bound (the test would not finish) or
/// leaves a body's state non-finite. Oracle: physics.md 22, what the mover
/// does on a surface world: it meets each triangle as the solid convex
/// collider it is offered as, and every step returns, finite, or is
/// `Unanswered` and leaves the state as it was. A capsule, a ball and a
/// box are stepped over a floor of 1 m quads, a ramp and a wall, and over
/// bumpy terrain, at walking speed, at a thousand units a second, from
/// starts on, inside and under the surface, with every option.
#[wasm_bindgen_test(unsupported = test)]
fn the_mover_on_a_triangle_world_returns_finite_state_from_every_step() {
    let mesh = floor_ramp_and_wall();
    let field = bumpy_field(0x0923);
    let bodies = [
        Shape::Capsule {
            half_axis: Vec3::new(0.0, 0.6, 0.0),
            radius: 0.3,
        },
        Shape::Ball { radius: 0.4 },
        cuboid(Vec3::new(0.3, 0.9, 0.3)),
    ];
    for (seed, body) in (1u64..).zip(&bodies) {
        // Enough steps end on the surface that the walk meets it (66 to 142
        // of each 240 when frozen; the rest fall, jump or start again).
        for stood in [drive(&mesh, body, seed), drive(&field, body, seed + 10)] {
            assert!(stood >= 40, "{stood} of 240");
        }
    }
}
