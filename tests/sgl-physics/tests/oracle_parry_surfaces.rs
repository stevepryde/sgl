//! A second engine as the oracle for surfaces (#923, #924 item 4): parry
//! (`parry3d`, Apache-2.0), which shares no code with `sgl-physics`. Its
//! `TriMesh` and `HeightField` hold the same triangles as a `TriangleSet`
//! and a `HeightField` built from the same numbers (parry splits a height
//! field's cells along the same diagonal as `Box3D`). Over seeded rays and
//! ball casts against a small random mesh and a small random height
//! field, a ray must meet the surface where parry's does, to the rounding
//! of the coordinates involved, and a ball cast within the contact band
//! the crate states. Native only, as `oracle_parry.rs`.
#![cfg(not(target_arch = "wasm32"))]
#![allow(clippy::cast_precision_loss)]

use parry3d::math::Pose;
use parry3d::query as query3;
use parry3d::query::Ray;
use parry3d::shape as shape3;
use parry3d::utils::Array2;

use sgl_core::SplitMix64;
use sgl_core::math::Vec3;
use sgl_physics::{ALL_LAYERS, HeightField, Shape, TriangleSet, World, cast, ray};

/// How far the crate's answer may stray beyond its band, in these scenes
/// of a few units: the rounding of a fraction or a point a few units long
/// and of parry's closest point on a triangle, well under a
/// ten-thousandth, a seventieth of the smallest ball slop here (0.0015).
const TOLERANCE: f32 = 1e-4;

/// How far apart two exact answers for a ray may be, in units of the
/// largest coordinate the ray involves: eight units in the last place. A
/// ray against a triangle is exact in the crate and in parry (a closed form
/// in `f32`, `local_ray_intersection_with_triangle`, with no tolerance:
/// parry's precision is its rounding), so each lands within a few units in
/// the last place of the surface; the test's own `from + motion * t` adds
/// one or two more. The first run's worst was 1.84 units. The slop the
/// crate's rays had before they were exact was a hundredth of a triangle's
/// largest half-extent, here at least 0.0025: thousands of these units.
const RAY_ROUNDING: f32 = 8.0 * f32::EPSILON;

/// How far parry's distance from a ball of `radius` to a triangle may lie
/// above the true one: parry measures it by GJK between the ball's centre
/// and the triangle, which stops once its bounds agree to `sqrt(10 ε)` of
/// that distance, the radius and the gap (`parry_bound` in
/// `oracle_parry.rs`). Its distance is never below the true one.
fn parry_bound(radius: f32) -> f32 {
    (10.0 * f32::EPSILON).sqrt() * (radius + 0.01)
}

fn draw(rng: &mut SplitMix64, low: f32, high: f32) -> f32 {
    low + (high - low) * ((rng.next_u64() >> 40) as f32 / 16_777_216.0)
}

/// What the comparison found.
#[derive(Default, Debug)]
struct Tally {
    rays: usize,
    ray_hits: usize,
    /// The largest distance between the crate's and parry's hits along
    /// the normal, or from the crate's hit to the surface, in units of
    /// `f32::EPSILON` times the ray's largest coordinate.
    worst_ray: f32,
    casts: usize,
    cast_hits: usize,
    worst_cast: f32,
}

/// A surface as both engines hold it.
struct Scene {
    ours: Box<dyn World<Vec3>>,
    theirs: Box<dyn shape3::Shape>,
    /// What parry measures distances against: `theirs`, or for a height
    /// field (which parry does not measure) a mesh of parry's own
    /// triangles of it.
    measured: Box<dyn shape3::Shape>,
    pose: Pose,
    /// The surface's footprint on the xz plane, low corner and high.
    footprint: (Vec3, Vec3),
}

/// A point over `scene`'s footprint, grown by `margin` on x and z, between
/// heights `low` and `high`.
fn over(scene: &Scene, rng: &mut SplitMix64, margin: f32, low: f32, high: f32) -> Vec3 {
    let (min, max) = scene.footprint;
    Vec3::new(
        draw(rng, min.x - margin, max.x + margin),
        draw(rng, low, high),
        draw(rng, min.z - margin, max.z + margin),
    )
}

