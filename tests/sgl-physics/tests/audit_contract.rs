//! Assurance audit of the `sgl-physics` contract (#924, physics.md),
//! written from the contract by an agent that did not write the crate.
//! Each test names the defect it would catch and its oracle: the shapes'
//! own definitions (the `f64` oracle in `audit_support`), a second source
//! of the same level that must agree bit for bit, or an exact translation.
//! Seeds and case counts are fixed and small. Runs natively and in a wasm
//! runtime.
// Results are compared bit for bit on purpose; casts are of small counts.
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::float_cmp,
    clippy::too_many_lines
)]

mod audit_support;

use audit_support::{
    Answers, Case, DT, Level, Refusing, Rng, SKIN, battery, battery_steps, battery_up, bits,
    config, dot, given_normal, level, overlapping, set_of, solids_inside, splat, up,
};
use sgl_core::math::{Vec2, Vec3};
use sgl_physics::{
    ALL_LAYERS, Collider, ColliderKind, Contacts, Floor, MoverOptions, MoverState, Shape,
    Unanswered, Vector, World, cast, move_and_collide, move_and_slide, overlaps, ray, slide,
    snap_to_ground,
};
use wasm_bindgen_test::wasm_bindgen_test;

/// A random case near the level's colliders: free in the air, standing on
/// a collider's top, pressed against its side, or (with `inside`) started
/// inside a solid. Bodies move a few units, mostly toward what is near.
fn random_case<V: Vector>(
    rng: &mut Rng,
    colliders: &[Collider<V>],
    inside: bool,
) -> Option<Case<V>> {
    let mut half = V::ZERO;
    for axis in 0..V::AXES {
        half[axis] = rng.grid(2, 8, 0.125);
    }
    let blockers: Vec<_> = colliders
        .iter()
        .filter(|c| c.kind != ColliderKind::Sensor && c.cut.is_none())
        .copied()
        .collect();
    let mut toward = V::ZERO;
    toward[0] = rng.pick(&[-1.0, 1.0]);
    let mut delta: V = rng.vector(-3.0, 3.0);
    let mut walking = rng.chance(30);
    let at = if inside {
        let solid = colliders
            .iter()
            .filter(|c| c.kind == ColliderKind::Solid)
            .nth(rng.below(colliders.len()))?;
        let mut at = solid.position;
        for axis in 0..V::AXES {
            let reach = solid.shape.half_extents()[axis] + half[axis];
            at[axis] += rng.range(-reach, reach) * 0.9;
        }
        at
    } else {
        let mode = if blockers.is_empty() { 0 } else { rng.below(3) };
        let target = if blockers.is_empty() {
            None
        } else {
            Some(blockers[rng.below(blockers.len())])
        };
        match (mode, target) {
            (1, Some(c)) => {
                // Standing on its top, walking.
                let (ch, mut at) = (c.shape.half_extents(), c.position);
                for axis in 0..V::AXES {
                    if axis != 1 {
                        at[axis] += rng.range(-ch[axis], ch[axis]);
                    }
                }
                at[1] = c.position[1] + ch[1] + half[1] + SKIN;
                delta[1] = -rng.range(0.0, 0.3);
                walking = true;
                at
            }
            (2, Some(c)) => {
                // Pressed against a side across up, facing it.
                let (ch, mut at) = (c.shape.half_extents(), c.position);
                let side = rng.pick(&[-1.0f32, 1.0]);
                at[0] += side * (ch[0] + half[0] + SKIN);
                at[1] += rng.range(-ch[1] - half[1], ch[1] + half[1]);
                toward = V::ZERO;
                toward[0] = -side;
                delta[0] = -side * rng.range(0.0, 3.0);
                at
            }
            _ => rng.vector(-6.0, 6.0),
        }
    };
    let free = colliders
        .iter()
        .filter(|c| c.kind == ColliderKind::Solid)
        .all(|c| audit_support::depth(at, half, c) <= 0.0);
    if free == inside {
        return None;
    }
    Some(Case {
        half,
        at,
        delta,
        mask: rng.pick(&[ALL_LAYERS, ALL_LAYERS, ALL_LAYERS, 1, 2, 0]),
        walking,
        options: MoverOptions {
            step_height: rng.pick(&[0.0, 0.3, 0.6, 1.5]),
            ledge_drop: rng.pick(&[None, Some(0.0), Some(0.2), Some(1.0)]),
            drop_through: rng.chance(25),
            mask: ALL_LAYERS,
        },
        toward,
    })
}

/// The places a battery left the body, checked against the oracle: never
/// inside a solid on the mask (physics.md 13, 15, 17, 18).
fn check_never_inside<V: Vector>(
    name: &str,
    case: &Case<V>,
    answers: &Answers<V>,
    colliders: &[Collider<V>],
    mask: u32,
) {
    for &(what, place) in &answers.places {
        if matches!(what, "overlap" | "effective") {
            continue;
        }
        let inside = solids_inside(place, case.half, colliders, mask);
        assert!(
            inside.is_empty(),
            "{name}: {what} ended at {place:?} inside {inside:?}\n{case:?}"
        );
    }
}

/// The slide, snap and collide translations of a battery: each must leave
/// the body outside every solid.
fn check_moves_never_inside<V: Vector>(
    name: &str,
    case: &Case<V>,
    world: &[Collider<V>],
    mask: u32,
) {
    let config = config(up::<V>());
    let body = Shape::Box { half: case.half };
    let (moved, _) = slide(&body, case.at, case.delta, world, mask, &config).unwrap();
    let (snap, _) = snap_to_ground(&body, case.at, world, mask, &config).unwrap();
    let (collided, _) = move_and_collide(&body, case.at, case.delta, world, mask, &config).unwrap();
    for (what, by) in [("slide", moved), ("snap", snap), ("collide", collided)] {
        let inside = solids_inside(case.at + by, case.half, world, mask);
        assert!(
            inside.is_empty(),
            "{name}: {what} by {by:?} ended inside {inside:?}\n{case:?}"
        );
    }
}

// -------------------------------------------------------- order never shows

