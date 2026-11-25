//! Surfaces and sight (#923): `TriangleSet`, `HeightField` and
//! `ground_under`. Each test states its oracle: hand-worked or analytic
//! geometry, the crate's own box and cut-box colliders (an independent
//! source of the same shape, met in closed form), or a requirement of
//! `specs/physics.md` ("Surfaces"). A ray and `ground_under` meet a
//! triangle exactly, to a few units in the last place of the coordinates
//! involved; a ball cast meets it through the kernel, within its contact
//! band: between one and one and a quarter slops short of it, the slop a
//! hundredth of the smaller shape's extent.
#![allow(clippy::cast_precision_loss)]

use sgl_core::math::Vec3;
use sgl_core::{CanonicalWrite, SplitMix64, StateHasher};
use sgl_physics::{
    ALL_LAYERS, Collider, Cut, Ground, HeightField, Hit, MAX_QUERY_CELLS, MAX_QUERY_TRIANGLES,
    MAX_TRIANGLES, Shape, SurfaceError, TriangleSet, Unanswered, World, cast, ground_under,
    overlaps, ray,
};
use wasm_bindgen_test::wasm_bindgen_test;

fn draw(rng: &mut SplitMix64, low: f32, high: f32) -> f32 {
    low + (high - low) * ((rng.next_u64() >> 40) as f32 / 16_777_216.0)
}

/// How far an exact answer may lie from the true one: four units in the
/// last place of `largest`, the largest coordinate the query involves (the
/// ray's start and length, and the triangles' corners relative to it).
fn rounding(largest: f32) -> f32 {
    4.0 * f32::EPSILON * largest
}

/// A uniform whole number below `n`.
fn pick(rng: &mut SplitMix64, n: usize) -> usize {
    usize::try_from(rng.next_u64() % n as u64).expect("small")
}

/// A triangle list: positions, three indices a triangle, an id each.
#[derive(Clone, Default)]
struct Mesh {
    positions: Vec<Vec3>,
    indices: Vec<u32>,
    ids: Vec<u64>,
}

impl Mesh {
    fn triangle(&mut self, a: Vec3, b: Vec3, c: Vec3, id: u64) {
        let base = u32::try_from(self.positions.len()).expect("small");
        self.positions.extend([a, b, c]);
        self.indices.extend([base, base + 1, base + 2]);
        self.ids.push(id);
    }

    /// The quad `a b c d` (a cycle) as the triangles `a b c` and `a c d`.
    fn quad(&mut self, [a, b, c, d]: [Vec3; 4], id: u64) {
        self.triangle(a, b, c, id);
        self.triangle(a, c, d, id);
    }

    fn set(&self) -> TriangleSet {
        TriangleSet::new(&self.positions, &self.indices, &self.ids).expect("a sound mesh")
    }

    /// The triangles as corner triples.
    fn triangles(&self) -> Vec<([Vec3; 3], u64)> {
        self.indices
            .chunks_exact(3)
            .zip(&self.ids)
            .map(|(t, &id)| {
                let at = |i: u32| self.positions[usize::try_from(i).expect("small")];
                ([at(t[0]), at(t[1]), at(t[2])], id)
            })
            .collect()
    }

    /// How far `p` is from the nearest edge of any triangle: where a
    /// neighbouring triangle can be met at the same instant.
    fn edge_distance(&self, p: Vec3) -> f32 {
        let segment = |a: Vec3, b: Vec3| {
            let ab = b - a;
            let s = ((p - a).dot(ab) / ab.dot(ab)).clamp(0.0, 1.0);
            (a + ab * s - p).length()
        };
        self.triangles()
            .iter()
            .map(|([a, b, c], _)| segment(*a, *b).min(segment(*b, *c)).min(segment(*c, *a)))
            .fold(f32::INFINITY, f32::min)
    }
}

/// The twelve triangles of the box of half-extents `half` at `centre`.
fn box_mesh(centre: Vec3, half: Vec3, id: u64) -> Mesh {
    let corner = |i: usize| {
        let sign = |bit: usize| if i >> bit & 1 == 0 { -1.0 } else { 1.0 };
        centre + half * Vec3::new(sign(0), sign(1), sign(2))
    };
    let mut mesh = Mesh::default();
    for face in [
        [0, 2, 6, 4],
        [1, 3, 7, 5],
        [0, 1, 5, 4],
        [2, 3, 7, 6],
        [0, 1, 3, 2],
        [4, 5, 7, 6],
    ] {
        mesh.quad(face.map(corner), id);
    }
    mesh
}

/// The ramp the box of half-extents `half` at `centre` cut by the plane
/// through its lower `-x` edge and upper `+x` edge makes, solid below it:
/// as a cut-box collider, and as its eight triangles.
fn ramp(centre: Vec3, half: Vec3, id: u64) -> (Collider<Vec3>, Mesh) {
    let collider = Collider::new(id, Shape::Box { half }, centre)
        .with_cut(Cut::new(Vec3::new(-half.y, half.x, 0.0), 0.0));
    let at = |x: f32, y: f32, z: f32| centre + half * Vec3::new(x, y, z);
    let (a0, b0, c0) = (
        at(-1.0, -1.0, -1.0),
        at(1.0, -1.0, -1.0),
        at(1.0, 1.0, -1.0),
    );
    let (a1, b1, c1) = (at(-1.0, -1.0, 1.0), at(1.0, -1.0, 1.0), at(1.0, 1.0, 1.0));
    let mut mesh = Mesh::default();
    mesh.quad([a0, b0, b1, a1], id);
    mesh.quad([b0, c0, c1, b1], id);
    mesh.quad([a0, c0, c1, a1], id);
    mesh.triangle(a0, b0, c0, id);
    mesh.triangle(a1, b1, c1, id);
    (collider, mesh)
}

/// A point on a sphere of `radius` about `centre`.
fn around(rng: &mut SplitMix64, centre: Vec3, radius: f32) -> Vec3 {
    loop {
        let v = Vec3::new(
            draw(rng, -1.0, 1.0),
            draw(rng, -1.0, 1.0),
            draw(rng, -1.0, 1.0),
        );
        let length = v.length();
        if length > 0.1 && length <= 1.0 {
            return centre + v * (radius / length);
        }
    }
}

/// The geometry of a box or a ramp: whether `p` lies in its solid part.
fn solid(collider: &Collider<Vec3>, p: Vec3) -> bool {
    let Shape::Box { half } = collider.shape else {
        unreachable!("boxes only")
    };
    let local = p - collider.position;
    local.abs().cmple(half).all()
        && collider
            .cut
            .is_none_or(|cut| local.dot(cut.normal()) <= cut.offset())
}

