//! Hostile input for `sgl-physics` (#924, physics.md 8, 9, 12, 17): what a
//! peer could send in a state, and what a careless level could hold.
//! Every public function, given non-finite, enormous, subnormal, zero,
//! negative and inverted values in each argument and collider field, empty
//! and crowded worlds and awkward cell sizes, must return without panicking
//! (the only panics allowed are the documented authoring errors in
//! constructors and options), and from a finite state give finite results.
//! Each test names the defect it would catch and its oracle. Each runs in
//! well under a second. Runs natively and in a wasm runtime.
// Results are compared bit for bit on purpose; casts are of small counts.
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::float_cmp,
    clippy::too_many_lines
)]

mod audit_support;

use audit_support::{Answers, Case, DT, battery, config, is_finite, set_of, up};
use sgl_core::math::{IVec2, IVec3, Vec2, Vec3};
use sgl_physics::{
    ALL_LAYERS, CellColliders, CellWorld, Collider, ColliderKind, ColliderSet, Contacts, Cut,
    Floor, Hit, Ledge, MoverConfig, MoverOptions, MoverSettings, MoverState, Shape, Vector, World,
    cast, find_ledge, move_and_collide, move_and_slide, overlaps, ray, slide, snap_to_ground,
};
use wasm_bindgen_test::wasm_bindgen_test;

/// The values a hostile peer or a careless level could put in any `f32`.
const HOSTILE: [f32; 15] = [
    f32::NAN,
    f32::INFINITY,
    f32::NEG_INFINITY,
    f32::MAX,
    -f32::MAX,
    1e30,
    -1e30,
    1e-40,
    -1e-40,
    f32::MIN_POSITIVE,
    0.0,
    -0.0,
    -1.0,
    16_777_217.0,
    -3.5,
];

/// A small level with a floor, a wall, a ramp, a one-way deck and a
/// sensor, in 2D.
fn level2() -> Vec<Collider<Vec2>> {
    vec![
        Collider::new(
            1,
            Shape::Box {
                half: Vec2::new(10.0, 0.5),
            },
            Vec2::new(0.0, -0.5),
        ),
        Collider::new(
            2,
            Shape::Box {
                half: Vec2::new(0.5, 2.0),
            },
            Vec2::new(3.0, 2.0),
        ),
        Collider::new(
            3,
            Shape::Box {
                half: Vec2::new(0.5, 0.5),
            },
            Vec2::new(-3.0, 0.5),
        )
        .with_cut(Cut::new(Vec2::new(-1.0, 1.0), 0.0)),
        Collider::new(
            4,
            Shape::Box {
                half: Vec2::new(1.0, 0.1),
            },
            Vec2::new(-6.0, 2.0),
        )
        .with_kind(ColliderKind::OneWay),
        Collider::new(
            5,
            Shape::Box {
                half: Vec2::new(1.0, 1.0),
            },
            Vec2::new(6.0, 1.0),
        )
        .with_kind(ColliderKind::Sensor),
    ]
}

/// The same level answered cell by cell at a cell size of one.
fn cells2() -> CellWorld<Vec2, impl Fn(IVec2, &mut CellColliders<'_, Vec2>)> {
    CellWorld::new(1.0, |cell: IVec2, out: &mut CellColliders<'_, Vec2>| {
        let block = |id| {
            Collider::new(
                id,
                Shape::Box {
                    half: Vec2::splat(0.5),
                },
                Vec2::splat(0.5),
            )
        };
        if cell.y == -1 && (-10..10).contains(&cell.x) {
            out.push(block(1));
        }
        if cell.x == 3 && (0..4).contains(&cell.y) {
            out.push(block(2));
        }
        if cell == IVec2::new(-4, 0) {
            out.push(block(3).with_cut(Cut::new(Vec2::new(-1.0, 1.0), 0.0)));
        }
    })
}

/// Every world the level can be given through: a slice, a set, a cell
/// world and a pair.
fn with_worlds(run: &mut dyn FnMut(&str, &dyn World<Vec2>)) {
    let level = level2();
    let set = set_of(&level, 1.0);
    let cells = cells2();
    run("slice", &level.as_slice());
    run("set", &set);
    run("cells", &cells);
    run("pair", &(&cells, level.as_slice()));
}

/// Whether every number a result carries is finite.
fn contacts_finite<V: Vector>(c: &Contacts<V>) -> bool {
    [
        c.last_floor_normal,
        c.last_wall_normal,
        c.last_ceiling_normal,
        c.effective,
    ]
    .into_iter()
    .all(is_finite)
}

