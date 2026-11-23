//! Triangle strips and fans loaded through the file and byte entry points,
//! against the same surface listed triangle by triangle.
use super::rig_tests::Buffer;
use super::*;
use crate::content::asset::CpuMesh;
use crate::content::geometry::{obstructed_distance, triangles};
use glam::Vec3;
use serde_json::json;
use wasm_bindgen_test::wasm_bindgen_test;

/// glTF 2.0's `mode` values.
const TRIANGLES: u32 = 4;
const TRIANGLE_STRIP: u32 = 5;
const TRIANGLE_FAN: u32 = 6;

/// One primitive of `mode` over `positions`, facing +Z (`NORMAL`), with
/// `uvs` and `indices` where given.
fn primitive(
    positions: &[[f32; 3]],
    uvs: Option<&[[f32; 2]]>,
    indices: Option<&[u16]>,
    mode: u32,
) -> Vec<u8> {
    let mut buffer = Buffer::default();
    let mut attributes = json!({
        "POSITION": buffer.floats(positions.as_flattened(), "VEC3", 3),
        "NORMAL": buffer.floats(&[0., 0., 1.].repeat(positions.len()), "VEC3", 3),
    });
    if let Some(uvs) = uvs {
        attributes["TEXCOORD_0"] = buffer.floats(uvs.as_flattened(), "VEC2", 2).into();
    }
    let mut primitive = json!({"attributes": attributes, "mode": mode});
    if let Some(indices) = indices {
        primitive["indices"] = buffer
            .accessor(bytemuck::cast_slice(indices), 5123, "SCALAR", indices.len())
            .into();
    }
    buffer.glb(json!({
        "asset": {"version": "2.0"},
        "meshes": [{"primitives": [primitive]}],
        "nodes": [{"mesh": 0}],
        "scenes": [{"nodes": [0]}], "scene": 0
    }))
}

/// Two quads side by side that meet along x = 1 at a UV seam (vertices 1
/// and 4, and 3 and 6, share positions but not UVs), bent out of the plane
/// so rays tell their triangles apart.
const SEAMED_POSITIONS: [[f32; 3]; 8] = [
    [0., 0., 0.],
    [1., 0., 0.],
    [0., 1., 0.],
    [1., 1., 0.5],
    [1., 0., 0.],
    [2., 0., 0.25],
    [1., 1., 0.5],
    [2., 1., 0.],
];
const SEAMED_UVS: [[f32; 2]; 8] = [
    [0., 0.],
    [1., 0.],
    [0., 1.],
    [1., 1.],
    [0., 0.],
    [1., 0.],
    [0., 1.],
    [1., 1.],
];
/// The two quads' four counter-clockwise triangles, listed by hand.
const SEAMED_LIST: [u16; 12] = [0, 1, 2, 2, 1, 3, 4, 5, 6, 6, 5, 7];
/// The same quads as one strip each, joined by repeating indices 3 and 4.
const SEAMED_STRIP: [u16; 10] = [0, 1, 2, 3, 3, 4, 4, 5, 6, 7];

/// `mesh`'s triangles, each rotated to start at its smallest index, which
/// keeps its winding.
fn rotated(mesh: &CpuMesh) -> Vec<[u32; 3]> {
    mesh.indices
        .as_chunks::<3>()
        .0
        .iter()
        .map(|&triangle| {
            let mut triangle = triangle;
            let first = (0..3).min_by_key(|&k| triangle[k]).unwrap();
            triangle.rotate_left(first);
            triangle
        })
        .collect()
}

/// `loaded` holds the hand-listed asset's vertices and triangles, wound
/// alike, and every triangle faces its vertices' +Z normal, as glTF's
/// counter-clockwise front faces must for back-face culling to keep them.
fn assert_same_surface(loaded: &Asset, listed: &Asset) {
    let ([mesh], [expected]) = (&loaded.meshes[..], &listed.meshes[..]) else {
        panic!("one mesh each");
    };
    let attributes = |mesh: &CpuMesh| -> Vec<([f32; 3], [f32; 2])> {
        mesh.vertices.iter().map(|v| (v.position, v.uv)).collect()
    };
    assert_eq!(attributes(mesh), attributes(expected));
    let (mut found, mut wanted) = (rotated(mesh), rotated(expected));
    found.sort_unstable();
    wanted.sort_unstable();
    assert_eq!(found, wanted);
    for [a, b, c] in triangles(std::slice::from_ref(mesh)) {
        assert!((b - a).cross(c - a).dot(Vec3::Z) > 0.0, "{a} {b} {c}");
    }
}

// Defect: strip expansion reverses its odd triangles, keeps the joining
// degenerates or flips the second strip's winding by shifting parity,
// reads a UV across the seam from the neighbouring quad, or loses
// triangles, so culling, texturing or rays differ from the listed asset.
// Oracle: the same quads listed by hand (glTF 2.0.1 3.7.2.1's strip).
#[wasm_bindgen_test(unsupported = test)]
fn a_joined_strip_loads_as_its_listed_triangles() {
    let listed = load_slice(&primitive(
        &SEAMED_POSITIONS,
        Some(&SEAMED_UVS),
        Some(&SEAMED_LIST),
        TRIANGLES,
    ))
    .unwrap();
    let strip = load_slice(&primitive(
        &SEAMED_POSITIONS,
        Some(&SEAMED_UVS),
        Some(&SEAMED_STRIP),
        TRIANGLE_STRIP,
    ))
    .unwrap();
    assert_same_surface(&strip, &listed);
    assert_eq!(strip.meshes[0].indices.len(), SEAMED_LIST.len());
    // Rays straight down onto each triangle and past both quads hit, or
    // miss, the strip where they hit the list.
    let (strip, listed) = (triangles(&strip.meshes), triangles(&listed.meshes));
    for (x, y) in [(0.2, 0.3), (0.8, 0.7), (1.3, 0.2), (1.7, 0.9), (2.5, 0.5)] {
        let origin = Vec3::new(x, y, 10.0);
        let hit = |faces| obstructed_distance(faces, origin, Vec3::NEG_Z, 20.0);
        assert!((hit(&strip) - hit(&listed)).abs() < 1e-5, "({x}, {y})");
        assert_eq!(hit(&strip) < 20.0, x < 2.0, "({x}, {y})");
    }
}

