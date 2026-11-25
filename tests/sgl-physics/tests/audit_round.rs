//! Audit of round bodies and convex colliders (#924, physics.md 19),
//! written from the contract: every public move of balls, capsules and
//! convex bodies through every world, checked against the shapes' own
//! definitions in `f64`; the resting gap; one-way platforms; order and
//! place; hostile shapes. Never an expectation read from the crate. Runs
//! natively and in a wasm runtime.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::float_cmp,
    clippy::many_single_char_names,
    clippy::similar_names,
    clippy::too_many_lines,
    clippy::type_complexity
)]

mod audit_support;

use audit_support::flat::{self, separation};
use audit_support::solid::{self, Q};
use audit_support::{Rng, SKIN, config};
use sgl_core::StateHasher;
use sgl_core::math::{IVec2, IVec3, Quat, Vec2, Vec3};
use sgl_physics::{
    ALL_LAYERS, CellColliders, CellWorld, Collider, ColliderKind, ColliderSet, Convex, Cut, Floor,
    MoverOptions, MoverState, Shape, World, cast, distance, move_and_collide, move_and_slide,
    overlaps, ray, slide, snap_to_ground,
};
use wasm_bindgen_test::wasm_bindgen_test;

fn v2(x: f32, y: f32) -> Vec2 {
    Vec2::new(x, y)
}

fn v3(x: f32, y: f32, z: f32) -> Vec3 {
    Vec3::new(x, y, z)
}

/// A convex polygon of three to six vertices about its centre, turned at
/// random, reaching at most `reach`.
fn polygon(rng: &mut Rng, reach: f32) -> Convex<Vec2> {
    let count = rng.int(3, 6) as usize;
    let step = core::f32::consts::TAU / count as f32;
    let points: Vec<Vec2> = (0..count)
        .map(|i| {
            let angle = step * (i as f32 + rng.range(0.0, 0.4));
            v2(angle.cos(), angle.sin()) * (reach * rng.range(0.6, 1.0))
        })
        .collect();
    let turn = rng.range(0.0, 6.2);
    Convex::polygon(&points, turn.cos(), turn.sin())
}

/// A 2D body: a ball, a capsule turned any way (one in five with no
/// axis: a ball as a capsule), or a convex polygon.
fn body2(rng: &mut Rng) -> Shape<Vec2> {
    match rng.below(3) {
        0 => Shape::Ball {
            radius: rng.range(0.15, 0.45),
        },
        1 => Shape::Capsule {
            half_axis: if rng.chance(20) {
                Vec2::ZERO
            } else {
                rng.vector(-0.3, 0.3)
            },
            radius: rng.range(0.1, 0.3),
        },
        _ => Shape::Convex(polygon(rng, 0.4)),
    }
}

/// What one cell of a 2D level holds, in cell-local units: a block, a
/// slab, a ramp, a ball, a capsule or a convex polygon, each inside its
/// cell; one in ten a one-way platform, one in ten a sensor.
fn cell2(rng: &mut Rng, id: u64) -> Collider<Vec2> {
    let centre = v2(0.5, 0.5);
    let collider = match rng.below(6) {
        0 => Collider::new(id, Shape::Box { half: centre }, centre),
        1 => Collider::new(id, Shape::Box { half: v2(0.5, 0.2) }, v2(0.5, 0.2)),
        2 => {
            let side = if rng.chance(50) { -1.0 } else { 1.0 };
            Collider::new(id, Shape::Box { half: centre }, centre)
                .with_cut(Cut::new(v2(side, rng.range(0.5, 2.0)), 0.0))
        }
        3 => Collider::new(
            id,
            Shape::Ball {
                radius: rng.range(0.2, 0.5),
            },
            centre,
        ),
        4 => Collider::new(
            id,
            Shape::Capsule {
                half_axis: rng.vector(-0.25, 0.25),
                radius: 0.15,
            },
            centre,
        ),
        _ => Collider::new(id, Shape::Convex(polygon(rng, 0.5)), centre),
    };
    let whole_box = matches!(collider.shape, Shape::Box { .. }) && collider.cut.is_none();
    match rng.below(10) {
        0 if whole_box => collider.with_kind(ColliderKind::OneWay),
        1 => collider.with_kind(ColliderKind::Sensor),
        _ => collider,
    }
}

/// A random 2D level of 10 by 8 cells, a third of them occupied, as cells
/// and in world units.
fn level2(rng: &mut Rng) -> (Vec<(IVec2, Collider<Vec2>)>, Vec<Collider<Vec2>>) {
    let mut cells = Vec::new();
    for y in 0..8 {
        for x in 0..10 {
            if rng.chance(33) {
                let id = (y * 10 + x) as u64;
                cells.push((IVec2::new(x, y), cell2(rng, id)));
            }
        }
    }
    let slice = cells
        .iter()
        .map(|(cell, local)| Collider {
            position: cell.as_vec2() + local.position,
            ..*local
        })
        .collect();
    (cells, slice)
}

/// How far the body at `at` reaches into the solid colliders on `mask`,
/// by the shapes' definitions: the most any overlaps (negative when clear
/// of them all).
fn into2(body: &Shape<Vec2>, at: Vec2, colliders: &[Collider<Vec2>], mask: u32) -> f64 {
    let placed = flat::place(body, flat::p(at), (1.0, 0.0));
    colliders
        .iter()
        .filter(|c| c.kind == ColliderKind::Solid && c.layers & mask != 0)
        .map(|c| -separation(&placed, &flat::collider(c)))
        .fold(f64::NEG_INFINITY, f64::max)
}

/// What a case's moves leave, hashed, for comparing worlds bit for bit.
fn digest(parts: &[String]) -> String {
    let mut hasher = StateHasher::new();
    for part in parts {
        hasher.bytes(part.as_bytes());
    }
    hasher.finish().to_hex()
}