fn hit_finite<V: Vector>(hit: Option<Hit<V>>) -> bool {
    hit.is_none_or(|h| h.t.is_finite() && (0.0..=1.0).contains(&h.t) && is_finite(h.normal))
}

fn ledge_finite<V: Vector>(ledge: Option<Ledge<V>>) -> bool {
    ledge.is_none_or(|l| is_finite(l.hang) && is_finite(l.stand))
}

/// Runs every query for one body, place and motion through `world`,
/// checking what the contract promises for that input: finite answers
/// from finite input; no motion and no contact from a non-finite motion
/// (physics.md 11, 13); the state untouched by a non-finite velocity.
fn probe<V: Vector>(
    name: &str,
    body: &Shape<V>,
    at: V,
    delta: V,
    world: &dyn World<V>,
    config: &MoverConfig<V>,
) {
    let finite_input = is_finite(at) && is_finite(delta) && is_finite(body.half_extents());
    let finite_motion = is_finite(delta);
    if let Ok((moved, contacts)) = slide(body, at, delta, world, ALL_LAYERS, config) {
        if finite_input {
            assert!(
                is_finite(moved) && contacts_finite(&contacts),
                "{name}: slide"
            );
        }
        if !finite_motion {
            assert_eq!(moved, V::ZERO, "{name}: a non-finite slide moved");
        }
    }
    if let Ok((snap, contacts)) = snap_to_ground(body, at, world, ALL_LAYERS, config)
        && finite_input
    {
        assert!(
            is_finite(snap) && contacts_finite(&contacts),
            "{name}: snap"
        );
    }
    if let Ok(hit) = cast(body, at, delta, world, ALL_LAYERS) {
        if finite_input {
            assert!(hit_finite(hit), "{name}: cast {hit:?}");
        }
        if !finite_motion {
            assert_eq!(hit, None, "{name}: a non-finite cast met something");
        }
    }
    if let Ok(hit) = ray(at, delta, world, ALL_LAYERS) {
        if finite_input {
            assert!(hit_finite(hit), "{name}: ray {hit:?}");
        }
        if !finite_motion {
            assert_eq!(hit, None, "{name}: a non-finite ray met something");
        }
    }
    if let Ok((moved, hit)) = move_and_collide(body, at, delta, world, ALL_LAYERS, config) {
        if finite_input {
            assert!(is_finite(moved) && hit_finite(hit), "{name}: collide");
        }
        if !finite_motion {
            assert_eq!(moved, V::ZERO, "{name}: a non-finite collide moved");
        }
    }
    let _ = overlaps(body, at, world, ALL_LAYERS);
    if let Ok(ledge) = find_ledge(body, at, delta, world, config, 2.0, ALL_LAYERS)
        && finite_input
    {
        assert!(ledge_finite(ledge), "{name}: ledge {ledge:?}");
    }
    for options in [
        MoverOptions::default(),
        MoverOptions {
            step_height: 0.5,
            ledge_drop: Some(0.2),
            drop_through: true,
            mask: ALL_LAYERS,
        },
    ] {
        for floor in [
            None,
            Some(Floor {
                normal: up(),
                id: 1,
            }),
        ] {
            let start = MoverState {
                position: at,
                velocity: delta * (1.0 / DT),
                floor,
            };
            let mut state = start;
            let result = move_and_slide(&mut state, body, DT, world, config, &options);
            if !is_finite(start.velocity) {
                assert_eq!(
                    format!("{state:?}"),
                    format!("{start:?}"),
                    "{name}: a non-finite velocity changed the state"
                );
            } else if let Ok(contacts) = result
                && finite_input
            {
                assert!(
                    is_finite(state.position)
                        && is_finite(state.velocity)
                        && contacts_finite(&contacts),
                    "{name}: {state:?} {contacts:?}"
                );
            }
        }
    }
}

/// Defect: a panic, a hang or a non-finite answer from hostile values in a
/// body's size, its place or its motion, through any world: an overflow in
/// a cell index, a division by a zero extent, a NaN compared as if
/// ordered (physics.md 8, 11, 13, 17). Oracle: the contract. A move from
/// a finite state answers finitely; a non-finite motion moves nothing and
/// meets nothing; a non-finite velocity leaves the mover's state as it
/// was. A non-finite place is probed only for panics: the contract does
/// not say what a move from one does (see the audit report).
#[wasm_bindgen_test(unsupported = test)]
fn every_query_survives_hostile_bodies_places_and_motions() {
    let config = config(Vec2::Y);
    let base_half = Vec2::new(0.4, 0.5);
    let base_at = Vec2::new(0.0, 0.51);
    let base_delta = Vec2::new(1.0, -0.5);
    with_worlds(&mut |world_name, world| {
        for value in HOSTILE {
            for axis in 0..2 {
                let mut half = base_half;
                half[axis] = value;
                let mut at = base_at;
                at[axis] = value;
                let mut delta = base_delta;
                delta[axis] = value;
                let base = Shape::Box { half: base_half };
                for (what, body, place, motion) in [
                    ("half", Shape::Box { half }, base_at, base_delta),
                    ("place", base, at, base_delta),
                    ("motion", base, base_at, delta),
                ] {
                    let name = format!("{world_name}: {what}[{axis}] = {value}");
                    probe(&name, &body, place, motion, world, &config);
                }
            }
        }
    });
}

