//! Audit of 3D surfaces (#924, physics.md 22), written from the contract:
//! rays against triangles by an exact crossing worked in `f64`,
//! `ground_under` on every world type and on pairs against the surfaces'
//! own heights, height fields against their triangles' planes, the
//! refusals, duplicate, degenerate and enormous triangles, and a surface
//! world that refuses under the mover. Expected values never come from
//! the crate. Runs natively and in a wasm runtime.
#![allow(
    clippy::single_match_else,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::float_cmp,
    clippy::many_single_char_names,
    clippy::similar_names,
    clippy::too_many_lines,
    clippy::type_complexity
)]

mod audit_support;

use audit_support::solid::{Q, add, cross, dot, length, q, scale, sub};
use audit_support::{Rng, config};
use sgl_core::math::{IVec3, Quat, Vec3};
use sgl_physics::{
    ALL_LAYERS, CellColliders, CellWorld, Collider, ColliderSet, Convex, Cut, Floor, HeightField,
    MAX_HEIGHT_SAMPLES, MoverOptions, MoverState, Shape, SurfaceError, TriangleSet, Unanswered,
    World, ground_under, move_and_slide, overlaps, ray,
};
use wasm_bindgen_test::wasm_bindgen_test;

fn v3(x: f32, y: f32, z: f32) -> Vec3 {
    Vec3::new(x, y, z)
}

/// Where the ray from `from` by `delta` crosses the plane of the triangle
/// `a b c`, as the fraction along it, with the triangle's unit normal and
/// how far inside the triangle the crossing lies (its distance to the
/// nearest edge, negative outside). `None` when the ray is parallel to the
/// plane. Exact geometry in `f64`.
fn crossing(from: Q, delta: Q, [a, b, c]: [Q; 3]) -> Option<(f64, Q, f64)> {
    let n = cross(sub(b, a), sub(c, a));
    let n = scale(n, 1.0 / length(n));
    let along = dot(n, delta);
    if along == 0.0 {
        return None;
    }
    let t = dot(n, sub(a, from)) / along;
    let point = add(from, scale(delta, t));
    let inside = [(a, b), (b, c), (c, a)]
        .iter()
        .map(|&(u, v)| {
            let edge = sub(v, u);
            dot(cross(edge, sub(point, u)), n) / length(edge)
        })
        .fold(f64::INFINITY, f64::min);
    Some((t, n, inside))
}

/// A random triangle of size `size` about `centre`, its corners at least a
/// fifth of `size` from the line through the other two.
fn triangle(rng: &mut Rng, centre: Vec3, size: f32) -> [Vec3; 3] {
    loop {
        let corners: [Vec3; 3] = [0; 3].map(|_| centre + rng.vector::<Vec3>(-size, size));
        let [a, b, c] = corners.map(q);
        let area = length(cross(sub(b, a), sub(c, a)));
        let longest = [length(sub(b, a)), length(sub(c, b)), length(sub(a, c))]
            .into_iter()
            .fold(0.0, f64::max);
        if area / longest > 0.2 * f64::from(size) {
            return corners;
        }
    }
}

fn set_of(triangles: &[[Vec3; 3]], ids: &[u64]) -> TriangleSet {
    let positions: Vec<Vec3> = triangles.iter().flatten().copied().collect();
    let indices: Vec<u32> = (0..positions.len() as u32).collect();
    TriangleSet::new(&positions, &indices, ids).unwrap()
}