/// Defect: a move of a round or convex body that leaves it inside a solid
/// (a kernel cast past a contact, a skin backed off the wrong way, a
/// push-out, step-up or ledge guard that lands inside), or a result that
/// depends on the world. Oracle: the shapes' own definitions in `f64`
/// (physics.md 19: a body that starts clear of every collider never ends
/// inside one by more than rounding), and the same level as a slice, a
/// set and a cell world, which must answer alike (10). Balls, capsules
/// (some with no axis) and convex polygons, clear of a random level of
/// blocks, slabs, ramps, balls, capsules and polygons, one-way platforms
/// and sensors among them, are moved by `slide`, `move_and_collide`,
/// `snap_to_ground` and three `move_and_slide` steps under gravity with a
/// step height and, on some, a ledge guard.
#[wasm_bindgen_test(unsupported = test)]
fn a_round_or_convex_body_never_ends_inside_a_solid_through_any_move_in_2d() {
    let mut rng = Rng::new(0x0924_1901);
    let config = config(Vec2::Y);
    let (mut ran, mut touched) = (0, 0);
    for _ in 0..5 {
        let (cells, slice) = level2(&mut rng);
        let mut set = ColliderSet::new(2.0);
        for c in &slice {
            set.insert(*c);
        }
        let cell_world = CellWorld::new(1.0, |cell: IVec2, out: &mut CellColliders<'_, Vec2>| {
            for (at, local) in &cells {
                if *at == cell {
                    out.push(*local);
                }
            }
        });
        for _ in 0..30 {
            let body = body2(&mut rng);
            let start = v2(rng.range(0.0, 10.0), rng.range(0.0, 8.0));
            let delta = rng.vector(-3.0, 3.0);
            let mask = rng.pick(&[ALL_LAYERS, ALL_LAYERS, 1]);
            if into2(&body, start, &slice, mask) > -f64::from(SKIN) {
                continue;
            }
            ran += 1;
            let options = MoverOptions {
                step_height: 0.3,
                ledge_drop: rng.chance(50).then_some(0.4),
                mask,
                ..MoverOptions::default()
            };
            let worlds: [&dyn World<Vec2>; 3] = [&&slice[..], &set, &cell_world];
            let digests = worlds.map(|world| {
                let mut ends = Vec::new();
                let mut log = Vec::new();
                let (moved, contacts) = slide(&body, start, delta, world, mask, &config).unwrap();
                touched += usize::from(contacts.wall || contacts.floor || contacts.ceiling);
                ends.push(("slide", start + moved));
                log.push(format!("{moved:?} {contacts:?}"));
                let (moved, hit) =
                    move_and_collide(&body, start, delta, world, mask, &config).unwrap();
                ends.push(("move_and_collide", start + moved));
                log.push(format!("{moved:?} {hit:?}"));
                let (moved, ground) = snap_to_ground(&body, start, world, mask, &config).unwrap();
                ends.push(("snap_to_ground", start + moved));
                log.push(format!("{moved:?} {ground:?}"));
                let mut state = MoverState {
                    position: start,
                    velocity: delta * 2.0,
                    floor: None,
                };
                for _ in 0..3 {
                    state.velocity.y -= 5.0;
                    let contacts =
                        move_and_slide(&mut state, &body, 0.25, world, &config, &options).unwrap();
                    ends.push(("move_and_slide", state.position));
                    log.push(format!("{state:?} {contacts:?}"));
                }
                for (name, end) in ends {
                    let deepest = into2(&body, end, &slice, mask);
                    let rounding = 64.0 * f64::from(f32::EPSILON) * 12.0;
                    assert!(
                        deepest <= rounding,
                        "{name}: {body:?} from {start} by {delta}, mask {mask}: {deepest} inside at {end}"
                    );
                }
                digest(&log)
            });
            assert!(
                digests.iter().all(|d| *d == digests[0]),
                "{body:?} from {start} by {delta}: the worlds differ"
            );
        }
    }
    assert!(ran >= 90 && touched >= 60, "{ran} cases, {touched} touches");
}

/// A cube of half-extents `half` turned by `turn`, as a hull of its
/// corners in the order of their sign bits.
fn turned_box(half: Vec3, turn: Quat) -> Convex<Vec3> {
    let corners: Vec<Vec3> = (0u8..8)
        .map(|i| {
            let sign = |bit: u8, h: f32| if i >> bit & 1 == 0 { -h } else { h };
            v3(sign(0, half.x), sign(1, half.y), sign(2, half.z))
        })
        .collect();
    Convex::hull(&corners, turn)
}

/// `v` turned back by the unit quaternion `turn`, in `f64`.
fn unturn(v: Q, turn: Quat) -> Q {
    let w = [
        f64::from(turn.x),
        f64::from(turn.y),
        f64::from(turn.z),
        f64::from(turn.w),
    ];
    let l = (w[0] * w[0] + w[1] * w[1] + w[2] * w[2] + w[3] * w[3]).sqrt();
    // The conjugate turns back.
    let axis = [-w[0] / l, -w[1] / l, -w[2] / l];
    let s = w[3] / l;
    let t = solid::scale(solid::cross(axis, v), 2.0);
    solid::add(solid::add(v, solid::scale(t, s)), solid::cross(axis, t))
}

/// A 3D collider and, for a turned box, its half-extents and turn, so the
/// oracle measures it in its own frame.
#[derive(Clone, Copy)]
struct Solid3 {
    collider: Collider<Vec3>,
    turned: Option<(Vec3, Quat)>,
}

impl Solid3 {
    fn to(&self, p: Q) -> f64 {
        match self.turned {
            Some((half, turn)) => solid::to_box(
                unturn(solid::sub(p, solid::q(self.collider.position)), turn),
                [0.0; 3],
                solid::q(half),
            ),
            None => solid::to_collider(p, &self.collider),
        }
    }
}

fn cell3(rng: &mut Rng, id: u64) -> Solid3 {
    let centre = Vec3::splat(0.5);
    let plain = |collider| Solid3 {
        collider,
        turned: None,
    };
    let solid = match rng.below(6) {
        0 => plain(Collider::new(id, Shape::Box { half: centre }, centre)),
        1 => plain(Collider::new(
            id,
            Shape::Box {
                half: v3(0.5, 0.2, 0.5),
            },
            v3(0.5, 0.2, 0.5),
        )),
        2 => {
            // One component zero (physics.md 16), the others at least a
            // fifth, so the plane cuts the box.
            let mut normal = Vec3::ZERO;
            let flat = rng.below(3);
            for axis in 0..3 {
                if axis != flat {
                    let size = rng.range(0.2, 1.0);
                    normal[axis] = if rng.chance(50) { size } else { -size };
                }
            }
            plain(
                Collider::new(id, Shape::Box { half: centre }, centre)
                    .with_cut(Cut::new(normal, 0.0)),
            )
        }
        3 => plain(Collider::new(
            id,
            Shape::Ball {
                radius: rng.range(0.2, 0.5),
            },
            centre,
        )),
        4 => plain(Collider::new(
            id,
            Shape::Capsule {
                half_axis: rng.vector(-0.2, 0.2),
                radius: 0.15,
            },
            centre,
        )),
        _ => {
            let half = rng.vector(0.1, 0.28);
            let axis: Vec3 = rng.vector(-1.0, 1.0);
            let turn = Quat::from_axis_angle(axis.normalize_or(Vec3::Y), rng.range(0.0, 3.0));
            Solid3 {
                collider: Collider::new(id, Shape::Convex(turned_box(half, turn)), centre),
                turned: Some((half, turn)),
            }
        }
    };
    if rng.chance(10)
        && solid.turned.is_none()
        && matches!(solid.collider.shape, Shape::Box { .. })
        && solid.collider.cut.is_none()
    {
        Solid3 {
            collider: solid.collider.with_kind(ColliderKind::OneWay),
            ..solid
        }
    } else {
        solid
    }
}

fn body3(rng: &mut Rng) -> Shape<Vec3> {
    if rng.chance(50) {
        Shape::Ball {
            radius: rng.range(0.15, 0.4),
        }
    } else {
        Shape::Capsule {
            half_axis: if rng.chance(20) {
                Vec3::ZERO
            } else {
                rng.vector(-0.3, 0.3)
            },
            radius: rng.range(0.1, 0.3),
        }
    }
}

