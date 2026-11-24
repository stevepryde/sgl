//! The Khronos NormalTangentTest and NormalTangentMirrorTest (glTF-Sample-
//! Assets e124e8d, `tests/fixtures/gltf/`) through production shading:
//! raster's `surface_normal` with actual raster derivatives, seen through
//! an orthographic camera or a probe face's or planar reflector's, and
//! `ray_normal` at actual BLAS hits, from the front and from the back. Each asset sets
//! flat quads beside dome-shaped cells of real geometry, the quads' normal
//! map baked from a dome (all alike) under rotated and mirrored UVs (the
//! Mirror asset with authored tangents whose handedness follows the
//! mirroring), so a quad lit correctly shows a dome. The oracle is the
//! asset's own geometry: at a point of a quad, the interpolated vertex
//! normal of a dome in its row at the same offset from its centre, reversed
//! on the back, and through the pose's normal matrix where the mesh is
//! posed. No frame, sign or channel convention enters it; a green channel
//! read the wrong way round puts the normal up to about 106° off.
//! Plausible defects: a bitangent toward increasing V (#894), a frame that
//! loses mirrored handedness, an authored frame read with the wrong sign of
//! w, a back face not reversed, a mirroring pose's side or handedness not
//! followed, a view that mirrors the image.
use super::normal_probe::{self, Observe, Probe};
use super::*;
use crate::asset::{Asset, CpuMesh};
use crate::scene::materials::maps::InEffect;
use crate::shading::bind::BindingTier;
use crate::view::ViewData;
use crate::view::reflected_camera::{self, Rect};
use glam::{Mat3, Mat4, Vec2, Vec3, Vec4, camera};
use std::collections::HashMap;
use std::path::Path;

/// The orthographic views' half extent in metres, about the origin, and
/// each view's side in pixels.
const HALF: f32 = 1.5;
const SIDE: u32 = 512;

/// Two views, each clip space from the world: the first sees the assets'
/// front (+Z) and draws into the target's left half, the second their back
/// and draws into its right half.
type Views = [Mat4; 2];

/// Orthographic views from +Z (X right, Y up) and from -Z (X reversed),
/// over HALF metres about the origin, device depth within 0..1 over the
/// assets' few centimetres of relief.
fn orthographic() -> Views {
    [1f32, -1.].map(|side| {
        Mat4::from_cols(
            Vec4::new(side / HALF, 0., 0., 0.),
            Vec4::new(0., 1. / HALF, 0., 0.),
            Vec4::new(0., 0., side * 0.1, 0.),
            Vec4::new(0., 0., 0.5, 1.),
        )
    })
}

/// The faces of probes captured 2 m before the assets (their -Z face) and
/// 2 m behind them (their +Z face): production's capture views.
fn probe_faces() -> Views {
    [(Vec3::new(0., 0., 2.), 4), (Vec3::new(0., 0., -2.), 5)]
        .map(|(centre, face)| ViewData::probe_face(centre, face).view_projection())
}

/// The cameras of planar reflectors that show a viewer the assets: a
/// viewer 1 m before them facing away, toward a mirror 1 m further on,
/// and one behind them likewise. Production's reflected camera of each, at
/// the mirrored eye 3 m from the assets, before its near plane is clipped.
fn reflected() -> Views {
    [1f32, -1.].map(|side| {
        let eye = Vec3::new(0., 0., side);
        let viewer = crate::Camera {
            view: camera::rh::view::look_at_mat4(eye, eye * 2., Vec3::Y),
            projection: crate::perspective(1., 1., 0.1),
            eye,
        };
        let mirror = reflected_camera::camera(&viewer, Vec4::new(0., 0., -side, 2.), Rect::FULL);
        mirror.projection * mirror.view
    })
}

/// A view's offset into its half of the target, in its clip x.
fn offset(view: usize) -> f32 {
    if view == 0 { -0.5 } else { 0.5 }
}