/// Defect: a triangle mesh met somewhere other than where the same solid
/// is, or with another normal, so a camera or a wheel sees a different
/// world than the game authored. Oracle: the crate's box and cut-box
/// colliders, met in closed form and exactly by a ray and a downward ray,
/// and through the kernel by a ball, against twelve triangles of the same
/// box and eight of the same ramp. A ray or `ground_under` meets the
/// triangles where it meets the solid, to [`rounding`] of the largest
/// coordinate involved along the entry face's normal, with the same normal
/// wherever it meets them further than that from a triangle's edge (at an
/// edge of the solid two faces are met at once). A ball meets both within its band
/// of its true contact, so the two agree within that band.
#[wasm_bindgen_test(unsupported = test)]
fn triangle_boxes_and_ramps_agree_with_box_and_cut_box_colliders() {
    let box_centre = Vec3::new(1.0, 0.5, -0.5);
    let box_half = Vec3::new(2.0, 1.0, 1.5);
    let solid_box = Collider::new(4, Shape::Box { half: box_half }, box_centre);
    let (solid_ramp, ramp_mesh) = ramp(Vec3::new(-1.0, 0.25, 0.75), Vec3::new(2.5, 1.25, 1.0), 4);
    let cases = [
        (solid_box, box_mesh(box_centre, box_half, 4)),
        (solid_ramp, ramp_mesh),
    ];
    let mut rng = SplitMix64::new(0x0923_0001);
    let ball = Shape::Ball { radius: 0.25 };
    let ball_slop = 0.25 / 100.0;
    let (mut worst_ray, mut worst_ball, mut rays) = (0.0f32, 0.0f32, 0);
    for (collider, mesh) in &cases {
        let (set, single) = (mesh.set(), [*collider]);
        let Shape::Box { half } = collider.shape else {
            unreachable!()
        };
        for _ in 0..150 {
            let from = around(&mut rng, collider.position, 8.0);
            let aim = loop {
                let p = collider.position
                    + half
                        * Vec3::new(
                            draw(&mut rng, -1.0, 1.0),
                            draw(&mut rng, -1.0, 1.0),
                            draw(&mut rng, -1.0, 1.0),
                        );
                if solid(collider, p) {
                    break p;
                }
            };
            let motion = (aim - from) * draw(&mut rng, 1.1, 2.0);
            let exact = ray(from, motion, &single[..], ALL_LAYERS)
                .unwrap()
                .expect("aimed at the solid");
            let met = ray(from, motion, &set, ALL_LAYERS)
                .unwrap()
                .expect("the triangles enclose the solid");
            let approach = -motion.dot(exact.normal);
            let apart = (exact.t - met.t).abs() * approach;
            // The corners relative to the ray reach 8 + 2.5 at most.
            let bound = rounding(from.abs().max((from + motion).abs()).max_element() + 10.5);
            worst_ray = worst_ray.max(apart / bound);
            rays += 1;
            assert!(
                apart <= bound,
                "ray from {from} by {motion}: {met:?} against {exact:?}"
            );
            if mesh.edge_distance(from + motion * met.t) > bound {
                assert!(
                    (met.normal - exact.normal).length() < 1e-6,
                    "ray from {from:?} by {motion:?}: {met:?} against {exact:?}"
                );
            }
            let by_box = cast(&ball, from, motion, &single[..], ALL_LAYERS)
                .unwrap()
                .expect("a ball aimed at the solid meets it");
            let by_triangles = cast(&ball, from, motion, &set, ALL_LAYERS)
                .unwrap()
                .expect("and meets its triangles");
            let approach = -motion.dot(by_box.normal);
            let apart = (by_box.t - by_triangles.t).abs() * approach;
            worst_ball = worst_ball.max(apart / ball_slop);
            assert!(
                apart <= 1.25 * ball_slop + 1e-5,
                "ball from {from:?} by {motion:?}: {by_triangles:?} against {by_box:?}"
            );
        }
        for _ in 0..60 {
            let local = half
                * Vec3::new(
                    draw(&mut rng, -0.95, 0.95),
                    0.0,
                    draw(&mut rng, -0.95, 0.95),
                );
            let point = collider.position + local + Vec3::new(0.0, half.y + 0.5, 0.0);
            let exact = ground_under(point, 1.0, 4.0, &single[..], ALL_LAYERS)
                .unwrap()
                .expect("over the solid");
            let met = ground_under(point, 1.0, 4.0, &set, ALL_LAYERS)
                .unwrap()
                .expect("over its triangles");
            let bound = rounding(point.abs().max_element() + 5.0 + 2.5);
            assert!(
                (met.height - exact.height).abs() <= bound,
                "ground under {point}: {met:?} against {exact:?}"
            );
            assert!(
                (met.normal - exact.normal).length() < 1e-6,
                "{met:?} {exact:?}"
            );
            assert_eq!(met.id, exact.id);
        }
    }
    assert!(rays == 300 && worst_ray <= 1.0, "{worst_ray} {worst_ball}");
}

/// Defect: `ground_under` mistakes a deck over a vehicle for the floor it
/// is on, misses the deck it is on, or finds ground beside the mesh.
/// Oracle: the geometry of a road at y = 0 and a bridge deck at y = 3
/// across it: a wheel under the bridge, reaching a metre up, stands on the
/// road; one on the bridge stands on the bridge; reaching ten metres up
/// from under it finds the bridge; one beside both finds nothing. Heights
/// are the surfaces' own, to [`rounding`] of the corners' 10, and the
/// normals straight up to the bit (a level triangle's normal is exact).
#[wasm_bindgen_test(unsupported = test)]
fn ground_under_a_bridge_is_the_road_and_on_it_the_bridge() {
    let mut mesh = Mesh::default();
    mesh.quad(
        [
            Vec3::new(-10.0, 0.0, -2.0),
            Vec3::new(10.0, 0.0, -2.0),
            Vec3::new(10.0, 0.0, 2.0),
            Vec3::new(-10.0, 0.0, 2.0),
        ],
        1,
    );
    mesh.quad(
        [
            Vec3::new(-2.0, 3.0, -3.0),
            Vec3::new(2.0, 3.0, -3.0),
            Vec3::new(2.0, 3.0, 3.0),
            Vec3::new(-2.0, 3.0, 3.0),
        ],
        2,
    );
    let set = mesh.set();
    let bound = rounding(10.5);
    let ground = |point: Vec3, max_height: f32| {
        ground_under(point, max_height, 5.0, &set, ALL_LAYERS).unwrap()
    };
    let under = ground(Vec3::new(0.3, 0.1, 0.4), 1.0).expect("the road");
    assert_eq!(under.id, 1);
    assert!(under.height.abs() <= bound, "{under:?}");
    let on = ground(Vec3::new(0.3, 3.1, 0.4), 1.0).expect("the bridge");
    assert_eq!(on.id, 2);
    assert!((on.height - 3.0).abs() <= bound, "{on:?}");
    let reaching = ground(Vec3::new(0.3, 0.1, 0.4), 10.0).expect("the bridge");
    assert_eq!(reaching.id, 2);
    assert_eq!(ground(Vec3::new(0.3, 0.1, 5.0), 10.0), None);
    for found in [under, on, reaching] {
        assert_eq!(found.normal, Vec3::Y, "{found:?}");
    }
}

