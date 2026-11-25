//! Frozen fixtures executed natively and in a wasm runtime: the values a
//! native server and a browser client must agree on (physics.md, "the same
//! on every target"). Floats are compared by bit pattern.
#![allow(clippy::cast_precision_loss)]

use sgl_core::math::{IVec3, Quat, Vec2, Vec3};
use sgl_core::{CanonicalWrite, SplitMix64, StateHasher};
use sgl_physics::{
    ALL_LAYERS, CellColliders, CellWorld, Collider, ColliderKind, ColliderSet, Convex, Cut,
    HeightField, Hit, MoverConfig, MoverOptions, MoverSettings, MoverState, Push, Shape,
    TriangleSet, Vector, World, cast, ground_under, move_and_slide, overlaps, planar, ray,
    sensor_events, sensors_at, slide, snap_to_ground,
};

/// Mover settings from positional values, in `MoverSettings`' order.
fn mover<V: Vector>(
    up: V,
    skin: f32,
    snap_distance: f32,
    normal_threshold: f32,
    block_epsilon: f32,
) -> MoverConfig<V> {
    MoverConfig::new(MoverSettings {
        up,
        skin,
        snap_distance,
        normal_threshold,
        block_epsilon,
    })
}
use wasm_bindgen_test::wasm_bindgen_test;

/// Moved with the box sweep from `sgl_core::collision`; the body-relative
/// arithmetic gives the same bits for it.
#[wasm_bindgen_test(unsupported = test)]
fn swept_box_hit_is_target_independent() {
    let body = Shape::Box {
        half: Vec2::new(0.5, 0.75),
    };
    let target = [Collider::new(
        1,
        Shape::Box {
            half: Vec2::new(1.0, 1.0),
        },
        Vec2::new(1.0, 0.0),
    )];
    let hit = cast(
        &body,
        Vec2::new(-3.25, 0.5),
        Vec2::new(7.3, 1.1),
        &target[..],
        ALL_LAYERS,
    )
    .unwrap()
    .expect("the sweep hits");
    assert_eq!(hit.t.to_bits(), 1_052_827_760);
    assert_eq!(hit.normal, Vec2::NEG_X);
}

/// A uniform draw in `[0, 1)` from 24 bits of the stream: exact in `f32`.
fn unit(rng: &mut SplitMix64) -> f32 {
    (rng.next_u64() >> 40) as f32 / 16_777_216.0
}

/// A seeded walk through `world`: a game's fixed step, run `steps` times,
/// with every result hashed. The velocity rules are the test's own: they
/// only have to drive the body into floors, walls, ceilings, one-way
/// platforms, sensors and ramps from one seed. Returns the digest and how
/// many steps the body stood on a ramp (a floor whose normal is not along
/// an axis).
fn walk<V: Vector, W: World<V> + ?Sized>(
    world: &W,
    body: &Shape<V>,
    start: V,
    config: &MoverConfig<V>,
    steps: u32,
) -> (String, u32) {
    let mut on_ramps = 0;
    let mut rng = SplitMix64::new(0x05ee_d914);
    let mut hasher = StateHasher::new();
    let (mut position, mut velocity, mut grounded) = (start, V::ZERO, false);
    let mut heading = V::ZERO;
    let up_axis = 1;
    for _ in 0..steps {
        // Keep a heading along each level axis, turning now and then, so the
        // body crosses the level and meets its walls.
        for axis in 0..V::AXES {
            if axis != up_axis {
                if heading[axis] == 0.0 || unit(&mut rng) < 0.05 {
                    heading[axis] = if unit(&mut rng) < 0.5 { -0.25 } else { 0.25 };
                }
                velocity[axis] = heading[axis] + unit(&mut rng) * 0.1 - 0.05;
            }
        }
        let jump = unit(&mut rng) < 0.15;
        velocity[up_axis] = if grounded && jump {
            0.45
        } else if velocity[up_axis] - 0.05 < -0.8 {
            -0.8
        } else {
            velocity[up_axis] - 0.05
        };
        let (moved, contacts) = slide(body, position, velocity, world, ALL_LAYERS, config).unwrap();
        position = position + moved;
        if (contacts.floor && contacts.down_blocked) || (contacts.ceiling && contacts.up_blocked) {
            velocity[up_axis] = 0.0;
        }
        if grounded && velocity[up_axis] <= 0.0 {
            let (snap, _) = snap_to_ground(body, position, world, ALL_LAYERS, config).unwrap();
            position = position + snap;
        }
        grounded = contacts.floor;
        let normal = contacts.last_floor_normal;
        let tilted = (0..V::AXES).filter(|&axis| normal[axis] != 0.0).count() > 1;
        on_ramps += u32::from(contacts.floor && tilted);
        for axis in 0..V::AXES {
            hasher.u32(position[axis].to_bits());
        }
        contacts.canonical_write(&mut hasher);

        let mut down = V::ZERO;
        down[up_axis] = -4.0;
        let below = ray(position, down, world, ALL_LAYERS).unwrap();
        hasher.bool(below.is_some());
        if let Some(hit) = below {
            hit.canonical_write(&mut hasher);
        }
        let inside = overlaps(body, position, world, ALL_LAYERS).unwrap();
        hasher.length(inside.len());
        for collider in &inside {
            collider.canonical_write(&mut hasher);
        }
    }
    (hasher.finish().to_hex(), on_ramps)
}

/// The mover walks' seed for the box bodies.
const BOX_SEED: u64 = 0x05ee_d916;