/// The vertex stage of `views`, each into its half of the target.
fn vertex(views: Views) -> String {
    let wgsl = |m: Mat4| {
        let columns = m.to_cols_array().map(|value| format!("{value:?}"));
        format!("mat4x4<f32>({})", columns.join(","))
    };
    format!(
        r#"
@vertex fn fixture_vs(v:FixtureVertex,@builtin(instance_index) view:u32)->Fragment {{
 var o:Fragment;
 let model=objects[0].model;
 o.world=(model*vec4(v.position,1.)).xyz;
 var clip_from_world={front};
 if view==1u {{
  clip_from_world={back};
 }}
 let clip=clip_from_world*vec4(o.world,1.);
 o.clip=vec4(clip.x*.5+select({left:?},{right:?},view==1u)*clip.w,clip.yzw);
 o.normal=object_normal(model,v.normal);o.tangent=object_tangent(model,v.tangent,v.normal);o.uv=v.uv;o.color=v.color;
 return o;
}}
fn fixture_front(raster:bool)->bool {{
 return object_front(0u,raster);
}}
"#,
        front = wgsl(views[0]),
        back = wgsl(views[1]),
        left = offset(0),
        right = offset(1),
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

/// The target's pixel nearest where view `view` of `views` shows the world
/// point `point`.
fn pixel(views: Views, view: usize, point: Vec3) -> [u32; 2] {
    let ndc = views[view].project_point3(point);
    let side = SIDE as f32;
    let x = ((ndc.x * 0.5 + offset(view) + 1.) * side - 0.5).round();
    let y = ((1. - ndc.y) * 0.5 * side - 0.5).round();
    [x as u32, y as u32]
}

/// Where the centre of the target's `pixel` sees, through view `view` of
/// `views`, the plane through `on` facing `facing`.
fn seen(views: Views, view: usize, [x, y]: [u32; 2], on: Vec3, facing: Vec3) -> Vec3 {
    let side = SIDE as f32;
    let ndc = Vec2::new(
        ((x as f32 + 0.5) / side - 1. - offset(view)) * 2.,
        1. - 2. * (y as f32 + 0.5) / side,
    );
    let world_from_clip = views[view].inverse();
    let [near, far] = [0.75, 0.25].map(|depth| world_from_clip.project_point3(ndc.extend(depth)));
    near + (far - near) * (on - near).dot(facing) / (far - near).dot(facing)
}

/// Which frame a test shades a Khronos asset's normal map on.
#[derive(Clone, Copy, PartialEq)]
enum Frames {
    /// Each triangle's coordinates': its tangents removed.
    Triangle,
    /// Its vertex frames as loaded: authored, or generated where it has
    /// none (`asset::LoadOptions::tangents`).
    Vertex,
}

/// A pose that mirrors: the mesh mirrored in X, turned about Z and scaled
/// by 0.75, which keeps every quad in the views (the assets reach 1.86 m
/// from the origin) and moves no point's X and Y with its Z.
fn mirroring() -> Mat4 {
    Mat4::from_rotation_z(0.6) * Mat4::from_scale(Vec3::new(-0.75, 0.75, 0.75))
}

/// Shades `file` on `frames`, posed by `pose` (a similarity that keeps the
/// XY plane facing Z), and compares each quad with a dome in its row at 24
/// points on the dome's curved part (4, 6 and 8 cm from its centre, where
/// it tilts about 24°, 37° and 53°; its rim beyond is flat), from the front
/// and the back: in raster through `views` and at ray hits along Z.
fn lit_as_its_geometry(
    file: &str,
    frames: Frames,
    pose: Mat4,
    views: Views,
    domes: usize,
    quads: usize,
) {
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
    for vertex in &mut asset.meshes[0].vertices {
        match frames {
            Frames::Triangle => vertex.tangent = [0.; 4],
            Frames::Vertex => assert_ne!(vertex.tangent, [0.; 4], "{file}: a vertex frame"),
        }
    }
    let mesh = &asset.meshes[0];
    let (dome_cells, quad_cells): (Vec<_>, Vec<_>) =
        cells(mesh).into_iter().partition(|cell| !cell.flat);
    assert_eq!(
        (dome_cells.len(), quad_cells.len()),
        (domes, quads),
        "{file}: its domes and quads"
    );
    // Each quad's points, each with a dome's normal there through the
    // pose: every dome is alike, the nearest in the quad's row is taken.
    // The rays meet each point; each view's pixel nearest it sees the
    // point at its centre.
    let inverse = pose.inverse();
    let normal_matrix = Mat3::from_mat4(pose).inverse().transpose();
    let facing = (normal_matrix * Vec3::Z).normalize();
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
        let z = mesh.vertices[quad.triangles[0][0]].position[2];
        let on = pose.transform_point3(quad.centre.extend(z));
        let normal = |at: Vec2| {
            let normal = dome_normal(mesh, dome, at - quad.centre + dome.centre)
                .expect("the dome covers the point");
            (normal_matrix * normal).normalize()
        };
        for radius in [0.04, 0.06, 0.08] {
            for k in 0..8 {
                let angle = (k as f32 + 0.5) * std::f32::consts::FRAC_PI_4;
                let at = quad.centre + radius * Vec2::from_angle(angle);
                let point = pose.transform_point3(at.extend(z));
                let pixels = [0, 1].map(|view| {
                    let pixel = pixel(views, view, point);
                    let centre = seen(views, view, pixel, on, facing);
                    (pixel, normal(inverse.transform_point3(centre).truncate()))
                });
                points.push((point, normal(at), pixels));
            }
        }
    }
    let rays: Vec<[f32; 8]> = points
        .iter()
        .flat_map(|&(point, _, _)| {
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
            world: pose,
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
            vertex: &vertex(views),
            views: 2,
            size: [2 * SIDE, SIDE],
            rays: &rays,
        },
    );
    let mut worst = (0f32, String::new());
    for (i, &(point, normal, pixels)) in points.iter().enumerate() {
        for (k, side) in [1f32, -1.].into_iter().enumerate() {
            let ([x, y], seen) = pixels[k];
            for (path, actual, expected) in [
                ("raster", observed.raster[(y * 2 * SIDE + x) as usize], seen),
                ("ray", observed.rays[2 * i + k], normal),
            ] {
                let expected = expected * side;
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

// Its tangents generated at load (it carries none): the vertex frames.
#[test]
fn normal_tangent_test_is_lit_as_its_geometry_on_generated_frames() {
    lit_as_its_geometry(
        "NormalTangentTest/glTF/NormalTangentTest.gltf",
        Frames::Vertex,
        Mat4::IDENTITY,
        orthographic(),
        15,
        15,
    );
}

// The derivative frame in raster and the triangle frame at hits.
#[test]
fn normal_tangent_test_is_lit_as_its_geometry_on_triangle_frames() {
    lit_as_its_geometry(
        "NormalTangentTest/glTF/NormalTangentTest.gltf",
        Frames::Triangle,
        Mat4::IDENTITY,
        orthographic(),
        15,
        15,
    );
}

// Its authored tangents, whose handedness follows the mirroring.
#[test]
fn normal_tangent_mirror_test_is_lit_as_its_geometry_on_its_tangents() {
    lit_as_its_geometry(
        "NormalTangentMirrorTest/glTF/NormalTangentMirrorTest.gltf",
        Frames::Vertex,
        Mat4::IDENTITY,
        orthographic(),
        10,
        30,
    );
}

// The derivative and triangle frames under mirrored coordinates.
#[test]
fn normal_tangent_mirror_test_is_lit_as_its_geometry_on_triangle_frames() {
    lit_as_its_geometry(
        "NormalTangentMirrorTest/glTF/NormalTangentMirrorTest.gltf",
        Frames::Triangle,
        Mat4::IDENTITY,
        orthographic(),
        10,
        30,
    );
}

// A mirrored instance (a game's pose or EXT_mesh_gpu_instancing's whose
// determinant is negative), on each frame of each asset: raster's winding
// reverses, so a pass takes the material's side through the pose
// (object_front), a vertex frame's handedness flips with it (object_tangent,
// and the hit decode's), and the derivative and triangle frames are built
// on the posed surface. Plausible defects: raster's side, or a vertex
// frame's handedness in raster or at a hit, not following the mirror; a
// frame measured against the mirrored winding. The oracle is the domes'
// geometry under the same pose: their normals through its normal matrix.
#[test]
fn khronos_assets_are_lit_as_their_geometry_on_a_mirrored_instance() {
    for (file, domes, quads) in [
        ("NormalTangentTest/glTF/NormalTangentTest.gltf", 15, 15),
        (
            "NormalTangentMirrorTest/glTF/NormalTangentMirrorTest.gltf",
            10,
            30,
        ),
    ] {
        for frames in [Frames::Triangle, Frames::Vertex] {
            lit_as_its_geometry(file, frames, mirroring(), orthographic(), domes, quads);
        }
    }
}

/// Each asset on each frame through `views`.
fn lit_as_their_geometry_through(views: Views) {
    for (file, domes, quads) in [
        ("NormalTangentTest/glTF/NormalTangentTest.gltf", 15, 15),
        (
            "NormalTangentMirrorTest/glTF/NormalTangentMirrorTest.gltf",
            10,
            30,
        ),
    ] {
        for frames in [Frames::Triangle, Frames::Vertex] {
            lit_as_its_geometry(file, frames, Mat4::IDENTITY, views, domes, quads);
        }
    }
}

// A probe capture renders each cube face through its own camera
// (ViewData::probe_face), stored as a cube mirrored in world Z. The
// derivative frame's sign follows the view's handedness, and raster's
// side its winding, so a face whose camera mirrors its image (as a capture
// matching WebGPU's cube layout by flipping its projection would) inverts
// every normal map's relief in the probe. Plausible defect: a face's
// camera that mirrors. The oracle is the assets' geometry, as above.
#[test]
fn khronos_assets_are_lit_as_their_geometry_in_probe_faces() {
    lit_as_their_geometry_through(probe_faces());
}

// A planar reflector's picture is rendered by a camera at the viewer's
// mirrored eye (reflected_camera::camera), which flips its image left to
// right so that it stays a proper camera. Plausible defect: a reflected
// camera that mirrors (the viewer's view times the reflection, unflipped),
// which inverts the derivative frame and raster's side in every
// reflection. The oracle is the assets' geometry, as above.
#[test]
fn khronos_assets_are_lit_as_their_geometry_in_planar_reflections() {
    lit_as_their_geometry_through(reflected());
}

// The Khronos Avocado (glTF-Sample-Assets e124e8d): a low-poly curved mesh
// whose normal map was baked on its own tangents, which it carries
// (Unity's). With TANGENT stripped, the loader's MikkTSpace frames must
// shade it as its own tangents do, where the per-triangle frame does not:
// around its cut face the per-triangle frame turns at every triangle edge
// while the frame the map was baked on turns smoothly. A tangent-level
// bound cannot hold on this asset: Unity's tangents differ from
// MikkTSpace's on the same data (up to about 18° in direction, three
// vertices' handedness), mostly where the map is flat. The oracle is the
// asset's own tangents, at the shading level and comparatively: at ray
// hits on every triangle (four points each), the generated frame's worst
// angle from the authored frame's shaded normal must be under a quarter of
// the per-triangle frame's worst on the same hits, and the per-triangle
// frame must be visibly wrong somewhere (over 10°), or the case shows
// nothing. A triangle where the file's own handedness contradicts its
// coordinates' (glTF's B = (N x T) w must point toward decreasing V) is
// left out: there the file's tangents are not the frame its map was baked
// on, and both frames built from the coordinates disagree with them alike.
// That is read from the fixture's data, and must leave out few. Defects:
// generation skipped (the stripped asset then shades on the per-triangle
// frame), its frames wrong-handed or turned, or shading not taking them
// (the per-triangle frame again).
#[test]
fn generated_frames_shade_the_avocado_as_its_own_tangents_do() {
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/gltf/Avocado/glTF");
    let normal_only =
        |image: crate::asset::GltfImage<'_>| -> crate::asset::Result<crate::asset::ImageSource> {
            Ok(if image.uri == Some("Avocado_normal.png") {
                crate::asset::ImageSource::Decode
            } else {
                crate::asset::ImageSource::Supplied(crate::asset::Image::Rgba8(
                    image::RgbaImage::from_pixel(1, 1, image::Rgba([255; 4])),
                ))
            })
        };
    let load = |file: &str| {
        crate::asset::load_with_options(
            &directory.join(file),
            crate::asset::LoadOptions {
                images: Some(&normal_only),
                ..Default::default()
            },
        )
        .unwrap_or_else(|error| panic!("{file}: {error}"))
    };
    let authored = load("Avocado.gltf");
    let generated = load("Avocado-NoTangents.gltf");
    let mut per_triangle = authored.clone();
    for vertex in &mut per_triangle.meshes[0].vertices {
        vertex.tangent = [0.; 4];
    }
    // Four points on each triangle whose handedness agrees with its
    // coordinates', each hit from 2 mm out along its face's normal.
    let mesh = &authored.meshes[0];
    let mut rays: Vec<[f32; 8]> = Vec::new();
    let mut contradicted = 0;
    for triangle in mesh.indices.as_chunks::<3>().0 {
        let corners = triangle.map(|i| mesh.vertices[i as usize]);
        let [a, b, c] = corners.map(|corner| Vec3::from_array(corner.position));
        let normal = (b - a).cross(c - a).normalize();
        // ∂p/∂U and ∂p/∂V from the triangle's positions and coordinates,
        // and the handedness glTF's frame of them takes.
        let [ua, ub, uc] = corners.map(|corner| glam::Vec2::from_array(corner.uv));
        let (d1, d2) = (ub - ua, uc - ua);
        let along_u = ((b - a) * d2.y - (c - a) * d1.y) / d1.perp_dot(d2);
        let along_v = ((c - a) * d1.x - (b - a) * d2.x) / d1.perp_dot(d2);
        let handedness = normal.cross(along_u).dot(-along_v).signum();
        if corners.iter().any(|corner| corner.tangent[3] != handedness) {
            contradicted += 1;
            continue;
        }
        for weights in [
            [1. / 3., 1. / 3., 1. / 3.],
            [0.6, 0.2, 0.2],
            [0.2, 0.6, 0.2],
            [0.2, 0.2, 0.6],
        ] {
            let point = a * weights[0] + b * weights[1] + c * weights[2];
            let origin = point + normal * 0.002;
            rays.push([
                origin.x, origin.y, origin.z, 0.0005, -normal.x, -normal.y, -normal.z, 0.004,
            ]);
        }
    }
    let shade = |asset: &Asset| {
        let material = &asset.materials[0];
        let maps = InEffect::of(material, BindingTier::Extended);
        let relief = material.normal_texture.as_ref().unwrap().image;
        normal_probe::observe(
            &device,
            &queue,
            &Probe {
                asset,
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
                // Rays alone: raster draws nothing on screen.
                vertex: "@vertex fn fixture_vs(v:FixtureVertex)->Fragment {var o:Fragment;o.world=(objects[0].model*vec4(v.position,1.)).xyz;o.clip=vec4(2.,2.,0.,1.);return o;}\nfn fixture_front(raster:bool)->bool {return raster;}",
                views: 1,
                size: [1, 1],
                rays: &rays,
            },
        )
        .rays
    };
    let [own, mikktspace, triangle] = [&authored, &generated, &per_triangle].map(shade);
    let degrees = |a: [f32; 4], b: [f32; 4]| {
        Vec3::from_slice(&a[..3])
            .normalize()
            .dot(Vec3::from_slice(&b[..3]).normalize())
            .clamp(-1., 1.)
            .acos()
            .to_degrees()
    };
    let (mut generated_worst, mut triangle_worst) = (0f32, 0f32);
    let mut hits = 0;
    for i in 0..rays.len() {
        // A front hit in all three.
        if [own[i], mikktspace[i], triangle[i]]
            .iter()
            .any(|hit| hit[3] != 1.)
        {
            continue;
        }
        hits += 1;
        generated_worst = generated_worst.max(degrees(mikktspace[i], own[i]));
        triangle_worst = triangle_worst.max(degrees(triangle[i], own[i]));
    }
    eprintln!(
        "{contradicted} triangles left out; {hits} of {} hits: generated frame at worst {generated_worst:.2}°, per-triangle frame {triangle_worst:.2}° from the asset's own",
        rays.len()
    );
    assert!(
        hits > rays.len() * 9 / 10,
        "{hits} of {} rays hit",
        rays.len()
    );
    assert!(
        contradicted * 100 < mesh.indices.len() / 3,
        "{contradicted} triangles contradict their coordinates"
    );
    assert!(
        triangle_worst > 10.,
        "the per-triangle frame is never visibly off"
    );
    assert!(
        generated_worst < triangle_worst / 4.,
        "generated {generated_worst:.2}° against per-triangle {triangle_worst:.2}°"
    );
}