/// Defect: a result that shows the order a world visits colliders in, how
/// often it visits one, which world holds a collider, or the order a set
/// was built in (a tie broken by visit order, a candidate one world drops,
/// a duplicate counted twice, a refusal swallowed), for any public query,
/// on levels with ramps, one-way platforms, sensors and layers, ids unique
/// or repeated. Oracle: every source of the same level must answer every
/// query bit for bit alike: the colliders as a slice in drawn order,
/// reversed, shuffled with every collider twice, as sets of four cell sizes
/// built in shuffled order, and as pairs splitting them between a cell
/// world, a set and a slice either way round. And the reference run never
/// ends inside a solid, by the oracle (physics.md 6, 10, 13, 17, 18).
fn order_never_shows<V: Vector>(seed: u64, cases: usize) {
    let mut rng = Rng::new(seed);
    for n in 0..cases {
        let level: Level<V> = level(&mut rng, true);
        let all = level.all();
        let Some(case) = random_case(&mut rng, &all, false) else {
            continue;
        };
        let name = format!("case {n}");
        let reference = battery(&case, &all[..], V::ZERO, case.mask).expect("a slice answers");
        if std::env::var("AUDIT_STATS").is_ok() {
            let text = reference.exact.join("\n");
            eprintln!(
                "STAT hit={} floor={} wall={} ledge={} stepped={} guarded={} pushed={} stuck={} ceiling={}",
                text.contains("cast Some"),
                text.contains("floor: true"),
                text.contains("wall: true"),
                text.contains("ledge "),
                text.contains("stepped: true"),
                text.contains("ledge_guarded: true"),
                text.contains("pushed_out: true"),
                text.contains("stuck: true"),
                text.contains("ceiling: true"),
            );
        }
        check_never_inside(&name, &case, &reference, &all, case.mask);
        check_moves_never_inside(&name, &case, &all, case.mask);

        let reversed: Vec<_> = all.iter().rev().copied().collect();
        let mut doubled: Vec<_> = all.iter().chain(&all).copied().collect();
        rng.shuffle(&mut doubled);
        let mut shuffled = all.clone();
        rng.shuffle(&mut shuffled);
        let cells = level.cell_world();
        let big_set = set_of(&level.big, 1.5);
        let split = rng.below(all.len() + 1);
        let (first, second) = shuffled.split_at(split);
        let worlds: Vec<(String, Box<dyn World<V> + '_>)> = vec![
            ("reversed".into(), Box::new(reversed.as_slice())),
            ("doubled".into(), Box::new(doubled.as_slice())),
            ("cells+big".into(), Box::new((&cells, level.big.as_slice()))),
            ("big set+cells".into(), Box::new((&big_set, &cells))),
            ("split".into(), Box::new((first, set_of(second, 0.75)))),
        ];
        let sets = [0.5, 1.0, 2.0, 3.7].map(|size| set_of(&shuffled, size));
        for (world_name, world) in worlds
            .iter()
            .map(|(name, world)| (name.clone(), &**world))
            .chain(
                sets.iter()
                    .map(|set| (format!("set {}", set.cell_size()), set as &dyn World<V>)),
            )
        {
            let answers = battery(&case, world, V::ZERO, case.mask).expect("answers");
            assert_eq!(
                answers, reference,
                "{name}: {world_name} disagreed\n{case:?}"
            );
        }

        // Ids repeated across colliders: a slice, a pair and a cell world
        // may hold them (physics.md 9). Ties then go to the lowest
        // position.
        let repeated: Vec<_> = all
            .iter()
            .map(|c| Collider { id: c.id % 3, ..*c })
            .collect();
        let reference = battery(&case, &repeated[..], V::ZERO, case.mask).expect("answers");
        let mut shuffled = repeated.clone();
        rng.shuffle(&mut shuffled);
        let (first, second) = shuffled.split_at(split);
        for (world_name, world) in [
            ("repeated shuffled", &shuffled.as_slice() as &dyn World<V>),
            ("repeated split", &(second, first)),
        ] {
            let answers = battery(&case, world, V::ZERO, case.mask).expect("answers");
            assert_eq!(
                answers, reference,
                "{name}: {world_name} disagreed\n{case:?}"
            );
        }
    }
}

#[wasm_bindgen_test(unsupported = test)]
fn order_never_shows_in_2d() {
    order_never_shows::<Vec2>(0x5eed_0001, 120);
}

#[wasm_bindgen_test(unsupported = test)]
fn order_never_shows_in_3d() {
    order_never_shows::<Vec3>(0x5eed_0002, 100);
}

// ----------------------------------------------------- near is as good as far

/// Defect: a query or mover stage computed in world coordinates, or with a
/// tolerance in absolute units, so a body far from the origin moves
/// differently from one near it (physics.md 3). Oracle: the same level and
/// body translated by an exact power of two (every position is on a
/// quarter-unit grid, so the translation is exact and every difference
/// between positions keeps its bits): every query's translation, hit,
/// contacts, velocity and floor must be bit-equal, and the absolute places
/// it ends at equal to within two rounding steps of the translated scene.
/// The mover runs one step, and its `effective` and blocked flags are held
/// to the places' tolerance, not bits: it measures them from absolute
/// positions today, which the audit report carries as a defect with its
/// own failing test. Shifts of 2^12 and 2^14 keep the skin well above the
/// coordinates' rounding step.
fn near_is_as_good_as_far<V: Vector>(seed: u64, cases: usize, shift_by: f32) {
    let mut rng = Rng::new(seed);
    let shift: V = splat(shift_by);
    let ulp = shift_by * f32::EPSILON;
    for n in 0..cases {
        let level: Level<V> = level(&mut rng, true);
        let all = level.all();
        let Some(mut case) = random_case(&mut rng, &all, false) else {
            continue;
        };
        // The body starts on the grid too, so its start translates exactly.
        for axis in 0..V::AXES {
            case.at[axis] = (case.at[axis] * 4.0).floor() * 0.25;
        }
        let far: Vec<_> = all
            .iter()
            .map(|c| Collider {
                position: c.position + shift,
                ..*c
            })
            .collect();
        let near = battery_steps(&case, &all[..], V::ZERO, case.mask, 1).expect("answers");
        let shifted = battery_steps(&case, &far[..], shift, case.mask, 1).expect("answers");
        for (a, b) in near.exact.iter().zip(&shifted.exact) {
            assert_eq!(a, b, "case {n}\n{case:?}\n{all:#?}");
        }
        assert_eq!(near.exact.len(), shifted.exact.len(), "case {n}\n{case:?}");
        assert_eq!(near.places.len(), shifted.places.len());
        for (&(what, a), &(_, b)) in near.places.iter().zip(&shifted.places) {
            for axis in 0..V::AXES {
                assert!(
                    (a[axis] - b[axis]).abs() <= 2.0 * ulp,
                    "case {n}: {what} {a:?} near, {b:?} far\n{case:?}\n{all:#?}\n{near:#?}\n{shifted:#?}"
                );
            }
        }
    }
}

#[wasm_bindgen_test(unsupported = test)]
fn near_is_as_good_as_far_in_2d() {
    near_is_as_good_as_far::<Vec2>(0x5eed_0003, 160, 4096.0);
}

#[wasm_bindgen_test(unsupported = test)]
fn near_is_as_good_as_far_in_3d() {
    near_is_as_good_as_far::<Vec3>(0x5eed_0004, 120, 16_384.0);
}

// ----------------------------------------------- 3D with no z motion is 2D