/// A 20 m quad as two triangles (ids 21 and 22) sharing the diagonal from
/// its `(-10, -10)` corner to its `(10, 10)` corner, on the plane `y = c +
/// a x + b z`.
fn twenty_metre_quad(height: f32, along_x: f32, along_z: f32) -> TriangleSet {
    let at = |x: f32, z: f32| Vec3::new(x, height + along_x * x + along_z * z, z);
    let mut mesh = Mesh::default();
    let (near, right, far, left) = (
        at(-10.0, -10.0),
        at(10.0, -10.0),
        at(10.0, 10.0),
        at(-10.0, 10.0),
    );
    mesh.triangle(near, right, far, 21);
    mesh.triangle(near, far, left, 22);
    mesh.set()
}

/// Defect: `ground_under` stops short of a triangle or reads a seam's
/// neighbour (the slop the kernel once met a ray with: a twentieth-metre
/// band above a 20 m triangle and a normal leaning across a seam). Oracle:
/// the analytic planes `y = 0.25` and `y = 0.25 + 0.5 x - 0.25 z` (normal
/// `(-0.5, 1, 0.25) / |.|`), worked in `f64`, under two 20 m triangles
/// sharing a diagonal, at interior points, points exactly on the shared
/// diagonal and the two corners both triangles share. Every height is the
/// plane's to [`rounding`] of the corners' 20 (about ten micrometres), and
/// every normal is the same to the bit, the plane's: straight up on the
/// floor, where on the seam both triangles meet the ray at the same instant
/// and the lower id (21) is reported.
#[wasm_bindgen_test(unsupported = test)]
fn ground_under_is_the_planes_height_with_one_normal_across_a_seam() {
    let interior = [(3.3, -6.1), (-7.9, 2.2), (9.5, 0.7), (-1.1, -9.3)];
    let seam = [(2.7, 2.7), (-7.3, -7.3), (0.0, 0.0), (9.9, 9.9)];
    let corners = [(-10.0, -10.0), (10.0, 10.0)];
    for (c, a, b) in [(0.25f32, 0.0f32, 0.0f32), (0.25, 0.5, -0.25)] {
        let set = twenty_metre_quad(c, a, b);
        let plane = |x: f32, z: f32| {
            f64::from(c) + f64::from(a) * f64::from(x) + f64::from(b) * f64::from(z)
        };
        let normal = Vec3::new(-a, 1.0, -b).normalize();
        let mut normals = Vec::new();
        for (x, z) in interior.iter().chain(&seam).chain(&corners).copied() {
            #[allow(clippy::cast_possible_truncation)]
            let point = Vec3::new(x, plane(x, z) as f32 + 0.5, z);
            let found = ground_under(point, 1.0, 2.0, &set, ALL_LAYERS)
                .unwrap()
                .unwrap_or_else(|| panic!("ground at ({x}, {z})"));
            let off = (f64::from(found.height) - plane(x, z)).abs();
            assert!(
                off <= f64::from(rounding(20.0)),
                "({x}, {z}): {found:?} is {off} off"
            );
            assert!(
                (found.normal - normal).length() < 1e-6,
                "({x}, {z}): {found:?}"
            );
            if a == 0.0 && seam.contains(&(x, z)) {
                assert_eq!(found.id, 21, "({x}, {z}): the lower id on a tie");
            }
            normals.push(found.normal);
        }
        assert!(normals.iter().all(|n| *n == normals[0]), "{normals:?}");
        if a == 0.0 {
            assert_eq!(normals[0], Vec3::Y);
        }
    }
}

/// Game scenario: a kart's four wheels rolling across the diagonal seam of
/// a two-triangle deck. Oracle: the deck's analytic plane `y = 2 + 0.25 x
/// + 0.125 z` (normal `(-0.25, 1, -0.125) / |.|`), worked in `f64`. The
/// kart, its wheels 1.2 apart across and 1.8 along, rolls along x in steps
/// of 0.05 across the seam from (0, 0) to (8, 8); at every step each wheel
/// gets the plane's height to [`rounding`] of the deck's 8 and the same
/// normal to the bit, so neither its height nor its tilt jumps at the
/// seam, and each wheel meets both triangles on the way.
#[wasm_bindgen_test(unsupported = test)]
fn a_karts_wheels_rolling_across_a_seam_never_jump() {
    let at = |x: f32, z: f32| Vec3::new(x, 2.0 + 0.25 * x + 0.125 * z, z);
    let mut mesh = Mesh::default();
    mesh.triangle(at(0.0, 0.0), at(8.0, 0.0), at(8.0, 8.0), 1);
    mesh.triangle(at(0.0, 0.0), at(8.0, 8.0), at(0.0, 8.0), 2);
    let set = mesh.set();
    let plane = |x: f32, z: f32| 2.0 + 0.25 * f64::from(x) + 0.125 * f64::from(z);
    let normal = Vec3::new(-0.25, 1.0, -0.125).normalize();
    let wheels = [(-0.9, -0.6), (-0.9, 0.6), (0.9, -0.6), (0.9, 0.6)];
    let mut first = None;
    let mut met = [[false; 2]; 4];
    for step in 0..100 {
        let (kx, kz) = (1.5 + step as f32 * 0.05, 4.1);
        for (wheel, (dx, dz)) in wheels.iter().enumerate() {
            let (x, z) = (kx + dx, kz + dz);
            #[allow(clippy::cast_possible_truncation)]
            let point = Vec3::new(x, plane(x, z) as f32 + 0.1, z);
            let ground = ground_under(point, 0.3, 0.5, &set, ALL_LAYERS)
                .unwrap()
                .unwrap_or_else(|| panic!("the deck under ({x}, {z})"));
            let off = (f64::from(ground.height) - plane(x, z)).abs();
            assert!(
                off <= f64::from(rounding(10.0)),
                "({x}, {z}): {ground:?} is {off} off"
            );
            assert!((ground.normal - normal).length() < 1e-6, "{ground:?}");
            assert_eq!(
                *first.get_or_insert(ground.normal),
                ground.normal,
                "({x}, {z})"
            );
            met[wheel][usize::from(ground.id == 2)] = true;
        }
    }
    assert_eq!(met, [[true; 2]; 4], "every wheel crosses the seam");
}