/// The mover walks' seeds for the round bodies, chosen so each walk climbs
/// a step, is stopped by its ledge guard and is pushed out.
const ROUND_SEED_2D: u64 = 14;
const ROUND_SEED_3D: u64 = 2;

/// What a mover walk met, so the digest is known to cover each rule.
#[derive(Default)]
struct Met {
    stepped: u32,
    ledge_guarded: u32,
    pushed_out: u32,
}

/// A seeded walk with [`move_and_slide`] through `world` and the colliders
/// `extra`, hashed every step: a step height throughout, the ledge guard
/// for a stretch in the middle, drop-through now and then, and a crate
/// dropped into the body at step `steps / 2`, which pushes it out.
fn walk_mover<V: Vector, W: World<V> + ?Sized>(
    world: &W,
    extra: &[Collider<V>],
    body: &Shape<V>,
    (start, seed): (V, u64),
    config: &MoverConfig<V>,
    steps: u32,
) -> (String, Met) {
    let mut rng = SplitMix64::new(seed);
    let mut hasher = StateHasher::new();
    let mut state = MoverState {
        position: start,
        ..MoverState::default()
    };
    let mut heading = V::ZERO;
    let mut dropped = extra.to_vec();
    let mut met = Met::default();
    for step in 0..steps {
        for axis in [0, 2].into_iter().take(V::AXES - 1) {
            if heading[axis] == 0.0 || unit(&mut rng) < 0.03 {
                heading[axis] = if unit(&mut rng) < 0.5 { -4.0 } else { 4.0 };
            }
            state.velocity[axis] = heading[axis] + unit(&mut rng) * 2.0 - 1.0;
        }
        let jump = unit(&mut rng) < 0.02;
        state.velocity[1] = if state.floor.is_some() && jump {
            9.0
        } else {
            state.velocity[1] - 20.0 / 60.0
        };
        if step == steps / 2 {
            // A crate a third of the body's size, in its upper half.
            let half = body.half_extents() * (1.0 / 3.0);
            let mut at = state.position;
            at[1] += body.half_extents()[1] * 0.5;
            dropped.push(Collider::new(9_999, Shape::Box { half }, at));
        }
        let options = MoverOptions {
            step_height: 0.3,
            ledge_drop: (steps / 4..steps * 3 / 4).contains(&step).then_some(0.2),
            drop_through: unit(&mut rng) < 0.1,
            mask: ALL_LAYERS,
        };
        let pair = (world, &dropped[..]);
        let contacts =
            move_and_slide(&mut state, body, 1.0 / 60.0, &pair, config, &options).unwrap();
        met.stepped += u32::from(contacts.stepped);
        met.ledge_guarded += u32::from(contacts.ledge_guarded);
        met.pushed_out += u32::from(contacts.pushed_out);
        state.canonical_write(&mut hasher);
        contacts.canonical_write(&mut hasher);
    }
    (hasher.finish().to_hex(), met)
}

/// A 2D side-on level of one-unit tiles: `#` solid, `=` a one-way platform
/// a quarter tile thick at the tile's top, `s` a sensor, `/` and `\\` ramps
/// rising to the right and to the left, `<` a half-height ramp rising to the
/// left. Row 0 is the top.
const LEVEL_2D: [&str; 8] = [
    "################",
    "#..............#",
    "#.....====.....#",
    "#..##......s...#",
    "#......../###..#",
    "#..====........#",
    "#<..../##\\..../#",
    "################",
];

fn level_2d() -> ColliderSet<Vec2> {
    let mut set = ColliderSet::new(2.0);
    for (row, line) in (0u64..).zip(LEVEL_2D) {
        for (column, tile) in (0u64..).zip(line.bytes()) {
            let (x, y) = (column as f32 + 0.5, 7.5 - row as f32);
            let id = row * 100 + column;
            let full = Shape::Box {
                half: Vec2::splat(0.5),
            };
            let ramp = |normal: Vec2, offset: f32| {
                Collider::new(id, full, Vec2::new(x, y)).with_cut(Cut::new(normal, offset))
            };
            let collider = match tile {
                b'#' => Collider::new(id, full, Vec2::new(x, y)),
                b'/' => ramp(Vec2::new(-1.0, 1.0), 0.0),
                b'\\' => ramp(Vec2::new(1.0, 1.0), 0.0),
                // Through the bottom-right corner, (0.5, -0.5) from the
                // centre: an offset of -0.5 / sqrt(5).
                b'<' => ramp(Vec2::new(1.0, 2.0), -0.223_606_8),
                b's' => Collider::new(id, full, Vec2::new(x, y)).with_kind(ColliderKind::Sensor),
                b'=' => Collider::new(
                    id,
                    Shape::Box {
                        half: Vec2::new(0.5, 0.125),
                    },
                    Vec2::new(x, y + 0.375),
                )
                .with_kind(ColliderKind::OneWay),
                _ => continue,
            };
            set.insert(collider);
        }
    }
    set
}