/// Defect: a 3D path that differs from the 2D one (an algorithm copied per
/// dimension, a z axis that leaks into a tie or a tolerance), for any query
/// or the mover, on ramps (physics.md 7). Oracle: the 2D level lifted into
/// 3D with every collider spanning the body on z and no motion along z
/// must give the 2D answers' x and y bit for bit.
#[wasm_bindgen_test(unsupported = test)]
fn a_3d_world_with_no_z_motion_answers_as_the_2d_one() {
    let lift = |v: Vec2, z: f32| Vec3::new(v.x, v.y, z);
    let mut rng = Rng::new(0x5eed_0005);
    for n in 0..48 {
        let level: Level<Vec2> = level(&mut rng, true);
        let flat = level.all();
        let Some(case) = random_case(&mut rng, &flat, false) else {
            continue;
        };
        let deep: Vec<Collider<Vec3>> = flat
            .iter()
            .map(|c| Collider {
                id: c.id,
                shape: Shape::Box {
                    half: lift(c.shape.half_extents(), 40.0),
                },
                position: lift(c.position, 0.0),
                kind: c.kind,
                layers: c.layers,
                cut: c
                    .cut
                    .map(|cut| sgl_physics::Cut::new(lift(given_normal(&cut), 0.0), cut.offset())),
                velocity: lift(c.velocity, 0.0),
                push: c.push,
            })
            .collect();
        let case3 = Case {
            half: lift(case.half, 0.5),
            at: lift(case.at, 0.0),
            delta: lift(case.delta, 0.0),
            mask: case.mask,
            walking: case.walking,
            options: case.options,
            toward: lift(case.toward, 0.0),
        };
        let two = battery(&case, &flat[..], Vec2::ZERO, case.mask).expect("answers");
        let three = battery(&case3, &deep[..], Vec3::ZERO, case.mask).expect("answers");
        assert_eq!(two.places.len(), three.places.len(), "case {n}\n{case:?}");
        for (&(what, a), &(_, b)) in two.places.iter().zip(&three.places) {
            assert_eq!(
                bits(a),
                bits(Vec2::new(b.x, b.y)),
                "case {n}: {what}\n{case:?}"
            );
        }
        // The exact answers print vectors; compare them with z removed by
        // re-running the flat parts: the hits and contacts' x and y.
        let body2 = Shape::Box { half: case.half };
        let body3 = Shape::Box { half: case3.half };
        let c2 = config(Vec2::Y);
        let c3 = config(Vec3::Y);
        let (m2, k2) = slide(&body2, case.at, case.delta, &flat[..], case.mask, &c2).unwrap();
        let (m3, k3) = slide(&body3, case3.at, case3.delta, &deep[..], case.mask, &c3).unwrap();
        assert_eq!(bits(m2), bits(Vec2::new(m3.x, m3.y)), "case {n}");
        assert_eq!(
            (
                k2.floor,
                k2.wall,
                k2.ceiling,
                k2.last_floor_id,
                k2.last_wall_id
            ),
            (
                k3.floor,
                k3.wall,
                k3.ceiling,
                k3.last_floor_id,
                k3.last_wall_id
            ),
            "case {n}"
        );
        let h2 = cast(&body2, case.at, case.delta, &flat[..], case.mask).unwrap();
        let h3 = cast(&body3, case3.at, case3.delta, &deep[..], case.mask).unwrap();
        assert_eq!(
            h2.map(|h| (h.t.to_bits(), bits(h.normal), h.id)),
            h3.map(|h| (h.t.to_bits(), bits(Vec2::new(h.normal.x, h.normal.y)), h.id)),
            "case {n}"
        );
    }
}

// ------------------------------------------------------ y-up and y-down

/// Defect: a rule that reads y as up instead of the configured axis (a
/// floor test, a tie order, a one-way face, the snap or step direction), so
/// a y-down game moves otherwise than a y-up one (physics.md 12). Oracle:
/// the level, the body and its motion mirrored across y, with `up`
/// `-y`: every place every query and the mover end at is the mirror image,
/// exactly, on levels with ramps, one-way platforms and sensors.
fn results_mirror_between_y_up_and_y_down<V: Vector>(seed: u64, cases: usize) {
    let mut rng = Rng::new(seed);
    let flip = |mut v: V| {
        v[1] = -v[1];
        v
    };
    for n in 0..cases {
        let level: Level<V> = level(&mut rng, true);
        let all = level.all();
        let Some(case) = random_case(&mut rng, &all, false) else {
            continue;
        };
        let mirrored: Vec<_> = all
            .iter()
            .map(|c| Collider {
                position: flip(c.position),
                cut: c
                    .cut
                    .map(|cut| sgl_physics::Cut::new(flip(given_normal(&cut)), cut.offset())),
                ..*c
            })
            .collect();
        let mirrored_case = Case {
            at: flip(case.at),
            delta: flip(case.delta),
            ..case
        };
        let up: V = up();
        let a = battery_up(&case, &all[..], V::ZERO, case.mask, 3, up).expect("answers");
        let b =
            battery_up(&mirrored_case, &mirrored[..], V::ZERO, case.mask, 3, -up).expect("answers");
        assert_eq!(a.places.len(), b.places.len(), "case {n}\n{case:?}");
        for (&(what, p), &(_, q)) in a.places.iter().zip(&b.places) {
            assert!(
                (0..V::AXES).all(|axis| p[axis] == flip(q)[axis]),
                "case {n}: {what} {p:?} and mirrored {q:?}\n{case:?}"
            );
        }
    }
}

#[wasm_bindgen_test(unsupported = test)]
fn results_mirror_between_y_up_and_y_down_in_2d() {
    results_mirror_between_y_up_and_y_down::<Vec2>(0x5eed_0018, 120);
}

#[wasm_bindgen_test(unsupported = test)]
fn results_mirror_between_y_up_and_y_down_in_3d() {
    results_mirror_between_y_up_and_y_down::<Vec3>(0x5eed_0019, 100);
}

// ------------------------------------------------------- Unanswered

/// Defect: a query or a mover stage that swallows a world's refusal
/// (treating an unanswered probe as "nothing there") or acts on part of an
/// answer, so a body moves, or its state changes, on a world it could not
/// see (physics.md 8, 10, 17). Oracle: the contract. A world that answers
/// its first `k` queries and refuses from then on, for every `k` until a
/// call stops asking, makes each function either answer exactly as the
/// slice does (when it never reached the refusal) or answer `Unanswered`,
/// and the mover leave its state bit for bit as it was; a pair with a
/// refusing side refuses either way round.
fn unanswered_moves_nothing<V: Vector>(seed: u64, cases: usize) {
    let mut rng = Rng::new(seed);
    let config = config(up::<V>());
    for n in 0..cases {
        let level: Level<V> = level(&mut rng, true);
        let all = level.all();
        let Some(case) = random_case(&mut rng, &all, false) else {
            continue;
        };
        let body = Shape::Box { half: case.half };
        let reference = battery(&case, &all[..], V::ZERO, case.mask).expect("answers");
        for k in 0..64 {
            let refusing = Refusing {
                inner: &all,
                refuse_at: k,
                asked: std::cell::Cell::new(0),
            };
            let answered = battery(&case, &refusing, V::ZERO, case.mask);
            let refused = refusing.asked.get() > k;
            match answered {
                Ok(answers) => {
                    assert!(!refused, "case {n}: a refusal at query {k} was swallowed");
                    assert_eq!(answers, reference, "case {n}");
                }
                Err(Unanswered) => assert!(refused, "case {n}: unanswered unasked"),
            }

            // The mover alone, one step: a refusal leaves the state as it
            // was.
            let refusing = Refusing {
                inner: &all,
                refuse_at: k,
                asked: std::cell::Cell::new(0),
            };
            let start = MoverState {
                position: case.at,
                velocity: case.delta * (1.0 / DT),
                floor: case.walking.then_some(Floor {
                    normal: up(),
                    id: 0,
                }),
            };
            let mut state = start;
            let result = move_and_slide(&mut state, &body, DT, &refusing, &config, &case.options);
            if refusing.asked.get() > k {
                assert_eq!(result, Err(Unanswered), "case {n}, refusal at {k}");
                assert_eq!(
                    format!("{state:?}"),
                    format!("{start:?}"),
                    "case {n}: refusal at {k} changed the state"
                );
            } else {
                assert!(result.is_ok());
            }
            if !refused {
                break;
            }
        }
        let never = Refusing {
            inner: &all,
            refuse_at: 0,
            asked: std::cell::Cell::new(0),
        };
        for pair in [
            &(&all[..], &never) as &dyn World<V>,
            &(&never, &all[..]) as &dyn World<V>,
        ] {
            assert_eq!(
                cast(&body, case.at, case.delta, pair, case.mask),
                Err(Unanswered)
            );
            assert_eq!(overlaps(&body, case.at, pair, case.mask), Err(Unanswered));
        }
    }
}

#[wasm_bindgen_test(unsupported = test)]
fn unanswered_moves_nothing_in_2d() {
    unanswered_moves_nothing::<Vec2>(0x5eed_0006, 16);
}

#[wasm_bindgen_test(unsupported = test)]
fn unanswered_moves_nothing_in_3d() {
    unanswered_moves_nothing::<Vec3>(0x5eed_0007, 12);
}

