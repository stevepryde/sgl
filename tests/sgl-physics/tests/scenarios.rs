//! Game scenarios for the mover (physics.md, "Mover"): a voxel walker through
//! a `CellWorld` and a 2D platformer through a `ColliderSet`, each driven
//! the way a game drives it, a fixed step with its own gravity and input.
//! Every expectation comes from the requirement or from the level's
//! geometry, worked in the comment beside it. Runs natively and in a wasm
//! runtime.

use sgl_core::math::{IVec3, Vec2, Vec3};
use sgl_physics::{
    ALL_LAYERS, CellColliders, CellWorld, Collider, ColliderKind, ColliderSet, Contacts, Cut,
    Floor, MoverConfig, MoverOptions, MoverSettings, MoverState, Shape, Vector, World, find_ledge,
    move_and_slide, overlaps,
};
use wasm_bindgen_test::wasm_bindgen_test;

const SKIN: f32 = 0.01;
const DT: f32 = 1.0 / 60.0;
const GRAVITY: f32 = 20.0;

fn config<V: Vector>(up: V) -> MoverConfig<V> {
    MoverConfig::new(MoverSettings {
        up,
        skin: SKIN,
        snap_distance: 0.1,
        normal_threshold: 0.7,
        block_epsilon: 1e-4,
    })
}

/// Whether two boxes share interior: plain geometry, no crate code.
fn overlap<V: Vector>(a: V, a_half: V, b: V, b_half: V) -> bool {
    (0..V::AXES).all(|axis| (a[axis] - b[axis]).abs() < a_half[axis] + b_half[axis])
}

fn close(value: f32, expected: f32, tolerance: f32) -> bool {
    (value - expected).abs() <= tolerance
}

/// Whether a body whose bottom is at `bottom` rests on a floor whose top is
/// at `top`: on it or above it by no more than the skin. A landing leaves a
/// skin; a later step whose fall is shorter than the gap closes some of it
/// without touching.
fn rests(bottom: f32, top: f32) -> bool {
    let gap = bottom - top;
    (0.0..=SKIN + 1e-5).contains(&gap)
}

/// Whether a body's leading edge at `edge` has stopped against a face at
/// `face` it moves toward along +x: not past it, and no more than a skin
/// short. The skin is left where the move meets the face; a move that ends
/// just short of it leaves less.
fn against(edge: f32, face: f32) -> bool {
    (0.0..=SKIN + 1e-5).contains(&(face - edge))
}

// ------------------------------------------------------------ voxel walker

/// The walker: 0.6 wide and deep, 1.8 tall, in a world of 1-unit cells.
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

/// The lower half of a cell.
fn slab(id: u64) -> Collider<Vec3> {
    Collider::new(
        id,
        Shape::Box {
            half: Vec3::new(0.5, 0.25, 0.5),
        },
        Vec3::new(0.5, 0.25, 0.5),
    )
}

fn cell_id(cell: IVec3) -> u64 {
    u64::from((cell.x + 512).unsigned_abs())
        | u64::from((cell.y + 512).unsigned_abs()) << 10
        | u64::from((cell.z + 512).unsigned_abs()) << 20
}

/// Ground everywhere near the origin: the cells at y = -1, so the floor's
/// top is at y = 0.
fn ground(cell: IVec3) -> bool {
    cell.y == -1 && cell.x.abs() <= 20 && cell.z.abs() <= 20
}

/// A voxel level from a rule saying which cells are full.
fn cells(
    rule: impl Fn(IVec3) -> Option<Collider<Vec3>>,
) -> CellWorld<Vec3, impl Fn(IVec3, &mut CellColliders<'_, Vec3>)> {
    CellWorld::new(
        1.0,
        move |cell: IVec3, out: &mut CellColliders<'_, Vec3>| {
            if let Some(collider) = rule(cell) {
                out.push(collider);
            }
        },
    )
}

/// Standing on the ground at (x, z): a skin above y = 0.
fn on_ground(x: f32, z: f32) -> MoverState<Vec3> {
    MoverState {
        position: Vec3::new(x, 0.9 + SKIN, z),
        velocity: Vec3::ZERO,
        floor: Some(Floor {
            normal: Vec3::Y,
            id: cell_id(IVec3::new(0, -1, 0)),
        }),
    }
}

/// One step of a walker holding `walk` (world units per second across), as
/// a game runs it: gravity, then the mover.
fn walk_step<W: World<Vec3>>(
    state: &mut MoverState<Vec3>,
    world: &W,
    walk: Vec3,
    options: &MoverOptions,
) -> Contacts<Vec3> {
    state.velocity.x = walk.x;
    state.velocity.z = walk.z;
    state.velocity.y -= GRAVITY * DT;
    move_and_slide(state, &walker(), DT, world, &config(Vec3::Y), options)
        .expect("a walker's reach is far under the cell cap")
}