/// How far the round body at `at` reaches into the solids near it, by
/// their definitions (negative when clear). Colliders whose boxes lie
/// beyond the body's reach and a margin cannot change the answer's sign
/// against a skin, and are skipped.
fn into3(body: &Shape<Vec3>, at: Vec3, solids: &[Solid3]) -> f64 {
    let reach = body.half_extents();
    solids
        .iter()
        .filter(|s| s.collider.kind == ColliderKind::Solid)
        .filter(|s| {
            let half = s.collider.shape.half_extents();
            (0..3).all(|axis| {
                (at[axis] - s.collider.position[axis]).abs() < reach[axis] + half[axis] + 0.1
            })
        })
        .map(|s| solid::depth(body, at, |p| s.to(p)))
        .fold(-0.1, f64::max)
}

/// The same in 3D: balls and capsules (some with no axis) through levels
/// of blocks, slabs, ramps of every orientation, balls, capsules and
/// turned boxes, by every public move.
#[wasm_bindgen_test(unsupported = test)]
fn a_round_body_never_ends_inside_a_solid_through_any_move_in_3d() {
    let mut rng = Rng::new(0x0924_1902);
    let config = config(Vec3::Y);
    let (mut ran, mut touched) = (0, 0);
    for _ in 0..4 {
        let mut cells = Vec::new();
        for y in 0..4 {
            for z in 0..5 {
                for x in 0..5 {
                    if rng.chance(30) {
                        let id = ((y * 5 + z) * 5 + x) as u64;
                        cells.push((IVec3::new(x, y, z), cell3(&mut rng, id)));
                    }
                }
            }
        }
        let solids: Vec<Solid3> = cells
            .iter()
            .map(|(cell, local)| Solid3 {
                collider: Collider {
                    position: cell.as_vec3() + local.collider.position,
                    ..local.collider
                },
                ..*local
            })
            .collect();
        let slice: Vec<Collider<Vec3>> = solids.iter().map(|s| s.collider).collect();
        let mut set = ColliderSet::new(2.0);
        for c in &slice {
            set.insert(*c);
        }
        let cell_world = CellWorld::new(1.0, |cell: IVec3, out: &mut CellColliders<'_, Vec3>| {
            for (at, local) in &cells {
                if *at == cell {
                    out.push(local.collider);
                }
            }
        });
        for _ in 0..25 {
            let body = body3(&mut rng);
            let start = v3(
                rng.range(0.0, 5.0),
                rng.range(0.0, 4.0),
                rng.range(0.0, 5.0),
            );
            let delta = rng.vector(-2.0, 2.0);
            if into3(&body, start, &solids) > -f64::from(SKIN) {
                continue;
            }
            ran += 1;
            let options = MoverOptions {
                step_height: 0.3,
                ledge_drop: rng.chance(50).then_some(0.4),
                ..MoverOptions::default()
            };
            let worlds: [&dyn World<Vec3>; 3] = [&&slice[..], &set, &cell_world];
            let digests = worlds.map(|world| {
                let mut ends = Vec::new();
                let mut log = Vec::new();
                let (moved, contacts) =
                    slide(&body, start, delta, world, ALL_LAYERS, &config).unwrap();
                touched += usize::from(contacts.wall || contacts.floor || contacts.ceiling);
                ends.push(("slide", start + moved));
                log.push(format!("{moved:?} {contacts:?}"));
                let (moved, hit) =
                    move_and_collide(&body, start, delta, world, ALL_LAYERS, &config).unwrap();
                ends.push(("move_and_collide", start + moved));
                log.push(format!("{moved:?} {hit:?}"));
                let (moved, ground) =
                    snap_to_ground(&body, start, world, ALL_LAYERS, &config).unwrap();
                ends.push(("snap_to_ground", start + moved));
                log.push(format!("{moved:?} {ground:?}"));
                let mut state = MoverState {
                    position: start,
                    velocity: delta * 2.0,
                    floor: None,
                };
                for _ in 0..3 {
                    state.velocity.y -= 5.0;
                    let contacts =
                        move_and_slide(&mut state, &body, 0.25, world, &config, &options).unwrap();
                    ends.push(("move_and_slide", state.position));
                    log.push(format!("{state:?} {contacts:?}"));
                }
                for (name, end) in ends {
                    let deepest = into3(&body, end, &solids);
                    assert!(
                        deepest <= 2e-5,
                        "{name}: {body:?} from {start} by {delta}: {deepest} inside at {end}"
                    );
                }
                digest(&log)
            });
            assert!(
                digests.iter().all(|d| *d == digests[0]),
                "{body:?} from {start} by {delta}: the worlds differ"
            );
        }
    }
    assert!(ran >= 50 && touched >= 30, "{ran} cases, {touched} touches");
}

/// Defect: a round or convex body that rests further from a face than the
/// contract's gap (a kernel contact reported where the slop leaves it,
/// not where the body would touch), or nearer than the skin (a back-off
/// too short, or none at an edge). Oracle: physics.md 19, The resting gap:
/// after a kernel contact the body rests between `skin` and
/// `skin * 65 / 64` from a flat face, measured to the face's plane (beside
/// the face's edge, where the back-off can carry it, its nearest point on
/// the collider is the edge, at least as far), and at least `skin` from an
/// edge or a corner; measured by the shapes' definitions in `f64`. Balls, capsules
/// and convex polygons moved by `move_and_collide` at a box from every
/// direction; a contact whose normal is a face's (axis-aligned) is a flat
/// face.
#[wasm_bindgen_test(unsupported = test)]
fn a_round_or_convex_body_rests_a_skin_from_what_it_meets() {
    let mut rng = Rng::new(0x0924_1903);
    let config = config(Vec2::Y);
    let target = Collider::new(4, Shape::Box { half: v2(1.0, 0.6) }, v2(3.0, -2.0));
    let placed_target = flat::collider(&target);
    let skin = f64::from(SKIN);
    let tol = 2e-6;
    let (mut faces, mut edges) = (0, 0);
    for _ in 0..300 {
        let body = body2(&mut rng);
        let angle = rng.range(0.0, core::f32::consts::TAU);
        let away = v2(angle.cos(), angle.sin()) * rng.range(1.8, 3.0);
        let start = target.position + away;
        let aim = target.position + rng.vector::<Vec2>(-0.9, 0.9);
        let delta = (aim - start) * 1.5;
        let placed = |at: Vec2| flat::place(&body, flat::p(at), (1.0, 0.0));
        if separation(&placed(start), &placed_target) < 0.1 {
            continue;
        }
        let (moved, hit) =
            move_and_collide(&body, start, delta, &[target][..], ALL_LAYERS, &config).unwrap();
        let Some(hit) = hit else {
            continue;
        };
        let gap = separation(&placed(start + moved), &placed_target);
        let why = format!("{body:?} from {start} by {delta}: {hit:?}, gap {gap}");
        assert!(gap >= skin - tol, "{why}");
        let n = hit.normal;
        if n.x.abs() == 1.0 || n.y.abs() == 1.0 {
            // From the face's plane: the skin back-off retraces the motion,
            // which can carry the body past the face's end, where its
            // nearest point on the box is the edge.
            faces += 1;
            let normal = flat::p(n);
            let face = flat::dot(normal, flat::p(target.position))
                + flat::dot((normal.0.abs(), normal.1.abs()), flat::p(v2(1.0, 0.6)));
            let body_at = placed(start + moved);
            let lowest = body_at
                .core
                .iter()
                .map(|&q| flat::dot(normal, q))
                .fold(f64::INFINITY, f64::min)
                - body_at.radius;
            let from_plane = lowest - face;
            assert!(
                from_plane >= skin - tol && from_plane <= skin * 65.0 / 64.0 + tol,
                "{why}: {from_plane} from the face's plane"
            );
        } else {
            edges += 1;
        }
    }
    assert!(
        faces >= 100 && edges >= 20,
        "{faces} face and {edges} edge contacts"
    );
}