// ----------------------------------------- layers, sensors and one-ways

/// Defect: a query that reads a collider its mask leaves out, lets a
/// sensor block, lets a dropped-through platform still catch a body, or
/// meets a one-way platform in a cast as anything but a box (physics.md 9,
/// 15, 17). Oracle: a second world. Every query with a mask must answer as
/// the same query with every layer over the level less the colliders the
/// mask leaves out; every query but `overlaps` must answer alike with and
/// without the sensors; the mover dropping through must answer as the
/// mover over the level without its one-way platforms; and `cast` and
/// `ray` must answer as over the level with its one-way platforms made
/// solid.
fn masks_sensors_and_one_ways_are_the_world_without_them<V: Vector>(seed: u64, cases: usize) {
    let mut rng = Rng::new(seed);
    let config = config(up::<V>());
    for n in 0..cases {
        let level: Level<V> = level(&mut rng, true);
        let all = level.all();
        let Some(case) = random_case(&mut rng, &all, false) else {
            continue;
        };
        let name = format!("case {n}");
        let body = Shape::Box { half: case.half };
        for mask in [0, 1, 2, 4] {
            let kept: Vec<_> = all
                .iter()
                .filter(|c| c.layers & mask != 0)
                .copied()
                .collect();
            let masked = battery(&case, &all[..], V::ZERO, mask).expect("answers");
            let filtered = battery(&case, &kept[..], V::ZERO, ALL_LAYERS).expect("answers");
            assert_eq!(masked, filtered, "{name}: mask {mask}\n{case:?}");
        }

        let solid_only: Vec<_> = all
            .iter()
            .filter(|c| c.kind != ColliderKind::Sensor)
            .copied()
            .collect();
        let strip = |answers: Answers<V>| Answers {
            exact: answers
                .exact
                .into_iter()
                .filter(|line| !line.starts_with("overlap"))
                .collect(),
            places: answers
                .places
                .into_iter()
                .filter(|(what, _)| *what != "overlap")
                .collect(),
        };
        let with = strip(battery(&case, &all[..], V::ZERO, case.mask).unwrap());
        let without = strip(battery(&case, &solid_only[..], V::ZERO, case.mask).unwrap());
        assert_eq!(with, without, "{name}: a sensor showed\n{case:?}");

        let no_one_ways: Vec<_> = all
            .iter()
            .filter(|c| c.kind != ColliderKind::OneWay)
            .copied()
            .collect();
        let dropping = MoverOptions {
            drop_through: true,
            mask: case.mask,
            ..case.options
        };
        let kept = MoverOptions {
            drop_through: false,
            ..dropping
        };
        let start = MoverState {
            position: case.at,
            velocity: case.delta * (1.0 / DT),
            floor: case.walking.then_some(Floor {
                normal: up(),
                id: 0,
            }),
        };
        let (mut a, mut b) = (start, start);
        for _ in 0..3 {
            let ca = move_and_slide(&mut a, &body, DT, &all[..], &config, &dropping).unwrap();
            let cb = move_and_slide(&mut b, &body, DT, &no_one_ways[..], &config, &kept).unwrap();
            assert_eq!(format!("{a:?} {ca:?}"), format!("{b:?} {cb:?}"), "{name}");
        }

        let made_solid: Vec<_> = all
            .iter()
            .map(|c| {
                if c.kind == ColliderKind::OneWay {
                    Collider {
                        kind: ColliderKind::Solid,
                        ..*c
                    }
                } else {
                    *c
                }
            })
            .collect();
        assert_eq!(
            cast(&body, case.at, case.delta, &all[..], case.mask),
            cast(&body, case.at, case.delta, &made_solid[..], case.mask),
            "{name}"
        );
        assert_eq!(
            ray(case.at, case.delta, &all[..], case.mask),
            ray(case.at, case.delta, &made_solid[..], case.mask),
            "{name}"
        );
    }
}

#[wasm_bindgen_test(unsupported = test)]
fn masks_sensors_and_one_ways_are_the_world_without_them_in_2d() {
    masks_sensors_and_one_ways_are_the_world_without_them::<Vec2>(0x5eed_0008, 40);
}

#[wasm_bindgen_test(unsupported = test)]
fn masks_sensors_and_one_ways_are_the_world_without_them_in_3d() {
    masks_sensors_and_one_ways_are_the_world_without_them::<Vec3>(0x5eed_0009, 32);
}

// ---------------------------------------------------------------- overlaps