/// Dropped from y = 3, the walker falls and lands on the ground. Oracle:
/// the requirement (landing zeroes the fall and sets the floor) and
/// geometry (it rests a skin above y = 0).
#[wasm_bindgen_test(unsupported = test)]
fn a_voxel_walker_falls_and_lands() {
    let world = cells(|cell| ground(cell).then(|| full(cell_id(cell))));
    let mut state = MoverState {
        position: Vec3::new(0.5, 3.0, 0.5),
        ..MoverState::default()
    };
    for _ in 0..90 {
        walk_step(&mut state, &world, Vec3::ZERO, &MoverOptions::default());
    }
    assert!(rests(state.position.y - 0.9, 0.0), "{state:?}");
    assert_eq!(state.velocity, Vec3::ZERO);
    assert_eq!(state.floor.map(|f| f.normal), Some(Vec3::Y));
}

/// A wall of cells at x = 3: walking right at 4 units a second for a second
/// and a half (six units), the walker stops with its right edge a skin
/// short of x = 3 and never passes it. Oracle: geometry.
#[wasm_bindgen_test(unsupported = test)]
fn a_voxel_walker_stops_at_a_wall() {
    let world = cells(|cell| {
        (ground(cell) || (cell.x == 3 && (0..3).contains(&cell.y))).then(|| full(cell_id(cell)))
    });
    let mut state = on_ground(0.5, 0.5);
    for _ in 0..90 {
        walk_step(&mut state, &world, Vec3::X * 4.0, &MoverOptions::default());
        assert!(state.position.x + 0.3 <= 3.0 - SKIN + 1e-5, "{state:?}");
    }
    assert!(against(state.position.x + 0.3, 3.0), "{state:?}");
    assert!(state.floor.is_some());
}

/// Walking at (4.3, 1.7) units a second across a flat floor of cells, the
/// walker crosses many cell boundaries; every step it travels exactly the
/// velocity times the step across, never caught on a seam. Oracle: the
/// requirement.
#[wasm_bindgen_test(unsupported = test)]
fn a_voxel_walker_crosses_cell_boundaries_without_snagging() {
    let world = cells(|cell| ground(cell).then(|| full(cell_id(cell))));
    let walk = Vec3::new(4.3, 0.0, 1.7);
    let mut state = on_ground(0.5, 0.5);
    for _ in 0..120 {
        let before = state.position;
        walk_step(&mut state, &world, walk, &MoverOptions::default());
        let moved = state.position - before;
        assert!(close(moved.x, walk.x * DT, 1e-5), "{moved:?}");
        assert!(close(moved.z, walk.z * DT, 1e-5), "{moved:?}");
        assert!(state.floor.is_some(), "{state:?}");
    }
    assert!(state.position.x > 8.0);
}

/// A doorway in the cells at x = 3: two posts leave a gap of `width` along
/// z, centred on the walker's path at z = 0.5. A gap the walker's width
/// plus a skin each side lets it through; one 0.02 narrower than the walker
/// stops it a skin short of the posts. Oracle: geometry.
#[wasm_bindgen_test(unsupported = test)]
fn a_voxel_walker_fits_a_gap_its_width_and_not_a_narrower_one() {
    for (width, fits) in [(0.6 + 2.0 * SKIN, true), (0.58, false)] {
        // Each post is (1 - width) / 2 deep along z: one from z = 0, one
        // up to z = 1, both a cell wide and two cells tall.
        let post = (1.0 - width) * 0.25;
        let world = CellWorld::new(
            1.0,
            move |cell: IVec3, out: &mut CellColliders<'_, Vec3>| {
                if ground(cell) {
                    out.push(full(cell_id(cell)));
                } else if cell.x == 3 && cell.z == 0 && (0..2).contains(&cell.y) {
                    for z in [post, 1.0 - post] {
                        let half = Vec3::new(0.5, 0.5, post);
                        let at = Vec3::new(0.5, 0.5, z);
                        out.push(Collider::new(cell_id(cell), Shape::Box { half }, at));
                    }
                }
            },
        );
        let mut state = on_ground(0.5, 0.5);
        for _ in 0..90 {
            walk_step(&mut state, &world, Vec3::X * 4.0, &MoverOptions::default());
        }
        if fits {
            assert!(state.position.x > 4.5, "width {width}: {state:?}");
        } else {
            assert!(
                against(state.position.x + 0.3, 3.0),
                "width {width}: {state:?}"
            );
        }
    }
}

