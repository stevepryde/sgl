//! The Khronos NormalTangentTest and NormalTangentMirrorTest (glTF-Sample-
//! Assets e124e8d, `tests/fixtures/gltf/`) through production shading:
//! raster's `surface_normal` with actual raster derivatives and `ray_normal`
//! at actual BLAS hits, from the front and from the back. Each asset sets
//! flat quads beside dome-shaped cells of real geometry, the quads' normal
//! map baked from a dome (all alike) under rotated and mirrored UVs (the
//! Mirror asset with authored tangents whose handedness follows the
//! mirroring), so a quad lit correctly shows a dome. The oracle is the
//! asset's own geometry: at a point of a quad, the interpolated vertex
//! normal of a dome in its row at the same offset from its centre, reversed
//! on the back. No frame, sign or
//! channel convention enters it; a green channel read the wrong way round
//! puts the normal up to about 106° off. Plausible defects: a bitangent
//! toward increasing V (#894), a frame that loses mirrored handedness, an
//! authored frame read with the wrong sign of w, a back face not reversed.
use super::normal_probe::{self, Observe, Probe};
use super::*;
use crate::asset::{Asset, CpuMesh};
use crate::scene::materials::maps::InEffect;
use crate::shading::bind::BindingTier;
use glam::{Mat4, Vec2, Vec3};
use std::collections::HashMap;
use std::path::Path;

/// The orthographic views' half extent in metres, about the origin, and
/// each view's side in pixels: the front view on the left of the target,
/// the back (seen from -Z, X reversed) on the right.
const HALF: f32 = 1.5;
const SIDE: u32 = 512;
/// The views' vertex stage: the front view, then the back.
fn vertex() -> String {
    format!(
        r#"
@vertex fn fixture_vs(v:FixtureVertex,@builtin(instance_index) view:u32)->Fragment {{
 var o:Fragment;
 let model=objects[0].model;
 o.world=(model*vec4(v.position,1.)).xyz;
 let side=select(1.,-1.,view==1u);
 o.clip=vec4(side*(o.world.x/{HALF:?}*.5-.5),o.world.y/{HALF:?},0.,1.);
 o.normal=object_normal(model,v.normal);o.tangent=object_tangent(model,v.tangent,v.normal);o.uv=v.uv;o.color=v.color;
 return o;
}}
"#
    )
}
/// The tolerance comes from the map: an 8-bit channel's step is 2/255 of
/// a unit component, about 0.45° of a normal, and the bake rounds to it;
/// two steps allow for the twin's own interpolation and filtering.
const TOLERANCE_DEGREES: f32 = 1.;

/// A connected piece of the mesh 0.3 m square: a dome of real geometry, or
/// a flat quad (every normal alike) whose normal map was baked from one.
struct Cell {
    triangles: Vec<[usize; 3]>,
    centre: Vec2,
    flat: bool,
}