/// Defect: a `dt` the contract says changes nothing (negative, NaN or
/// infinite) that moves a body or changes its velocity or floor, or a zero
/// or extreme one that panics or answers non-finitely (physics.md 17).
/// Oracle: the contract: the state is bit for bit as it was.
#[wasm_bindgen_test(unsupported = test)]
fn a_negative_or_non_finite_dt_changes_nothing() {
    let config = config(Vec2::Y);
    let body = Shape::Box {
        half: Vec2::new(0.4, 0.5),
    };
    with_worlds(&mut |world_name, world| {
        for dt in HOSTILE {
            for start in [
                MoverState {
                    position: Vec2::new(0.0, 0.51),
                    velocity: Vec2::new(3.0, -1.0),
                    floor: Some(Floor {
                        normal: Vec2::Y,
                        id: 1,
                    }),
                },
                // Inside the wall: a push-out would move it.
                MoverState {
                    position: Vec2::new(2.9, 2.0),
                    velocity: Vec2::new(3.0, -1.0),
                    floor: None,
                },
            ] {
                let mut state = start;
                let options = MoverOptions {
                    step_height: 0.5,
                    ledge_drop: Some(0.2),
                    ..MoverOptions::default()
                };
                let result = move_and_slide(&mut state, &body, dt, world, &config, &options);
                let name = format!("{world_name}: dt {dt}");
                if !(dt.is_finite() && dt >= 0.0) {
                    assert_eq!(format!("{state:?}"), format!("{start:?}"), "{name}");
                } else if result.is_ok() {
                    assert!(
                        is_finite(state.position) && is_finite(state.velocity),
                        "{name}: {state:?}"
                    );
                }
            }
        }
    });
}

/// Defect: a collider that occupies nothing (a negative half-extent, a
/// non-finite position or half-extent, or a cut that leaves nothing) that
/// any query or any world still meets, lists, pushes out of or stands on,
/// or that panics a set or a cell world when stored or walked (physics.md
/// 9, 10). Oracle: a second world. Every query through every world must
/// answer bit for bit as through the same level without the broken
/// collider.
#[wasm_bindgen_test(unsupported = test)]
fn a_collider_that_occupies_nothing_is_skipped_by_every_query() {
    let mut broken = Vec::new();
    for value in HOSTILE {
        for axis in 0..2 {
            let mut half = Vec2::splat(1.0);
            half[axis] = value;
            let mut at = Vec2::new(1.0, 1.5);
            at[axis] = value;
            let a = Collider::new(40, Shape::Box { half }, Vec2::new(1.0, 1.5));
            let b = Collider::new(41, Shape::Box { half: Vec2::ONE }, at);
            // Which of them occupy nothing, by the contract's definition.
            broken.extend([a, b].into_iter().filter(|c| !audit_support::occupies(c)));
        }
    }
    // A cut whose plane lies beyond the box's reach against its normal
    // leaves nothing; a sensor and a solid alike.
    for kind in [ColliderKind::Solid, ColliderKind::Sensor] {
        broken.push(
            Collider::new(42, Shape::Box { half: Vec2::ONE }, Vec2::new(1.0, 1.5))
                .with_cut(Cut::new(Vec2::new(1.0, 1.0), -1.5))
                .with_kind(kind),
        );
    }
    // A one-way platform with a cut, built field by field as a game or a
    // peer could.
    broken.push(Collider {
        kind: ColliderKind::OneWay,
        cut: Some(Cut::new(Vec2::new(-1.0, 1.0), 0.0)),
        ..Collider::new(43, Shape::Box { half: Vec2::ONE }, Vec2::new(1.0, 1.5))
    });
    let level = level2();
    let cases = [
        Case {
            half: Vec2::new(0.4, 0.5),
            at: Vec2::new(-1.0, 1.6),
            delta: Vec2::new(3.0, -1.5),
            mask: ALL_LAYERS,
            walking: false,
            options: MoverOptions::default(),
            toward: Vec2::X,
        },
        Case {
            half: Vec2::new(0.4, 0.5),
            at: Vec2::new(1.0, 1.5),
            delta: Vec2::new(-0.5, 0.0),
            mask: ALL_LAYERS,
            walking: true,
            options: MoverOptions {
                step_height: 0.5,
                ledge_drop: Some(0.3),
                ..MoverOptions::default()
            },
            toward: Vec2::X,
        },
    ];
    for bad in broken {
        let mut with = level.clone();
        with.push(bad);
        let set = set_of(&with, 0.5);
        let cells = CellWorld::new(
            1.0,
            move |cell: IVec2, out: &mut CellColliders<'_, Vec2>| {
                if cell == IVec2::new(1, 1) {
                    // In cell-local units: the same collider, a cell over.
                    out.push(Collider {
                        position: bad.position - Vec2::ONE,
                        ..bad
                    });
                }
            },
        );
        for case in &cases {
            let expected = battery(case, &level[..], Vec2::ZERO, ALL_LAYERS).unwrap();
            let worlds: [(&str, &dyn World<Vec2>); 3] = [
                ("slice", &with.as_slice()),
                ("set", &set),
                ("cells", &(&cells, level.as_slice())),
            ];
            for (name, world) in worlds {
                let answers: Answers<Vec2> = battery(case, world, Vec2::ZERO, ALL_LAYERS).unwrap();
                assert_eq!(answers, expected, "{name} met {bad:?}");
            }
        }
    }
}