/// Defect: a ray that meets a triangle away from its plane (a kernel slop,
/// a crossing computed in world coordinates far out), misses one it
/// crosses, meets one it passes, or reports another normal. Oracle: the
/// exact crossing of the ray and the triangle's plane in `f64`, and
/// physics.md 15, Rays (exact against a triangle: `t` and its point to a
/// few units in the last place of the coordinates involved; the normal the
/// triangle's own, turned against the ray) and 22 (a triangle in a
/// `TriangleSet` is met as the convex collider of its corners). Seeded
/// triangles of sizes from a tenth to twenty units up to fifty units out,
/// and rays from every side, through a set of the one triangle and a slice
/// of its hull; rays within a thousandth of the triangle's size of an
/// edge, or within three degrees of its plane, are left out, as the
/// crossing there is ill-conditioned or either answer is right.
#[wasm_bindgen_test(unsupported = test)]
fn a_ray_meets_a_triangle_exactly_where_it_crosses_it() {
    let mut rng = Rng::new(0x0924_2201);
    let (mut hits, mut misses) = (0, 0);
    for case in 0..400 {
        let size = rng.range(0.1, 20.0);
        let centre: Vec3 = rng.vector(-50.0, 50.0);
        let corners = triangle(&mut rng, centre, size);
        let from = centre + rng.vector::<Vec3>(-3.0 * size, 3.0 * size);
        // At the triangle's plane: within it, or on one case in four a
        // little beside it.
        let (mut u, mut w) = (rng.range(0.0, 1.0), rng.range(0.0, 1.0));
        if u + w > 1.0 {
            (u, w) = (1.0 - u, 1.0 - w);
        }
        if rng.chance(25) {
            u -= 0.3;
        }
        let aim = corners[0] + (corners[1] - corners[0]) * u + (corners[2] - corners[0]) * w;
        let delta = (aim - from) * rng.range(0.8, 2.0);
        let Some((t, normal, inside)) = crossing(q(from), q(delta), corners.map(q)) else {
            continue;
        };
        let sine = dot(normal, q(delta)).abs() / length(q(delta));
        if sine < 0.05 || inside.abs() < 1e-3 * f64::from(size) {
            continue;
        }
        let met = inside > 0.0 && (0.0..=1.0).contains(&t);
        let facing = if dot(normal, q(delta)) > 0.0 {
            scale(normal, -1.0)
        } else {
            normal
        };
        let hull = Collider::new(
            7,
            Shape::Convex(Convex::hull(&corners.map(|p| p - centre), Quat::IDENTITY)),
            centre,
        );
        let set = set_of(&[corners], &[7]);
        let slice = [hull];
        let worlds: [&dyn World<Vec3>; 2] = [&set, &&slice[..]];
        for world in worlds {
            let found = ray(from, delta, world, ALL_LAYERS).unwrap();
            let why = format!("case {case}: {corners:?} from {from} by {delta}: {found:?}, t {t}");
            match found {
                Some(hit) => {
                    assert!(met, "{why}");
                    // A few units in the last place of the coordinates.
                    let reach =
                        length(sub(q(centre), q(from))) + 2.0 * f64::from(size) + length(q(delta));
                    let off = (f64::from(hit.t) - t).abs() * length(q(delta));
                    assert!(
                        off <= 32.0 * f64::from(f32::EPSILON) * reach,
                        "{why}: {off} off"
                    );
                    assert!(length(sub(q(hit.normal), facing)) < 1e-5, "{why}");
                    assert_eq!(hit.id, 7, "{why}");
                    hits += 1;
                }
                None => {
                    assert!(!met, "{why}");
                    misses += 1;
                }
            }
        }
    }
    assert!(
        hits >= 300 && misses >= 150,
        "{hits} hits, {misses} misses (two worlds a case)"
    );
}

/// The height of the plane through `a b c` at `(x, z)`, its upward unit
/// normal, and whether `(x, z)` lies within the triangle seen from above
/// by more than `margin`.
fn top_of_triangle([a, b, c]: [Q; 3], x: f64, z: f64, margin: f64) -> Option<(f64, Q)> {
    let mut n = cross(sub(b, a), sub(c, a));
    if n[1] < 0.0 {
        n = scale(n, -1.0);
    }
    if n[1] == 0.0 {
        return None;
    }
    let flat = |p: Q| (p[0], p[2]);
    let (pa, pb, pc) = (flat(a), flat(b), flat(c));
    let side = |u: (f64, f64), v: (f64, f64)| {
        let edge = (v.0 - u.0, v.1 - u.1);
        let l = (edge.0 * edge.0 + edge.1 * edge.1).sqrt();
        (edge.0 * (z - u.1) - edge.1 * (x - u.0)) / l
    };
    let sides = [side(pa, pb), side(pb, pc), side(pc, pa)];
    let inside = sides.iter().all(|&s| s > margin) || sides.iter().all(|&s| s < -margin);
    if !inside {
        return None;
    }
    let y = a[1] - (n[0] * (x - a[0]) + n[2] * (z - a[2])) / n[1];
    Some((y, scale(n, 1.0 / length(n))))
}