/// A 6 x 6 grid of unit cells with seeded heights and a few loose
/// triangles above it.
fn mesh_scene(rng: &mut SplitMix64) -> Scene {
    let mut positions = Vec::new();
    for z in 0..7 {
        for x in 0..7 {
            positions.push(Vec3::new(x as f32, draw(rng, 0.0, 1.5), z as f32));
        }
    }
    let mut triangles: Vec<[u32; 3]> = Vec::new();
    for z in 0..6u32 {
        for x in 0..6u32 {
            let at = |dx: u32, dz: u32| (z + dz) * 7 + x + dx;
            triangles.push([at(0, 0), at(1, 0), at(1, 1)]);
            triangles.push([at(0, 0), at(1, 1), at(0, 1)]);
        }
    }
    for _ in 0..6 {
        let base = Vec3::new(
            draw(rng, 0.5, 5.5),
            draw(rng, 2.5, 4.0),
            draw(rng, 0.5, 5.5),
        );
        let first = u32::try_from(positions.len()).unwrap();
        for _ in 0..3 {
            positions.push(
                base + Vec3::new(
                    draw(rng, -1.0, 1.0),
                    draw(rng, -0.5, 0.5),
                    draw(rng, -1.0, 1.0),
                ),
            );
        }
        triangles.push([first, first + 1, first + 2]);
    }
    let indices: Vec<u32> = triangles.iter().flatten().copied().collect();
    let ids: Vec<u64> = (0..triangles.len() as u64).collect();
    let set = TriangleSet::new(&positions, &indices, &ids).expect("a sound mesh");
    assert_eq!(set.degenerate(), 0);
    let theirs = shape3::TriMesh::new(positions, triangles).expect("parry takes the mesh");
    Scene {
        ours: Box::new(set),
        measured: Box::new(theirs.clone()),
        theirs: Box::new(theirs),
        pose: Pose::IDENTITY,
        footprint: (Vec3::ZERO, Vec3::new(6.0, 0.0, 6.0)),
    }
}

/// A 7 x 6 grid of seeded heights, 0.75 apart, from `(-1, 0.5, -2)`.
fn field_scene(rng: &mut SplitMix64) -> Scene {
    let (columns, rows, cell) = (7usize, 6usize, 0.75f32);
    let origin = Vec3::new(-1.0, 0.5, -2.0);
    let heights: Vec<f32> = (0..columns * rows).map(|_| draw(rng, -0.5, 1.0)).collect();
    let ours = HeightField::new(&heights, columns, rows, cell, origin, &[1]).expect("a sound grid");
    // Parry's grid: row `i` along z, column `j` along x, spanning
    // `[-0.5, 0.5]` of its scale about its pose.
    let mut grid = Array2::repeat(rows, columns, 0.0f32);
    for row in 0..rows {
        for column in 0..columns {
            grid[(row, column)] = heights[row * columns + column];
        }
    }
    let size = Vec3::new((columns - 1) as f32 * cell, 1.0, (rows - 1) as f32 * cell);
    let theirs = shape3::HeightField::new(grid, size);
    let centre = origin + Vec3::new(size.x * 0.5, 0.0, size.z * 0.5);
    let corners: Vec<[Vec3; 3]> = theirs.triangles().map(|t| [t.a, t.b, t.c]).collect();
    let positions: Vec<Vec3> = corners.iter().flatten().copied().collect();
    let indices = (0..u32::try_from(corners.len()).unwrap())
        .map(|t| [3 * t, 3 * t + 1, 3 * t + 2])
        .collect();
    let measured = shape3::TriMesh::new(positions, indices).expect("parry's own triangles");
    Scene {
        ours: Box::new(ours),
        theirs: Box::new(theirs),
        measured: Box::new(measured),
        pose: Pose::from_translation(centre),
        footprint: (origin, origin + size),
    }
}

/// How far the point or ball at `at` is from the surface, by parry.
fn gap(scene: &Scene, at: Vec3, radius: f32) -> f32 {
    let ball = shape3::Ball::new(radius);
    query3::distance(
        &scene.pose,
        &*scene.measured,
        &Pose::from_translation(at),
        &ball,
    )
    .expect("parry measures a ball against a mesh and a height field")
    .distance
}

/// Rays from a box around the scene toward points in it. The crate meets
/// the surface exactly when parry does, and where it does: its hit and
/// parry's lie within [`RAY_ROUNDING`] of the ray's largest coordinate of
/// each other along the normal, and parry puts the crate's hit that close
/// to the surface.
///
/// Parry casts against `measured`: for a height field, a `TriMesh` of
/// parry's own triangles of it. Parry 0.31.1's `HeightField` ray cast
/// misses a ray its own triangles meet: in the second field of
/// [`rays_and_ball_casts_against_a_height_field_agree_with_parry`], the
/// ray from (1.4818087, 4.90184, 3.9017124) by (-4.06292, -7.7515244,
/// -3.8969166) meets the field's last row of cells at t = 0.5540239 by
/// parry's `TriMesh` (0.554024 by the crate, one unit in the last place
/// apart, with the same normal), and parry's `HeightField` reports nothing.
fn compare_rays(scene: &Scene, rng: &mut SplitMix64, tally: &mut Tally) {
    for _ in 0..200 {
        let from = over(scene, rng, 3.0, 3.0, 7.0);
        let aim = over(scene, rng, 0.5, -0.5, 2.0);
        let motion = (aim - from) * draw(rng, 0.8, 1.6);
        let ours = ray(from, motion, &*scene.ours, ALL_LAYERS).expect("answered");
        let theirs = scene.measured.cast_ray_and_get_normal(
            &scene.pose,
            &Ray::new(from, motion),
            1.0,
            false,
        );
        tally.rays += 1;
        let unit = f32::EPSILON * from.abs().max((from + motion).abs()).max_element();
        match (ours, theirs) {
            (Some(hit), Some(found)) => {
                tally.ray_hits += 1;
                let at = from + motion * hit.t;
                let along = (at - (from + motion * found.time_of_impact))
                    .dot(hit.normal)
                    .abs();
                let apart = gap(scene, at, 0.0);
                tally.worst_ray = tally.worst_ray.max(along.max(apart) / unit);
                assert!(
                    along <= RAY_ROUNDING / f32::EPSILON * unit
                        && apart <= RAY_ROUNDING / f32::EPSILON * unit,
                    "ray from {from} by {motion}: {hit:?} is {along} from parry's hit at {} \
                     along the normal and {apart} off the surface",
                    found.time_of_impact
                );
            }
            (None, None) => {}
            (ours, theirs) => {
                panic!("ray from {from} by {motion}: the crate meets {ours:?}, parry {theirs:?}")
            }
        }
    }
}