/// A 3D voxel level answered cell by cell: a floor, walls, pillars, a low
/// roof, a slab, a run of one-way platforms, a patch of sensors, slope
/// blocks of each kind (rows rising to the walls, inverted ones under the
/// roof's edge, and a wedge of two sideways ones),
/// and full blocks past the walls standing for unloaded cells.
fn voxel(cell: IVec3, out: &mut CellColliders<'_, Vec3>) {
    let id = u64::from((cell.x + 64).unsigned_abs())
        | u64::from((cell.y + 64).unsigned_abs()) << 8
        | u64::from((cell.z + 64).unsigned_abs()) << 16;
    let full = Collider::new(
        id,
        Shape::Box {
            half: Vec3::splat(0.5),
        },
        Vec3::splat(0.5),
    );
    let (x, y, z) = (cell.x, cell.y, cell.z);
    let outside = x.abs() > 7 || z.abs() > 7;
    let wall = (x.abs() == 7 || z.abs() == 7) && y < 3;
    let pillar = matches!((x, z), (2, 2) | (-3, 1) | (4, -4)) && (0..2).contains(&y);
    let roof = y == 2 && (1..5).contains(&x) && (-3..2).contains(&z);
    let slope = |normal: Vec3| full.with_cut(Cut::new(normal, 0.0));
    if outside || wall || y == -1 || pillar || roof {
        out.push(full);
    } else if y == 0 && x == 6 {
        out.push(slope(Vec3::new(-1.0, 1.0, 0.0)));
    } else if y == 0 && z == 6 {
        out.push(slope(Vec3::new(0.0, 1.0, -1.0)));
    } else if y == 0 && z == -6 && x >= 0 {
        out.push(slope(Vec3::new(0.0, 1.0, 1.0)));
    } else if y == 0 && x == -6 && z >= 0 {
        out.push(slope(Vec3::new(1.0, 1.0, 0.0)));
    } else if y == 1 && z == 1 && (1..5).contains(&x) {
        out.push(slope(Vec3::new(0.0, -1.0, 1.0)));
    } else if (0..2).contains(&y) && z == -4 && (0..2).contains(&x) {
        out.push(slope(Vec3::new(if x == 0 { -1.0 } else { 1.0 }, 0.0, -1.0)));
    } else if y == 0 && (-6..-1).contains(&x) && (-6..-1).contains(&z) {
        out.push(full.with_kind(ColliderKind::Sensor));
    } else if (x, y, z) == (0, 0, -3) {
        out.push(Collider {
            shape: Shape::Box {
                half: Vec3::new(0.5, 0.25, 0.5),
            },
            position: Vec3::new(0.5, 0.25, 0.5),
            ..full
        });
    } else if y == 0 && (-1..2).contains(&z) && (-5..0).contains(&x) {
        out.push(
            Collider {
                shape: Shape::Box {
                    half: Vec3::new(0.5, 0.125, 0.5),
                },
                position: Vec3::new(0.5, 0.875, 0.5),
                ..full
            }
            .with_kind(ColliderKind::OneWay),
        );
    }
}

/// Defect: any arithmetic that rounds differently in the browser (a fused
/// multiply-add, a platform maths function, an order that depends on a
/// hash map). Oracle: native and `wasm32` must produce this digest from the
/// same seeded walk through a fixed 2D level (a set) and a fixed 3D level
/// (a cell world paired with a set of crates), and from a seeded walk with
/// the mover (#916) through the same levels: steps, a guarded stretch,
/// drop-through and a push-out. Frozen on first run (#914); refrozen when
/// `ColliderKind` replaced the three flag bools in the canonical encoding
/// of the colliders `overlaps` lists; again when both levels gained ramps,
/// `Collider`'s encoding gained its cut, and a move's floor contacts began
/// keeping their run (#915); and again when #916 added `Contacts`'
/// `last_wall_normal` and its four mover flags to its canonical encoding
/// and the mover walks to the fixture, merged with #915: a wall or ceiling
/// contact began keeping the skin back-off's share of the motion along
/// the face, the mover's walk became one slide, and its ledge guard began
/// treating a drop within the snap distance as ground; and again when the
/// mover's review fixes (#916) clipped the velocity against every face a
/// step met, took a walking body's floor from the ground under where it
/// ends, measured a step-up's gain along the requested direction, and let
/// the push-out follow a ramp's normal; and again when the mover began
/// keeping a step's motion as a translation from where the body started
/// and writing back the start plus it (#924), which rounds the mover walks
/// differently; and again when #920 added a collider's velocity and push to
/// its encoding (the slide walks hash the colliders `overlaps` lists) and
/// `crushed`, `carried_by` and `pushed_by` to `Contacts`' (the mover
/// walks), every body moving bit for bit as before. Each walk must
/// stand on its ramps, and each mover walk climb a step, be stopped by its
/// guard and be pushed out, for the digest to cover them.
#[wasm_bindgen_test(unsupported = test)]
fn seeded_walks_through_fixed_levels_are_target_independent() {
    let level = level_2d();
    let config_2d = mover(Vec2::Y, 0.01, 0.1, 0.7, 1e-4);
    let body_2d = Shape::Box {
        half: Vec2::new(0.375, 0.5),
    };
    let (walked_2d, on_ramps_2d) = walk(&level, &body_2d, Vec2::new(12.5, 1.6), &config_2d, 600);

    let mut crates = ColliderSet::new(2.0);
    for (id, (x, z)) in (1u64..).zip([(1.5, 3.5), (-2.25, -2.5), (5.0, 1.0)]) {
        crates.insert(Collider::new(
            id,
            Shape::Box {
                half: Vec3::new(0.4, 0.4, 0.4),
            },
            Vec3::new(x, 0.4, z),
        ));
    }
    let cells = CellWorld::new(1.0, voxel);
    let config_3d = mover(Vec3::Y, 0.01, 0.1, 0.7, 1e-4);
    let body_3d = Shape::Box {
        half: Vec3::new(0.3, 0.9, 0.3),
    };
    let world = (&cells, &crates);
    let (walked_3d, on_ramps_3d) =
        walk(&world, &body_3d, Vec3::new(-2.5, 2.0, 0.5), &config_3d, 600);
    // Steps the walks stood on a ramp: 123 in 2D, 22 in 3D.
    assert!(
        on_ramps_2d > 50 && on_ramps_3d > 10,
        "{on_ramps_2d}, {on_ramps_3d}"
    );
    // A low platform, 0.3 high, on each level's floor: a step the mover
    // climbs and an edge its guard keeps it on.
    let low_2d = [Collider::new(
        5_000,
        Shape::Box {
            half: Vec2::new(1.5, 0.15),
        },
        Vec2::new(11.5, 1.15),
    )];
    let low_3d = [Collider::new(
        5_000,
        Shape::Box {
            half: Vec3::new(2.0, 0.15, 2.0),
        },
        Vec3::new(-3.0, 0.15, 0.0),
    )];
    let start_2d = Vec2::new(12.5, 1.6);
    let (mover_2d, met_2d) = walk_mover(
        &level,
        &low_2d,
        &body_2d,
        (start_2d, BOX_SEED),
        &config_2d,
        600,
    );
    let start_3d = Vec3::new(-2.5, 2.0, 0.5);
    let (mover_3d, met_3d) = walk_mover(
        &world,
        &low_3d,
        &body_3d,
        (start_3d, BOX_SEED),
        &config_3d,
        600,
    );
    for met in [&met_2d, &met_3d] {
        assert!(
            met.stepped > 0 && met.ledge_guarded > 0 && met.pushed_out > 0,
            "{} {} {}",
            met.stepped,
            met.ledge_guarded,
            met.pushed_out
        );
    }

    assert_eq!(
        walked_2d,
        "e44d9e00fcc4cf8c1053871ca7f5dd486e58b6c9f6b45a5d6453bc2c70583570"
    );
    assert_eq!(
        walked_3d,
        "8b10913b42e61973361019d473e79aa941dbbccef74623999d90372a218e69c6"
    );
    assert_eq!(
        mover_2d,
        "32c70934bc40d9f6a66291cc4a101531fa46944d96981b343c12b474820fe690"
    );
    assert_eq!(
        mover_3d,
        "ae636884e2c40d60c1b4277cf679602ca8ea9d177b5e4e487d46e3077cee6642"
    );
}