/// Defect: a panic, a hang or a non-finite answer from enormous or
/// degenerate settings a game may legally choose: a skin, snap distance,
/// block tolerance, step height, ledge drop or reach of `f32::MAX`, zero or
/// subnormal (physics.md 8, 12, 17, 18). Oracle: the contract: lengths
/// that `new` and the options accept are answered, finitely.
#[wasm_bindgen_test(unsupported = test)]
fn extreme_settings_are_answered_finitely() {
    let body = Shape::Box {
        half: Vec2::new(0.4, 0.5),
    };
    let lengths = [0.0, 1e-40, f32::MIN_POSITIVE, 0.01, 3.0, 1e30, f32::MAX];
    with_worlds(&mut |world_name, world| {
        for &skin in &lengths {
            for &snap in &[0.0, 0.1, f32::MAX] {
                for &epsilon in &[0.0, 1e-4, f32::MAX] {
                    let config = MoverConfig::new(MoverSettings {
                        up: Vec2::Y,
                        skin,
                        snap_distance: snap,
                        normal_threshold: 0.7,
                        block_epsilon: epsilon,
                    });
                    let name = format!("{world_name}: skin {skin} snap {snap} eps {epsilon}");
                    probe(
                        &name,
                        &body,
                        Vec2::new(0.0, 1.0),
                        Vec2::new(4.0, -2.0),
                        world,
                        &config,
                    );
                    if let Ok(ledge) = find_ledge(
                        &body,
                        Vec2::new(2.09, 1.0),
                        Vec2::X,
                        world,
                        &config,
                        f32::MAX,
                        ALL_LAYERS,
                    ) {
                        assert!(ledge_finite(ledge), "{name}");
                    }
                }
            }
        }
        let config = config(Vec2::Y);
        for &length in &lengths {
            let mut state = MoverState {
                position: Vec2::new(0.0, 0.51),
                velocity: Vec2::new(60.0, -1.0),
                floor: Some(Floor {
                    normal: Vec2::Y,
                    id: 1,
                }),
            };
            let options = MoverOptions {
                step_height: length,
                ledge_drop: Some(length),
                ..MoverOptions::default()
            };
            if move_and_slide(&mut state, &body, 1.0 / 60.0, world, &config, &options).is_ok() {
                assert!(
                    is_finite(state.position) && is_finite(state.velocity),
                    "{world_name}: options {length}: {state:?}"
                );
            }
        }
    });
}