/// Defect: a round or convex body that passes into a one-way platform
/// while falling onto it (the kernel's normal at an edge or a corner
/// leaning off the floor), or that a one-way platform stops while rising.
/// Oracle: physics.md 19, One-way platforms (a body meeting it while
/// falling lands on it, or, where the edge's normal leans past the floor
/// threshold, is turned off it as a solid box would turn it, and never
/// passes into it; rising through it passes) and the shapes' definitions.
/// Bodies of every round and convex kind start wholly above the platform's
/// top, along it and past its ends, and fall straight down in steps: each
/// step ends where it ends against the same platform made solid, bit for
/// bit, and never overlapping it. Thrown up from wholly beneath it, they
/// rise their whole way. (A body beside the platform, below its top, may
/// pass into its side: a side is no floor.)
#[wasm_bindgen_test(unsupported = test)]
fn a_round_or_convex_body_falls_onto_a_one_way_platform_as_onto_a_solid_and_rises_through_it() {
    let mut rng = Rng::new(0x0924_1904);
    let config = config(Vec2::Y);
    let deck = Collider::new(
        2,
        Shape::Box {
            half: v2(2.0, 0.25),
        },
        v2(0.0, 0.0),
    );
    let one_way = [deck.with_kind(ColliderKind::OneWay)];
    let solid = [deck];
    let placed_deck = flat::collider(&deck);
    let (mut landed, mut turned) = (0, 0);
    for _ in 0..150 {
        let body = body2(&mut rng);
        let reach = body.half_extents();
        let x = rng.range(-2.0 - reach.x, 2.0 + reach.x);
        let mut at = v2(x, 0.25 + reach.y + rng.range(0.02, 0.6));
        for _ in 0..4 {
            let step = v2(0.0, -0.3);
            let (moved, contacts) =
                slide(&body, at, step, &one_way[..], ALL_LAYERS, &config).unwrap();
            let (as_solid, _) = slide(&body, at, step, &solid[..], ALL_LAYERS, &config).unwrap();
            assert_eq!(moved, as_solid, "{body:?} at {at}: {contacts:?}");
            at += moved;
            landed += usize::from(contacts.floor);
            turned += usize::from(contacts.wall);
            let gap = separation(&flat::place(&body, flat::p(at), (1.0, 0.0)), &placed_deck);
            assert!(gap > -1e-5, "{body:?} fell {gap} into the deck at {at}");
        }
        let below = v2(x, -0.25 - reach.y - 0.02);
        let up = v2(0.0, 1.0 + 2.0 * reach.y);
        let (moved, _) = slide(&body, below, up, &one_way[..], ALL_LAYERS, &config).unwrap();
        assert_eq!(moved, up, "{body:?} from {below} was stopped rising");
    }
    assert!(
        landed >= 60 && turned >= 3,
        "{landed} landings, {turned} turned off an end"
    );
}

/// Everything every query and one mover step answer for one round or
/// convex body against a world: what is relative to the body as text
/// (every distinct `f32` prints distinctly), and the places it ends at less
/// `shift`, kept apart because a moved scene rounds them at its own
/// magnitude.
fn answers<W: World<Vec2> + ?Sized>(
    body: &Shape<Vec2>,
    at: Vec2,
    delta: Vec2,
    world: &W,
    shift: Vec2,
) -> (String, Vec<Vec2>) {
    let config = config(Vec2::Y);
    let at = at + shift;
    let (slid, contacts) = slide(body, at, delta, world, ALL_LAYERS, &config).unwrap();
    let (collided, hit) = move_and_collide(body, at, delta, world, ALL_LAYERS, &config).unwrap();
    let (snapped, ground) = snap_to_ground(body, at, world, ALL_LAYERS, &config).unwrap();
    let cast = cast(body, at, delta, world, ALL_LAYERS).unwrap();
    let ray = ray(at, delta, world, ALL_LAYERS).unwrap();
    let listed = overlaps(body, at + delta * 0.5, world, ALL_LAYERS).unwrap();
    let ids: Vec<u64> = listed.iter().map(|c| c.id).collect();
    let mut places: Vec<Vec2> = listed.iter().map(|c| c.position - shift).collect();
    let mut state = MoverState {
        position: at,
        velocity: delta * 4.0,
        floor: Some(Floor {
            normal: Vec2::Y,
            id: 0,
        }),
    };
    let options = MoverOptions {
        step_height: 0.3,
        ..MoverOptions::default()
    };
    let step = move_and_slide(&mut state, body, 0.25, world, &config, &options).unwrap();
    places.push(state.position - shift);
    // A step's later stages start from the body's place rounded at its
    // magnitude, so far out its translation is the same only to that
    // rounding, scaled by how the contacts lie: it is a place.
    places.push(step.effective);
    let exact = format!(
        "{slid:?} {contacts:?} {collided:?} {hit:?} {snapped:?} {ground:?} {cast:?} {ray:?} {ids:?} {:?}",
        state.floor
    );
    (exact, places)
}