/// A ball, a capsule and a convex triangle in open tiles of the 2D level.
fn round_extras_2d() -> [Collider<Vec2>; 3] {
    let triangle = [
        Vec2::new(-0.4, -0.3),
        Vec2::new(0.4, -0.3),
        Vec2::new(0.0, 0.4),
    ];
    [
        Collider::new(6_001, Shape::Ball { radius: 0.3 }, Vec2::new(3.5, 1.3)),
        Collider::new(
            6_002,
            Shape::Capsule {
                half_axis: Vec2::new(0.0, 0.25),
                radius: 0.2,
            },
            Vec2::new(13.5, 1.45),
        ),
        Collider::new(
            6_003,
            Shape::Convex(Convex::polygon(&triangle, 0.8, 0.6)),
            Vec2::new(7.5, 4.5),
        ),
    ]
}

/// A ball, a lying capsule and a cube turned a quarter turn about y as a
/// hull, in open cells of the 3D level.
fn round_extras_3d() -> [Collider<Vec3>; 3] {
    let corners: Vec<Vec3> = (0u8..8)
        .map(|i| {
            let sign = |bit: u8| if i >> bit & 1 == 0 { -0.35 } else { 0.35 };
            Vec3::new(sign(0), sign(1), sign(2))
        })
        .collect();
    let turn = Quat::from_xyzw(0.0, 0.382_683_43, 0.0, 0.923_879_5);
    [
        Collider::new(
            6_001,
            Shape::Ball { radius: 0.4 },
            Vec3::new(-1.5, 0.4, 4.5),
        ),
        Collider::new(
            6_002,
            Shape::Capsule {
                half_axis: Vec3::new(0.6, 0.0, 0.0),
                radius: 0.25,
            },
            Vec3::new(3.5, 0.25, 4.5),
        ),
        Collider::new(
            6_003,
            Shape::Convex(Convex::hull(&corners, turn)),
            Vec3::new(5.5, 0.5, -1.5),
        ),
    ]
}