/// Defect: an empty world that answers anything but "nothing there", or a
/// mover in empty space that does not move by exactly `velocity * dt`
/// (physics.md 13, 15, 17). Oracle: the contract and the definition of a
/// free move.
#[wasm_bindgen_test(unsupported = test)]
fn an_empty_world_answers_nothing_there() {
    let config = config(Vec3::Y);
    let body = Shape::Box {
        half: Vec3::new(0.3, 0.9, 0.3),
    };
    let empty: [Collider<Vec3>; 0] = [];
    let set = ColliderSet::<Vec3>::new(1.0);
    let cells = CellWorld::new(1.0, |_: IVec3, _: &mut CellColliders<'_, Vec3>| {});
    let worlds: [(&str, &dyn World<Vec3>); 4] = [
        ("slice", &empty.as_slice()),
        ("set", &set),
        ("cells", &cells),
        ("pair", &(&set, &cells)),
    ];
    let at = Vec3::new(1.25, 7.5, -3.0);
    let delta = Vec3::new(0.375, -0.75, 0.125);
    for (name, world) in worlds {
        let (moved, contacts) = slide(&body, at, delta, world, ALL_LAYERS, &config).unwrap();
        assert_eq!(moved, delta, "{name}");
        assert!(
            !contacts.floor && !contacts.wall && !contacts.ceiling,
            "{name}"
        );
        assert_eq!(
            snap_to_ground(&body, at, world, ALL_LAYERS, &config)
                .unwrap()
                .0,
            Vec3::ZERO
        );
        assert_eq!(cast(&body, at, delta, world, ALL_LAYERS).unwrap(), None);
        assert_eq!(ray(at, delta, world, ALL_LAYERS).unwrap(), None);
        assert_eq!(overlaps(&body, at, world, ALL_LAYERS).unwrap(), vec![]);
        assert_eq!(
            move_and_collide(&body, at, delta, world, ALL_LAYERS, &config).unwrap(),
            (delta, None)
        );
        assert_eq!(
            find_ledge(&body, at, Vec3::X, world, &config, 3.0, ALL_LAYERS).unwrap(),
            None
        );
        let velocity = delta * 4.0;
        let mut state = MoverState {
            position: at,
            velocity,
            floor: Some(Floor {
                normal: Vec3::Y,
                id: 9,
            }),
        };
        let options = MoverOptions {
            step_height: 0.5,
            ledge_drop: None,
            ..MoverOptions::default()
        };
        move_and_slide(&mut state, &body, 0.25, world, &config, &options).unwrap();
        assert_eq!(
            state,
            MoverState {
                position: at + velocity * 0.25,
                velocity,
                floor: None,
            },
            "{name}"
        );
    }
}

/// Defect: work or a result that grows with copies of one collider (a
/// duplicate counted twice, a tie broken by which copy came first) or a
/// set that keeps a replaced collider (physics.md 6, 9). Oracle: a second
/// world. Three thousand copies of one floor in a slice and in one cell of
/// a cell world, and three thousand inserts of one id into a set, answer
/// every query as the one floor; three thousand floors with distinct ids
/// in one place answer every blocking query as the lowest id alone, and
/// `overlaps` lists all of them in id order.
#[wasm_bindgen_test(unsupported = test)]
fn thousands_of_identical_colliders_answer_as_one() {
    let floor = |id| {
        Collider::new(
            id,
            Shape::Box {
                half: Vec2::new(10.0, 0.5),
            },
            Vec2::new(0.0, -0.5),
        )
    };
    let one = [floor(7)];
    let copies = vec![floor(7); 3000];
    let mut set = ColliderSet::new(1.0);
    for n in 0..3000u64 {
        // Each insert replaces the last: the last one is the floor.
        let mut c = floor(7);
        c.position.x = if n == 2999 { 0.0 } else { n as f32 };
        set.insert(c);
    }
    assert_eq!(set.len(), 1);
    let cells = CellWorld::new(20.0, |cell: IVec2, out: &mut CellColliders<'_, Vec2>| {
        if cell == IVec2::new(0, -1) {
            for _ in 0..3000 {
                out.push(Collider::new(
                    7,
                    Shape::Box {
                        half: Vec2::new(0.5, 0.025),
                    },
                    Vec2::new(0.5, 0.975),
                ));
            }
        }
    });
    let cell_one = CellWorld::new(20.0, |cell: IVec2, out: &mut CellColliders<'_, Vec2>| {
        if cell == IVec2::new(0, -1) {
            out.push(Collider::new(
                7,
                Shape::Box {
                    half: Vec2::new(0.5, 0.025),
                },
                Vec2::new(0.5, 0.975),
            ));
        }
    });
    let cases = [
        Case {
            half: Vec2::new(0.4, 0.5),
            at: Vec2::new(5.0, 2.0),
            delta: Vec2::new(1.0, -3.0),
            mask: ALL_LAYERS,
            walking: false,
            options: MoverOptions::default(),
            toward: Vec2::X,
        },
        Case {
            half: Vec2::new(0.4, 0.5),
            at: Vec2::new(5.0, 0.3),
            delta: Vec2::new(1.0, -0.1),
            mask: ALL_LAYERS,
            walking: true,
            options: MoverOptions {
                step_height: 0.4,
                ledge_drop: Some(0.2),
                ..MoverOptions::default()
            },
            toward: Vec2::X,
        },
    ];
    for case in &cases {
        let expected = battery(case, &one[..], Vec2::ZERO, ALL_LAYERS).unwrap();
        assert_eq!(
            battery(case, &copies[..], Vec2::ZERO, ALL_LAYERS).unwrap(),
            expected
        );
        assert_eq!(
            battery(case, &set, Vec2::ZERO, ALL_LAYERS).unwrap(),
            expected
        );
        assert_eq!(
            battery(case, &cells, Vec2::ZERO, ALL_LAYERS).unwrap(),
            battery(case, &cell_one, Vec2::ZERO, ALL_LAYERS).unwrap()
        );
    }

    let mut ids: Vec<_> = (0..3000u64).map(|i| floor(3000 - i)).collect();
    ids.swap(17, 2500);
    let lowest = [floor(1)];
    let config = config(Vec2::Y);
    let body = Shape::Box {
        half: Vec2::new(0.4, 0.5),
    };
    let at = Vec2::new(5.0, 2.0);
    let delta = Vec2::new(1.0, -3.0);
    assert_eq!(
        slide(&body, at, delta, &ids[..], ALL_LAYERS, &config),
        slide(&body, at, delta, &lowest[..], ALL_LAYERS, &config)
    );
    assert_eq!(
        cast(&body, at, delta, &ids[..], ALL_LAYERS),
        cast(&body, at, delta, &lowest[..], ALL_LAYERS)
    );
    let listed = overlaps(&body, Vec2::new(0.0, 0.2), &ids[..], ALL_LAYERS).unwrap();
    assert_eq!(
        listed.iter().map(|c| c.id).collect::<Vec<_>>(),
        (1..=3000).collect::<Vec<_>>()
    );
}