/// Defect: a seam with a gap: two triangles that share an edge compute
/// its corners and edge each from its own position and corners, relative
/// to the ray, and these can round a unit in the last place apart, so a
/// ray exactly on the seam passes between them. Oracle: the geometry. A
/// gently sloped 8 x 8 grid of cells (heights within a tenth of a cell),
/// none of its coordinates on a binary grid, near the origin and a
/// thousand units out; rays from a cell above a point of a shared edge or
/// corner, aimed through it at most half a cell aside per cell down, are
/// steeper than any slope, so each crosses the surface before it ends a
/// cell below. Every one of 2000 meets it. (Without the edge reach the
/// first runs lost about one ray in thirty near the origin.)
#[wasm_bindgen_test(unsupported = test)]
fn a_ray_onto_a_seam_never_falls_between_its_triangles() {
    for (origin, cell) in [
        (Vec3::new(0.1, 0.3, 0.3), 1.1),
        (Vec3::new(1000.3, 0.3, -2000.7), 3.7),
    ] {
        let mut rng = SplitMix64::new(0x0923_5ea3);
        let mut heights = [[0.0f32; 9]; 9];
        for row in &mut heights {
            for h in row.iter_mut() {
                *h = draw(&mut rng, -0.1, 0.1) * cell;
            }
        }
        let corner = |i: usize, j: usize| {
            origin + Vec3::new(cell * i as f32, heights[j][i], cell * j as f32)
        };
        let mut mesh = Mesh::default();
        for j in 0..8 {
            for i in 0..8 {
                let at = |di: usize, dj: usize| corner(i + di, j + dj);
                mesh.quad([at(0, 0), at(1, 0), at(1, 1), at(0, 1)], 1);
            }
        }
        let set = mesh.set();
        for _ in 0..1000 {
            let (i, j) = (2 + pick(&mut rng, 5), 2 + pick(&mut rng, 5));
            let start = corner(i, j);
            // A shared corner, or a point on one of the edges from it:
            // along x, along z, or the diagonal back to the cell before.
            let end = match pick(&mut rng, 4) {
                0 => start,
                1 => corner(i + 1, j),
                2 => corner(i, j + 1),
                _ => corner(i - 1, j - 1),
            };
            let on = start + (end - start) * draw(&mut rng, 0.0, 1.0);
            let aside = Vec3::new(draw(&mut rng, -0.5, 0.5), 1.0, draw(&mut rng, -0.5, 0.5));
            let from = on + aside * cell;
            let hit = ray(from, (on - from) * 2.0, &set, ALL_LAYERS).unwrap();
            assert!(
                hit.is_some(),
                "a ray from {from} through {on} fell through the seam"
            );
        }
    }
}

/// `mesh` with its triangles in a seeded order, each wound the other way
/// and started from another corner, each with corners of its own.
fn reordered(mesh: &Mesh, seed: u64) -> Mesh {
    let mut triangles = mesh.triangles();
    let mut rng = SplitMix64::new(seed);
    for i in (1..triangles.len()).rev() {
        triangles.swap(i, pick(&mut rng, i + 1));
    }
    let mut out = Mesh::default();
    for ([a, b, c], id) in triangles {
        match pick(&mut rng, 3) {
            0 => out.triangle(c, b, a, id),
            1 => out.triangle(b, a, c, id),
            _ => out.triangle(a, c, b, id),
        }
    }
    out
}

/// Defect: over a seam the answer depends on which triangle the world
/// offered first. Oracle: the requirement (order never shows) and the
/// geometry: straight down onto a quad's shared diagonal, and onto a
/// vertex six triangles share, each triangle with an id of its own, the
/// ground is one answer at the plane's height (to [`rounding`]), the same
/// to the bit in three triangle orders and windings, its normal the
/// plane's, straight up, to the bit.
#[wasm_bindgen_test(unsupported = test)]
fn ground_over_a_shared_edge_or_vertex_is_one_answer_in_any_order() {
    let mut mesh = Mesh::default();
    let mut id = 10;
    for (x, z) in [(-1.0, -1.0), (0.0, -1.0), (-1.0, 0.0), (0.0, 0.0)] {
        let at = |dx: f32, dz: f32| Vec3::new(x + dx, 0.0, z + dz);
        mesh.triangle(at(0.0, 0.0), at(0.0, 1.0), at(1.0, 0.0), id);
        mesh.triangle(at(1.0, 1.0), at(1.0, 0.0), at(0.0, 1.0), id + 1);
        id += 2;
    }
    let sets = [
        mesh.set(),
        reordered(&mesh, 1).set(),
        reordered(&mesh, 2).set(),
    ];
    // Over the diagonal from (1, 0, -1) to (0, 0, 0), and over (0, 0, 0).
    for point in [Vec3::new(0.25, 0.5, -0.25), Vec3::new(0.0, 0.5, 0.0)] {
        let found: Vec<Ground> = sets
            .iter()
            .map(|set| {
                ground_under(point, 0.5, 1.0, set, ALL_LAYERS)
                    .unwrap()
                    .expect("on the mesh")
            })
            .collect();
        let bits = |g: &Ground| {
            (
                g.height.to_bits(),
                g.normal.to_array().map(f32::to_bits),
                g.id,
            )
        };
        assert!(
            found.iter().all(|g| bits(g) == bits(&found[0])),
            "{found:?}"
        );
        assert!(found[0].height.abs() <= rounding(2.0), "{found:?}");
        assert_eq!(found[0].normal, Vec3::Y, "{found:?}");
    }
}

/// A whole number from 0 to 7 held as a float, as an integer.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn whole(value: f32) -> u64 {
    value as u64
}

/// The analytic plane `y = 1 + 0.25 x - 0.5 z` sampled on a 9 x 7 grid a
/// unit apart from (-4, 0, -3), one id a cell.
fn plane_field() -> HeightField {
    let (columns, rows) = (9, 7);
    let mut heights = Vec::new();
    for row in 0..rows {
        for column in 0..columns {
            let (x, z) = (-4.0 + column as f32, -3.0 + row as f32);
            heights.push(1.0 + 0.25 * x - 0.5 * z);
        }
    }
    let ids: Vec<u64> = (100..148).collect();
    HeightField::new(
        &heights,
        columns,
        rows,
        1.0,
        Vec3::new(-4.0, 0.0, -3.0),
        &ids,
    )
    .expect("a sound grid")
}