/// Defect: arithmetic in the convex kernel or its wiring that rounds
/// differently in the browser. Oracle: native and `wasm32` must produce
/// these digests from the walks above with round bodies (#919): a ball
/// through the 2D level and a capsule through the 3D level, each level
/// with a ball, a capsule and a convex collider added, with `slide` and
/// with the mover. The box walks above keep their digests: a box against a
/// box is the closed-form sweep, which #919 left as it was. Frozen on first
/// run (#919), and refrozen (#924) when the mover began keeping a step's
/// motion as a translation from its start (the mover walks) and rotations
/// began to be normalised by their largest component first (the 3D slide
/// walk, whose hull is turned by a quaternion); again when #920 grew
/// the encodings of `Collider` and `Contacts`, every body moving bit for
/// bit as before; and the 2D slide walk again
/// when rays became exact against balls, capsules and polygons (#923): its
/// ray down each step meets the added ball, capsule and polygon where they
/// are, not a slop short. The 3D slide walk keeps its digest: its rays
/// down met no ball or capsule, and its hull, of eight points, stays on
/// the kernel. The mover walks cast no rays. The 2D slide walk was
/// refrozen once more when #920 and #923 met, since it hashes both what
/// #920 encodes anew (the colliders it overlaps and its contacts) and the
/// exact rays of #923: with the exact rays turned off it gives #920's
/// digest, and with #920's new fields left out of the encodings #923's.
#[wasm_bindgen_test(unsupported = test)]
fn seeded_round_walks_through_fixed_levels_are_target_independent() {
    let level = level_2d();
    let extras_2d = round_extras_2d();
    let world_2d = (&level, &extras_2d[..]);
    let config_2d = mover(Vec2::Y, 0.01, 0.1, 0.7, 1e-4);
    let ball = Shape::Ball { radius: 0.4 };
    let start_2d = Vec2::new(12.5, 1.6);
    let (walked_2d, on_ramps_2d) = walk(&world_2d, &ball, start_2d, &config_2d, 600);
    let low_2d = [Collider::new(
        5_000,
        Shape::Box {
            half: Vec2::new(1.5, 0.15),
        },
        Vec2::new(11.5, 1.15),
    )];
    let (mover_2d, met_2d) = walk_mover(
        &world_2d,
        &low_2d,
        &ball,
        (start_2d, ROUND_SEED_2D),
        &config_2d,
        600,
    );

    let mut crates = ColliderSet::new(2.0);
    for (id, (x, z)) in (1u64..).zip([(1.5, 3.5), (-2.25, -2.5), (5.0, 1.0)]) {
        crates.insert(Collider::new(
            id,
            Shape::Box {
                half: Vec3::new(0.4, 0.4, 0.4),
            },
            Vec3::new(x, 0.4, z),
        ));
    }
    let cells = CellWorld::new(1.0, voxel);
    let extras_3d = round_extras_3d();
    let world_3d = ((&cells, &crates), &extras_3d[..]);
    let config_3d = mover(Vec3::Y, 0.01, 0.1, 0.7, 1e-4);
    let walker = Shape::Capsule {
        half_axis: Vec3::new(0.0, 0.6, 0.0),
        radius: 0.3,
    };
    let start_3d = Vec3::new(-2.5, 2.0, 0.5);
    let (walked_3d, on_ramps_3d) = walk(&world_3d, &walker, start_3d, &config_3d, 600);
    let low_3d = [Collider::new(
        5_000,
        Shape::Box {
            half: Vec3::new(2.0, 0.15, 2.0),
        },
        Vec3::new(-3.0, 0.15, 0.0),
    )];
    let (mover_3d, met_3d) = walk_mover(
        &world_3d,
        &low_3d,
        &walker,
        (start_3d, ROUND_SEED_3D),
        &config_3d,
        600,
    );

    // Steps the walks stood on a ramp: 87 in 2D, 27 in 3D.
    assert!(
        on_ramps_2d > 10 && on_ramps_3d > 10,
        "{on_ramps_2d}, {on_ramps_3d}"
    );
    for met in [&met_2d, &met_3d] {
        assert!(
            met.stepped > 0 && met.ledge_guarded > 0 && met.pushed_out > 0,
            "{} {} {}",
            met.stepped,
            met.ledge_guarded,
            met.pushed_out
        );
    }

    assert_eq!(
        walked_2d,
        "4dbf3d20d41db4e2032fbd90595d5b828f41cb5cb73606c98844314117914808"
    );
    assert_eq!(
        walked_3d,
        "18e803a4d94b71e72b7885089cb8c429aa214f81359182251f2008e54968d2ba"
    );
    assert_eq!(
        mover_2d,
        "e3161b8d89697b5c5382badddaee6e8d7122ffc9bf702bcb2084c5897bf2e537"
    );
    assert_eq!(
        mover_3d,
        "e87bb98fe9e4f0d83af1f4569a06a689fba3c0ff38be8ae173f125a7d535ac71"
    );
}

/// What a moving walk met, so the digest is known to cover each rule.
#[derive(Default)]
struct Moving {
    carried: u32,
    pushed: u32,
    nudged: u32,
    crushed: u32,
    events: u32,
}

/// A vector from its x, y and z; 2D drops z.
fn xyz<V: Vector>(x: f32, y: f32, z: f32) -> V {
    let mut out = V::ZERO;
    out[0] = x;
    out[1] = y;
    if V::AXES == 3 {
        out[2] = z;
    }
    out
}

/// The game moving its lift (up and down between tops at 0.5 and 3) and its
/// door (to and fro between -1 and 5.8), turning each at the ends, and
/// telling the set by id.
fn move_lift_and_door<V: Vector>(
    lift: &mut Collider<V>,
    door: &mut Collider<V>,
    set: &mut ColliderSet<V>,
) {
    let top = lift.position[1] + 0.25;
    if (lift.velocity[1] > 0.0 && top >= 3.0) || (lift.velocity[1] < 0.0 && top <= 0.5) {
        lift.velocity = -lift.velocity;
    }
    if (door.velocity[0] > 0.0 && door.position[0] >= 5.8)
        || (door.velocity[0] < 0.0 && door.position[0] <= -1.0)
    {
        door.velocity = -door.velocity;
    }
    for moving in [lift, door] {
        moving.position = moving.position + moving.velocity * (1.0 / 60.0);
        set.set_position(moving.id, moving.position);
        set.set_velocity(moving.id, moving.velocity);
    }
}