/// With a step height of half a cell, the walker climbs a half-cell slab at
/// x = 3 (standing a skin above y = 0.5 past it) and not a full cell, at
/// which it stops a skin short. Oracle: the requirement and geometry.
#[wasm_bindgen_test(unsupported = test)]
fn a_voxel_walker_climbs_half_a_cell_and_not_a_full_one() {
    let options = MoverOptions {
        step_height: 0.5,
        ..MoverOptions::default()
    };
    for climbs in [true, false] {
        let world = cells(move |cell| {
            if ground(cell) {
                Some(full(cell_id(cell)))
            } else if cell.x >= 3 && cell.y == 0 {
                Some(if climbs {
                    slab(cell_id(cell))
                } else {
                    full(cell_id(cell))
                })
            } else {
                None
            }
        });
        let mut state = on_ground(0.5, 0.5);
        let mut stepped = false;
        for _ in 0..90 {
            stepped |= walk_step(&mut state, &world, Vec3::X * 4.0, &options).stepped;
        }
        assert_eq!(stepped, climbs);
        if climbs {
            assert!(state.position.x > 4.5, "{state:?}");
            assert!(rests(state.position.y - 0.9, 0.5), "{state:?}");
        } else {
            assert!(against(state.position.x + 0.3, 3.0), "{state:?}");
        }
    }
}

/// The ground ends at x = 5; past it, a drop of half a cell (a slab whose
/// top is at y = -0.5) or of a whole cell (the top at y = -1). Guarded with
/// a drop of 0.6, the walker takes the half-cell drop and walks on, and
/// stops at the whole-cell edge with its trailing edge within a skin of it,
/// every step. Oracle: the requirement and geometry.
#[wasm_bindgen_test(unsupported = test)]
fn a_guarded_voxel_walker_stops_at_an_edge_and_takes_a_shallow_drop() {
    let options = MoverOptions {
        ledge_drop: Some(0.6),
        ..MoverOptions::default()
    };
    for shallow in [true, false] {
        let world = cells(move |cell| {
            let id = cell_id(cell);
            if cell.x < 5 {
                ground(cell).then(|| full(id))
            } else if shallow {
                (cell.y == -1 && cell.x <= 20).then(|| slab(id))
            } else {
                (cell.y == -2 && cell.x <= 20).then(|| full(id))
            }
        });
        let mut state = on_ground(3.5, 0.5);
        for _ in 0..90 {
            walk_step(&mut state, &world, Vec3::X * 4.0, &options);
            if !shallow {
                assert!(state.position.x - 0.3 < 5.0, "{state:?}");
            }
        }
        if shallow {
            assert!(state.position.x > 6.0, "{state:?}");
            assert!(rests(state.position.y - 0.9, -0.5), "{state:?}");
        } else {
            assert!(state.position.x - 0.3 >= 5.0 - SKIN, "{state:?}");
            assert!(state.floor.is_some());
        }
    }
}

/// Cells from x = 6 on are unloaded, and the game answers them as full
/// boxes: the walker stops at them as at a wall, its right edge a skin
/// short of x = 6, still on the ground. Oracle: the requirement and
/// geometry.
#[wasm_bindgen_test(unsupported = test)]
fn an_unloaded_cell_answered_as_a_full_box_blocks_like_a_wall() {
    let world = cells(|cell| (ground(cell) || cell.x >= 6).then(|| full(cell_id(cell))));
    let mut state = on_ground(3.5, 0.5);
    for _ in 0..90 {
        walk_step(&mut state, &world, Vec3::X * 4.0, &MoverOptions::default());
    }
    assert!(against(state.position.x + 0.3, 6.0), "{state:?}");
    assert!(state.floor.is_some());
}

// ------------------------------------------------------------ voxel ramps

/// The four directions across `up` a slope block can rise toward.
const HEADINGS: [Vec3; 4] = [Vec3::X, Vec3::NEG_X, Vec3::Z, Vec3::NEG_Z];

/// How far along `heading` a cell lies, counted so that cell `a` spans `a`
/// to `a + 1` along it in either sign.
fn along(cell: IVec3, heading: Vec3) -> i32 {
    let index = if heading.x == 0.0 { cell.z } else { cell.x };
    if heading.x + heading.z > 0.0 {
        index
    } else {
        -index - 1
    }
}

/// The cell `a` cells along `heading` at height `y`, in the walker's row.
fn cell_along(a: i32, y: i32, heading: Vec3) -> IVec3 {
    let index = if heading.x + heading.z > 0.0 {
        a
    } else {
        -a - 1
    };
    if heading.x == 0.0 {
        IVec3::new(0, y, index)
    } else {
        IVec3::new(index, y, 0)
    }
}