/// Defect: a kernel result that depends on the order a world visits its
/// colliders, on visiting one twice, or on how far the scene is from the
/// origin (a computation in world coordinates, or a tie broken by
/// visiting order). Oracle: physics.md 6 (order never shows: the same
/// results from any visiting order and any number of visits) and 3 (a
/// query is computed relative to the body: a scene on a quarter grid moved
/// by 2^12, which keeps every difference exact, answers every query
/// bit-equal and lists colliders at their places to the rounding of their
/// coordinates; a mover step, whose later stages start from a place
/// rounded there, ends within a quarter of the skin, which is well above
/// the spacing of `f32` there, 12). Random levels of round and convex
/// colliders, many touching one another so ties arise, met by every query
/// and a mover step of round and convex bodies, as a slice, the slice
/// reversed with every collider twice, a set built shuffled, and the scene
/// moved.
#[wasm_bindgen_test(unsupported = test)]
fn kernel_results_never_depend_on_order_visits_or_place() {
    let mut rng = Rng::new(0x0924_1905);
    let shift = v2(4096.0, -4096.0);
    let rounding = 4096.0 * f32::EPSILON;
    let mut far = 0;
    for _ in 0..30 {
        let mut level = Vec::new();
        for id in 0..8u64 {
            let at = v2(rng.grid(-8, 8, 0.25), rng.grid(-8, 8, 0.25));
            let shape = match rng.below(4) {
                0 => Shape::Box {
                    half: v2(rng.grid(1, 4, 0.25), rng.grid(1, 4, 0.25)),
                },
                1 => Shape::Ball {
                    radius: rng.grid(1, 4, 0.25),
                },
                2 => Shape::Capsule {
                    half_axis: v2(rng.grid(-2, 2, 0.25), rng.grid(-2, 2, 0.25)),
                    radius: 0.25,
                },
                _ => Shape::Convex(Convex::polygon(
                    &[v2(-0.5, -0.5), v2(0.5, -0.5), v2(0.0, 0.75)],
                    1.0,
                    0.0,
                )),
            };
            // Ids repeat in pairs, so ties fall to the position.
            level.push(Collider::new(id / 2, shape, at));
        }
        let mut twice: Vec<Collider<Vec2>> = level.iter().rev().copied().collect();
        twice.extend(level.iter().copied());
        let moved: Vec<Collider<Vec2>> = level
            .iter()
            .map(|c| Collider {
                position: c.position + shift,
                ..*c
            })
            .collect();
        for _ in 0..4 {
            let body = match rng.below(3) {
                0 => Shape::Ball { radius: 0.25 },
                1 => Shape::Capsule {
                    half_axis: v2(0.0, 0.25),
                    radius: 0.25,
                },
                _ => Shape::Convex(Convex::polygon(
                    &[
                        v2(-0.25, -0.25),
                        v2(0.25, -0.25),
                        v2(0.25, 0.25),
                        v2(-0.25, 0.25),
                    ],
                    0.8,
                    0.6,
                )),
            };
            let at = v2(rng.grid(-12, 12, 0.25), rng.grid(-12, 12, 0.25));
            let delta = v2(rng.grid(-24, 24, 0.25), rng.grid(-24, 24, 0.25));
            let why = format!("{body:?} at {at} by {delta}");
            // A body that starts inside a collider is compared far out by
            // `a_push_out_far_from_the_origin_ends_where_it_does_near_it`,
            // which keeps out the places a rounding allowance decides.
            let placed = flat::place(&body, flat::p(at), (1.0, 0.0));
            let clear = level
                .iter()
                .all(|c| separation(&placed, &flat::collider(c)) > f64::from(SKIN));
            let reference = answers(&body, at, delta, &level[..], Vec2::ZERO);
            assert_eq!(
                answers(&body, at, delta, &twice[..], Vec2::ZERO),
                reference,
                "visited reversed and twice: {why}"
            );
            let mut shuffled = level.clone();
            rng.shuffle(&mut shuffled);
            // A set keeps one collider per id: compare it with the slice
            // of the colliders it kept.
            let mut set = ColliderSet::new(1.5);
            for c in &shuffled {
                set.insert(*c);
            }
            let kept: Vec<Collider<Vec2>> = set.iter().copied().collect();
            assert_eq!(
                answers(&body, at, delta, &set, Vec2::ZERO),
                answers(&body, at, delta, &kept[..], Vec2::ZERO),
                "a set built shuffled: {why}"
            );
            if !clear {
                continue;
            }
            far += 1;
            let (exact, places) = answers(&body, at, delta, &moved[..], shift);
            assert_eq!(exact, reference.0, "moved by 2^12: {why}");
            assert_eq!(places.len(), reference.1.len(), "moved by 2^12: {why}");
            let listed = places.len() - 2;
            for (i, (away, near)) in places.iter().zip(&reference.1).enumerate() {
                let tolerance = if i < listed { rounding } else { SKIN / 4.0 };
                assert!(
                    (*away - *near).abs().max_element() <= tolerance,
                    "moved by 2^12: {why}: {away} against {near}"
                );
            }
        }
    }
    assert!(far >= 40, "{far} cases compared far from the origin");
    // A tie by construction: a floor and a wall with one id, met at one
    // instant from a corner's diagonal, by a box body (exact) and a ball.
    // The contract picks the lower position (the floor), whatever the
    // order of the slice.
    let floor = Collider::new(1, Shape::Box { half: v2(2.0, 0.5) }, v2(0.0, -0.5));
    let wall = Collider::new(1, Shape::Box { half: v2(0.5, 1.0) }, v2(1.5, 1.0));
    for body in [
        Shape::Box {
            half: v2(0.25, 0.25),
        },
        Shape::Ball { radius: 0.25 },
    ] {
        let (at, delta) = (v2(0.0, 1.0), v2(1.0, -1.0));
        let one = answers(&body, at, delta, &[floor, wall][..], Vec2::ZERO);
        let other = answers(&body, at, delta, &[wall, floor][..], Vec2::ZERO);
        assert_eq!(one, other, "{body:?} at a tie");
        let hit = cast(&body, at, delta, &[wall, floor][..], ALL_LAYERS).unwrap();
        assert_eq!(hit.map(|h| h.normal), Some(Vec2::Y), "{body:?}: {hit:?}");
    }
}

/// Whether a 2D answer is finite, for the hostile shapes.
fn finite2(v: Vec2) -> bool {
    v.x.is_finite() && v.y.is_finite()
}