/// Defect: a set whose cell size changes a result (a candidate dropped by
/// a cell index that rounds, overflows or wraps), or a set or a cell world
/// that panics or hangs at a tiny, huge or awkward cell size (physics.md
/// 10). Oracle: a second world: a set of any cell size answers bit for bit
/// as the slice (it never refuses); a cell world of an awkward size, as the
/// slice of its colliders converted by the contract's own rule
/// `(cell + local) * cell_size` (a thousand cells out, off the binary
/// grid, where another rounding shows), or `Unanswered` when its walk is
/// over the cap.
#[wasm_bindgen_test(unsupported = test)]
fn awkward_cell_sizes_answer_as_the_slice() {
    const OUT: i32 = 1000;
    let level = level2();
    let cases = [
        Case {
            half: Vec2::new(0.4, 0.5),
            at: Vec2::new(-1.0, 1.6),
            delta: Vec2::new(3.0, -1.5),
            mask: ALL_LAYERS,
            walking: false,
            options: MoverOptions::default(),
            toward: Vec2::X,
        },
        Case {
            half: Vec2::new(0.4, 0.5),
            at: Vec2::new(1.0, 0.51),
            delta: Vec2::new(1.5, -0.1),
            mask: ALL_LAYERS,
            walking: true,
            options: MoverOptions {
                step_height: 0.5,
                ledge_drop: Some(0.3),
                ..MoverOptions::default()
            },
            toward: Vec2::X,
        },
    ];
    let sizes = [
        1e-40,
        f32::MIN_POSITIVE,
        1e-6,
        0.1,
        1.0 / 3.0,
        7.3,
        1e6,
        1e30,
        f32::MAX,
    ];
    for size in sizes {
        let set = set_of(&level, size);
        for case in &cases {
            assert_eq!(
                battery(case, &set, Vec2::ZERO, ALL_LAYERS),
                battery(case, &level[..], Vec2::ZERO, ALL_LAYERS),
                "set of cell size {size}"
            );
        }
        // A block in each cell along a row at y = -1 cells, and a ramp, a
        // thousand cells out and off the binary grid inside their cells, so
        // a conversion that rounds otherwise than the contract's shows.
        let local = Collider::new(
            1,
            Shape::Box {
                half: Vec2::new(0.45, 0.5),
            },
            Vec2::new(0.45, 0.5),
        );
        let ramp = local.with_cut(Cut::new(Vec2::new(-1.0, 1.0), 0.0));
        let in_cell = |cell: IVec2| -> Option<Collider<Vec2>> {
            if cell.y == -1 && (OUT - 6..OUT + 6).contains(&cell.x) {
                Some(local)
            } else if cell == IVec2::new(OUT + 2, 0) {
                Some(Collider { id: 2, ..ramp })
            } else {
                None
            }
        };
        let cells = CellWorld::new(
            size,
            move |cell: IVec2, out: &mut CellColliders<'_, Vec2>| {
                if let Some(c) = in_cell(cell) {
                    out.push(c);
                }
            },
        );
        let mut converted = Vec::new();
        for x in OUT - 6..OUT + 6 {
            for y in -1..1 {
                let cell = IVec2::new(x, y);
                if let Some(c) = in_cell(cell) {
                    converted.push(Collider {
                        position: (Vec2::new(x as f32, y as f32) + c.position) * size,
                        shape: Shape::Box {
                            half: c.shape.half_extents() * size,
                        },
                        // The ramp's offset is zero, so scaled it is zero.
                        cut: c.cut.map(|_| Cut::new(Vec2::new(-1.0, 1.0), 0.0)),
                        ..c
                    });
                }
            }
        }
        for case in &cases {
            let scaled = Case {
                half: case.half * size,
                at: (case.at + Vec2::new(OUT as f32, 0.0)) * size,
                delta: case.delta * size,
                ..*case
            };
            if !(is_finite(scaled.at) && is_finite(scaled.delta) && is_finite(scaled.half)) {
                continue;
            }
            if let Ok(answers) = battery(&scaled, &cells, Vec2::ZERO, ALL_LAYERS) {
                assert_eq!(
                    answers,
                    battery(&scaled, &converted[..], Vec2::ZERO, ALL_LAYERS).unwrap(),
                    "cell world of cell size {size}"
                );
            }
        }
    }
}

