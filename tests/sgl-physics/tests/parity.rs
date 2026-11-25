//! Frozen fixtures executed natively and in a wasm runtime: the values a
//! native server and a browser client must agree on (physics.md, "the same
//! on every target"). Floats are compared by bit pattern.
#![allow(clippy::cast_precision_loss)]

use sgl_core::math::{IVec3, Vec2, Vec3};
use sgl_core::{CanonicalWrite, SplitMix64, StateHasher};
use sgl_physics::{
    ALL_LAYERS, CellColliders, CellWorld, Collider, ColliderKind, ColliderSet, Cut, MoverConfig,
    MoverSettings, Shape, Vector, World, cast, overlaps, ray, slide, snap_to_ground,
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
/// (a cell world paired with a set of crates). Frozen on first run (#914);
/// refrozen when `ColliderKind` replaced the three flag bools in the
/// canonical encoding of the colliders `overlaps` lists, and again when
/// both levels gained ramps, `Collider`'s encoding gained its cut, and a
/// move's floor contacts began keeping their run (#915). Each walk must
/// stand on its ramps for the digest to cover them.
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
    // Steps the walks stood on a ramp: 124 in 2D, 61 in 3D.
    assert!(
        on_ramps_2d > 50 && on_ramps_3d > 30,
        "{on_ramps_2d}, {on_ramps_3d}"
    );

    assert_eq!(
        walked_2d,
        "4d09066832d46becb611d2fdb35c0df3f952fd237020e33f0d98f3588a521410"
    );
    assert_eq!(
        walked_3d,
        "a3e3d0b82b2cff810fed18c8724f190a45436ffd2336555f51a0cff2f3110eb0"
    );
}