/// A slope block rising toward `heading` at 45°: the cell cut by the plane
/// of unit normal (up - heading) / √2, solid below it. Across the cell
/// (`u` from -0.5 to 0.5 along `heading`, `y` likewise from its centre)
/// the solid part is `(y - u) / √2 <= offset`, so `y <= u + offset·√2`: an
/// offset of `lip / √2` puts the low end's top `lip` above the cell's
/// floor, a vertical face `lip` tall, and reaches the top `lip` short of
/// the high end. A floor to the walker: `normal · up` is 0.7071, over the
/// 0.7 threshold.
fn slope(id: u64, heading: Vec3, lip: f32) -> Collider<Vec3> {
    full(id).with_cut(Cut::new(
        Vec3::Y - heading,
        lip * core::f32::consts::FRAC_1_SQRT_2,
    ))
}

/// Ground, a row of slope blocks 3 cells along `heading` and, with
/// `platform`, full blocks from 4 to 20 cells along it, whose top is at
/// y = 1.
fn ramp_level(
    heading: Vec3,
    lip: f32,
    platform: bool,
) -> CellWorld<Vec3, impl Fn(IVec3, &mut CellColliders<'_, Vec3>)> {
    cells(move |cell| {
        let (a, id) = (along(cell, heading), cell_id(cell));
        if ground(cell) {
            Some(full(id))
        } else if cell.y == 0 && a == 3 {
            Some(slope(id, heading, lip))
        } else if cell.y == 0 && platform && (4..=20).contains(&a) {
            Some(full(id))
        } else {
            None
        }
    })
}

/// The walker on the ground `t` along `heading`, in the middle of its row.
fn on_ground_along(heading: Vec3, t: f32) -> MoverState<Vec3> {
    let lateral = if heading.x == 0.0 { Vec3::X } else { Vec3::Z };
    let at = heading * t + lateral * 0.5;
    on_ground(at.x, at.z)
}

/// The slope block's unit normal, worked by hand: (up - heading) / √2.
fn slope_normal(heading: Vec3) -> Vec3 {
    (Vec3::Y - heading) * core::f32::consts::FRAC_1_SQRT_2
}

fn same_direction(a: Vec3, b: Vec3) -> bool {
    (a - b).abs().max_element() <= 1e-6
}

/// Walking at 4 units a second toward a 45° slope block, up it onto the
/// platform behind it, then back down to where it started, in each of the
/// four directions across `up`: every step the walker covers exactly the
/// velocity times the step across `up`, as on flat ground; its velocity
/// keeps all of its part across `up` and none against it; it stays on a
/// floor, which on the slope is the slope block's, by its normal and id;
/// it is never pushed out. It ends the climb resting on the platform
/// (y = 1) and the descent on the ground. Oracle: the requirement (a
/// walkable slope does not slow a body, and a walker stays grounded down
/// it) and geometry. Six units out and six back at 4 units a second is 90
/// steps each way.
#[wasm_bindgen_test(unsupported = test)]
fn a_voxel_walker_walks_up_and_down_a_ramp_at_its_flat_speed() {
    for heading in HEADINGS {
        let world = ramp_level(heading, 0.0, true);
        let ramp_id = cell_id(cell_along(3, 0, heading));
        let mut state = on_ground_along(heading, 0.5);
        let mut on_slope = 0;
        for (walk, top) in [(heading * 4.0, 1.0), (heading * -4.0, 0.0)] {
            for _ in 0..90 {
                let before = state.position;
                let contacts = walk_step(&mut state, &world, walk, &MoverOptions::default());
                let moved = state.position - before;
                assert!(close(moved.x, walk.x * DT, 1e-5), "{heading}: {state:?}");
                assert!(close(moved.z, walk.z * DT, 1e-5), "{heading}: {state:?}");
                assert_eq!(state.velocity, walk, "{heading}");
                assert!(!contacts.pushed_out, "{heading}");
                let floor = state.floor.expect("grounded every step");
                if floor.normal != Vec3::Y {
                    assert!(same_direction(floor.normal, slope_normal(heading)));
                    assert_eq!(floor.id, ramp_id, "{heading}");
                    on_slope += 1;
                }
            }
            assert!(rests(state.position.y - 0.9, top), "{heading}: {state:?}");
        }
        // The walker's 0.6 footprint spends 1.6 units each way over the
        // slope, of which the slope is its floor for most.
        assert!(on_slope > 20, "{heading}: {on_slope}");
    }
}