// Defect: a fan pivots on the wrong vertex or winds backwards, or a
// primitive without indices expands something other than its vertices in
// order. Oracle: each surface listed by hand (glTF 2.0.1 3.7.2.1).
#[wasm_bindgen_test(unsupported = test)]
fn fans_and_unindexed_strips_load_as_their_listed_triangles() {
    // A fan about vertex 0, its rim counter-clockwise.
    let fan = [
        [0., 0., 0.],
        [1., 0., 0.],
        [1., 1., 0.],
        [0., 1., 0.],
        [-1., 0., 0.],
    ];
    let fan_list = [0, 1, 2, 0, 2, 3, 0, 3, 4];
    // A ribbon two quads tall, in strip order.
    let ribbon = [
        [0., 0., 0.],
        [1., 0., 0.],
        [0., 1., 0.],
        [1., 1., 0.],
        [0., 2., 0.],
        [1., 2., 0.],
    ];
    let ribbon_list = [0, 1, 2, 1, 3, 2, 2, 3, 4, 3, 5, 4];
    for (positions, list, mode, indices) in [
        (
            &fan[..],
            &fan_list[..],
            TRIANGLE_FAN,
            Some(&[0, 1, 2, 3, 4][..]),
        ),
        (&fan, &fan_list, TRIANGLE_FAN, None),
        (&ribbon, &ribbon_list, TRIANGLE_STRIP, None),
    ] {
        let listed = load_slice(&primitive(positions, None, Some(list), TRIANGLES)).unwrap();
        let loaded = load_slice(&primitive(positions, None, indices, mode)).unwrap();
        assert_same_surface(&loaded, &listed);
    }
}

// Defect: the decoded budget is charged for a strip's authored indices,
// so expansion allocates beyond the cap. Oracle: the published charge
// (README "Asset and environment limits"): 88 bytes a rigid vertex and 4
// an index, and an unindexed four-vertex strip's two triangles (glTF 2.0.1
// 3.7.2.1) hold six indices: 4 × 88 + 6 × 4 = 376 bytes, where its
// authored four would charge 368.
#[wasm_bindgen_test(unsupported = test)]
fn a_strip_is_charged_as_its_triangle_list_before_expanding() {
    let bytes = primitive(&SEAMED_POSITIONS[..4], None, None, TRIANGLE_STRIP);
    let decoded = |limit| LoadOptions {
        budget: LoadBudget::new(1 << 20, limit).unwrap(),
        ..LoadOptions::default()
    };
    let error = load_slice_with_options(&bytes, decoded(375)).err().unwrap();
    assert!(error.to_string().contains("decoded load budget"), "{error}");
    let asset = load_slice_with_options(&bytes, decoded(376)).unwrap();
    assert_eq!(asset.meshes[0].indices.len(), 6);
}

// Defect: a point or line primitive loads as triangles. Oracle: glTF
// 2.0.1 3.7.2.1's modes 0 to 3 describe no surface.
#[wasm_bindgen_test(unsupported = test)]
fn points_and_lines_are_refused() {
    for mode in 0..=3 {
        let bytes = primitive(&SEAMED_POSITIONS[..4], None, Some(&[0, 1, 2, 3]), mode);
        let error = load_slice(&bytes).err().unwrap().to_string();
        assert!(error.contains("are not supported"), "mode {mode}: {error}");
    }
}

// Defect: a strip whose every triangle repeats an index loads as an asset
// with no meshes. Oracle: S3D-2, a scene with no triangles fails the load.
#[wasm_bindgen_test(unsupported = test)]
fn a_strip_of_only_degenerates_is_no_mesh() {
    let bytes = primitive(
        &SEAMED_POSITIONS[..4],
        None,
        Some(&[0, 1, 1, 2]),
        TRIANGLE_STRIP,
    );
    let error = load_slice(&bytes).err().unwrap().to_string();
    assert!(error.contains("no triangle meshes"), "{error}");
}

// Defect: the file entry point reads a strip differently from the byte
// entry point. Oracle: the hand-listed quads, as above.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn a_strip_file_loads_as_its_listed_triangles() {
    let directory = std::env::temp_dir().join(format!("sgl-gltf-strip-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("strip.glb");
    std::fs::write(
        &path,
        primitive(
            &SEAMED_POSITIONS,
            Some(&SEAMED_UVS),
            Some(&SEAMED_STRIP),
            TRIANGLE_STRIP,
        ),
    )
    .unwrap();
    let strip = load(&path);
    let _ = std::fs::remove_dir_all(&directory);
    let listed = load_slice(&primitive(
        &SEAMED_POSITIONS,
        Some(&SEAMED_UVS),
        Some(&SEAMED_LIST),
        TRIANGLES,
    ))
    .unwrap();
    assert_same_surface(&strip.unwrap(), &listed);
}