/// Defect: a height field's ground off its samples' plane, its normal
/// not the plane's, a cell boundary or the grid's edge missed, or ground
/// found outside the grid. Oracle: the analytic plane `y = 1 + 0.25 x -
/// 0.5 z` and its normal `(-0.25, 1, 0.5) / |.|`. The heights are the
/// plane's, to [`rounding`] of the coordinates involved (4, 3 and the
/// heights up to 3.5), at random points, on every cell boundary line, on
/// the grid's edges and corners, and the normal is the plane's everywhere,
/// the same to the bit on every triangle and at every seam (each cell's
/// corners lie on a grid, so every triangle's edges cross exactly). Beyond
/// the grid's edge, a twentieth of a unit out, there is nothing.
#[wasm_bindgen_test(unsupported = test)]
fn a_height_field_of_a_plane_returns_the_planes_height_and_normal() {
    let field = plane_field();
    let normal = Vec3::new(-0.25, 1.0, 0.5).normalize();
    let height = |x: f32, z: f32| 1.0 + 0.25 * x - 0.5 * z;
    let mut rng = SplitMix64::new(0x0923_0003);
    let mut normals = Vec::new();
    let mut points: Vec<(f32, f32)> = (0..80)
        .map(|_| (draw(&mut rng, -4.0, 4.0), draw(&mut rng, -3.0, 3.0)))
        .collect();
    for i in -4..=4 {
        points.push((i as f32, draw(&mut rng, -3.0, 3.0)));
        points.push((i as f32, (i % 4) as f32 * 0.75));
    }
    for j in -3..=3 {
        points.push((draw(&mut rng, -4.0, 4.0), j as f32));
    }
    points.extend([
        (-4.0, -3.0),
        (4.0, -3.0),
        (-4.0, 3.0),
        (4.0, 3.0),
        (0.0, 0.0),
    ]);
    for (x, z) in points {
        let point = Vec3::new(x, height(x, z) + 0.5, z);
        let found = ground_under(point, 1.0, 2.0, &field, ALL_LAYERS)
            .unwrap()
            .unwrap_or_else(|| panic!("ground at ({x}, {z})"));
        let above = found.height - height(x, z);
        assert!(above.abs() <= rounding(4.5), "({x}, {z}): {found:?}");
        assert!(
            (found.normal - normal).length() < 1e-6,
            "({x}, {z}): {found:?}"
        );
        normals.push(found.normal);
        let column = whole((x + 4.0).floor().clamp(0.0, 7.0));
        let row = whole((z + 3.0).floor().clamp(0.0, 5.0));
        let fractional = x.fract() != 0.0 && z.fract() != 0.0;
        if fractional {
            assert_eq!(found.id, 100 + row * 8 + column, "({x}, {z})");
        }
    }
    for (x, z) in [
        (-4.05, 0.0),
        (4.05, 0.5),
        (1.0, -3.05),
        (-2.5, 3.05),
        (6.0, 6.0),
    ] {
        let point = Vec3::new(x, 5.0, z);
        assert_eq!(ground_under(point, 1.0, 20.0, &field, ALL_LAYERS), Ok(None));
    }
    assert!(normals.iter().all(|n| *n == normals[0]), "{normals:?}");
}

/// Defect: a ridge whose two slopes meet on a sample line reads either
/// slope's plane on the other side, or the crest off its height. Oracle:
/// the analytic tent `y = 2 - 0.5 |x - 1|` along z, sampled half a unit
/// apart: on each side the slope's height, and its normal `(∓0.5, 1, 0) /
/// |.|` away from the crest (on it, either slope's); on the crest the
/// crest's height; each to [`rounding`] of the coordinates involved.
#[wasm_bindgen_test(unsupported = test)]
fn a_height_field_of_a_ridge_returns_each_slope_and_the_crest() {
    let (columns, rows, cell) = (13, 5, 0.5);
    let tent = |x: f32| 2.0 - 0.5 * (x - 1.0).abs();
    let mut heights = Vec::new();
    for _ in 0..rows {
        for column in 0..columns {
            heights.push(tent(-2.0 + column as f32 * cell));
        }
    }
    let field = HeightField::new(
        &heights,
        columns,
        rows,
        cell,
        Vec3::new(-2.0, 0.0, -1.0),
        &[7],
    )
    .unwrap();
    let bound = rounding(4.5);
    let mut rng = SplitMix64::new(0x0923_0004);
    for _ in 0..60 {
        let (x, z) = (draw(&mut rng, -1.9, 3.9), draw(&mut rng, -0.9, 0.9));
        let found = ground_under(
            Vec3::new(x, tent(x) + 0.25, z),
            0.5,
            1.0,
            &field,
            ALL_LAYERS,
        )
        .unwrap()
        .expect("over the ridge");
        let normal = Vec3::new(if x < 1.0 { -0.5 } else { 0.5 }, 1.0, 0.0).normalize();
        let above = found.height - tent(x);
        assert!(above.abs() <= bound, "{x}: {found:?}");
        if (x - 1.0).abs() > bound {
            assert!((found.normal - normal).length() < 1e-6, "{x}: {found:?}");
        }
        assert_eq!(found.id, 7);
    }
    let crest = ground_under(Vec3::new(1.0, 2.1, 0.3), 0.5, 1.0, &field, ALL_LAYERS)
        .unwrap()
        .expect("the crest");
    assert!((crest.height - 2.0).abs() <= bound, "{crest:?}");
    assert!(crest.normal.y > 0.0);
}

/// A seeded mesh: a bumpy 6 x 6 grid of quarter-unit heights and a few
/// loose triangles above it, ids repeating so position breaks ties.
fn bumpy_mesh(seed: u64, offset: Vec3) -> Mesh {
    let mut rng = SplitMix64::new(seed);
    let quarter = |rng: &mut SplitMix64, n: usize| pick(rng, n) as f32 * 0.25;
    let mut heights = [[0.0f32; 7]; 7];
    for row in &mut heights {
        for h in row.iter_mut() {
            *h = quarter(&mut rng, 5);
        }
    }
    let mut mesh = Mesh::default();
    let mut n = 0u64;
    for (z, pair) in heights.windows(2).enumerate() {
        for x in 0..6 {
            let at = |dx: usize, dz: usize| {
                let h = pair[dz][x + dx];
                offset + Vec3::new((x + dx) as f32, h, (z + dz) as f32)
            };
            mesh.quad([at(0, 0), at(1, 0), at(1, 1), at(0, 1)], n % 5);
            n += 1;
        }
    }
    for _ in 0..8 {
        let base = Vec3::new(
            quarter(&mut rng, 24),
            2.0 + quarter(&mut rng, 8),
            quarter(&mut rng, 24),
        );
        let corner = |rng: &mut SplitMix64| {
            base + Vec3::new(
                quarter(rng, 8) - 1.0,
                quarter(rng, 4) - 0.5,
                quarter(rng, 8) - 1.0,
            )
        };
        let (a, b, c) = (corner(&mut rng), corner(&mut rng), corner(&mut rng));
        mesh.triangle(offset + a, offset + b, offset + c, n % 5);
        n += 1;
    }
    mesh
}