/// `mesh`'s cells: its pieces connected by shared vertices or positions,
/// those 0.3 m square (the labels are not).
fn cells(mesh: &CpuMesh) -> Vec<Cell> {
    let mut parent: Vec<usize> = (0..mesh.vertices.len()).collect();
    fn find(parent: &mut [usize], i: usize) -> usize {
        let mut root = i;
        while parent[root] != root {
            root = parent[root];
        }
        parent[i] = root;
        root
    }
    let join = |parent: &mut Vec<usize>, a: usize, b: usize| {
        let (a, b) = (find(parent, a), find(parent, b));
        parent[a] = b;
    };
    for triangle in mesh.indices.chunks_exact(3) {
        join(&mut parent, triangle[0] as usize, triangle[1] as usize);
        join(&mut parent, triangle[0] as usize, triangle[2] as usize);
    }
    let mut at = HashMap::new();
    for (i, vertex) in mesh.vertices.iter().enumerate() {
        let key = vertex.position.map(f32::to_bits);
        if let Some(&other) = at.get(&key) {
            join(&mut parent, i, other);
        } else {
            at.insert(key, i);
        }
    }
    let mut pieces: HashMap<usize, Vec<[usize; 3]>> = HashMap::new();
    for triangle in mesh.indices.chunks_exact(3) {
        let triangle = [0, 1, 2].map(|k| triangle[k] as usize);
        pieces
            .entry(find(&mut parent, triangle[0]))
            .or_default()
            .push(triangle);
    }
    pieces
        .into_values()
        .filter_map(|triangles| {
            let corners = triangles.iter().flatten().map(|&i| &mesh.vertices[i]);
            let (min, max) = corners.clone().fold(
                (Vec2::INFINITY, Vec2::NEG_INFINITY),
                |(min, max), vertex| {
                    let p = Vec2::new(vertex.position[0], vertex.position[1]);
                    (min.min(p), max.max(p))
                },
            );
            let first = Vec3::from_array(mesh.vertices[triangles[0][0]].normal);
            let flat = corners
                .clone()
                .all(|vertex| Vec3::from_array(vertex.normal).distance(first) < 1e-5);
            ((max - min - Vec2::splat(0.3)).abs().max_element() < 2e-3).then(|| Cell {
                triangles,
                centre: (min + max) / 2.,
                flat,
            })
        })
        .collect()
}

/// `dome`'s interpolated vertex normal above `point`, a height field over
/// the XY plane.
fn dome_normal(mesh: &CpuMesh, dome: &Cell, point: Vec2) -> Option<Vec3> {
    dome.triangles.iter().find_map(|triangle| {
        let [a, b, c] = triangle.map(|i| Vec2::from_slice(&mesh.vertices[i].position[..2]));
        let area = (b - a).perp_dot(c - a);
        let wb = (point - a).perp_dot(c - a) / area;
        let wc = (b - a).perp_dot(point - a) / area;
        let weights = [1. - wb - wc, wb, wc];
        weights.iter().all(|&w| w >= -1e-6).then(|| {
            triangle
                .iter()
                .zip(weights)
                .map(|(&i, w)| Vec3::from_array(mesh.vertices[i].normal) * w)
                .sum::<Vec3>()
                .normalize()
        })
    })
}

/// The front view's pixel nearest `point`, and the point at its centre.
fn front_pixel(point: Vec2) -> ([u32; 2], Vec2) {
    let side = SIDE as f32;
    let x = ((point.x / HALF * 0.5 + 0.5) * side - 0.5).round();
    let y = ((1. - point.y / HALF) * 0.5 * side - 0.5).round();
    let centre = Vec2::new(
        ((x + 0.5) / side - 0.5) * 2. * HALF,
        (1. - 2. * (y + 0.5) / side) * HALF,
    );
    ([x as u32, y as u32], centre)
}