/// The game's input for body `index` at step `n`: the first keeps to the
/// lift, the second heads for the wall and the lift by turns, and the third
/// keeps to the middle, each wandering and now and then jumping.
fn steer<V: Vector>(index: usize, n: u32, state: &mut MoverState<V>, rng: &mut SplitMix64) {
    let heading = match index {
        0 => 0.0,
        1 if (n / 150).is_multiple_of(2) => 2.0,
        1 => -2.0,
        _ => -state.position[0],
    };
    state.velocity[0] = heading + unit(rng) * 2.0 - 1.0;
    if V::AXES == 3 {
        state.velocity[2] = unit(rng) * 2.0 - 1.0;
    }
    state.velocity[1] = if state.floor.is_some() && unit(rng) < 0.03 {
        7.0
    } else {
        state.velocity[1] - 20.0 / 60.0
    };
}

/// A seeded walk among moving colliders (#920), hashed every step: a lift
/// (10) rising and falling, a door (11) sliding to and fro against a wall
/// (2), and three bodies (20, 21 and 22) that are soft colliders to each
/// other, walking and jumping at random, stepped in ascending id against
/// the others as they stand, with a sensor (30) among them. The game moves
/// the lift and the door first, turning each at the ends of its travel,
/// then steps the bodies. Each step hashes every body's state, contacts and
/// sensor list.
fn walk_moving<V: Vector>(seed: u64, config: &MoverConfig<V>, steps: u32) -> (String, Moving) {
    let mut rng = SplitMix64::new(seed);
    let mut hasher = StateHasher::new();
    let mut met = Moving::default();
    let block = |id: u64, half: V, at: V| Collider::new(id, Shape::Box { half }, at);
    let body = Shape::Box {
        half: xyz(0.4, 0.5, 0.4),
    };
    let still = [
        block(1, xyz(8.0, 0.5, 8.0), xyz(0.0, -0.5, 0.0)),
        block(2, xyz(0.5, 2.0, 8.0), xyz(6.5, 2.0, 0.0)),
        block(30, xyz(0.5, 0.5, 0.5), xyz(2.0, 0.5, 0.0)).with_kind(ColliderKind::Sensor),
    ];
    let mut lift =
        block(10, xyz(1.0, 0.25, 1.0), xyz(-4.0, 0.25, 0.0)).with_velocity(xyz(0.0, 1.5, 0.0));
    let mut door =
        block(11, xyz(0.25, 1.0, 1.0), xyz(-1.0, 1.0, 0.0)).with_velocity(xyz(2.5, 0.0, 0.0));
    let mut bodies = [
        MoverState {
            position: xyz(-4.0, 1.0 + 0.005, 0.0),
            ..MoverState::default()
        },
        MoverState {
            position: xyz(1.0, 0.5 + 0.005, 0.0),
            ..MoverState::default()
        },
        MoverState {
            position: xyz(-1.0, 0.5 + 0.005, 0.0),
            ..MoverState::default()
        },
    ];
    let soft = |id: u64, state: &MoverState<V>| {
        Collider::new(id, body, state.position)
            .with_velocity(state.velocity)
            .with_push(Push::Soft(0.04))
    };
    let mut set = ColliderSet::new(2.0);
    set.insert(lift);
    set.insert(door);
    for (id, state) in (20u64..).zip(&bodies) {
        set.insert(soft(id, state));
    }
    let mut sensors: [Vec<u64>; 3] = Default::default();
    for n in 0..steps {
        move_lift_and_door(&mut lift, &mut door, &mut set);
        for (index, id) in [(0, 20u64), (1, 21), (2, 22)] {
            let state = &mut bodies[index];
            set.remove(id);
            steer(index, n, state, &mut rng);
            let world = (&still[..], &set);
            let contacts = move_and_slide(
                state,
                &body,
                1.0 / 60.0,
                &world,
                config,
                &MoverOptions::default(),
            )
            .unwrap();
            met.carried += u32::from(contacts.carried_by == Some(10));
            met.pushed += u32::from(contacts.pushed_by == Some(11));
            met.nudged += u32::from(matches!(contacts.pushed_by, Some(20..=22)));
            met.crushed += u32::from(contacts.crushed);
            let now = sensors_at(&body, state.position, &world, ALL_LAYERS).unwrap();
            let (entered, exited) = sensor_events(&sensors[index], &now);
            met.events += u32::try_from(entered.len() + exited.len()).unwrap();
            state.canonical_write(&mut hasher);
            contacts.canonical_write(&mut hasher);
            hasher.length(now.len());
            for &sensor in &now {
                hasher.u64(sensor);
            }
            sensors[index] = now;
            set.insert(soft(id, state));
        }
    }
    (hasher.finish().to_hex(), met)
}

/// Defect: arithmetic in the moving-collider rules (the carry, the press,
/// the plane solver of soft contact, the sensor lists) that rounds
/// differently in the browser. Oracle: native and `wasm32` must produce
/// these digests from the seeded walks above, in 2D and 3D. Frozen on first
/// run (#920). Each walk must ride the lift, be pushed by the door, be
/// nudged by another body, be crushed against the wall and enter or leave
/// the sensor for the digest to cover them (369, 375, 343, 22 and 18 times
/// in 2D; 414, 408, 99, 44 and 14 in 3D).
#[wasm_bindgen_test(unsupported = test)]
fn seeded_walks_among_moving_colliders_are_target_independent() {
    let (walked_2d, met_2d) = walk_moving(0x05ee_d920, &mover(Vec2::Y, 0.01, 0.1, 0.7, 1e-4), 900);
    let (walked_3d, met_3d) = walk_moving(0x05ee_d920, &mover(Vec3::Y, 0.01, 0.1, 0.7, 1e-4), 900);
    for met in [&met_2d, &met_3d] {
        assert!(
            met.carried > 0
                && met.pushed > 0
                && met.nudged > 0
                && met.crushed > 0
                && met.events > 0,
            "{} {} {} {} {}",
            met.carried,
            met.pushed,
            met.nudged,
            met.crushed,
            met.events
        );
    }
    assert_eq!(
        walked_2d,
        "5b2859d895d2fb06ce2a4e51753997be0282eeaf89abe96d417d31c713e34065"
    );
    assert_eq!(
        walked_3d,
        "0dbc7d645a6b0626827cf1155a20249b6676b4930e0fccebba9d8f32183ad589"
    );
}