/// Defect: `overlaps` missing a collider the body overlaps (a cut box met
/// as its whole box, or not at all; a sensor or one-way left out), listing
/// one it only touches or a part a cut removed, listing one twice, or in
/// an order that shows the world (physics.md 9, 15, 16). Oracle: a
/// brute-force scan with the `f64` shape oracle over every collider on the
/// mask that occupies space, sorted by id then position, each once, from
/// bodies at random places, including through a doubled slice.
fn overlaps_lists_exactly_what_the_body_overlaps<V: Vector>(seed: u64, cases: usize) {
    let mut rng = Rng::new(seed);
    let mut listed_any = 0;
    for n in 0..cases {
        let level: Level<V> = level(&mut rng, true);
        let all = level.all();
        let mut half = V::ZERO;
        for axis in 0..V::AXES {
            half[axis] = rng.range(0.3, 3.0);
        }
        let at: V = rng.vector(-6.0, 6.0);
        let mask = rng.pick(&[ALL_LAYERS, 1, 2]);
        let mut expected: Vec<_> = all
            .iter()
            .filter(|c| c.layers & mask != 0 && overlapping(at, half, c))
            .copied()
            .collect();
        expected.sort_by(|a, b| {
            a.id.cmp(&b.id).then_with(|| {
                (0..V::AXES)
                    .map(|axis| a.position[axis].total_cmp(&b.position[axis]))
                    .find(|o| o.is_ne())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
        });
        let doubled: Vec<_> = all.iter().chain(&all).copied().collect();
        let body = Shape::Box { half };
        for world in [&all.as_slice() as &dyn World<V>, &doubled.as_slice()] {
            let listed = overlaps(&body, at, world, mask).unwrap();
            assert_eq!(listed, expected, "case {n}: body {half:?} at {at:?}");
        }
        listed_any += usize::from(!expected.is_empty());
    }
    assert!(listed_any > cases / 4, "too few cases overlapped anything");
}

#[wasm_bindgen_test(unsupported = test)]
fn overlaps_lists_exactly_what_the_body_overlaps_in_2d() {
    overlaps_lists_exactly_what_the_body_overlaps::<Vec2>(0x5eed_000a, 200);
}

#[wasm_bindgen_test(unsupported = test)]
fn overlaps_lists_exactly_what_the_body_overlaps_in_3d() {
    overlaps_lists_exactly_what_the_body_overlaps::<Vec3>(0x5eed_000b, 200);
}

// ------------------------------------------------------- started inside

/// Defect: a slide, snap or collide that drives a body deeper into a solid
/// it starts inside, or into another, and a push-out that leaves a body
/// inside something or moves a stuck one (physics.md 13, 15, 17). Oracle:
/// the `f64` shape oracle. `slide`, `snap_to_ground` and
/// `move_and_collide` never grow the depth of any solid the body starts
/// in by more than a rounding step and never end inside another; the mover
/// either frees the body (ending inside nothing) or reports it `stuck` and
/// leaves its position as it was.
fn a_body_started_inside_never_goes_deeper<V: Vector>(seed: u64, cases: usize) {
    let mut rng = Rng::new(seed);
    let config = config(up::<V>());
    let (mut freed, mut stuck) = (0, 0);
    for n in 0..cases {
        let level: Level<V> = level(&mut rng, true);
        let all = level.all();
        let Some(case) = random_case(&mut rng, &all, true) else {
            continue;
        };
        let body = Shape::Box { half: case.half };
        let solids: Vec<_> = all
            .iter()
            .filter(|c| c.kind == ColliderKind::Solid && c.layers & case.mask != 0)
            .collect();
        let before: Vec<f64> = solids
            .iter()
            .map(|c| audit_support::depth(case.at, case.half, c))
            .collect();
        let (moved, _) = slide(&body, case.at, case.delta, &all[..], case.mask, &config).unwrap();
        let (snap, _) = snap_to_ground(&body, case.at, &all[..], case.mask, &config).unwrap();
        let (collided, _) =
            move_and_collide(&body, case.at, case.delta, &all[..], case.mask, &config).unwrap();
        for (what, by) in [("slide", moved), ("snap", snap), ("collide", collided)] {
            for (c, &was) in solids.iter().zip(&before) {
                let now = audit_support::depth(case.at + by, case.half, c);
                let tol = audit_support::rounding(case.at + by, case.half, c);
                assert!(
                    now <= was.max(0.0) + tol,
                    "case {n}: {what} by {by:?} went from {was} to {now} into {}\n{case:?}",
                    c.id
                );
            }
        }

        let mut state = MoverState {
            position: case.at,
            velocity: case.delta * (1.0 / DT),
            floor: None,
        };
        let options = MoverOptions {
            mask: case.mask,
            ..case.options
        };
        let contacts = move_and_slide(&mut state, &body, DT, &all[..], &config, &options).unwrap();
        if contacts.stuck {
            stuck += 1;
            assert_eq!(
                bits(state.position),
                bits(case.at),
                "case {n}: stuck but moved"
            );
        } else {
            freed += usize::from(contacts.pushed_out);
            let inside = solids_inside(state.position, case.half, &all, case.mask);
            assert!(
                inside.is_empty(),
                "case {n}: ended at {:?} inside {inside:?}\n{case:?}",
                state.position
            );
        }
    }
    assert!(
        freed > cases / 8 && stuck > 0,
        "freed {freed}, stuck {stuck}"
    );
}

#[wasm_bindgen_test(unsupported = test)]
fn a_body_started_inside_never_goes_deeper_in_2d() {
    a_body_started_inside_never_goes_deeper::<Vec2>(0x5eed_000c, 160);
}

#[wasm_bindgen_test(unsupported = test)]
fn a_body_started_inside_never_goes_deeper_in_3d() {
    a_body_started_inside_never_goes_deeper::<Vec3>(0x5eed_000d, 120);
}

// -------------------------------------------------------------- push-out

/// The push-out the contract describes, worked by brute force (physics.md
/// 17): along each axis, no move or a move that puts the body a `skin`
/// from a face of a solid across that axis, no further than the body's
/// own size; of every combination that leaves the body inside no solid
/// (the `f64` oracle), the shortest. `None` when none frees it.
fn shortest_push_out<V: Vector>(at: V, half: V, solids: &[Collider<V>]) -> Option<f64> {
    let mut per_axis: Vec<Vec<f32>> = Vec::new();
    for axis in 0..V::AXES {
        let mut options = vec![0.0f32];
        for c in solids {
            let reach = c.shape.half_extents()[axis] + half[axis] + SKIN;
            for side in [-1.0, 1.0] {
                let d = c.position[axis] + side * reach - at[axis];
                if d.abs() <= 2.0 * half[axis] {
                    options.push(d);
                }
            }
        }
        per_axis.push(options);
    }
    let mut best: Option<f64> = None;
    let mut index = vec![0usize; V::AXES];
    loop {
        let mut d = V::ZERO;
        for axis in 0..V::AXES {
            d[axis] = per_axis[axis][index[axis]];
        }
        let free = solids.iter().all(|c| {
            audit_support::depth(at + d, half, c) <= audit_support::rounding(at + d, half, c)
        });
        if d != V::ZERO && free {
            let length = (0..V::AXES)
                .map(|axis| f64::from(d[axis]).powi(2))
                .sum::<f64>()
                .sqrt();
            best = Some(best.map_or(length, |b| b.min(length)));
        }
        let mut axis = 0;
        loop {
            if axis == V::AXES {
                return best;
            }
            index[axis] += 1;
            if index[axis] < per_axis[axis].len() {
                break;
            }
            index[axis] = 0;
            axis += 1;
        }
    }
}

/// Defect: a push-out that takes a longer way than it needs, reaches
/// further than the body's size, frees a body it could not, reports
/// `stuck` with a free place in reach, or is pushed by a one-way platform
/// or a sensor (physics.md 17). Oracle: the brute-force search above over
/// whole boxes (at most three solids, so at most six distances per axis,
/// under the cap of eight), and the `f64` shape oracle.
fn push_out_takes_the_shortest_way<V: Vector>(seed: u64, cases: usize) {
    let mut rng = Rng::new(seed);
    let config = config(up::<V>());
    let (mut pushed, mut stuck, mut untouched) = (0, 0, 0);
    for n in 0..cases {
        let mut half = V::ZERO;
        for axis in 0..V::AXES {
            half[axis] = rng.grid(2, 8, 0.125);
        }
        let at: V = rng.vector(-0.5, 0.5);
        let mut colliders = Vec::new();
        for id in 0..rng.int(1, 4) as u64 {
            let mut size = V::ZERO;
            let mut place = V::ZERO;
            for axis in 0..V::AXES {
                size[axis] = rng.grid(1, 8, 0.125);
                place[axis] = rng.grid(-12, 12, 0.125);
            }
            let kind = match rng.below(6) {
                0 => ColliderKind::OneWay,
                1 => ColliderKind::Sensor,
                _ => ColliderKind::Solid,
            };
            colliders.push(Collider::new(id, Shape::Box { half: size }, place).with_kind(kind));
        }
        let solids: Vec<_> = colliders
            .iter()
            .filter(|c| c.kind == ColliderKind::Solid)
            .copied()
            .collect();
        let inside = solids
            .iter()
            .any(|c| audit_support::depth(at, half, c) > 0.0);
        let expected = shortest_push_out(at, half, &solids);
        let mut state = MoverState {
            position: at,
            velocity: V::ZERO,
            floor: None,
        };
        let body = Shape::Box { half };
        let contacts = move_and_slide(
            &mut state,
            &body,
            DT,
            &colliders[..],
            &config,
            &MoverOptions::default(),
        )
        .unwrap();
        let moved = state.position - at;
        let length = f64::from(dot(moved, moved)).sqrt();
        let context = format!("case {n}: body {half:?} at {at:?} in {colliders:?}");
        if !inside {
            untouched += 1;
            assert!(!contacts.pushed_out && !contacts.stuck, "{context}");
            assert_eq!(bits(state.position), bits(at), "{context}");
            continue;
        }
        match expected {
            None => {
                stuck += 1;
                assert!(contacts.stuck, "{context}: freed by {moved:?}");
                assert_eq!(bits(state.position), bits(at), "{context}");
            }
            Some(shortest) => {
                pushed += 1;
                assert!(
                    contacts.pushed_out && !contacts.stuck,
                    "{context}: not freed, {shortest} would"
                );
                assert!(
                    (length - shortest).abs() <= 1e-4,
                    "{context}: pushed {length} by {moved:?}, shortest {shortest}"
                );
                for axis in 0..V::AXES {
                    assert!(moved[axis].abs() <= 2.0 * half[axis] + 1e-4, "{context}");
                }
                let inside = solids_inside(state.position, half, &colliders, ALL_LAYERS);
                assert!(inside.is_empty(), "{context}: still inside {inside:?}");
            }
        }
    }
    assert!(
        pushed > cases / 4 && stuck > 0 && untouched > 0,
        "pushed {pushed}, stuck {stuck}, untouched {untouched}"
    );
}

#[wasm_bindgen_test(unsupported = test)]
fn push_out_takes_the_shortest_way_in_2d() {
    push_out_takes_the_shortest_way::<Vec2>(0x5eed_000e, 300);
}

#[wasm_bindgen_test(unsupported = test)]
fn push_out_takes_the_shortest_way_in_3d() {
    push_out_takes_the_shortest_way::<Vec3>(0x5eed_000f, 200);
}

// ------------------------------------------------------------ mover rules

/// A ground slab whose top is at y = 0, and up to three boxes on it ahead
/// of a body at x = 0 whose half-width is `reach`: steps of random height,
/// some with a ceiling over them. Half the time the ground ends a little
/// past the first step, over a void, so a climb can carry a body into the
/// air.
fn steps_level<V: Vector>(rng: &mut Rng, reach: f32) -> Vec<Collider<V>> {
    let mut ground_half: V = splat(8.0);
    ground_half[1] = 1.0;
    let mut ground_at = V::ZERO;
    ground_at[1] = -1.0;
    let mut out = vec![Collider::new(
        100,
        Shape::Box { half: ground_half },
        ground_at,
    )];
    let void = rng.chance(50);
    for id in 0..rng.int(1, 3) as u64 {
        let height = rng.grid(1, 40, 0.05);
        let mut half: V = splat(rng.grid(2, 12, 0.125));
        half[1] = height * 0.5;
        let mut at = V::ZERO;
        at[0] = reach + half[0] + rng.grid(1, 16, 0.125);
        at[1] = height * 0.5;
        if V::AXES == 3 {
            at[2] = rng.grid(-4, 4, 0.125);
        }
        out.push(Collider::new(id, Shape::Box { half }, at));
        if void && id == 0 {
            // The ground runs from x = -8 to just past this step.
            let end = at[0] + half[0] + rng.grid(0, 4, 0.125);
            out[0].shape = Shape::Box {
                half: {
                    let mut h = ground_half;
                    h[0] = (end + 8.0) * 0.5;
                    h
                },
            };
            out[0].position[0] = (end - 8.0) * 0.5;
        }
        if rng.chance(30) {
            let mut lid = half;
            lid[1] = 0.25;
            let mut over = at;
            over[1] = height + rng.grid(4, 20, 0.125) + 0.25;
            out.push(Collider::new(id + 10, Shape::Box { half: lid }, over));
        }
    }
    out
}

/// Defect: a step-up kept when it ends in the air, kept when it gets no
/// further than the plain move, taken by a body that is not walking or
/// with no step height, climbing higher than the step height, or changing
/// a move it does not keep (physics.md 17). Oracle: the contract read
/// against a second run of the same step with `step_height` zero (the
/// plain move it is defined against): a step that is not kept leaves
/// exactly the plain result; one that is kept ends on a floor, gains more
/// than `block_epsilon` along the requested direction across `up` over
/// the plain move, rises no more than `step_height`, and ends inside no
/// solid.
fn a_step_up_is_kept_only_onto_a_floor_further_on<V: Vector>(seed: u64, cases: usize) {
    let mut rng = Rng::new(seed);
    let config = config(up::<V>());
    let mut stepped = 0;
    for n in 0..cases {
        let mut half: V = splat(rng.grid(2, 5, 0.125));
        half[1] = rng.grid(3, 8, 0.125);
        let world = steps_level::<V>(&mut rng, half[0]);
        let mut at = V::ZERO;
        at[1] = half[1] + SKIN;
        let mut velocity: V = rng.vector(-1.0, 1.0);
        // Some fast enough to carry the body clear over a step in one move.
        velocity[0] = if rng.chance(70) {
            rng.range(0.5, 12.0)
        } else {
            rng.range(12.0, 48.0)
        };
        velocity[1] = rng.pick(&[0.0, -0.5, 0.5]);
        let walking = rng.chance(80);
        let step_height = rng.grid(1, 24, 0.05);
        let start = MoverState {
            position: at,
            velocity,
            floor: walking.then_some(Floor {
                normal: up(),
                id: 100,
            }),
        };
        let body = Shape::Box { half };
        let run = |step_height: f32| {
            let mut state = start;
            let options = MoverOptions {
                step_height,
                ..MoverOptions::default()
            };
            let contacts = move_and_slide(&mut state, &body, DT, &world[..], &config, &options)
                .expect("a slice answers");
            (state, contacts)
        };
        let (plain, plain_contacts) = run(0.0);
        let (state, contacts) = run(step_height);
        let context = format!("case {n}: {half:?} {start:?} step {step_height} in {world:?}");
        if !contacts.stepped {
            assert_eq!(
                format!("{plain:?} {plain_contacts:?}"),
                format!("{state:?} {contacts:?}"),
                "{context}"
            );
            continue;
        }
        stepped += 1;
        assert!(
            walking && velocity[1] <= 0.0,
            "{context}: stepped, not walking"
        );
        assert!(state.floor.is_some(), "{context}: stepped into the air");
        let mut across = velocity;
        across[1] = 0.0;
        let gain = |end: V| dot(end - at, across) / dot(across, across).sqrt();
        assert!(
            gain(state.position) > gain(plain.position) + config.block_epsilon(),
            "{context}: no further than the plain move"
        );
        assert!(
            state.position[1] - at[1] <= step_height + 1e-4,
            "{context}: climbed {}",
            state.position[1] - at[1]
        );
        let inside = solids_inside(state.position, half, &world, ALL_LAYERS);
        assert!(inside.is_empty(), "{context}: inside {inside:?}");
    }
    assert!(stepped > cases / 20, "only {stepped} steps kept");
}

#[wasm_bindgen_test(unsupported = test)]
fn a_step_up_is_kept_only_onto_a_floor_further_on_in_2d() {
    a_step_up_is_kept_only_onto_a_floor_further_on::<Vec2>(0x5eed_0010, 300);
}

#[wasm_bindgen_test(unsupported = test)]
fn a_step_up_is_kept_only_onto_a_floor_further_on_in_3d() {
    a_step_up_is_kept_only_onto_a_floor_further_on::<Vec3>(0x5eed_0011, 200);
}

/// Defect: a ledge guard that lets a walking body leave its support (a
/// halving that ends on the wrong side, an axis made again from the wrong
/// place, a one-way top not counted as ground, a depth read without the
/// snap distance or the skin), or one that guards a body that is not
/// walking (physics.md 17). Oracle: the boxes' own geometry. A walking
/// body that starts on a floor and moves with `ledge_drop` set ends with
/// the top face of a solid or one-way box under its footprint (closed:
/// its trailing edge may reach the edge) no further below its bottom than
/// the larger of `ledge_drop` and the snap distance, plus `skin`; and when
/// the same step without the guard ends so supported, the guard changes
/// nothing (a second run).
fn the_ledge_guard_never_leaves_a_walking_body_unsupported<V: Vector>(seed: u64, cases: usize) {
    let mut rng = Rng::new(seed);
    let config = config(up::<V>());
    let mut guarded = 0;
    for n in 0..cases {
        let mut world = Vec::new();
        for id in 0..rng.int(1, 4) as u64 {
            let mut half: V = splat(rng.grid(2, 16, 0.125));
            half[1] = 0.25;
            let mut at: V = V::ZERO;
            for axis in 0..V::AXES {
                at[axis] = rng.grid(-16, 16, 0.125);
            }
            // Floors a thirty-second apart in height, so drops shorter
            // than the snap distance occur.
            at[1] = rng.grid(-64, 0, 1.0 / 32.0) - 0.25;
            let kind = if rng.chance(25) {
                ColliderKind::OneWay
            } else {
                ColliderKind::Solid
            };
            world.push(Collider::new(id, Shape::Box { half }, at).with_kind(kind));
        }
        if rng.chance(50) {
            // A step down along +x, shorter than the snap distance or not.
            let width = rng.grid(4, 16, 0.125);
            let mut half: V = splat(width);
            half[1] = 0.25;
            let mut high = V::ZERO;
            high[1] = -0.25;
            let mut low = high;
            low[0] = 2.0 * width;
            low[1] -= rng.grid(1, 6, 1.0 / 32.0);
            world = vec![
                Collider::new(0, Shape::Box { half }, high),
                Collider::new(1, Shape::Box { half }, low),
            ];
        }
        let floor = world[0];
        let mut half: V = splat(rng.grid(2, 5, 0.125));
        half[1] = 0.5;
        let mut at = floor.position;
        for axis in 0..V::AXES {
            if axis != 1 {
                at[axis] += rng.range(-1.0, 1.0) * floor.shape.half_extents()[axis];
            }
        }
        at[1] = floor.position[1] + 0.25 + half[1] + SKIN;
        if !solids_inside(at, half, &world, ALL_LAYERS).is_empty() {
            continue;
        }
        let mut velocity: V = rng.vector(-12.0, 12.0);
        velocity[1] = -rng.range(0.0, 1.0);
        let drop = rng.pick(&[0.0, 0.0, 1.0 / 32.0, 0.125, 0.5, 1.5]);
        let mut state = MoverState {
            position: at,
            velocity,
            floor: Some(Floor {
                normal: up(),
                id: floor.id,
            }),
        };
        let options = MoverOptions {
            ledge_drop: Some(drop),
            ..MoverOptions::default()
        };
        let body = Shape::Box { half };
        let start = state;
        let contacts = move_and_slide(&mut state, &body, DT, &world[..], &config, &options)
            .expect("a slice answers");
        guarded += usize::from(contacts.ledge_guarded);
        let depth = drop.max(config.snap_distance()) + SKIN;
        // Whether a floor's top lies under the footprint at `end`, no
        // further below its bottom than `depth`, by `margin` or more.
        let supported = |end: V, margin: f32| {
            world.iter().any(|c| {
                let ch = c.shape.half_extents();
                let gap = (end[1] - half[1]) - (c.position[1] + ch[1]);
                (-1e-4..=depth - margin).contains(&gap)
                    && (0..V::AXES)
                        .filter(|&a| a != 1)
                        .all(|a| (end[a] - c.position[a]).abs() <= half[a] + ch[a] - margin)
            })
        };
        let end = state.position;
        assert!(
            supported(end, -1e-4),
            "case {n}: unsupported at {end:?} from {at:?} by {velocity:?}, drop {drop}, {contacts:?}\n{world:?}"
        );
        // A move that ends supported without the guard is not guarded: the
        // guard changes nothing.
        let mut plain = start;
        let plain_contacts = move_and_slide(
            &mut plain,
            &body,
            DT,
            &world[..],
            &config,
            &MoverOptions::default(),
        )
        .expect("a slice answers");
        if supported(plain.position, 1e-3) {
            assert_eq!(
                format!("{plain:?} {plain_contacts:?}"),
                format!("{state:?} {contacts:?}"),
                "case {n}: guarded a supported move, drop {drop}\n{world:?}"
            );
        }
    }
    assert!(guarded > cases / 10, "only {guarded} guarded");
}

#[wasm_bindgen_test(unsupported = test)]
fn the_ledge_guard_never_leaves_a_walking_body_unsupported_in_2d() {
    the_ledge_guard_never_leaves_a_walking_body_unsupported::<Vec2>(0x5eed_0012, 300);
}

#[wasm_bindgen_test(unsupported = test)]
fn the_ledge_guard_never_leaves_a_walking_body_unsupported_in_3d() {
    the_ledge_guard_never_leaves_a_walking_body_unsupported::<Vec3>(0x5eed_0013, 200);
}

/// Defect: a body at rest on a floor that creeps (a skin back-off that
/// rounds toward or away from the floor each step), loses its floor for a
/// step, or keeps a fall in its velocity, or one standing over a seam
/// whose floor shows which box a world visited first (physics.md 6, 17).
/// Oracle: the requirement. Under gravity for two seconds, a body resting
/// a skin above one to three flush boxes (the floor's top at a random
/// height) never moves across `up` and stays within the skin above the
/// floor (a fall shorter than the gap closes part of it without touching,
/// as the existing scenarios' `rests` allows), keeps no velocity, and
/// stands every step on the lowest-id box under its footprint, with the
/// floor normal `up`.
fn a_resting_body_stays_at_rest_on_its_floor<V: Vector>(seed: u64, cases: usize) {
    let mut rng = Rng::new(seed);
    let config = config(up::<V>());
    let gravity = 20.0;
    let dt = 1.0 / 60.0;
    for n in 0..cases {
        let top = rng.grid(-64, 64, 0.125);
        let width = rng.grid(2, 16, 0.125);
        let count = rng.int(1, 3);
        let mut ids: Vec<u64> = (0..count as u64).map(|i| i * 5 + 2).collect();
        rng.shuffle(&mut ids);
        let world: Vec<_> = ids
            .iter()
            .enumerate()
            .map(|(i, &id)| {
                let mut half: V = splat(width);
                half[1] = 0.5;
                let mut at = V::ZERO;
                at[0] = (2 * i as i32 - count + 1) as f32 * width;
                at[1] = top - 0.5;
                Collider::new(id, Shape::Box { half }, at)
            })
            .collect();
        let mut half: V = splat(rng.grid(2, 8, 0.125));
        half[1] = rng.grid(2, 8, 0.125);
        let mut at = V::ZERO;
        at[0] = rng.range(-1.0, 1.0) * (count as f32 * width);
        if V::AXES == 3 {
            at[2] = rng.range(-1.0, 1.0) * width;
        }
        at[1] = top + half[1] + SKIN;
        let under: Vec<u64> = world
            .iter()
            .filter(|c| (at[0] - c.position[0]).abs() < half[0] + width)
            .map(|c| c.id)
            .collect();
        let expected = Floor {
            normal: up(),
            id: *under.iter().min().expect("the body stands on the floor"),
        };
        let mut state = MoverState {
            position: at,
            velocity: V::ZERO,
            floor: Some(expected),
        };
        let body = Shape::Box { half };
        for step in 0..120 {
            state.velocity[1] -= gravity * dt;
            move_and_slide(
                &mut state,
                &body,
                dt,
                &world[..],
                &config,
                &MoverOptions::default(),
            )
            .expect("a slice answers");
            let context = format!("case {n}, step {step}: {state:?} from {at:?} on {world:?}");
            assert_eq!(state.floor, Some(expected), "{context}");
            assert_eq!(state.velocity, V::ZERO, "{context}");
            for axis in (0..V::AXES).filter(|&axis| axis != 1) {
                assert_eq!(state.position[axis], at[axis], "{context}");
            }
            let gap = state.position[1] - half[1] - top;
            assert!((0.0..=SKIN + 1e-5).contains(&gap), "{context}");
        }
    }
}

#[wasm_bindgen_test(unsupported = test)]
fn a_resting_body_stays_at_rest_on_its_floor_in_2d() {
    a_resting_body_stays_at_rest_on_its_floor::<Vec2>(0x5eed_0014, 24);
}

#[wasm_bindgen_test(unsupported = test)]
fn a_resting_body_stays_at_rest_on_its_floor_in_3d() {
    a_resting_body_stays_at_rest_on_its_floor::<Vec3>(0x5eed_0015, 24);
}

/// Defect: a velocity that keeps a part into a face the step met (a floor
/// that leaves the fall, a wall that leaves the push, a ceiling that leaves
/// the rise, a later face that turns it back into an earlier one), so the
/// next step drives into it again (physics.md 14, 17). Oracle: the
/// contract, through the contacts' own last normals: after a step whose
/// contacts report a floor, the velocity has no part against `up`; a
/// wall or a ceiling, no part into its last normal.
fn velocity_keeps_no_part_into_a_face_met<V: Vector>(seed: u64, cases: usize) {
    let mut rng = Rng::new(seed);
    let config = config(up::<V>());
    let mut met = 0;
    for n in 0..cases {
        let level: Level<V> = level(&mut rng, true);
        let all = level.all();
        let Some(case) = random_case(&mut rng, &all, false) else {
            continue;
        };
        let mut state = MoverState {
            position: case.at,
            velocity: case.delta * (1.0 / DT),
            floor: case.walking.then_some(Floor {
                normal: up(),
                id: 0,
            }),
        };
        let body = Shape::Box { half: case.half };
        let contacts =
            move_and_slide(&mut state, &body, DT, &all[..], &config, &case.options).unwrap();
        let v = state.velocity;
        let tol = 1e-5 * (1.0 + dot(v, v).sqrt());
        let context = format!("case {n}: {state:?} {contacts:?}\n{case:?}");
        if contacts.floor {
            assert!(dot(v, up()) >= -tol, "{context}");
        }
        if contacts.wall {
            assert!(dot(v, contacts.last_wall_normal) >= -tol, "{context}");
        }
        if contacts.ceiling {
            assert!(dot(v, contacts.last_ceiling_normal) >= -tol, "{context}");
        }
        met += usize::from(contacts.floor || contacts.wall || contacts.ceiling);
    }
    assert!(met > cases / 5, "only {met} cases met a face");
}

#[wasm_bindgen_test(unsupported = test)]
fn velocity_keeps_no_part_into_a_face_met_in_2d() {
    velocity_keeps_no_part_into_a_face_met::<Vec2>(0x5eed_0016, 300);
}

#[wasm_bindgen_test(unsupported = test)]
fn velocity_keeps_no_part_into_a_face_met_in_3d() {
    velocity_keeps_no_part_into_a_face_met::<Vec3>(0x5eed_0017, 200);
}

// ------------------------------------------------------ hashing and serde

/// Defect: a field of a state type left out of its canonical digest (or
/// two fields written so they can swap), so a client and a server whose
/// states differ in it compare equal digests and miss the desync; or one
/// left out of its postcard form, so it does not survive the wire
/// (physics.md 5). Oracle: the values themselves. Changing any one field
/// of a `MoverState`, `Floor`, `Contacts`, `Hit`, `Ledge`, `Collider` or
/// `Shape` changes its digest; every variant round-trips through postcard
/// to an equal value with the same digest.
#[wasm_bindgen_test(unsupported = test)]
fn every_field_reaches_the_digest_and_the_wire() {
    use sgl_core::digest_of;
    use sgl_physics::{Cut, Hit, Ledge};

    fn check<T>(base: &T, variants: &[T])
    where
        T: sgl_core::CanonicalWrite
            + serde::Serialize
            + serde::de::DeserializeOwned
            + PartialEq
            + std::fmt::Debug,
    {
        let mut digests = vec![digest_of(base)];
        for v in std::iter::once(base).chain(variants) {
            let back: T = postcard::from_bytes(&postcard::to_allocvec(v).unwrap()).unwrap();
            assert_eq!(&back, v);
            assert_eq!(digest_of(&back), digest_of(v));
        }
        for v in variants {
            let d = digest_of(v);
            assert!(!digests.contains(&d), "{v:?} hashed like an earlier value");
            digests.push(d);
        }
    }

    let v = Vec3::new(0.25, -0.5, 0.75);
    let w = Vec3::new(0.25, -0.5, 0.875);
    let floor = Floor { normal: v, id: 7 };
    check(
        &floor,
        &[Floor { normal: w, ..floor }, Floor { id: 8, ..floor }],
    );
    let state = MoverState {
        position: v,
        velocity: v,
        floor: Some(floor),
    };
    check(
        &state,
        &[
            MoverState {
                position: w,
                ..state
            },
            MoverState {
                velocity: w,
                ..state
            },
            MoverState {
                floor: None,
                ..state
            },
            MoverState {
                floor: Some(Floor { id: 0, ..floor }),
                ..state
            },
        ],
    );
    let contacts = Contacts {
        last_floor_normal: v,
        last_ceiling_normal: v,
        last_wall_normal: v,
        last_floor_id: Some(1),
        last_wall_id: Some(1),
        last_ceiling_id: Some(1),
        desired: v,
        effective: v,
        ..Contacts::default()
    };
    check(
        &contacts,
        &[
            Contacts {
                floor: true,
                ..contacts
            },
            Contacts {
                ceiling: true,
                ..contacts
            },
            Contacts {
                wall: true,
                ..contacts
            },
            Contacts {
                last_floor_normal: w,
                ..contacts
            },
            Contacts {
                last_ceiling_normal: w,
                ..contacts
            },
            Contacts {
                last_wall_normal: w,
                ..contacts
            },
            Contacts {
                last_floor_id: None,
                ..contacts
            },
            Contacts {
                last_wall_id: Some(0),
                ..contacts
            },
            Contacts {
                last_ceiling_id: Some(2),
                ..contacts
            },
            Contacts {
                desired: w,
                ..contacts
            },
            Contacts {
                effective: w,
                ..contacts
            },
            Contacts {
                down_blocked: true,
                ..contacts
            },
            Contacts {
                up_blocked: true,
                ..contacts
            },
            Contacts {
                stepped: true,
                ..contacts
            },
            Contacts {
                ledge_guarded: true,
                ..contacts
            },
            Contacts {
                pushed_out: true,
                ..contacts
            },
            Contacts {
                stuck: true,
                ..contacts
            },
        ],
    );
    let hit = Hit {
        t: 0.5,
        normal: v,
        id: 3,
    };
    check(
        &hit,
        &[
            Hit { t: 0.25, ..hit },
            Hit { normal: w, ..hit },
            Hit { id: 4, ..hit },
        ],
    );
    let ledge = Ledge {
        hang: v,
        stand: v,
        id: 3,
    };
    check(
        &ledge,
        &[
            Ledge { hang: w, ..ledge },
            Ledge { stand: w, ..ledge },
            Ledge { id: 4, ..ledge },
        ],
    );
    let collider = Collider::new(5, Shape::Box { half: v }, v).with_layers(6);
    let cut = Cut::new(Vec3::new(1.0, 1.0, 0.0), 0.25);
    check(
        &collider,
        &[
            Collider { id: 6, ..collider },
            Collider {
                shape: Shape::Box { half: w },
                ..collider
            },
            Collider {
                position: w,
                ..collider
            },
            collider.with_kind(ColliderKind::OneWay),
            collider.with_kind(ColliderKind::Sensor),
            collider.with_layers(7),
            collider.with_cut(cut),
            collider.with_cut(Cut::new(Vec3::new(1.0, 1.0, 0.0), 0.5)),
            collider.with_cut(Cut::new(Vec3::new(1.0, 0.0, 1.0), 0.25)),
        ],
    );
}