/// Walking toward a slope block's high end, a vertical face a cell tall,
/// with a step height of half a cell, the walker stops with its leading
/// edge a skin short of the face (4 cells along), as at a wall. Pressed
/// against the slope's low end from the other side, `find_ledge` finds no
/// ledge there: a slope to stand on is walked, not climbed. Oracle: the
/// requirement and geometry.
#[wasm_bindgen_test(unsupported = test)]
fn a_ramps_high_face_is_a_wall_and_its_slope_no_ledge() {
    let options = MoverOptions {
        step_height: 0.5,
        ..MoverOptions::default()
    };
    for heading in HEADINGS {
        let world = ramp_level(heading, 0.0, false);
        let mut state = on_ground_along(heading, 6.5);
        let mut wall = false;
        for _ in 0..60 {
            wall |= walk_step(&mut state, &world, heading * -4.0, &options).wall;
        }
        let t = state.position.dot(heading);
        assert!(wall, "{heading}");
        // Walking toward lower `t`, the leading edge is `t - 0.3` and the
        // face behind it at 4: the gap is `(t - 0.3) - 4`.
        assert!(against(4.0, t - 0.3), "{heading}: {state:?}");
        assert!(rests(state.position.y - 0.9, 0.0), "{heading}: {state:?}");

        let foot = on_ground_along(heading, 3.0 - 0.3 - SKIN);
        let found = find_ledge(
            &walker(),
            foot.position,
            heading,
            &world,
            &config(Vec3::Y),
            2.0,
            ALL_LAYERS,
        )
        .unwrap();
        assert_eq!(found, None, "{heading}");
    }
}

/// A slope block whose low end is a face a quarter of a cell tall, with the
/// platform behind it: with a step height of 0.3 the walker steps up onto
/// the slope (the climb ends on it: its floor is the slope block's) and
/// walks on up to the platform; with 0.2 it stops a skin short of the
/// face, 3 cells along. Oracle: the requirement (a step no taller than
/// `step_height` is climbed, and may end on a ramp) and geometry.
#[wasm_bindgen_test(unsupported = test)]
fn a_voxel_walker_steps_up_onto_a_ramps_low_end() {
    for heading in HEADINGS {
        for step_height in [0.3, 0.2] {
            let world = ramp_level(heading, 0.25, true);
            let options = MoverOptions {
                step_height,
                ..MoverOptions::default()
            };
            let mut state = on_ground_along(heading, 0.5);
            let mut stepped_onto = None;
            for _ in 0..90 {
                let contacts = walk_step(&mut state, &world, heading * 4.0, &options);
                if contacts.stepped && stepped_onto.is_none() {
                    stepped_onto = state.floor;
                }
            }
            let t = state.position.dot(heading);
            if step_height >= 0.25 {
                let floor = stepped_onto.expect("climbed the low end");
                assert!(same_direction(floor.normal, slope_normal(heading)));
                assert_eq!(floor.id, cell_id(cell_along(3, 0, heading)));
                assert!(t > 5.0 && rests(state.position.y - 0.9, 1.0), "{state:?}");
            } else {
                assert_eq!(stepped_onto, None, "{heading}");
                assert!(against(t + 0.3, 3.0), "{heading}: {state:?}");
            }
        }
    }
}

/// Guarded with a ledge drop of zero (it may not step off anything), the
/// walker walks up a slope block with nothing behind it and stops at its
/// top: the drop past it is a whole cell, so its trailing edge stops
/// within a skin short of the top edge, 4 cells along, every step short of
/// it. Walking back down, the ground falling away under it, a fifteenth of
/// a unit a step at 45°, is followed by the snap (0.1), so it is no ledge:
/// it walks down at its flat speed, grounded. Oracle: the requirement and
/// geometry.
#[wasm_bindgen_test(unsupported = test)]
fn a_guarded_voxel_walker_stops_at_a_ramps_top_and_walks_down_it() {
    let options = MoverOptions {
        ledge_drop: Some(0.0),
        ..MoverOptions::default()
    };
    for heading in HEADINGS {
        let world = ramp_level(heading, 0.0, false);
        let mut state = on_ground_along(heading, 0.5);
        for _ in 0..90 {
            walk_step(&mut state, &world, heading * 4.0, &options);
            assert!(
                state.position.dot(heading) - 0.3 < 4.0,
                "{heading}: {state:?}"
            );
        }
        let t = state.position.dot(heading);
        assert!(t - 0.3 >= 4.0 - SKIN, "{heading}: {state:?}");
        assert!(state.floor.is_some(), "{heading}");
        for _ in 0..30 {
            let before = state.position;
            let contacts = walk_step(&mut state, &world, heading * -4.0, &options);
            let moved = (state.position - before).dot(heading);
            assert!(close(moved, -4.0 * DT, 1e-5), "{heading}: {state:?}");
            assert!(
                state.floor.is_some() && !contacts.ledge_guarded,
                "{heading}"
            );
        }
    }
}