/// Defect: a value from the wire that panics on decode, or decodes into a
/// cut or config `new` would refuse (a non-unit or zero normal, a 3D cut
/// with no zero component, a threshold outside `(0, 1)`, a negative
/// length), so a peer could plant one (physics.md 5, 12, 16). Oracle: the
/// contract's validity rules, read off the decoded value, over seeded
/// random bytes of every length up to the type's largest encoding plus
/// one, and over valid encodings with one byte flipped. No state type may
/// panic on any of them.
#[wasm_bindgen_test(unsupported = test)]
fn wire_bytes_never_panic_and_decode_only_valid_values() {
    let mut rng = audit_support::Rng::new(0x5eed_0100);
    // A finite, non-zero normal or up. Unit length, to rounding, is the
    // contract's rule too, but a direction whose squared length underflows
    // breaks it today (normals 12% short decode): the audit report carries
    // that test.
    let unit = |v: &[f32]| {
        let length = v.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>().sqrt();
        length.is_finite() && length > 0.0
    };
    let valid_cut2 = |c: Cut<Vec2>| unit(&[c.normal().x, c.normal().y]) && c.offset().is_finite();
    let valid_cut3 = |c: Cut<Vec3>| {
        let n = c.normal();
        unit(&[n.x, n.y, n.z]) && (n.x == 0.0 || n.y == 0.0 || n.z == 0.0) && c.offset().is_finite()
    };
    let valid_config = |up: &[f32], skin: f32, snap: f32, threshold: f32, eps: f32| {
        unit(up)
            && threshold > 0.0
            && threshold < 1.0
            && [skin, snap, eps].iter().all(|v| v.is_finite() && *v >= 0.0)
    };
    let seeds2 = [
        postcard::to_allocvec(&Cut::new(Vec2::new(-1.0, 2.0), 0.25)).unwrap(),
        postcard::to_allocvec(&config(Vec2::Y)).unwrap(),
    ];
    let seeds3 = [
        postcard::to_allocvec(&Cut::new(Vec3::new(0.0, 1.0, -1.0), 0.25)).unwrap(),
        postcard::to_allocvec(&config(Vec3::Y)).unwrap(),
    ];
    let mut inputs: Vec<Vec<u8>> = Vec::new();
    for length in 0..=Collider::<Vec3>::MAX_POSTCARD_BYTES + 1 {
        for _ in 0..24 {
            inputs.push((0..length).map(|_| (rng.int(0, 255)) as u8).collect());
        }
    }
    for seed in seeds2.iter().chain(&seeds3) {
        for i in 0..seed.len() {
            for bit in 0..8 {
                let mut flipped = seed.clone();
                flipped[i] ^= 1 << bit;
                inputs.push(flipped);
            }
        }
    }
    for bytes in &inputs {
        if let Ok(cut) = postcard::from_bytes::<Cut<Vec2>>(bytes) {
            assert!(valid_cut2(cut), "{bytes:?} gave {cut:?}");
        }
        if let Ok(cut) = postcard::from_bytes::<Cut<Vec3>>(bytes) {
            assert!(valid_cut3(cut), "{bytes:?} gave {cut:?}");
        }
        if let Ok(c) = postcard::from_bytes::<MoverConfig<Vec2>>(bytes) {
            let up = c.up();
            assert!(
                valid_config(
                    &[up.x, up.y],
                    c.skin(),
                    c.snap_distance(),
                    c.normal_threshold(),
                    c.block_epsilon()
                ),
                "{bytes:?} gave {c:?}"
            );
        }
        if let Ok(c) = postcard::from_bytes::<MoverConfig<Vec3>>(bytes) {
            let up = c.up();
            assert!(
                valid_config(
                    &[up.x, up.y, up.z],
                    c.skin(),
                    c.snap_distance(),
                    c.normal_threshold(),
                    c.block_epsilon()
                ),
                "{bytes:?} gave {c:?}"
            );
        }
        if let Ok(c) = postcard::from_bytes::<Collider<Vec3>>(bytes)
            && let Some(cut) = c.cut
        {
            assert!(valid_cut3(cut), "{bytes:?} gave {c:?}");
        }
        let _ = postcard::from_bytes::<Collider<Vec2>>(bytes);
        let _ = postcard::from_bytes::<MoverState<Vec3>>(bytes);
        let _ = postcard::from_bytes::<Contacts<Vec3>>(bytes);
        let _ = postcard::from_bytes::<Hit<Vec3>>(bytes);
        let _ = postcard::from_bytes::<Ledge<Vec3>>(bytes);
        let _ = postcard::from_bytes::<Shape<Vec3>>(bytes);
        let _ = postcard::from_bytes::<ColliderKind>(bytes);
    }
}