/// Defect: a panic, a non-finite result, or a move into a wall for a shape
/// at the edge of what the contract allows: no radius, a radius or an axis
/// enormous but finite, a capsule with no axis, a hull of one point
/// repeated or of points in a line, a rotation far from unit. Oracle:
/// physics.md 9 and 19 (these are sound shapes: every query answers
/// them), 12 (a rotation is normalised first, so one of any finite length
/// turns a shape as the unit one does) and 13 (a finite body never moves
/// to an infinite place). Every query answers each body and collider
/// finitely, a body clear of a wall never ends past it, and polygons and
/// hulls turned by `(cos, sin)` or a quaternion scaled by 1e-30 or 1e30
/// have the unit rotation's points to a few units in the last place.
#[wasm_bindgen_test(unsupported = test)]
fn hostile_round_and_convex_shapes_are_answered_finitely() {
    let config2 = config(Vec2::Y);
    let wall = Collider::new(
        1,
        Shape::Box {
            half: v2(1.0, 10.0),
        },
        v2(5.0, 0.0),
    );
    let bodies = [
        Shape::Ball { radius: 0.0 },
        Shape::Ball { radius: 1e-30 },
        Shape::Ball { radius: 1e30 },
        Shape::Ball { radius: 3e38 },
        Shape::Capsule {
            half_axis: Vec2::ZERO,
            radius: 0.0,
        },
        Shape::Capsule {
            half_axis: v2(0.0, 0.5),
            radius: 0.0,
        },
        Shape::Capsule {
            half_axis: v2(1e30, 0.0),
            radius: 0.5,
        },
        Shape::Capsule {
            half_axis: v2(-3e38, 3e38),
            radius: 3e38,
        },
    ];
    for body in bodies {
        let colliders = [wall, Collider::new(2, body, v2(-5.0, 0.0))];
        for (shape, at) in [
            (body, Vec2::ZERO),
            (Shape::Ball { radius: 0.5 }, Vec2::ZERO),
        ] {
            let why = format!("{shape:?} among {:?}", colliders[1]);
            let delta = v2(10.0, 0.5);
            if let Some(hit) = cast(&shape, at, delta, &colliders[..], ALL_LAYERS).unwrap() {
                assert!(hit.t.is_finite() && finite2(hit.normal), "{why}");
            }
            let (moved, _) =
                slide(&shape, at, delta, &colliders[..], ALL_LAYERS, &config2).unwrap();
            assert!(finite2(moved) && moved.x <= 4.0, "{why}: {moved}");
            let (moved, _) =
                move_and_collide(&shape, at, -delta, &colliders[..], ALL_LAYERS, &config2).unwrap();
            assert!(finite2(moved), "{why}: {moved}");
            for c in &colliders {
                if let Some(found) = distance(&shape, at, c) {
                    assert!(found.distance.is_finite() && finite2(found.normal), "{why}");
                }
            }
            overlaps(&shape, at, &colliders[..], ALL_LAYERS).unwrap();
            let mut state = MoverState {
                position: at,
                velocity: v2(3.0, -3.0),
                floor: None,
            };
            move_and_slide(
                &mut state,
                &shape,
                0.5,
                &colliders[..],
                &config2,
                &MoverOptions::default(),
            )
            .unwrap();
            assert!(
                finite2(state.position) && finite2(state.velocity),
                "{why}: {state:?}"
            );
        }
    }
    // Hulls of one point repeated, and of points in a line: sound shapes.
    let config3 = config(Vec3::Y);
    let repeated = [v3(0.2, 0.3, 0.1); 16];
    let line: Vec<Vec3> = (0..16)
        .map(|i| v3(i as f32 * 0.1 - 0.75, 0.0, 0.0))
        .collect();
    for points in [&repeated[..], &line[..], &line[..2], &line[..1]] {
        let hull = Shape::Convex(Convex::hull(points, Quat::IDENTITY));
        let colliders = [Collider::new(3, hull, v3(3.0, 0.0, 0.0))];
        for shape in [hull, Shape::Ball { radius: 0.3 }] {
            let why = format!("{shape:?} at {points:?}");
            let delta = v3(6.0, 0.0, 0.0);
            if let Some(hit) = cast(&shape, Vec3::ZERO, delta, &colliders[..], ALL_LAYERS).unwrap()
            {
                assert!(hit.t.is_finite(), "{why}");
            }
            let (moved, _) = slide(
                &shape,
                Vec3::ZERO,
                delta,
                &colliders[..],
                ALL_LAYERS,
                &config3,
            )
            .unwrap();
            assert!(moved.is_finite(), "{why}");
            let mut state = MoverState {
                position: Vec3::ZERO,
                velocity: delta,
                floor: None,
            };
            move_and_slide(
                &mut state,
                &shape,
                1.0,
                &colliders[..],
                &config3,
                &MoverOptions::default(),
            )
            .unwrap();
            assert!(state.position.is_finite(), "{why}");
        }
    }
    // A ball swept along a hull of points in a line meets it: it is the
    // segment from x = 2.25 to 3.75, by the shapes' definitions.
    let hull = Shape::Convex(Convex::hull(&line, Quat::IDENTITY));
    let hit = cast(
        &Shape::Ball { radius: 0.25 },
        v3(0.0, 0.1, 0.0),
        v3(6.0, 0.0, 0.0),
        &[Collider::new(3, hull, v3(3.0, 0.0, 0.0))][..],
        ALL_LAYERS,
    )
    .unwrap()
    .expect("the ball meets the segment");
    let centre = 6.0 * hit.t;
    let gap = (f64::from(centre) - 2.25).hypot(0.1) - 0.25;
    assert!(
        gap > 0.0 && gap < 1.25 * 0.0025 + 1e-5,
        "{hit:?}: gap {gap}"
    );
    // Rotations far from unit turn as the unit one.
    let square = [v2(-1.0, -0.5), v2(1.0, -0.5), v2(1.0, 0.5), v2(-1.0, 0.5)];
    let unit = Convex::polygon(&square, 0.6, 0.8);
    for scale in [1e-30f32, 1e30, 2e38] {
        let scaled = Convex::polygon(&square, 0.6 * scale, 0.8 * scale);
        for (a, b) in unit.points().iter().zip(scaled.points()) {
            assert!(
                (*a - *b).abs().max_element() <= 4.0 * f32::EPSILON,
                "{scale}: {a} {b}"
            );
        }
    }
    let cube: Vec<Vec3> = (0u8..8)
        .map(|i| {
            v3(
                f32::from(i & 1),
                f32::from(i >> 1 & 1),
                f32::from(i >> 2 & 1),
            ) - 0.5
        })
        .collect();
    let turn = Quat::from_axis_angle(v3(1.0, 2.0, 3.0).normalize(), 0.7);
    let unit = Convex::hull(&cube, turn);
    for scale in [1e-30f32, 1e30] {
        let scaled = Convex::hull(&cube, turn * scale);
        for (a, b) in unit.points().iter().zip(scaled.points()) {
            assert!(
                (*a - *b).abs().max_element() <= 8.0 * f32::EPSILON,
                "{scale}: {a} {b}"
            );
        }
    }
}