/// Shades `asset`, anisotropic by `anisotropy` (which selects its authored
/// tangent frame), and compares each quad with a dome in its row at 24
/// points on the dome's curved part (4, 6 and 8 cm from its centre, where
/// it tilts about 24°, 37° and 53°; its rim beyond is flat), from the front
/// and the back, in raster and at ray hits.
fn lit_as_its_geometry(file: &str, anisotropy: f32, domes: usize, quads: usize) {
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    let mut asset: Asset = crate::asset::load(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/gltf")
            .join(file),
    )
    .unwrap_or_else(|error| panic!("{file}: {error}"));
    assert_eq!(asset.meshes.len(), 1, "{file}: one primitive");
    asset.materials[0].anisotropy_strength = anisotropy;
    let mesh = &asset.meshes[0];
    let (dome_cells, quad_cells): (Vec<_>, Vec<_>) =
        cells(mesh).into_iter().partition(|cell| !cell.flat);
    assert_eq!(
        (dome_cells.len(), quad_cells.len()),
        (domes, quads),
        "{file}: its domes and quads"
    );
    // Each quad's points, each with a dome's normal there: every dome is
    // alike, the nearest in the quad's row is taken.
    let mut points = Vec::new();
    for quad in &quad_cells {
        let dome = dome_cells
            .iter()
            .filter(|dome| (dome.centre.y - quad.centre.y).abs() < 0.01)
            .min_by(|a, b| {
                (a.centre.x - quad.centre.x)
                    .abs()
                    .total_cmp(&(b.centre.x - quad.centre.x).abs())
            })
            .expect("a dome in the quad's row");
        for radius in [0.04, 0.06, 0.08] {
            for k in 0..8 {
                let angle = (k as f32 + 0.5) * std::f32::consts::FRAC_PI_4;
                let (pixel, point) = front_pixel(quad.centre + radius * Vec2::from_angle(angle));
                let normal = dome_normal(mesh, dome, point - quad.centre + dome.centre)
                    .expect("the dome covers the point");
                points.push((pixel, point, normal));
            }
        }
    }
    let rays: Vec<[f32; 8]> = points
        .iter()
        .flat_map(|&(_, point, _)| {
            [1f32, -1.].map(|side| [point.x, point.y, side, 0.001, 0., 0., -side, 2.])
        })
        .collect();
    let material = &asset.materials[0];
    let maps = InEffect::of(material, BindingTier::Extended);
    let relief = material.normal_texture.as_ref().unwrap().image;
    let observed = normal_probe::observe(
        &device,
        &queue,
        &Probe {
            asset: &asset,
            world: Mat4::IDENTITY,
            material: MaterialUniform::new(
                &crate::SurfaceMaterial::authored(material),
                maps.maps(),
                &maps.samplers(),
                &maps.uvs(),
            ),
            relief,
            coat: relief,
            normal_maps_enabled: true,
            observe: Observe::Base,
            vertex: &vertex(),
            views: 2,
            size: [2 * SIDE, SIDE],
            rays: &rays,
        },
    );
    let mut worst = (0f32, String::new());
    for (i, &([x, y], point, normal)) in points.iter().enumerate() {
        for (k, side) in [1f32, -1.].into_iter().enumerate() {
            let column = if side > 0. { x } else { 2 * SIDE - 1 - x };
            let expected = normal * side;
            for (path, actual) in [
                ("raster", observed.raster[(y * 2 * SIDE + column) as usize]),
                ("ray", observed.rays[2 * i + k]),
            ] {
                let shaded = Vec3::from_slice(&actual[..3]);
                let degrees = shaded
                    .normalize()
                    .dot(expected)
                    .clamp(-1., 1.)
                    .acos()
                    .to_degrees();
                assert_eq!(actual[3], side, "{file} {path} at {point}: the face");
                if degrees.is_nan() || degrees > worst.0 {
                    worst = (
                        degrees,
                        format!("{path} at {point} side {side}: {shaded} for {expected}"),
                    );
                }
            }
        }
    }
    eprintln!("{file}: worst {:.3}° ({})", worst.0, worst.1);
    assert!(
        worst.0 <= TOLERANCE_DEGREES,
        "{file}: {:.2}° from its geometry, {}",
        worst.0,
        worst.1
    );
}

// No tangents: the derivative frame in raster, the triangle frame at hits.
#[test]
fn normal_tangent_test_is_lit_as_its_geometry() {
    lit_as_its_geometry("NormalTangentTest/glTF/NormalTangentTest.gltf", 0., 15, 15);
}

// Its tangents unused (an isotropic material): the derivative and triangle
// frames under mirrored UVs.
#[test]
fn normal_tangent_mirror_test_is_lit_as_its_geometry() {
    lit_as_its_geometry(
        "NormalTangentMirrorTest/glTF/NormalTangentMirrorTest.gltf",
        0.,
        10,
        30,
    );
}

// Its authored tangents: an anisotropic material takes its normal map on
// them, whose slight strength leaves the normal as it is.
#[test]
fn normal_tangent_mirror_test_is_lit_as_its_geometry_on_its_tangents() {
    lit_as_its_geometry(
        "NormalTangentMirrorTest/glTF/NormalTangentMirrorTest.gltf",
        1e-3,
        10,
        30,
    );
}