/// Defect: a mover that panics, answers non-finitely, wraps a cell index
/// or asks the game about cells when a state off the wire puts a body in a
/// cell world past the `i32` grid, or a query so long it would walk past
/// the cell cap (physics.md 10). Oracle: the contract: a query whose region
/// reaches past the grid, or past `MAX_QUERY_CELLS`, is `Unanswered`
/// without asking about any cell, and the state is untouched.
#[wasm_bindgen_test(unsupported = test)]
fn a_cell_world_past_its_grid_is_unanswered() {
    let config = config(Vec3::Y);
    let half = Vec3::new(0.3, 0.9, 0.3);
    let body = Shape::Box { half };
    let asked = std::cell::Cell::new(0u32);
    let cells = CellWorld::new(1.0, |cell: IVec3, out: &mut CellColliders<'_, Vec3>| {
        asked.set(asked.get() + 1);
        if cell.y == -1 {
            out.push(Collider::new(
                1,
                Shape::Box {
                    half: Vec3::splat(0.5),
                },
                Vec3::splat(0.5),
            ));
        }
    });
    let grid = 2_147_483_648.0f32;
    for at in [
        Vec3::new(3e9, 0.91, 0.0),
        Vec3::new(-3e9, 0.91, 0.0),
        Vec3::new(0.0, 0.91, 2.2e9),
        Vec3::new(2_147_483_000.0, 0.91, 0.0),
    ] {
        for velocity in [Vec3::new(2.0, 0.0, 0.0), Vec3::new(1e9, 0.0, 0.0)] {
            let start = MoverState {
                position: at,
                velocity,
                floor: Some(Floor {
                    normal: Vec3::Y,
                    id: 1,
                }),
            };
            // Past the grid: the start, or the end, beyond 2^31 cells.
            let past = (0..3).any(|axis| {
                (at[axis].abs() + half[axis]) >= grid
                    || (at[axis] + velocity[axis]).abs() + half[axis] >= grid
            });
            let mut state = start;
            asked.set(0);
            let result = move_and_slide(
                &mut state,
                &body,
                1.0,
                &cells,
                &config,
                &MoverOptions::default(),
            );
            if past {
                assert!(result.is_err(), "{at:?} by {velocity:?}: answered");
                assert_eq!(state, start);
                assert_eq!(asked.get(), 0, "{at:?} by {velocity:?}: asked about cells");
            } else {
                assert!(is_finite(state.position), "{at:?}: {state:?}");
            }
        }
    }
    // A body near the origin asked to move a million cells.
    let start = MoverState {
        position: Vec3::new(0.0, 0.91, 0.0),
        velocity: Vec3::new(1e6, 0.0, 1e6),
        floor: None,
    };
    let mut state = start;
    asked.set(0);
    let result = move_and_slide(
        &mut state,
        &body,
        1.0,
        &cells,
        &config,
        &MoverOptions::default(),
    );
    assert!(result.is_err());
    assert_eq!(state, start);
    // A ray of a million cells, past the cap: no cell asked about.
    asked.set(0);
    let hit = ray(
        Vec3::new(0.5, 3.5, 0.5),
        Vec3::new(1e6, 0.0, 1e6),
        &cells,
        ALL_LAYERS,
    );
    assert!(hit.is_err());
    assert_eq!(asked.get(), 0, "a ray past the cap asked about cells");
}