/// Whether a box at `at` of half-extents `half` shares interior with the
/// collider: the boxes overlap on every axis and the box's deepest corner
/// against the cut's normal lies below its plane. Plain geometry from the
/// cut's definition (solid where `normal · (p - centre) <= offset`), no
/// sweep maths.
fn inside_cut_box(at: Vec3, half: Vec3, collider: &Collider<Vec3>) -> bool {
    let boxes = overlap(at, half, collider.position, collider.shape.half_extents());
    let Some(cut) = collider.cut else {
        return boxes;
    };
    let n = cut.normal();
    let deepest = (at - collider.position).dot(n) - half.dot(n.abs());
    boxes && deepest < cut.offset()
}

/// A slope block rising a half cell over its cell along x (its solid part
/// below y = x / 2 across the cell, from (0, 0) to (1, 0.5): normal
/// (-1, 2, 0) / √5, offset -0.5 / √5) is placed on a walker standing on the
/// ground in that cell, its footprint over x 0.2 to 0.8. Its deepest
/// corner against the normal, (0.8, 0.01), lies (0.8 - 2·0.01) / √5 =
/// 0.78 / √5 inside the plane, so the shortest way out moves it along the
/// normal by that plus a skin, D = 0.78 / √5 + 0.01: by (-D, 2D, 0) / √5.
/// Lifting it straight up instead takes (0.8 + 0.01·√5) / 2 = 0.40118,
/// along x 0.8 or more, and over the block's top 0.99. One step later it
/// shares interior with nothing, by the cut's own definition, and has moved
/// that way alone. Oracle: geometry.
#[wasm_bindgen_test(unsupported = test)]
fn a_slope_block_placed_on_a_walker_moves_it_out_along_the_slopes_normal() {
    let placed = full(cell_id(IVec3::ZERO))
        .with_cut(Cut::new(Vec3::new(-1.0, 2.0, 0.0), -0.5 / 5f32.sqrt()));
    let world = cells(move |cell| {
        if ground(cell) {
            Some(full(cell_id(cell)))
        } else if cell == IVec3::ZERO {
            Some(placed)
        } else {
            None
        }
    });
    let start = Vec3::new(0.5, 0.9 + SKIN, 0.5);
    assert!(inside_cut_box(start, WALKER, &placed));
    let mut state = MoverState {
        position: start,
        ..MoverState::default()
    };
    let contacts = move_and_slide(
        &mut state,
        &walker(),
        DT,
        &world,
        &config(Vec3::Y),
        &MoverOptions::default(),
    )
    .unwrap();
    assert!(contacts.pushed_out && !contacts.stuck);
    let root5 = 5f32.sqrt();
    let depth = 0.78 / root5 + SKIN;
    let moved = state.position - start;
    assert!(close(moved.x, -depth / root5, 1e-5), "{moved}");
    assert!(close(moved.y, 2.0 * depth / root5, 1e-5), "{moved}");
    assert_eq!(moved.z.to_bits(), 0f32.to_bits());
    assert!(!inside_cut_box(state.position, WALKER, &placed));
}

// ------------------------------------------------------------ platformer

const STANDING: Vec2 = Vec2::new(0.4, 0.9);
const CROUCHED: Vec2 = Vec2::new(0.4, 0.45);

fn platform(set: &mut ColliderSet<Vec2>, id: u64, min: Vec2, max: Vec2, kind: ColliderKind) {
    set.insert(
        Collider::new(
            id,
            Shape::Box {
                half: (max - min) * 0.5,
            },
            (min + max) * 0.5,
        )
        .with_kind(kind),
    );
}

/// The platformer's level: a floor whose top is at y = 0 (id 1), a ceiling
/// whose underside is at y = 3 over x 2 to 4 (id 2), a wall whose face is
/// at x = 8 (id 3), and a one-way deck whose top is at y = 3 over x -6 to
/// -2 (id 4).
fn platformer() -> ColliderSet<Vec2> {
    let mut set = ColliderSet::new(2.0);
    let solid = ColliderKind::Solid;
    platform(
        &mut set,
        1,
        Vec2::new(-20.0, -1.0),
        Vec2::new(20.0, 0.0),
        solid,
    );
    platform(&mut set, 2, Vec2::new(2.0, 3.0), Vec2::new(4.0, 4.0), solid);
    platform(
        &mut set,
        3,
        Vec2::new(8.0, 0.0),
        Vec2::new(9.0, 10.0),
        solid,
    );
    let deck = ColliderKind::OneWay;
    platform(
        &mut set,
        4,
        Vec2::new(-6.0, 2.75),
        Vec2::new(-2.0, 3.0),
        deck,
    );
    set
}