/// Defect: a capsule with no axis that is met, or meets, unlike the ball
/// it is. Oracle: physics.md 19 (a capsule is the segment from
/// `-half_axis` to `half_axis` grown by `radius`: with no axis, the ball of
/// that radius) and the contact band (a cast stops between one and a
/// quarter slops short). Cast as a body and met as a collider, against a
/// box, a ball and a polygon, a capsule with no axis and the ball of its
/// radius are cast to within the band of each other and measured to the
/// same distance to within the band.
#[wasm_bindgen_test(unsupported = test)]
fn a_capsule_with_no_axis_is_met_as_the_ball_it_is() {
    let mut rng = Rng::new(0x0924_1906);
    let others = [
        Shape::Box { half: v2(0.6, 0.4) },
        Shape::Ball { radius: 0.5 },
        Shape::Convex(Convex::polygon(
            &[v2(-0.5, -0.4), v2(0.6, -0.3), v2(0.0, 0.7)],
            1.0,
            0.0,
        )),
    ];
    let mut hits = 0;
    for _ in 0..120 {
        let radius = rng.range(0.1, 0.6);
        let ball = Shape::Ball { radius };
        let capsule = Shape::Capsule {
            half_axis: Vec2::ZERO,
            radius,
        };
        let other = others[rng.below(3)];
        let at: Vec2 = rng.vector(-3.0, 3.0);
        let delta: Vec2 = (rng.vector::<Vec2>(-0.8, 0.8) - at) * 2.0;
        // Two slop bands of the larger pair: a hundredth of 0.6, plus
        // rounding.
        let band = 2.0 * 1.25 * 0.006 + 1e-5;
        // The round shape as the body, then as the collider.
        let pairs = [
            ((capsule, other), (ball, other)),
            ((other, capsule), (other, ball)),
        ];
        for ((body_a, collider_a), (body_b, collider_b)) in pairs {
            let world_a = [Collider::new(1, collider_a, Vec2::ZERO)];
            let world_b = [Collider::new(1, collider_b, Vec2::ZERO)];
            let a = cast(&body_a, at, delta, &world_a[..], ALL_LAYERS).unwrap();
            let b = cast(&body_b, at, delta, &world_b[..], ALL_LAYERS).unwrap();
            let why = format!("{body_a:?} into {collider_a:?} from {at} by {delta}: {a:?} {b:?}");
            match (a, b) {
                (Some(a), Some(b)) => {
                    hits += 1;
                    assert!(((a.t - b.t) * delta.length()).abs() <= band, "{why}");
                }
                (None, None) => {}
                // Only a graze, within the band of the surface, may read
                // either way.
                _ => {
                    let reach = distance(&body_b, at, &world_b[0]).unwrap();
                    assert!(reach.distance.is_finite(), "{why}");
                }
            }
            let (da, db) = (
                distance(&body_a, at, &world_a[0]).unwrap(),
                distance(&body_b, at, &world_b[0]).unwrap(),
            );
            assert!(
                (da.distance - db.distance).abs() <= band,
                "{why}: {da:?} {db:?}"
            );
        }
    }
    assert!(hits >= 40, "{hits} hits");
}

/// Defect: a push-out that panics (a kernel proxy with a negative radius)
/// when the body is inside a crate and beside a round collider thinner
/// than its rounding allowance, as a pole a few centimetres thick is a
/// hundred kilometres out. Oracle: physics.md 9 (a collider with a
/// negative radius occupies nothing: every query skips it), 17 (a body
/// inside a rigid solid is pushed out, or reported `stuck`) and the
/// shapes' definitions. A capsule walker 100 km from the origin, a crate
/// dropped on its head and a pole 4 cm thick touching its side: the step
/// returns, and the walker ends clear of the crate.
#[wasm_bindgen_test(unsupported = test)]
fn a_thin_round_collider_far_out_never_breaks_a_push_out() {
    let config = config(Vec3::Y);
    let far = 100_000.0;
    let walker = Shape::Capsule {
        half_axis: v3(0.0, 0.6, 0.0),
        radius: 0.3,
    };
    let crate_box = Collider::new(
        4,
        Shape::Box {
            half: Vec3::splat(0.25),
        },
        v3(far + 0.2, 1.8, 0.0),
    );
    let pole = Collider::new(
        3,
        Shape::Capsule {
            half_axis: v3(0.0, 1.0, 0.0),
            radius: 0.04,
        },
        v3(far, 1.0, 0.0),
    );
    let mut state = MoverState {
        position: v3(far + 0.2, 1.0, 0.0),
        velocity: Vec3::ZERO,
        floor: None,
    };
    let contacts = move_and_slide(
        &mut state,
        &walker,
        1.0 / 60.0,
        &[pole, crate_box][..],
        &config,
        &MoverOptions::default(),
    )
    .unwrap();
    let into = solid::depth(&walker, state.position, |p| {
        solid::to_collider(p, &crate_box)
    });
    assert!(
        contacts.pushed_out && into <= 0.0,
        "{contacts:?}: {into} inside the crate at {}",
        state.position
    );
}

/// Where one step of a body at rest with no floor ends, less `shift`: the
/// push-out and nothing else.
fn pushed_out<V: sgl_physics::Vector>(
    body: &Shape<V>,
    at: V,
    world: &[Collider<V>],
    shift: V,
) -> Option<V> {
    let config = config(audit_support::up::<V>());
    let mut state = MoverState {
        position: at + shift,
        velocity: V::ZERO,
        floor: None,
    };
    let step = move_and_slide(
        &mut state,
        body,
        0.25,
        world,
        &config,
        &MoverOptions::default(),
    )
    .unwrap();
    step.pushed_out.then_some(state.position - shift)
}

/// A collider's rounding allowance at its place, its largest component
/// (physics.md 10: `4ε(|position| + half)` on each axis).
fn allowance<V: sgl_physics::Vector>(c: &Collider<V>) -> f64 {
    let half = c.shape.half_extents();
    (0..V::AXES)
        .map(|axis| {
            4.0 * f64::from(f32::EPSILON)
                * (f64::from(c.position[axis].abs()) + f64::from(half[axis]))
        })
        .fold(0.0, f64::max)
}