/// What the ground is at `(x, z)` by the surfaces' definitions: the
/// highest top among `tops`, each the height and normal of one surface
/// there (or `None` where it has none), and its id.
type Top = Box<dyn Fn(f64, f64) -> Option<(f64, Q)>>;

/// Defect: a `ground_under` that reads one world type wrong: a cell world
/// or a height field offering too little, a pair that answers the lower of
/// its worlds or only its first, a round collider's top off its sphere.
/// Oracle: physics.md 22 (`ground_under` is the highest surface on the
/// mask straight down within the window, through any `World<Vec3>`, exact
/// on boxes, ramps, balls, capsules and triangles, within the contact band
/// on a hull of four or more points) and the surfaces' own heights in
/// `f64`. A slice and a set of a box, a ramp, a ball, a capsule, a
/// triangle and a tetrahedron; a cell world of blocks; a triangle set; a
/// height field; and the pair of a deck's triangles and the height field
/// under it, in both orders. Points over each, away from its edges.
#[wasm_bindgen_test(unsupported = test)]
fn ground_under_reads_every_world_where_its_surfaces_are() {
    let mut rng = Rng::new(0x0924_2202);
    let tri = [v3(-1.0, 0.0, -1.0), v3(1.0, 0.5, -1.0), v3(0.0, 1.0, 1.5)];
    let tetra = [
        v3(-1.0, 0.0, -1.0),
        v3(1.2, 0.2, -0.8),
        v3(0.0, 0.1, 1.3),
        v3(0.1, 1.4, 0.0),
    ];
    let shapes = [
        Collider::new(
            1,
            Shape::Box {
                half: v3(2.0, 0.5, 2.0),
            },
            v3(0.0, 0.0, 0.0),
        ),
        Collider::new(
            2,
            Shape::Box {
                half: v3(2.0, 1.0, 2.0),
            },
            v3(6.0, 0.0, 0.0),
        )
        .with_cut(Cut::new(v3(-1.0, 1.0, 0.0), 0.0)),
        Collider::new(3, Shape::Ball { radius: 1.5 }, v3(0.0, 0.0, 6.0)),
        Collider::new(
            4,
            Shape::Capsule {
                half_axis: v3(1.5, 0.0, 0.0),
                radius: 0.8,
            },
            v3(6.0, 0.0, 6.0),
        ),
        Collider::new(
            5,
            Shape::Convex(Convex::hull(&tri, Quat::IDENTITY)),
            v3(-6.0, 0.0, 0.0),
        ),
        Collider::new(
            6,
            Shape::Convex(Convex::hull(&tetra, Quat::IDENTITY)),
            v3(-6.0, 0.0, 6.0),
        ),
    ];
    // Each shape's top by its definition, with the margin from its edges.
    let tops: Vec<(Top, (f32, f32, f32, f32), f64)> = vec![
        (
            Box::new(|x, z| (x.abs() < 1.99 && z.abs() < 1.99).then_some((0.5, [0.0, 1.0, 0.0]))),
            (-2.0, 2.0, -2.0, 2.0),
            1e-5,
        ),
        (
            // Solid where y - (x - 6) <= 0 (normal (-1, 1, 0) / sqrt 2), so
            // its top is y = min(x - 6, 1) for x - 6 in -1..2.
            Box::new(|x, z| {
                let u = x - 6.0;
                (u > -0.99 && u < 1.99 && z.abs() < 1.99).then(|| {
                    if u < 1.0 {
                        (u, [-(0.5f64.sqrt()), 0.5f64.sqrt(), 0.0])
                    } else {
                        (1.0, [0.0, 1.0, 0.0])
                    }
                })
            }),
            (4.0, 8.0, -2.0, 2.0),
            1e-5,
        ),
        (
            Box::new(|x, z| {
                let r2 = 2.25 - x * x - (z - 6.0) * (z - 6.0);
                (r2 > 0.01).then(|| {
                    let y = r2.sqrt();
                    (y, scale([x, y, z - 6.0], 1.0 / 1.5))
                })
            }),
            (-1.5, 1.5, 4.5, 7.5),
            1e-5,
        ),
        (
            Box::new(|x, z| {
                let along = ((x - 6.0).abs() - 1.5).max(0.0) * (x - 6.0).signum();
                let r2 = 0.64 - along * along - (z - 6.0) * (z - 6.0);
                (r2 > 0.01).then(|| {
                    let y = r2.sqrt();
                    (y, scale([along, y, z - 6.0], 1.0 / 0.8))
                })
            }),
            (3.7, 8.3, 5.2, 6.8),
            1e-5,
        ),
        (
            Box::new(move |x, z| {
                top_of_triangle(tri.map(|p| q(p + v3(-6.0, 0.0, 0.0))), x, z, 1e-3)
            }),
            (-7.0, -5.0, -1.0, 1.5),
            1e-5,
        ),
        (
            Box::new(move |x, z| {
                let p = tetra.map(|p| q(p + v3(-6.0, 0.0, 6.0)));
                [[0, 1, 2], [0, 1, 3], [1, 2, 3], [0, 2, 3]]
                    .iter()
                    .filter_map(|f| top_of_triangle([p[f[0]], p[f[1]], p[f[2]]], x, z, 1e-3))
                    .max_by(|a, b| a.0.total_cmp(&b.0))
            }),
            (-7.0, -4.8, 5.0, 7.3),
            // A hull of four points is met through the kernel, within its
            // band: 1.25 slops of a hundredth of its extent.
            1.25 * 0.014 + 1e-5,
        ),
    ];
    let mut set = ColliderSet::new(2.0);
    for c in &shapes {
        set.insert(*c);
    }
    let worlds: [&dyn World<Vec3>; 2] = [&&shapes[..], &set];
    let mut checked = 0;
    for world in worlds {
        for (index, (top, (x0, x1, z0, z1), tolerance)) in tops.iter().enumerate() {
            for _ in 0..30 {
                let (x, z) = (rng.range(*x0, *x1), rng.range(*z0, *z1));
                let Some((height, normal)) = top(f64::from(x), f64::from(z)) else {
                    continue;
                };
                let point = v3(x, height as f32 + rng.range(0.1, 1.0), z);
                let found = ground_under(point, 0.5, 3.0, world, ALL_LAYERS)
                    .unwrap()
                    .expect("the surface is there");
                let why = format!("shape {index} at {point}: {found:?}, height {height}");
                assert_eq!(found.id, shapes[index].id, "{why}");
                let off = f64::from(found.height) - height;
                if *tolerance < 1e-4 {
                    assert!(off.abs() <= *tolerance, "{why}");
                    assert!(length(sub(q(found.normal), normal)) < 1e-4, "{why}");
                } else {
                    // The band is a distance from the surface: straight
                    // down it is that over the normal's upward part. Never
                    // below the surface.
                    assert!(off >= -1e-5 && off <= *tolerance / normal[1], "{why}");
                }
                checked += 1;
            }
        }
    }
    // A cell world of unit blocks whose top is y = 0, under x and z in
    // 10..14.
    let cells = CellWorld::new(1.0, |cell: IVec3, out: &mut CellColliders<'_, Vec3>| {
        if cell.y == -1 && (10..14).contains(&cell.x) && (10..14).contains(&cell.z) {
            out.push(Collider::new(
                9,
                Shape::Box {
                    half: Vec3::splat(0.5),
                },
                Vec3::splat(0.5),
            ));
        }
    });
    // A height field over x 20..30 and z 0..10 on the plane y = 0.1 x +
    // 0.2 z - 3, and a deck of two triangles at y = 5 over x 22..26.
    let mut heights = Vec::new();
    for row in 0..11 {
        for column in 0..11 {
            heights.push(0.1 * (20.0 + column as f32) + 0.2 * row as f32 - 3.0);
        }
    }
    let field = HeightField::new(&heights, 11, 11, 1.0, Vec3::new(0.0, 0.0, 0.0), &[11]).unwrap();
    // The samples sit at x = column, so the plane is 0.1 (x + 20) there:
    // the field spans x 0..10 and z 0..10.
    let deck = set_of(
        &[
            [v3(2.0, 5.0, 2.0), v3(6.0, 5.0, 2.0), v3(6.0, 5.0, 6.0)],
            [v3(2.0, 5.0, 2.0), v3(6.0, 5.0, 6.0), v3(2.0, 5.0, 6.0)],
        ],
        &[12],
    );
    let plane = |x: f64, z: f64| 0.1 * (x + 20.0) + 0.2 * z - 3.0;
    let slope = {
        let n = [-0.1, 1.0, -0.2];
        scale(n, 1.0 / length(n))
    };
    for _ in 0..40 {
        let (x, z) = (rng.range(10.05, 13.95), rng.range(10.05, 13.95));
        let found = ground_under(v3(x, 0.5, z), 1.0, 1.0, &cells, ALL_LAYERS)
            .unwrap()
            .expect("the blocks are there");
        assert_eq!(
            (found.height, found.normal, found.id),
            (0.0, Vec3::Y, 9),
            "{x} {z}"
        );
        // On the field, clear of the deck's footprint and its edges.
        let (x, z) = (rng.range(0.05, 9.95), rng.range(0.05, 9.95));
        let height = plane(f64::from(x), f64::from(z));
        let on_deck = (2.0..=6.0).contains(&x) && (2.0..=6.0).contains(&z);
        for world in [
            &field as &dyn World<Vec3>,
            &(&deck, &field),
            &(&field, &deck),
        ] {
            // From under the deck, its height out of reach: the field.
            let point = v3(x, height as f32 + 0.5, z);
            let found = ground_under(point, 1.0, 2.0, world, ALL_LAYERS)
                .unwrap()
                .unwrap();
            let why = format!("at {point}: {found:?}, field at {height}");
            assert_eq!(found.id, 11, "{why}");
            assert!((f64::from(found.height) - height).abs() < 1e-5, "{why}");
            assert!(length(sub(q(found.normal), slope)) < 1e-5, "{why}");
            checked += 1;
        }
        if on_deck && (2.05..=5.95).contains(&x) && (2.05..=5.95).contains(&z) {
            // From above the deck: the deck, in either order.
            let point = v3(x, 6.0, z);
            for world in [&(&deck, &field) as &dyn World<Vec3>, &(&field, &deck)] {
                let found = ground_under(point, 1.0, 20.0, world, ALL_LAYERS)
                    .unwrap()
                    .unwrap();
                assert_eq!((found.height, found.id), (5.0, 12), "{point}: {found:?}");
            }
        }
    }
    assert!(checked >= 300, "{checked} checked");
}