/// Ball casts from clear starts: wherever parry meets the surface the
/// crate meets it no later; wherever the crate meets it, the ball there is
/// clear of the surface by parry and within the band of it.
fn compare_casts(scene: &Scene, rng: &mut SplitMix64, tally: &mut Tally) {
    for _ in 0..120 {
        let radius = draw(rng, 0.15, 0.5);
        let ball = Shape::Ball { radius };
        let from = over(scene, rng, 2.0, 2.5, 6.0);
        if gap(scene, from, radius) < 0.1 {
            continue;
        }
        let aim = over(scene, rng, 0.0, -0.5, 2.0);
        let motion = (aim - from) * draw(rng, 0.8, 1.6);
        let ours = cast(&ball, from, motion, &*scene.ours, ALL_LAYERS).expect("answered");
        let options = query3::ShapeCastOptions {
            max_time_of_impact: 1.0,
            ..query3::ShapeCastOptions::default()
        };
        let theirs = query3::cast_shapes(
            &scene.pose,
            Vec3::ZERO,
            &*scene.theirs,
            &Pose::from_translation(from),
            motion,
            &shape3::Ball::new(radius),
            options,
        )
        .expect("parry casts a ball against a mesh and a height field");
        tally.casts += 1;
        // The ball's slop: a hundredth of its radius, the smaller extent.
        let band = 1.25 * radius / 100.0 + parry_bound(radius) + TOLERANCE;
        if let Some(hit) = ours {
            let apart = gap(scene, from + motion * hit.t, radius);
            tally.worst_cast = tally.worst_cast.max(apart / (radius / 100.0));
            assert!(
                apart > 0.0 && apart <= band,
                "ball {radius} from {from} by {motion}: {hit:?} leaves it {apart} off"
            );
        }
        match (ours, theirs) {
            (Some(hit), Some(found)) => {
                tally.cast_hits += 1;
                assert!(
                    hit.t <= found.time_of_impact + TOLERANCE / motion.length(),
                    "ball {radius} from {from} by {motion}: t {} after parry's {}",
                    hit.t,
                    found.time_of_impact
                );
            }
            (None, Some(found)) => panic!(
                "ball {radius} from {from} by {motion}: parry meets the surface at {}",
                found.time_of_impact
            ),
            (Some(_) | None, None) => {}
        }
    }
}

/// Defect: the crate's triangles, met exactly by a ray and through the
/// kernel by a ball, and offered by its tree, are somewhere other than the
/// mesh the game gave: a ray or a ball passes through it, or stops off it
/// (a ray by more than rounding). Oracle: parry's `TriMesh`, built from the
/// same positions and indices.
#[test]
fn rays_and_ball_casts_against_a_mesh_agree_with_parry() {
    let mut rng = SplitMix64::new(0x0923_1001);
    let mut tally = Tally::default();
    for _ in 0..2 {
        let scene = mesh_scene(&mut rng);
        compare_rays(&scene, &mut rng, &mut tally);
        compare_casts(&scene, &mut rng, &mut tally);
    }
    assert!(tally.ray_hits > 150 && tally.cast_hits > 150, "{tally:?}");
}

/// Defect: the crate's height field is laid out, split or walked
/// differently from the grid the game gave. Oracle: parry's `HeightField`
/// of the same heights, which splits each cell along the same diagonal.
#[test]
fn rays_and_ball_casts_against_a_height_field_agree_with_parry() {
    let mut rng = SplitMix64::new(0x0923_1002);
    let mut tally = Tally::default();
    for _ in 0..2 {
        let scene = field_scene(&mut rng);
        compare_rays(&scene, &mut rng, &mut tally);
        compare_casts(&scene, &mut rng, &mut tally);
    }
    assert!(tally.ray_hits > 150 && tally.cast_hits > 150, "{tally:?}");
}