/// Seeded queries against `world`, every result written to one digest:
/// rays, ball overlaps, ball casts from where the ball overlaps nothing,
/// and `ground_under`, from points on a quarter-unit grid offset by
/// `offset`; the colliders overlapped are written about `offset`.
fn queries<W: World<Vec3> + ?Sized>(world: &W, offset: Vec3, seed: u64) -> String {
    let mut rng = SplitMix64::new(seed);
    let mut out = StateHasher::new();
    let quarter = |rng: &mut SplitMix64, low: f32, n: usize| low + pick(rng, n) as f32 * 0.25;
    let write_hit =
        |out: &mut StateHasher, found: Result<Option<Hit<Vec3>>, Unanswered>| match found {
            Err(Unanswered) => out.u8(2),
            Ok(None) => out.u8(0),
            Ok(Some(hit)) => {
                out.u8(1);
                hit.canonical_write(out);
            }
        };
    for _ in 0..60 {
        let from = offset
            + Vec3::new(
                quarter(&mut rng, -1.0, 32),
                quarter(&mut rng, 0.0, 24),
                quarter(&mut rng, -1.0, 32),
            );
        let motion = Vec3::new(
            quarter(&mut rng, -4.0, 32),
            quarter(&mut rng, -6.0, 32),
            quarter(&mut rng, -4.0, 32),
        );
        write_hit(&mut out, ray(from, motion, world, ALL_LAYERS));
        let ball = Shape::Ball {
            radius: quarter(&mut rng, 0.25, 3),
        };
        let inside = overlaps(&ball, from, world, ALL_LAYERS).unwrap();
        out.length(inside.len());
        for collider in &inside {
            let moved = Collider {
                position: collider.position - offset,
                ..*collider
            };
            moved.canonical_write(&mut out);
        }
        // A cast that starts overlapping a collider meets it at once when
        // the overlap is within the collider's rounding allowance, which
        // grows with its distance from the origin (`cast`): only a clear
        // start is cast.
        if inside.is_empty() {
            write_hit(&mut out, cast(&ball, from, motion, world, ALL_LAYERS));
        }
        match ground_under(from, 1.0, 6.0, world, ALL_LAYERS).unwrap() {
            None => out.u8(0),
            Some(ground) => {
                out.u8(1);
                ground.canonical_write(&mut out);
            }
        }
    }
    out.finish().to_hex()
}

/// Defect: a result shows the order the triangles were given in, how each
/// was wound, or the tree built from them. Oracle: the requirement (order
/// never shows): the same seeded rays, casts, overlaps and grounds against
/// a mesh and against two reorderings of it (triangles shuffled, each wound
/// the other way and started from another corner, each with its own
/// copies of its corners) digest to the same bits. Ids repeat, so
/// position and shape break ties.
#[wasm_bindgen_test(unsupported = test)]
fn shuffling_and_rewinding_the_triangles_changes_no_result() {
    let mesh = bumpy_mesh(0x0923_0005, Vec3::ZERO);
    let given = queries(&mesh.set(), Vec3::ZERO, 9);
    for seed in [3, 4] {
        assert_eq!(queries(&reordered(&mesh, seed).set(), Vec3::ZERO, 9), given);
    }
}

/// Defect: a query far from the origin rounds differently from the same
/// query near it. Oracle: the requirement (near is as good as far): the
/// bumpy mesh and a height field of quarter-unit heights, with every query,
/// moved by 2^17 on x and z (y kept, so `ground_under`'s height is the
/// same number) digest to the same bits; every coordinate lies on a
/// quarter-unit grid, so the move is exact.
#[wasm_bindgen_test(unsupported = test)]
fn a_scene_moved_by_a_power_of_two_gives_the_same_bits() {
    let far = Vec3::new(131_072.0, 0.0, 131_072.0);
    let near_mesh = bumpy_mesh(0x0923_0006, Vec3::ZERO).set();
    let far_mesh = bumpy_mesh(0x0923_0006, far).set();
    assert_eq!(
        queries(&far_mesh, far, 11),
        queries(&near_mesh, Vec3::ZERO, 11)
    );
    let mut rng = SplitMix64::new(0x0923_0007);
    let heights: Vec<f32> = (0..64).map(|_| pick(&mut rng, 9) as f32 * 0.25).collect();
    let field = |origin: Vec3| HeightField::new(&heights, 8, 8, 0.75, origin, &[5]).unwrap();
    let base = Vec3::new(-1.0, 0.0, -1.0);
    assert_eq!(
        queries(&field(base + far), far, 12),
        queries(&field(base), Vec3::ZERO, 12)
    );
}

/// A flat `n` x `n` grid of unit cells on y = 0, two triangles a cell.
fn flat_grid(n: usize) -> (Vec<Vec3>, Vec<u32>) {
    let mut positions = Vec::new();
    for z in 0..=n {
        for x in 0..=n {
            positions.push(Vec3::new(x as f32, 0.0, z as f32));
        }
    }
    let at = |x: usize, z: usize| u32::try_from(z * (n + 1) + x).expect("small");
    let mut indices = Vec::new();
    for z in 0..n {
        for x in 0..n {
            indices.extend([at(x, z), at(x, z + 1), at(x + 1, z)]);
            indices.extend([at(x + 1, z + 1), at(x + 1, z), at(x, z + 1)]);
        }
    }
    (positions, indices)
}

/// Defect: a world walks everything its region's box encloses, so a long
/// ray across a large mesh costs the whole mesh, or a query the cap should
/// refuse is answered. Oracle: the geometry and the caps. On a flat 64 x
/// 64 grid of unit cells, a point swept along the diagonal from (0.5, 0,
/// 0.5) to (63.5, 0, 63.5) meets the 64 cells it crosses and, at their
/// corners, the 63 cells either side of the diagonal: 190 cells, 380
/// triangles, though its enclosing box holds all 8192; the height field of
/// the same grid offers the same 380. A ball covering every cell is
/// unanswered by both (8192 triangles over [`MAX_QUERY_TRIANGLES`], 4096
/// cells within [`MAX_QUERY_CELLS`] for the field, so it takes a grid of 65
/// x 65 cells), and a ray across a 65 x 65 grid's diagonal is answered.
#[wasm_bindgen_test(unsupported = test)]
fn a_long_ray_visits_only_what_lies_along_it_and_a_query_over_the_cap_is_unanswered() {
    let n = 64;
    let (positions, indices) = flat_grid(n);
    let set = TriangleSet::new(&positions, &indices, &[1]).unwrap();
    let field = |n: usize| {
        HeightField::new(
            &vec![0.0; (n + 1) * (n + 1)],
            n + 1,
            n + 1,
            1.0,
            Vec3::ZERO,
            &[1],
        )
        .unwrap()
    };
    let start = Vec3::new(0.5, 0.0, 0.5);
    let motion = Vec3::new(63.0, 0.0, 63.0);
    let along = |world: &dyn World<Vec3>| {
        let mut count = 0u64;
        world
            .colliders_along(start, start, motion, &mut |_| count += 1)
            .map(|()| count)
    };
    assert_eq!(along(&set), Ok(2 * (3 * 64 - 2)));
    assert_eq!(along(&field(64)), Ok(2 * (3 * 64 - 2)));
    assert!(
        ray(start + Vec3::Y, motion - Vec3::Y, &set, ALL_LAYERS)
            .unwrap()
            .is_some()
    );

    let everything = Shape::Ball { radius: 50.0 };
    let middle = Vec3::new(32.0, 0.0, 32.0);
    const { assert!(8192 > MAX_QUERY_TRIANGLES) };
    assert_eq!(
        overlaps(&everything, middle, &set, ALL_LAYERS),
        Err(Unanswered)
    );
    const { assert!(65 * 65 > MAX_QUERY_CELLS && 64 * 64 <= MAX_QUERY_CELLS) };
    assert!(
        overlaps(
            &Shape::Ball { radius: 31.9 },
            middle,
            &field(64),
            ALL_LAYERS
        )
        .is_ok()
    );
    let big = field(65);
    let centre = Vec3::new(32.5, 0.0, 32.5);
    assert_eq!(
        overlaps(&Shape::Ball { radius: 33.0 }, centre, &big, ALL_LAYERS),
        Err(Unanswered)
    );
    let across = ray(
        Vec3::new(0.5, 1.0, 0.5),
        Vec3::new(64.0, -1.0, 64.0),
        &big,
        ALL_LAYERS,
    );
    assert!(across.is_ok_and(|hit| hit.is_some()));
}