/// The height of the field cell triangle under `(u, v)` within its cell,
/// by its definition (physics.md 22: each cell two triangles split from
/// its `(column + 1, row)` corner to its `(column, row + 1)` corner), with
/// the triangle's upward unit normal.
fn field_top(heights: &[f32], columns: usize, cell: f64, x: f64, z: f64) -> (f64, Q) {
    let (column, row) = ((x / cell).floor() as usize, (z / cell).floor() as usize);
    let (u, v) = (x / cell - column as f64, z / cell - row as f64);
    let corner = |c: usize, r: usize| {
        [
            c as f64 * cell,
            f64::from(heights[r * columns + c]),
            r as f64 * cell,
        ]
    };
    let (c11, c12, c21, c22) = (
        corner(column, row),
        corner(column + 1, row),
        corner(column, row + 1),
        corner(column + 1, row + 1),
    );
    let triangle = if u + v <= 1.0 {
        [c11, c12, c21]
    } else {
        [c12, c22, c21]
    };
    top_of_triangle(triangle, x, z, -1.0).expect("inside its cell")
}

/// Defect: a height field whose ground is not its triangles' planes (the
/// split the wrong way, a sample read from the wrong row), that answers
/// outside its grid, or that accepts a grid it cannot hold. Oracle:
/// physics.md 22 (sample `(column, row)` is `heights[row * columns +
/// column]`; each cell two triangles split from `(column + 1, row)` to
/// `(column, row + 1)`; outside the grid there is no ground; it refuses
/// non-finite numbers, a cell that is not positive, a sample count other
/// than `columns * rows`, and more than `MAX_HEIGHT_SAMPLES`) and the
/// planes of the triangles in `f64`. A field of one cell and one of five
/// by four samples with random heights, read at random points away from
/// the cells' diagonals and edges, and every refusal, a sample count that
/// overflows `usize` and one past the cap included.
#[wasm_bindgen_test(unsupported = test)]
fn a_height_field_is_its_triangles_and_refuses_what_it_cannot_hold() {
    let mut rng = Rng::new(0x0924_2203);
    for (columns, rows) in [(2usize, 2usize), (5, 4)] {
        let heights: Vec<f32> = (0..columns * rows).map(|_| rng.range(-2.0, 2.0)).collect();
        let cell = 1.5;
        let field = HeightField::new(&heights, columns, rows, cell, Vec3::ZERO, &[4]).unwrap();
        let mut read = 0;
        while read < 60 {
            let x = rng.range(0.0, (columns - 1) as f32 * cell);
            let z = rng.range(0.0, (rows - 1) as f32 * cell);
            let (u, v) = (x / cell, z / cell);
            let (fu, fv) = (u - u.floor(), v - v.floor());
            if !(0.01..=0.99).contains(&fu)
                || !(0.01..=0.99).contains(&fv)
                || (fu + fv - 1.0).abs() < 0.01
            {
                continue;
            }
            read += 1;
            let (height, normal) = field_top(
                &heights,
                columns,
                f64::from(cell),
                f64::from(x),
                f64::from(z),
            );
            let found = ground_under(v3(x, 3.0, z), 0.0, 10.0, &field, ALL_LAYERS)
                .unwrap()
                .expect("the field is there");
            let why = format!("{columns}x{rows} at ({x}, {z}): {found:?}, plane at {height}");
            assert!((f64::from(found.height) - height).abs() < 1e-5, "{why}");
            assert!(length(sub(q(found.normal), normal)) < 1e-5, "{why}");
        }
        let past = (columns - 1) as f32 * cell + 0.01;
        for (x, z) in [
            (-0.01, 0.5),
            (past, 0.5),
            (0.5, -0.01),
            (0.5, (rows - 1) as f32 * cell + 0.01),
        ] {
            assert_eq!(
                ground_under(v3(x, 3.0, z), 0.0, 10.0, &field, ALL_LAYERS).unwrap(),
                None
            );
        }
    }
    let four = [0.0; 4];
    let refused = |heights: &[f32], columns, rows, cell, origin, ids: &[u64]| {
        HeightField::new(heights, columns, rows, cell, origin, ids).err()
    };
    assert_eq!(
        refused(&four, usize::MAX, 2, 1.0, Vec3::ZERO, &[1]),
        Some(SurfaceError::Grid)
    );
    assert_eq!(
        refused(&four, 2, usize::MAX / 2 + 1, 1.0, Vec3::ZERO, &[1]),
        Some(SurfaceError::Grid)
    );
    assert_eq!(
        refused(&four, 2, 2, -1.0, Vec3::ZERO, &[1]),
        Some(SurfaceError::Grid)
    );
    assert_eq!(
        refused(&four, 2, 2, f32::NAN, Vec3::ZERO, &[1]),
        Some(SurfaceError::NotFinite)
    );
    assert_eq!(
        refused(&[0.0, f32::INFINITY, 0.0, 0.0], 2, 2, 1.0, Vec3::ZERO, &[1]),
        Some(SurfaceError::NotFinite)
    );
    // Finite heights whose samples `f32` cannot place.
    assert_eq!(
        refused(&[3e38; 4], 2, 2, 1.0, v3(0.0, 3e38, 0.0), &[1]),
        Some(SurfaceError::NotFinite)
    );
    let side = 2049;
    let many = vec![0.0f32; side * side];
    assert!(side * side > MAX_HEIGHT_SAMPLES);
    assert_eq!(
        refused(&many, side, side, 1.0, Vec3::ZERO, &[1]),
        Some(SurfaceError::TooMany)
    );
}