fn standing_at(x: f32) -> MoverState<Vec2> {
    MoverState {
        position: Vec2::new(x, 0.9 + SKIN),
        velocity: Vec2::ZERO,
        floor: Some(Floor {
            normal: Vec2::Y,
            id: 1,
        }),
    }
}

/// One platformer step: hold `run` across, jump when asked and grounded,
/// fall under gravity, move.
fn run_step(
    state: &mut MoverState<Vec2>,
    world: &ColliderSet<Vec2>,
    half: Vec2,
    run: f32,
    jump: bool,
    options: &MoverOptions,
) -> Contacts<Vec2> {
    state.velocity.x = run;
    state.velocity.y = if jump && state.floor.is_some() {
        12.0
    } else {
        state.velocity.y - GRAVITY * DT
    };
    move_and_slide(
        state,
        &Shape::Box { half },
        DT,
        world,
        &config(Vec2::Y),
        options,
    )
    .expect("a set always answers")
}

/// Dropped from y = 5 the player lands a skin above the floor; jumping
/// under the ceiling (12 units a second would carry it 3.6 up) it bumps
/// its head, its top never passing a skin below y = 3, and the rise is
/// zeroed that step; jumping right into the wall it slides up and down the
/// face, never past it, the motion into the wall gone and the motion along
/// it kept. Oracle: the requirement and geometry.
#[wasm_bindgen_test(unsupported = test)]
fn a_platformer_lands_bumps_its_head_and_slides_along_a_wall() {
    let world = platformer();
    let options = MoverOptions::default();
    let mut state = MoverState {
        position: Vec2::new(0.0, 5.0),
        ..MoverState::default()
    };
    for _ in 0..90 {
        run_step(&mut state, &world, STANDING, 0.0, false, &options);
    }
    assert!(rests(state.position.y - 0.9, 0.0), "{state:?}");
    assert_eq!(state.floor.map(|f| f.id), Some(1));

    let mut state = standing_at(3.0);
    let mut bumped = false;
    for n in 0..60 {
        let contacts = run_step(&mut state, &world, STANDING, 0.0, n == 0, &options);
        assert!(state.position.y + 0.9 <= 3.0 - SKIN + 1e-4, "{state:?}");
        if contacts.ceiling {
            bumped = true;
            assert_eq!(state.velocity, Vec2::ZERO, "the bump zeroes the rise");
        }
    }
    assert!(bumped);

    let mut state = standing_at(6.5);
    let mut slid = 0;
    for n in 0..90 {
        let rise = state.velocity.y - GRAVITY * DT;
        let contacts = run_step(&mut state, &world, STANDING, 5.0, n == 0, &options);
        assert!(state.position.x + 0.4 <= 8.0 - SKIN + 1e-5, "{state:?}");
        if contacts.wall && state.floor.is_none() {
            slid += 1;
            let along = if n == 0 { 12.0 } else { rise };
            assert_eq!(state.velocity, Vec2::new(0.0, along));
        }
    }
    assert!(slid > 20, "slid along the wall for {slid} steps");
}

/// Under the one-way deck, a jump rises through it (12 units a second
/// lifts the feet 3.6, past its top at 3) and the fall lands on it. Held
/// down, `drop_through` drops the player through to the floor; held on the
/// solid floor it drops nowhere. Oracle: the requirement and geometry.
#[wasm_bindgen_test(unsupported = test)]
fn a_platformer_rises_through_a_deck_lands_on_it_and_drops_through_it() {
    let world = platformer();
    let options = MoverOptions::default();
    let mut state = standing_at(-4.0);
    for n in 0..90 {
        run_step(&mut state, &world, STANDING, 0.0, n == 0, &options);
    }
    assert_eq!(state.floor.map(|f| f.id), Some(4), "{state:?}");
    assert!(rests(state.position.y - 0.9, 3.0), "{state:?}");

    let dropping = MoverOptions {
        drop_through: true,
        ..MoverOptions::default()
    };
    for n in 0..90 {
        let held = if n < 20 { &dropping } else { &options };
        run_step(&mut state, &world, STANDING, 0.0, false, held);
    }
    assert_eq!(state.floor.map(|f| f.id), Some(1), "{state:?}");
    assert!(rests(state.position.y - 0.9, 0.0), "{state:?}");

    for _ in 0..30 {
        run_step(&mut state, &world, STANDING, 0.0, false, &dropping);
    }
    assert_eq!(state.floor.map(|f| f.id), Some(1));
    assert!(rests(state.position.y - 0.9, 0.0), "{state:?}");
}