/// Defect: a malformed mesh or grid accepted, or a degenerate triangle
/// refused or kept. Oracle: the requirements of the constructors: each
/// malformed input is refused for its own reason; a triangle with its
/// corners in a line and one with two corners the same are dropped and
/// counted, the rest kept; more than [`MAX_TRIANGLES`] is refused.
#[wasm_bindgen_test(unsupported = test)]
fn malformed_surfaces_are_refused_and_degenerate_triangles_dropped() {
    let p = [Vec3::ZERO, Vec3::X, Vec3::Z, Vec3::new(2.0, 0.0, 0.0)];
    assert_eq!(
        TriangleSet::new(&p, &[0, 1], &[1]).err(),
        Some(SurfaceError::Indices)
    );
    assert_eq!(
        TriangleSet::new(&p, &[0, 1, 4], &[1]).err(),
        Some(SurfaceError::Indices)
    );
    assert_eq!(
        TriangleSet::new(&p, &[0, 1, 2], &[1, 2]).err(),
        Some(SurfaceError::Ids)
    );
    let nan = [Vec3::ZERO, Vec3::X, Vec3::new(0.0, f32::NAN, 1.0)];
    assert_eq!(
        TriangleSet::new(&nan, &[0, 1, 2], &[1]).err(),
        Some(SurfaceError::NotFinite)
    );
    let many = vec![0; 3 * (MAX_TRIANGLES + 1)];
    assert_eq!(
        TriangleSet::new(&p, &many, &[1]).err(),
        Some(SurfaceError::TooMany)
    );
    let set = TriangleSet::new(&p, &[0, 1, 2, 0, 1, 3, 1, 1, 2], &[1, 2, 3]).unwrap();
    assert_eq!((set.len(), set.degenerate()), (1, 2));

    let heights = [0.0; 6];
    let field = |h: &[f32], columns, rows, cell, origin, ids: &[u64]| {
        HeightField::new(h, columns, rows, cell, origin, ids).err()
    };
    assert_eq!(field(&heights, 3, 2, 1.0, Vec3::ZERO, &[1]), None);
    assert_eq!(
        field(&heights, 3, 3, 1.0, Vec3::ZERO, &[1]),
        Some(SurfaceError::Grid)
    );
    assert_eq!(
        field(&heights, 6, 1, 1.0, Vec3::ZERO, &[1]),
        Some(SurfaceError::Grid)
    );
    assert_eq!(
        field(&heights, 3, 2, 0.0, Vec3::ZERO, &[1]),
        Some(SurfaceError::Grid)
    );
    assert_eq!(
        field(&heights, 3, 2, 1.0, Vec3::ZERO, &[1, 2, 3]),
        Some(SurfaceError::Ids)
    );
    let far = Vec3::new(1e9, 0.0, 0.0);
    assert_eq!(
        field(&heights, 3, 2, 1.0, far, &[1]),
        Some(SurfaceError::Grid)
    );
    let inf = Vec3::new(f32::INFINITY, 0.0, 0.0);
    assert_eq!(
        field(&heights, 3, 2, 1.0, inf, &[1]),
        Some(SurfaceError::NotFinite)
    );
    let bad = [0.0, 0.0, f32::NAN, 0.0, 0.0, 0.0];
    assert_eq!(
        field(&bad, 3, 2, 1.0, Vec3::ZERO, &[1]),
        Some(SurfaceError::NotFinite)
    );
}

/// Game scenario (#924, item 3): a kart's four wheels on a ramp of two
/// triangles. Oracle: the ramp's analytic plane `y = 0.5 x`, normal `(-0.5,
/// 1, 0) / |.|`: each wheel, resting a little above it, gets the plane's
/// height (to [`rounding`] of the ramp's 4) and normal, the same to the bit
/// on both triangles.
#[wasm_bindgen_test(unsupported = test)]
fn a_karts_four_wheels_get_the_ramps_height_and_normal() {
    let mut mesh = Mesh::default();
    mesh.quad(
        [
            Vec3::new(0.0, 0.0, -1.5),
            Vec3::new(4.0, 2.0, -1.5),
            Vec3::new(4.0, 2.0, 1.5),
            Vec3::new(0.0, 0.0, 1.5),
        ],
        3,
    );
    let set = mesh.set();
    let normal = Vec3::new(-0.5, 1.0, 0.0).normalize();
    let mut normals = Vec::new();
    for (x, z) in [(1.2, -0.8), (1.2, 0.8), (2.9, -0.8), (2.9, 0.8)] {
        let wheel = Vec3::new(x, 0.5 * x + 0.05, z);
        let ground = ground_under(wheel, 0.3, 0.5, &set, ALL_LAYERS)
            .unwrap()
            .expect("the ramp");
        let above = ground.height - 0.5 * x;
        assert!(above.abs() <= rounding(4.0), "{wheel}: {ground:?}");
        assert!((ground.normal - normal).length() < 1e-6, "{ground:?}");
        assert_eq!(ground.id, 3);
        normals.push(ground.normal);
    }
    assert!(normals.iter().all(|n| *n == normals[0]), "{normals:?}");
}

/// The two triangles of a wall at x = 5, from y = 0 to 4 and z = -5 to 5.
fn wall() -> TriangleSet {
    let mut mesh = Mesh::default();
    mesh.quad(
        [
            Vec3::new(5.0, 0.0, -5.0),
            Vec3::new(5.0, 4.0, -5.0),
            Vec3::new(5.0, 4.0, 5.0),
            Vec3::new(5.0, 0.0, 5.0),
        ],
        9,
    );
    mesh.set()
}

/// Game scenario (#924, item 3): a chase camera's ball cast from the
/// vehicle toward where the camera wants to be stops before a wall mesh
/// and says how far it got. Oracle: hand-worked geometry: a ball of radius
/// 0.3 moving from (0, 1, 0) by (8, 1, 0) touches the wall at x = 5 when
/// its centre reaches x = 4.7; it stops one to one and a quarter slops
/// (0.003) before that, facing the wall's `-x` normal.
#[wasm_bindgen_test(unsupported = test)]
fn a_chase_camera_stops_before_a_wall_and_says_how_far_it_got() {
    let set = wall();
    let (vehicle, wanted) = (Vec3::new(0.0, 1.0, 0.0), Vec3::new(8.0, 2.0, 0.0));
    let motion = wanted - vehicle;
    let camera = Shape::Ball { radius: 0.3 };
    let hit = cast(&camera, vehicle, motion, &set, ALL_LAYERS)
        .unwrap()
        .expect("the wall is in the way");
    let reached = (vehicle + motion * hit.t).x;
    let slop = 0.003;
    assert!(
        reached >= 4.7 - 1.25 * slop - 1e-5 && reached <= 4.7 - slop + 1e-5,
        "{hit:?}: {reached}"
    );
    assert!((hit.normal - Vec3::NEG_X).length() < 1e-4, "{hit:?}");
    assert_eq!(hit.id, 9);
}