/// Defect: a triangle given twice that is met twice or by its later id, a
/// degenerate one that is kept, or one so large it is kept and never met.
/// Oracle: physics.md 6 (a list holds each collider once; ties go to the
/// lowest id) and 22 (a triangle with no area to `f32`, or whose edges
/// overflow, is dropped and counted; every triangle kept is met by a ray
/// exactly). One triangle given with ids 5, 3 and 9 and wound three ways
/// is the ground with id 3 and is listed once per id; the same triangle
/// with one id, given twice, is listed once; triangles in a line or at one
/// point are dropped and counted; a triangle two hundred quintillion units
/// across is either dropped and counted or the ground under a point on it.
#[wasm_bindgen_test(unsupported = test)]
fn duplicate_degenerate_and_enormous_triangles_are_met_as_the_contract_says() {
    let p = [v3(0.0, 0.0, 0.0), v3(1.0, 0.0, 0.0), v3(0.0, 0.0, 1.0)];
    let set = TriangleSet::new(&p, &[0, 1, 2, 0, 2, 1, 1, 2, 0], &[5, 3, 9]).unwrap();
    let ground = ground_under(v3(0.2, 1.0, 0.2), 0.0, 2.0, &set, ALL_LAYERS)
        .unwrap()
        .unwrap();
    assert_eq!((ground.height, ground.id, ground.normal), (0.0, 3, Vec3::Y));
    let listed: Vec<u64> = overlaps(
        &Shape::Ball { radius: 0.5 },
        v3(0.2, 0.0, 0.2),
        &set,
        ALL_LAYERS,
    )
    .unwrap()
    .iter()
    .map(|c| c.id)
    .collect();
    assert_eq!(listed, [3, 5, 9]);
    let twice = TriangleSet::new(&p, &[0, 1, 2, 2, 0, 1], &[4]).unwrap();
    let listed = overlaps(
        &Shape::Ball { radius: 0.5 },
        v3(0.2, 0.0, 0.2),
        &twice,
        ALL_LAYERS,
    )
    .unwrap();
    assert_eq!(listed.len(), 1, "{listed:?}");
    let line = [
        v3(0.0, 0.0, 0.0),
        v3(1.0, 1.0, 1.0),
        v3(2.0, 2.0, 2.0),
        v3(5.0, 5.0, 5.0),
    ];
    let flat = TriangleSet::new(&line, &[0, 1, 2, 0, 0, 0, 1, 2, 3, 0, 1, 3], &[1]).unwrap();
    assert_eq!((flat.len(), flat.degenerate()), (0, 4));
    let huge = [
        v3(-1e20, 0.0, -1e20),
        v3(1e20, 0.0, -1e20),
        v3(0.0, 0.0, 1e20),
    ];
    let set = TriangleSet::new(&huge, &[0, 1, 2], &[1]).unwrap();
    if set.degenerate() == 0 {
        let ground = ground_under(v3(3.0, 1.0, 2.0), 5.0, 5.0, &set, ALL_LAYERS).unwrap();
        assert!(ground.is_some(), "a kept triangle is never met");
    } else {
        assert_eq!(set.len(), 0);
    }
}