/// Defect: a push-out whose result depends on how far the scene is from
/// the origin: one that measures its way out of a round or convex collider
/// from the collider less its rounding allowance, which grows with the
/// distance from the origin, so far out it leaves the body short of a skin
/// clear and chooses another place (the audit measured two skins apart at
/// 4096 units). Oracle: physics.md 3 and 17 (a query is computed relative
/// to the body; the push-out puts a body a skin from what it leaves). A
/// scene on a quarter grid moved by `2^k` on every axis, which keeps every
/// difference exact, is pushed out to the place near the origin rounded at
/// the far coordinate: within one unit in the last place there. Exempt, as
/// the contract states (10, 17): a body that starts, or ends far out,
/// inside a collider by no more than that collider's rounding allowance
/// there, by the shapes' own definitions in `f64`; that is touching far
/// out and inside near the origin. Balls, capsules and convex bodies
/// started inside random levels of boxes, balls, capsules and convex
/// colliders, in 2D and 3D, at 16, 256, 4096 and 8192 units.
#[wasm_bindgen_test(unsupported = test)]
fn a_push_out_far_from_the_origin_ends_where_it_does_near_it() {
    let mut compared = 0;
    for k in [4, 8, 12, 13] {
        let s = (1u32 << k) as f32;
        let ulp = s * f32::EPSILON;
        let shift2 = v2(s, -s);
        let mut rng = Rng::new(0x0924_C002);
        for _ in 0..40 {
            let level: Vec<Collider<Vec2>> = (0..8u64)
                .map(|id| {
                    let at = v2(rng.grid(-8, 8, 0.25), rng.grid(-8, 8, 0.25));
                    let shape = match rng.below(4) {
                        0 => Shape::Box {
                            half: v2(rng.grid(1, 4, 0.25), rng.grid(1, 4, 0.25)),
                        },
                        1 => Shape::Ball {
                            radius: rng.grid(1, 4, 0.25),
                        },
                        2 => Shape::Capsule {
                            half_axis: v2(rng.grid(-2, 2, 0.25), rng.grid(-2, 2, 0.25)),
                            radius: 0.25,
                        },
                        _ => Shape::Convex(Convex::polygon(
                            &[v2(-0.5, -0.5), v2(0.5, -0.5), v2(0.0, 0.75)],
                            1.0,
                            0.0,
                        )),
                    };
                    Collider::new(id, shape, at)
                })
                .collect();
            let moved: Vec<Collider<Vec2>> = level
                .iter()
                .map(|c| Collider {
                    position: c.position + shift2,
                    ..*c
                })
                .collect();
            let in_band = |body: &Shape<Vec2>, at: Vec2| {
                let placed = flat::place(body, flat::p(at), (1.0, 0.0));
                moved.iter().any(|c| {
                    let depth = -separation(&placed, &flat::collider(c));
                    depth > 0.0 && depth <= allowance(c)
                })
            };
            for _ in 0..4 {
                let body = match rng.below(3) {
                    0 => Shape::Ball { radius: 0.25 },
                    1 => Shape::Capsule {
                        half_axis: v2(0.0, 0.25),
                        radius: 0.25,
                    },
                    _ => Shape::Convex(Convex::polygon(
                        &[
                            v2(-0.25, -0.25),
                            v2(0.25, -0.25),
                            v2(0.25, 0.25),
                            v2(-0.25, 0.25),
                        ],
                        0.8,
                        0.6,
                    )),
                };
                let at = v2(rng.grid(-12, 12, 0.25), rng.grid(-12, 12, 0.25));
                let Some(near) = pushed_out(&body, at, &level, Vec2::ZERO) else {
                    continue;
                };
                let far = pushed_out(&body, at, &moved, shift2);
                if in_band(&body, at + shift2)
                    || far.is_some_and(|far| in_band(&body, far + shift2))
                {
                    continue;
                }
                let far = far.expect("inside far out as near the origin");
                assert!(
                    (far - near).abs().max_element() <= ulp,
                    "2D at 2^{k}: {body:?} from {at}: {far} far, {near} near"
                );
                compared += 1;
            }
        }
        let shift3 = v3(s, -s, s);
        for _ in 0..40 {
            let level: Vec<Collider<Vec3>> = (0..8u64)
                .map(|id| {
                    let at = v3(
                        rng.grid(-6, 6, 0.25),
                        rng.grid(-6, 6, 0.25),
                        rng.grid(-6, 6, 0.25),
                    );
                    let shape = match rng.below(3) {
                        0 => Shape::Box {
                            half: v3(
                                rng.grid(1, 4, 0.25),
                                rng.grid(1, 4, 0.25),
                                rng.grid(1, 4, 0.25),
                            ),
                        },
                        1 => Shape::Ball {
                            radius: rng.grid(1, 4, 0.25),
                        },
                        _ => Shape::Capsule {
                            half_axis: v3(
                                rng.grid(-2, 2, 0.25),
                                rng.grid(-2, 2, 0.25),
                                rng.grid(-2, 2, 0.25),
                            ),
                            radius: 0.25,
                        },
                    };
                    Collider::new(id, shape, at)
                })
                .collect();
            let moved: Vec<Collider<Vec3>> = level
                .iter()
                .map(|c| Collider {
                    position: c.position + shift3,
                    ..*c
                })
                .collect();
            let in_band = |body: &Shape<Vec3>, at: Vec3| {
                moved.iter().any(|c| {
                    let depth = solid::depth(body, at, |p| solid::to_collider(p, c));
                    depth > 0.0 && depth <= allowance(c)
                })
            };
            for _ in 0..4 {
                let body = if rng.chance(50) {
                    Shape::Ball { radius: 0.25 }
                } else {
                    Shape::Capsule {
                        half_axis: v3(0.0, 0.25, 0.0),
                        radius: 0.25,
                    }
                };
                let at = v3(
                    rng.grid(-8, 8, 0.25),
                    rng.grid(-8, 8, 0.25),
                    rng.grid(-8, 8, 0.25),
                );
                let Some(near) = pushed_out(&body, at, &level, Vec3::ZERO) else {
                    continue;
                };
                let far = pushed_out(&body, at, &moved, shift3);
                if in_band(&body, at + shift3)
                    || far.is_some_and(|far| in_band(&body, far + shift3))
                {
                    continue;
                }
                let far = far.expect("inside far out as near the origin");
                assert!(
                    (far - near).abs().max_element() <= ulp,
                    "3D at 2^{k}: {body:?} from {at}: {far} far, {near} near"
                );
                compared += 1;
            }
        }
    }
    assert!(compared >= 300, "{compared} push-outs compared");
}

/// Defect: a ray that meets a ball of no radius (a point, which nothing
/// with no extent can meet), or that passes through a capsule of no radius
/// (a segment) it crosses. Oracle: physics.md 15, Rays (a ball of no
/// radius is a point a ray never meets; a capsule of no radius is a
/// segment, met through the kernel a slop short where the ray crosses it)
/// and 19 (a cast stops between one and one and a quarter slops short; the
/// slop is a hundredth of the smaller shape's extent, the ray's not
/// counting). Rays straight through a point and across a unit-half-length
/// segment, in 2D and 3D: the point is never met; the segment is met
/// between `(1 - 1.25 * 0.01) / 2` and `(1 - 0.01) / 2` of the way,
/// facing the ray.
#[wasm_bindgen_test(unsupported = test)]
fn a_ray_meets_a_segment_it_crosses_and_never_a_point() {
    let (low, high) = ((1.0 - 1.25 * 0.01) / 2.0, (1.0 - 0.01) / 2.0);
    for at in [v2(0.0, 0.0), v2(37.5, -12.25)] {
        let point = [Collider::new(1, Shape::Ball { radius: 0.0 }, at)];
        let segment = [Collider::new(
            2,
            Shape::Capsule {
                half_axis: v2(0.0, 1.0),
                radius: 0.0,
            },
            at,
        )];
        let from = at - v2(1.0, 0.0);
        let along = v2(2.0, 0.0);
        assert_eq!(ray(from, along, &point[..], u32::MAX).unwrap(), None);
        let hit = ray(from, along, &segment[..], u32::MAX)
            .unwrap()
            .expect("the segment is crossed");
        assert!(
            (low..=high).contains(&hit.t) && hit.normal == v2(-1.0, 0.0),
            "{hit:?}"
        );
    }
    for at in [v3(0.0, 0.0, 0.0), v3(37.5, -12.25, 3.0)] {
        let point = [Collider::new(1, Shape::Ball { radius: 0.0 }, at)];
        let segment = [Collider::new(
            2,
            Shape::Capsule {
                half_axis: v3(0.0, 1.0, 0.0),
                radius: 0.0,
            },
            at,
        )];
        let from = at - v3(1.0, 0.0, 0.0);
        let along = v3(2.0, 0.0, 0.0);
        assert_eq!(ray(from, along, &point[..], u32::MAX).unwrap(), None);
        let hit = ray(from, along, &segment[..], u32::MAX)
            .unwrap()
            .expect("the segment is crossed");
        let normal = flat::length(flat::p(v2(hit.normal.y, hit.normal.z)));
        assert!(
            (low..=high).contains(&hit.t) && hit.normal.x < -0.999_999 && normal < 1e-3,
            "{hit:?}"
        );
    }
}