/// A seeded pile-up of planar bodies (#921): cars, balls, a capsule and a
/// convex prop thrown together and spinning, beside an immovable wall,
/// with headings drawn as grid pairs the crate normalises, so no
/// platform maths function is used to build it.
fn planar_pile_up() -> Vec<planar::Body> {
    let mut rng = SplitMix64::new(0x0921);
    let prop = Convex::polygon(
        &[
            Vec2::new(-0.5, -0.4),
            Vec2::new(0.6, -0.3),
            Vec2::new(0.4, 0.5),
            Vec2::new(-0.3, 0.6),
        ],
        1.0,
        0.0,
    );
    let shapes = [
        Shape::Box {
            half: Vec2::new(1.0, 0.5),
        },
        Shape::Ball { radius: 0.45 },
        Shape::Box {
            half: Vec2::new(0.9, 0.45),
        },
        Shape::Capsule {
            half_axis: Vec2::new(0.5, 0.0),
            radius: 0.3,
        },
        Shape::Convex(prop),
        Shape::Box {
            half: Vec2::new(1.1, 0.55),
        },
        Shape::Ball { radius: 0.35 },
        Shape::Box {
            half: Vec2::new(0.8, 0.4),
        },
    ];
    let mut bodies: Vec<planar::Body> = (0u64..)
        .zip(shapes)
        .map(|(id, shape)| {
            let at = Vec2::new((id % 4) as f32 * 2.75 - 4.0, (id / 4) as f32 * 2.75 - 1.5);
            let half = shape.half_extents();
            planar::Body {
                id: 10 + 3 * id,
                shape,
                position: at,
                heading: Vec2::new(unit(&mut rng) * 2.0 - 1.0, unit(&mut rng) + 0.25),
                velocity: -at * (1.0 + 2.0 * unit(&mut rng))
                    + Vec2::new(unit(&mut rng) - 0.5, unit(&mut rng) - 0.5),
                spin: unit(&mut rng) * 8.0 - 4.0,
                inverse_mass: 1.0 / (0.5 + unit(&mut rng)),
                inverse_inertia: planar::box_inverse_inertia(1.0, half),
            }
        })
        .collect();
    bodies.push(planar::Body {
        id: 1,
        shape: Shape::Box {
            half: Vec2::new(0.25, 6.0),
        },
        position: Vec2::new(5.0, 0.0),
        heading: Vec2::new(1.0, 0.0),
        velocity: Vec2::ZERO,
        spin: 0.0,
        inverse_mass: 0.0,
        inverse_inertia: 0.0,
    });
    bodies
}

/// Defect: arithmetic in the planar bumps (#921) that rounds differently
/// in the browser: the kernel's time of impact with turning sweeps, the
/// manifold port, the bump and `advance`'s ordering. Oracle: native and
/// `wasm32` must produce this digest from the same seeded pile-up, every
/// step's impacts and bodies hashed over ninety steps. The pile-up must
/// bump for the digest to cover the bump. Frozen on first run (#921).
#[wasm_bindgen_test(unsupported = test)]
fn a_seeded_planar_pile_up_is_target_independent() {
    let mut bodies = planar_pile_up();
    let settings = planar::Settings::new(0.3, 0.5);
    let mut hasher = StateHasher::new();
    let mut impacts = 0;
    for _ in 0..90 {
        for impact in planar::advance(&mut bodies, 1.0 / 60.0, &settings, |_, _| true) {
            impacts += 1;
            hasher.u64(impact.a);
            hasher.u64(impact.b);
            for value in [
                impact.time,
                impact.point.x,
                impact.point.y,
                impact.normal.x,
                impact.normal.y,
                impact.impulse,
            ] {
                hasher.u32(value.to_bits());
            }
        }
        for body in &bodies {
            body.canonical_write(&mut hasher);
        }
    }
    // 80 impacts in all.
    assert!(impacts > 20, "{impacts}");
    assert_eq!(
        hasher.finish().to_hex(),
        "11b5935271aca8e2f6cbed22d4fc57372b39a1528fe8d9c8d2bbfb5307e16a7b"
    );
}