/// A lintel over x 4 to 6 whose underside is at y = 1.2: the standing
/// player (1.8 tall) stops against it; crouched (0.9 tall) it walks under.
/// Crouch is held until x = 5, under the lintel; from there the player
/// stands as soon as `overlaps` finds room for the taller shape, which is
/// once its left edge has passed x = 6. Oracle: the requirement and
/// geometry.
#[wasm_bindgen_test(unsupported = test)]
fn a_platformer_fits_under_a_lintel_only_crouched() {
    let mut world = ColliderSet::new(2.0);
    let solid = ColliderKind::Solid;
    platform(
        &mut world,
        1,
        Vec2::new(-20.0, -1.0),
        Vec2::new(20.0, 0.0),
        solid,
    );
    platform(
        &mut world,
        2,
        Vec2::new(4.0, 1.2),
        Vec2::new(6.0, 5.0),
        solid,
    );
    let options = MoverOptions::default();

    let mut state = standing_at(2.0);
    for _ in 0..90 {
        run_step(&mut state, &world, STANDING, 4.0, false, &options);
    }
    assert!(against(state.position.x + 0.4, 4.0), "{state:?}");

    let mut state = standing_at(2.0);
    state.position.y -= STANDING.y - CROUCHED.y;
    let mut crouched = true;
    for _ in 0..90 {
        let half = if crouched { CROUCHED } else { STANDING };
        run_step(&mut state, &world, half, 4.0, false, &options);
        let held = state.position.x < 5.0;
        if crouched && !held {
            let up = state.position + Vec2::Y * (STANDING.y - CROUCHED.y);
            let room = overlaps(&Shape::Box { half: STANDING }, up, &world, ALL_LAYERS).unwrap();
            if room.is_empty() {
                crouched = false;
                state.position = up;
                assert!(state.position.x - 0.4 >= 6.0, "{state:?}");
            }
        }
        let half = if crouched { CROUCHED } else { STANDING };
        for c in [Vec2::new(5.0, 3.1), Vec2::new(0.0, -0.5)] {
            let size = if c.y > 0.0 {
                Vec2::new(1.0, 1.9)
            } else {
                Vec2::new(20.0, 0.5)
            };
            assert!(!overlap(state.position, half, c, size), "{state:?}");
        }
    }
    assert!(!crouched && state.position.x > 7.5, "{state:?}");
}

/// A wall whose face is at x = 5 and top at y = 3.5. Running at it from
/// x = 3.8 and jumping, the player is pressed against it a fifth of a
/// second later, its top at about 3.8, within reach (0.6) of the ledge, and
/// asks `find_ledge` each step it is against the wall; it finds the ledge,
/// moves to where it would stand, stops running, and lands on it. With an
/// overhang from y = 4 starting 0.2 past the face (too tall to have a
/// ledge of its own in reach), it never finds one. Oracle: the
/// requirement and geometry.
#[wasm_bindgen_test(unsupported = test)]
fn a_platformer_climbs_a_ledge_and_refuses_one_under_an_overhang() {
    for covered in [false, true] {
        let mut world = ColliderSet::new(2.0);
        let solid = ColliderKind::Solid;
        platform(
            &mut world,
            1,
            Vec2::new(-20.0, -1.0),
            Vec2::new(20.0, 0.0),
            solid,
        );
        platform(
            &mut world,
            2,
            Vec2::new(5.0, 0.0),
            Vec2::new(9.0, 3.5),
            solid,
        );
        if covered {
            platform(
                &mut world,
                3,
                Vec2::new(5.2, 4.0),
                Vec2::new(9.0, 20.0),
                solid,
            );
        }
        let config = config(Vec2::Y);
        let options = MoverOptions::default();
        let shape = Shape::Box { half: STANDING };
        let mut state = standing_at(3.8);
        let mut climbed = false;
        for n in 0..90 {
            let run = if climbed { 0.0 } else { 4.0 };
            let contacts = run_step(&mut state, &world, STANDING, run, n == 0, &options);
            if climbed || !contacts.wall {
                continue;
            }
            let found = find_ledge(
                &shape,
                state.position,
                Vec2::X,
                &world,
                &config,
                0.6,
                ALL_LAYERS,
            )
            .unwrap();
            if let Some(ledge) = found {
                assert_eq!(ledge.id, 2);
                state = MoverState {
                    position: ledge.stand,
                    ..MoverState::default()
                };
                climbed = true;
            }
        }
        assert_eq!(climbed, !covered);
        if climbed {
            assert_eq!(state.floor.map(|f| f.id), Some(2), "{state:?}");
            assert!(rests(state.position.y - 0.9, 3.5), "{state:?}");
        }
    }
}