/// Game scenario (#924, item 3): a line of sight. Oracle: the geometry of
/// the wall at x = 5 up to y = 4: between (0, 1, 0) and (8, 1, 0) it
/// blocks the line where it crosses x = 5, at exactly 5/8 of it, facing
/// back along it; between (0, 5, 0) and (8, 5, 0) the line passes over it.
#[wasm_bindgen_test(unsupported = test)]
fn a_line_of_sight_is_blocked_by_a_wall_and_clear_over_it() {
    let set = wall();
    let blocked = ray(
        Vec3::new(0.0, 1.0, 0.0),
        Vec3::new(8.0, 0.0, 0.0),
        &set,
        ALL_LAYERS,
    )
    .unwrap()
    .expect("blocked");
    assert_eq!(
        (blocked.t, blocked.normal),
        (0.625, Vec3::NEG_X),
        "{blocked:?}"
    );
    let over = ray(
        Vec3::new(0.0, 5.0, 0.0),
        Vec3::new(8.0, 0.0, 0.0),
        &set,
        ALL_LAYERS,
    );
    assert_eq!(over, Ok(None));
}

/// Every collider `world` offers anywhere: its triangles.
fn every_triangle<W: World<Vec3>>(world: &W) -> Vec<Collider<Vec3>> {
    let mut found = Vec::new();
    let everywhere = Vec3::splat(1e6);
    world
        .colliders_near(-everywhere, everywhere, &mut |c| found.push(*c))
        .expect("a small surface");
    found
}

/// Defect: a world leaves out a triangle a query needed (one whose box,
/// grown by its rounding allowance, meets the region or the path), so a
/// result depends on the world. Oracle: a slice of the same triangles,
/// which offers every collider to every query: the seeded rays, overlaps,
/// casts and grounds against the bumpy mesh and a height field, near the
/// origin and moved by 2^17, digest the same as against the slice.
#[wasm_bindgen_test(unsupported = test)]
fn the_surface_worlds_answer_as_a_slice_of_their_own_triangles() {
    let far = Vec3::new(131_072.0, 0.0, 131_072.0);
    let mut rng = SplitMix64::new(0x0923_0008);
    let heights: Vec<f32> = (0..64).map(|_| pick(&mut rng, 9) as f32 * 0.25).collect();
    for offset in [Vec3::ZERO, far] {
        let set = bumpy_mesh(0x0923_0009, offset).set();
        let slice = every_triangle(&set);
        assert_eq!(slice.len(), set.len());
        assert_eq!(queries(&set, offset, 13), queries(&slice[..], offset, 13));
        let origin = offset + Vec3::new(-1.0, 0.0, -1.0);
        let field = HeightField::new(&heights, 8, 8, 0.75, origin, &[5]).unwrap();
        let slice = every_triangle(&field);
        assert_eq!(slice.len(), 2 * 7 * 7);
        assert_eq!(queries(&field, offset, 14), queries(&slice[..], offset, 14));
    }
}

/// Defect: a world finds a triangle by its bare box, so a query near a
/// face that rounds differently in world coordinates than relative to the
/// body misses it. Oracle: the duty every world has (`specs/physics.md`,
/// "Worlds"): offer every collider whose box, grown on each axis by
/// `4ε(|position| + half)`, meets the region. A triangle from x = 0 to
/// 200 000 (centre 100 000, half 100 000) has an allowance of about 0.095
/// on x, so a region from x = -0.09 to -0.05, beside its face at x = 0, is
/// offered it; a height field's cell from x = 0 to 100 000 has one of
/// about 0.048, so a region from -0.04 to -0.02 is offered both its
/// triangles. Each allowance is worked here from the corners. A height
/// field a million units out with cells a tenth across, where a sample's
/// position rounds by a sixteenth: a region beside each triangle's low-x
/// face, within that triangle's allowance (about 0.48, several cells), is
/// offered it.
#[wasm_bindgen_test(unsupported = test)]
fn a_region_within_a_triangles_rounding_allowance_is_offered_it() {
    let allowance = |lo: f32, hi: f32| {
        let (position, half) = ((lo + hi) * 0.5, (hi - lo) * 0.5);
        4.0 * f32::EPSILON * (position.abs() + half)
    };
    let offered = |world: &dyn World<Vec3>, from: f32, to: f32| {
        let mut count = 0;
        world
            .colliders_near(
                Vec3::new(from, -1.0, 1.0),
                Vec3::new(to, 1.0, 2.0),
                &mut |_| {
                    count += 1;
                },
            )
            .map(|()| count)
    };
    let corners = [
        Vec3::new(0.0, 0.0, 0.0),
        Vec3::new(200_000.0, 0.0, 0.0),
        Vec3::new(0.0, 0.0, 200_000.0),
    ];
    let set = TriangleSet::new(&corners, &[0, 1, 2], &[1]).unwrap();
    assert!(allowance(0.0, 200_000.0) > 0.09);
    assert_eq!(offered(&set, -0.09, -0.05), Ok(1));
    let field = HeightField::new(&[0.0; 4], 2, 2, 100_000.0, Vec3::ZERO, &[1]).unwrap();
    assert!(allowance(0.0, 100_000.0) > 0.04);
    assert_eq!(offered(&field, -0.04, -0.02), Ok(2));

    let origin = Vec3::new(1_000_000.3, 0.0, 0.0);
    let far = HeightField::new(
        &[0.0, 0.5, 0.25, 1.0, 0.0, 0.75, 0.5, 0.25, 1.0, 0.0],
        5,
        2,
        0.1,
        origin,
        &[1],
    )
    .unwrap();
    let triangles = every_triangle(&far);
    assert_eq!(triangles.len(), 8);
    for triangle in &triangles {
        let half = triangle.shape.half_extents();
        let low = triangle.position.x - half.x;
        let reach = 4.0 * f32::EPSILON * (triangle.position.x.abs() + half.x);
        let (from, to) = (low - 0.9 * reach, low - 0.8 * reach);
        let mut met = false;
        far.colliders_near(
            Vec3::new(from, -1.0, 0.0),
            Vec3::new(to, 2.0, 0.1),
            &mut |c| met |= c == triangle,
        )
        .unwrap();
        assert!(met, "{triangle:?} from {from} to {to}");
    }
}