/// The surfaces the drive crosses: a ramp rising to a deck, the deck, a
/// ramp down and a wall behind, as triangles, and beside them a height
/// field of heights drawn from `rng`.
fn surface_scene(rng: &mut SplitMix64) -> (TriangleSet, HeightField) {
    let mut positions = Vec::new();
    let mut indices = Vec::new();
    let mut ids = Vec::new();
    let mut quad = |corners: [Vec3; 4], id: u64| {
        let base = u32::try_from(positions.len()).expect("small");
        positions.extend(corners);
        indices.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
        ids.extend([id, id]);
    };
    // A ramp rising to a deck, a deck, a ramp down, and a wall behind.
    quad(
        [
            Vec3::new(0.0, 0.0, -2.0),
            Vec3::new(4.0, 1.5, -2.0),
            Vec3::new(4.0, 1.5, 2.0),
            Vec3::new(0.0, 0.0, 2.0),
        ],
        1,
    );
    quad(
        [
            Vec3::new(4.0, 1.5, -2.0),
            Vec3::new(8.0, 1.5, -2.0),
            Vec3::new(8.0, 1.5, 2.0),
            Vec3::new(4.0, 1.5, 2.0),
        ],
        2,
    );
    quad(
        [
            Vec3::new(8.0, 1.5, -2.0),
            Vec3::new(11.0, 0.0, -2.0),
            Vec3::new(11.0, 0.0, 2.0),
            Vec3::new(8.0, 1.5, 2.0),
        ],
        3,
    );
    quad(
        [
            Vec3::new(-3.0, 0.0, -3.0),
            Vec3::new(-3.0, 6.0, -3.0),
            Vec3::new(-3.0, 6.0, 3.0),
            Vec3::new(-3.0, 0.0, 3.0),
        ],
        4,
    );
    let mesh = TriangleSet::new(&positions, &indices, &ids).expect("a sound mesh");
    let heights: Vec<f32> = (0..16 * 6)
        .map(|_| (rng.next_u64() % 5) as f32 * 0.125 - 0.25)
        .collect();
    let field = HeightField::new(&heights, 16, 6, 1.0, Vec3::new(-4.0, 0.0, 2.0), &[9])
        .expect("a sound grid");
    (mesh, field)
}

/// A drive across surfaces (#923): a kart crossing a ramp and a deck built
/// from triangles beside a height field of seeded quarter-unit heights,
/// each step asking `ground_under` for its four wheels, casting its chase
/// camera's ball back toward a wall of triangles and looking ahead along a
/// ray, then a seeded batch of rays and ball casts across the scene, every
/// result hashed. The kart's path and the queries are the test's own; they
/// only have to reach the triangles, their seams and the field from one
/// seed. Frozen on first run (#923), and refrozen when rays became exact
/// against triangles, which they had met a slop short through the kernel
/// (#923).
#[wasm_bindgen_test(unsupported = test)]
fn seeded_queries_against_surfaces_are_target_independent() {
    let mut rng = SplitMix64::new(0x0923_2000);
    let (mesh, field) = surface_scene(&mut rng);
    let world = (&mesh, &field);

    let mut hasher = StateHasher::new();
    let write = |hasher: &mut StateHasher, found: Option<Hit<Vec3>>| {
        hasher.bool(found.is_some());
        if let Some(hit) = found {
            hit.canonical_write(hasher);
        }
    };
    let mut stood_on = [0u32; 10];
    let mut camera_stopped = 0;
    let camera = Shape::Ball { radius: 0.3 };
    for step in 0..240 {
        let x = -1.0 + step as f32 * 0.05;
        let z = 0.75 * unit(&mut rng) - 0.375 + if step >= 120 { 2.5 } else { 0.0 };
        let kart = Vec3::new(x, 2.5, z);
        for (dx, dz) in [(-0.6, -0.4), (-0.6, 0.4), (0.6, -0.4), (0.6, 0.4)] {
            let wheel = kart + Vec3::new(dx, 0.0, dz);
            let ground = ground_under(wheel, 0.5, 4.0, &world, ALL_LAYERS).expect("answered");
            hasher.bool(ground.is_some());
            if let Some(ground) = ground {
                stood_on[usize::try_from(ground.id).expect("small")] += 1;
                ground.canonical_write(&mut hasher);
            }
        }
        // Toward a point beyond the wall at x = -3.
        let behind = Vec3::new(-(x + 3.5) - unit(&mut rng), 1.0 + unit(&mut rng), 0.0);
        let stopped = cast(&camera, kart, behind, &world, ALL_LAYERS).expect("answered");
        camera_stopped += u32::from(stopped.is_some());
        write(&mut hasher, stopped);
        let ahead = Vec3::new(6.0, -2.0 * unit(&mut rng), 0.5 - unit(&mut rng));
        write(
            &mut hasher,
            ray(kart, ahead, &world, ALL_LAYERS).expect("answered"),
        );
    }
    for _ in 0..120 {
        let from = Vec3::new(
            16.0 * unit(&mut rng) - 4.0,
            2.0 + 3.0 * unit(&mut rng),
            10.0 * unit(&mut rng) - 3.0,
        );
        let motion = Vec3::new(
            8.0 * unit(&mut rng) - 4.0,
            -6.0 * unit(&mut rng),
            8.0 * unit(&mut rng) - 4.0,
        );
        write(
            &mut hasher,
            ray(from, motion, &world, ALL_LAYERS).expect("answered"),
        );
        let ball = Shape::Ball {
            radius: 0.1 + 0.4 * unit(&mut rng),
        };
        if overlaps(&ball, from, &world, ALL_LAYERS)
            .expect("answered")
            .is_empty()
        {
            write(
                &mut hasher,
                cast(&ball, from, motion, &world, ALL_LAYERS).expect("answered"),
            );
        }
    }
    // Wheels stood on both ramps, the deck and the field, and the camera
    // was stopped by the wall or the deck.
    assert!(
        [1, 2, 3, 9].iter().all(|&id| stood_on[id] > 20) && camera_stopped > 20,
        "{stood_on:?} {camera_stopped}"
    );
    assert_eq!(
        hasher.finish().to_hex(),
        "1f4cb91d83a9547ac354371ecd4010741fcba0b80360e156eefd06a784f961e7"
    );
}