/// A surface world that answers as `inner` does and then refuses, from its
/// `refuse_at`-th query on: what any world may do (physics.md 10).
struct Refusing<'a, W> {
    inner: &'a W,
    refuse_at: usize,
    asked: std::cell::Cell<usize>,
}

impl<W: World<Vec3>> Refusing<'_, W> {
    fn refuse(&self) -> bool {
        let n = self.asked.get();
        self.asked.set(n + 1);
        n >= self.refuse_at
    }
}

impl<W: World<Vec3>> World<Vec3> for Refusing<'_, W> {
    fn colliders_near(
        &self,
        min: Vec3,
        max: Vec3,
        visit: &mut dyn FnMut(&Collider<Vec3>),
    ) -> Result<(), Unanswered> {
        if self.refuse() {
            return Err(Unanswered);
        }
        self.inner.colliders_near(min, max, visit)
    }

    fn colliders_along(
        &self,
        min: Vec3,
        max: Vec3,
        motion: Vec3,
        visit: &mut dyn FnMut(&Collider<Vec3>),
    ) -> Result<(), Unanswered> {
        if self.refuse() {
            return Err(Unanswered);
        }
        self.inner.colliders_along(min, max, motion, visit)
    }
}

/// Defect: a mover step over a surface world that keeps part of its work
/// when a later query is refused, or one that returns a non-finite state.
/// Oracle: physics.md 22 (given a surface world, every step returns finite
/// state or is `Unanswered` and leaves the state as it was) and 10 (any
/// world may refuse). A ball and a capsule walking over a bumpy height
/// field and a ramp of triangles, with a step height and a ledge guard,
/// in a world that refuses its k-th query, for every k until the step
/// completes: each refusal leaves the state bit-equal, and the step that
/// completes is finite and answers as the world that never refuses.
#[wasm_bindgen_test(unsupported = test)]
fn a_surface_world_that_refuses_leaves_the_mover_as_it_was() {
    let mut rng = Rng::new(0x0924_2204);
    let heights: Vec<f32> = (0..36).map(|_| rng.range(-0.3, 0.3)).collect();
    let field = HeightField::new(&heights, 6, 6, 1.0, v3(-2.5, 0.0, -2.5), &[1]).unwrap();
    let ramp = set_of(
        &[
            [v3(0.0, 0.0, -2.0), v3(2.0, 1.0, -2.0), v3(2.0, 1.0, 2.0)],
            [v3(0.0, 0.0, -2.0), v3(2.0, 1.0, 2.0), v3(0.0, 0.0, 2.0)],
        ],
        &[2],
    );
    let world = (&field, &ramp);
    let config = config(Vec3::Y);
    let options = MoverOptions {
        step_height: 0.3,
        ledge_drop: Some(0.5),
        ..MoverOptions::default()
    };
    let bodies = [
        Shape::Ball { radius: 0.4 },
        Shape::Capsule {
            half_axis: v3(0.0, 0.5, 0.0),
            radius: 0.3,
        },
    ];
    for body in bodies {
        let mut state = MoverState {
            position: v3(-1.5, 1.5, 0.3),
            velocity: v3(3.0, 0.0, 0.5),
            floor: Some(Floor {
                normal: Vec3::Y,
                id: 1,
            }),
        };
        for step in 0..20 {
            state.velocity.y -= 10.0 / 60.0;
            let start = state;
            let mut plain = start;
            let expected = move_and_slide(&mut plain, &body, 1.0 / 60.0, &world, &config, &options);
            let mut answered = false;
            for k in 0..400 {
                let refusing = Refusing {
                    inner: &world,
                    refuse_at: k,
                    asked: std::cell::Cell::new(0),
                };
                let mut trial = start;
                match move_and_slide(&mut trial, &body, 1.0 / 60.0, &refusing, &config, &options) {
                    Err(_) => assert_eq!(
                        format!("{trial:?}"),
                        format!("{start:?}"),
                        "step {step}, refused at {k}"
                    ),
                    Ok(contacts) => {
                        assert_eq!(Ok(contacts), expected, "step {step}");
                        assert_eq!(trial, plain, "step {step}");
                        answered = true;
                        break;
                    }
                }
            }
            assert!(answered, "step {step} never completed");
            assert!(
                plain.position.is_finite() && plain.velocity.is_finite(),
                "{plain:?}"
            );
            state = plain;
        }
    }
}
