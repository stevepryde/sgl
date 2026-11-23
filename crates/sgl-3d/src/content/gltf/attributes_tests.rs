//! Quantized vertex attributes (`KHR_mesh_quantization`) against float
//! sources: Khronos' Animated Morph Cube in both exports, and documents
//! built here whose world-space geometry is known by construction.
use super::rig_tests::Buffer;
use super::*;
use glam::{Mat3, Mat4, Quat, Vec3};
use serde_json::{Value, json};
use wasm_bindgen_test::wasm_bindgen_test;

const CUBE_QUANTIZED: (&[u8], &[u8]) = (
    include_bytes!(
        "../../../tests/fixtures/gltf/AnimatedMorphCube/glTF-Quantized/AnimatedMorphCube.gltf"
    ),
    include_bytes!(
        "../../../tests/fixtures/gltf/AnimatedMorphCube/glTF-Quantized/AnimatedMorphCube.bin"
    ),
);
const CUBE_FLOAT: (&[u8], &[u8]) = (
    include_bytes!("../../../tests/fixtures/gltf/AnimatedMorphCube/glTF/AnimatedMorphCube.gltf"),
    include_bytes!("../../../tests/fixtures/gltf/AnimatedMorphCube/glTF/AnimatedMorphCube.bin"),
);

/// A `.gltf` and the one buffer file it names, as one GLB.
fn embedded((json, bin): (&[u8], &[u8])) -> Vec<u8> {
    let mut document: Value = serde_json::from_slice(json).unwrap();
    document["buffers"][0]
        .as_object_mut()
        .unwrap()
        .remove("uri");
    let mut json = serde_json::to_vec(&document).unwrap();
    json.resize(json.len().next_multiple_of(4), b' ');
    let mut bin = bin.to_vec();
    bin.resize(bin.len().next_multiple_of(4), 0);
    let mut glb = Vec::new();
    for word in [
        0x46546c67u32,
        2,
        (28 + json.len() + bin.len()) as u32,
        json.len() as u32,
        0x4e4f534a,
    ] {
        glb.extend(word.to_le_bytes());
    }
    glb.extend(json);
    glb.extend((bin.len() as u32).to_le_bytes());
    glb.extend(0x004e4942u32.to_le_bytes());
    glb.extend(bin);
    glb
}

/// The angle between `a` and `b`, in radians, resolved near zero, where
/// an `f32` arccosine of their dot product is not.
fn angle(a: Vec3, b: Vec3) -> f32 {
    let (a, b) = (a.normalize(), b.normalize());
    a.cross(b).length().atan2(a.dot(b))
}

/// Asserts that the quantized cube loads as its float export within the
/// quantized file's rounding: each vertex's world position and both morph
/// targets' position displacements within half a step an axis, normals and
/// normal displacements within half a BYTE step a component, the same
/// triangles with the same winding, and the cube's box, ±1 in the world
/// (±0.01 under its node's scale of 100).
pub(super) fn assert_cube_matches(quantized: &Asset, float: &Asset) {
    // The quantized file's leaf node scale times its parent's: one
    // UNSIGNED_SHORT step in the world.
    const STEP: f32 = 3.649_006_9e-6 * 100.;
    let position_error = STEP / 2. * 3f32.sqrt() + 1e-6;
    let direction_error = 0.5 / 127. * 3f32.sqrt();
    assert!(quantized.ignored.is_empty(), "{:?}", quantized.ignored);
    let ([quantized], [float]) = (&quantized.meshes[..], &float.meshes[..]) else {
        panic!("one mesh each");
    };
    assert_eq!(quantized.vertices.len(), float.vertices.len());
    // gltfpack reorders vertices; each corner's three faces' vertices are
    // told apart by their normals.
    let matched: Vec<usize> = quantized
        .vertices
        .iter()
        .map(|vertex| {
            let found: Vec<usize> = (0..float.vertices.len())
                .filter(|&f| {
                    let other = &float.vertices[f];
                    Vec3::from(vertex.position).distance(Vec3::from(other.position))
                        <= position_error
                        && angle(vertex.normal.into(), other.normal.into()) <= direction_error
                })
                .collect();
            assert_eq!(found.len(), 1, "{:?} matched {found:?}", vertex.position);
            found[0]
        })
        .collect();
    let mut low = Vec3::INFINITY;
    let mut high = Vec3::NEG_INFINITY;
    for vertex in &quantized.vertices {
        low = low.min(vertex.position.into());
        high = high.max(vertex.position.into());
    }
    assert!(low.abs_diff_eq(Vec3::splat(-1.), STEP / 2. + 1e-6), "{low}");
    assert!(
        high.abs_diff_eq(Vec3::splat(1.), STEP / 2. + 1e-6),
        "{high}"
    );
    let triangles = |indices: &[u32], map: &dyn Fn(u32) -> u32| {
        let mut triangles: Vec<[u32; 3]> = indices
            .as_chunks::<3>()
            .0
            .iter()
            .map(|t| {
                let mut t = t.map(map);
                let first = (0..3).min_by_key(|&k| t[k]).unwrap();
                t.rotate_left(first);
                t
            })
            .collect();
        triangles.sort();
        triangles
    };
    assert_eq!(
        triangles(&quantized.indices, &|i| matched[i as usize] as u32),
        triangles(&float.indices, &|i| i)
    );
    let (targets, float_targets) = (
        &quantized.deformation.morph_targets,
        &float.deformation.morph_targets,
    );
    assert_eq!((targets.len(), float_targets.len()), (2, 2));
    for (target, float_target) in targets.iter().zip(float_targets) {
        for (vertex, delta) in target.deltas.iter().enumerate() {
            let expected = &float_target.deltas[matched[vertex]];
            let position = Vec3::from(delta.position) - Vec3::from(expected.position);
            assert!(position.length() <= position_error, "{position}");
            let normal = Vec3::from(delta.normal) - Vec3::from(expected.normal);
            assert!(normal.length() <= direction_error, "{normal}");
        }
    }
}

// Plausible defects: the file is refused for requiring the extension;
// UNSIGNED_SHORT positions read as floats or as normalized; the leaf node's
// dequantisation skipped or applied twice; BYTE normals or SHORT morph
// displacements misread. The oracle is Khronos' float export of the same
// cube, loaded through the unchanged float path, and the cube's ±1 box.
#[wasm_bindgen_test(unsupported = test)]
fn khronos_quantized_cube_loads_as_its_float_export() {
    let quantized = load_slice(&embedded(CUBE_QUANTIZED)).unwrap();
    let float = load_slice(&embedded(CUBE_FLOAT)).unwrap();
    assert_cube_matches(&quantized, &float);
}

/// The largest magnitude `component` (5120 BYTE to 5123 UNSIGNED_SHORT)
/// stores.
fn largest(component: u32) -> f32 {
    match component {
        5120 => 127.,
        5121 => 255.,
        5122 => 32767.,
        _ => 65535.,
    }
}

/// A mesh's four vertices in its own space, each with a unit normal, a
/// tangent perpendicular to it with handedness -1, and a morph target's
/// displacement of each.
struct Source {
    positions: [Vec3; 4],
    normals: [Vec3; 4],
    tangents: [Vec3; 4],
    displacements: [Vec3; 4],
    normal_displacements: [Vec3; 4],
    tangent_displacements: [Vec3; 4],
}

fn source() -> Source {
    let normals = [
        Vec3::new(0.2, 0.9, 0.3),
        Vec3::new(-0.5, 0.4, 0.7),
        Vec3::new(0.6, -0.1, 0.8),
        Vec3::new(0.1, 0.3, -0.9),
    ]
    .map(Vec3::normalize);
    let tangents = normals
        .map(|n| (Vec3::new(1., 0.2, -0.3) - n * n.dot(Vec3::new(1., 0.2, -0.3))).normalize());
    Source {
        positions: [
            Vec3::new(-0.3, 0.2, 0.7),
            Vec3::new(1.9, -0.6, 0.1),
            Vec3::new(0.4, 1.3, -0.8),
            Vec3::new(1.2, 0.9, 0.5),
        ],
        normals,
        tangents,
        displacements: [
            Vec3::new(0.1, 0., -0.05),
            Vec3::new(0., 0.2, 0.),
            Vec3::new(-0.15, 0.05, 0.1),
            Vec3::ZERO,
        ],
        normal_displacements: [
            Vec3::new(0.1, -0.05, 0.),
            Vec3::new(0., 0.2, -0.1),
            Vec3::ZERO,
            Vec3::new(-0.3, 0., 0.1),
        ],
        tangent_displacements: [
            Vec3::new(0., 0.1, 0.05),
            Vec3::ZERO,
            Vec3::new(0.2, 0., -0.1),
            Vec3::new(0., -0.1, 0.),
        ],
    }
}

/// The parent of every built mesh's node: a non-uniform scale (κ = 6),
/// a turn and a translation.
fn parent() -> Mat4 {
    Mat4::from_scale_rotation_translation(
        Vec3::new(2., 0.5, 3.),
        Quat::from_axis_angle(Vec3::new(1., 2., 3.).normalize(), 0.7),
        Vec3::new(5., -2., 1.),
    )
}

/// Ratio of `parent`'s largest axis scale to its smallest.
const CONDITION: f32 = 6.;

/// How `source` is stored: POSITION and TEXCOORD_0 as `component`,
/// `normalized` or not; NORMAL, TANGENT and the morph target's NORMAL and
/// TANGENT as normalized BYTE for 8-bit components and SHORT for 16-bit;
/// the morph target's POSITION as the signed component of the same size,
/// as normalized as POSITION, with the base positions' scale.
#[derive(Clone, Copy, Debug)]
struct Format {
    component: u32,
    normalized: bool,
}

impl Format {
    fn signed(self) -> bool {
        matches!(self.component, 5120 | 5122)
    }

    /// The signed component of this one's size: directions' and
    /// displacements'.
    fn signed_component(self) -> u32 {
        if self.component < 5122 { 5120 } else { 5122 }
    }

    /// UVs in what this format stores: across its normalized range, or
    /// whole numbers, which an unnormalized integer holds exactly.
    fn uvs(self) -> [[f32; 2]; 4] {
        match (self.normalized, self.signed()) {
            (true, true) => [[-0.8, 0.9], [0.75, -0.2], [0., 1.], [-1., 0.333]],
            (true, false) => [[0.1, 0.9], [0.75, 0.2], [0., 1.], [0.5, 0.333]],
            (false, true) => [[-2., 3.], [2., -1.], [0., 0.], [7., -5.]],
            (false, false) => [[0., 3.], [2., 1.], [5., 0.], [1., 1.]],
        }
    }
}

/// A document of `source`'s mesh under `parent`, weighted 0.5 to its
/// morph target: in floats (without UVs), or quantized as `format` under a leaf node
/// holding the dequantisation (an offset and a uniform scale). The
/// quantized positions' step, in the mesh's space, comes with it. `edit`
/// changes the quantized integers before they are stored.
fn built(format: Option<Format>, edit: impl FnOnce(&mut Vec<i32>)) -> (Vec<u8>, f32) {
    let source = source();
    let flat =
        |vectors: &[Vec3]| -> Vec<f32> { vectors.iter().flat_map(|v| v.to_array()).collect() };
    let tangents: Vec<f32> = source
        .tangents
        .iter()
        .flat_map(|t| [t.x, t.y, t.z, -1.])
        .collect();
    let mut buffer = Buffer::default();
    let indices = buffer.accessor(
        bytemuck::cast_slice(&[0u16, 1, 2, 2, 1, 3]),
        5123,
        "SCALAR",
        6,
    );
    let (attributes, target, leaf, step) = match format {
        None => (
            json!({
                "POSITION": buffer.floats(&flat(&source.positions), "VEC3", 3),
                "NORMAL": buffer.floats(&flat(&source.normals), "VEC3", 3),
                "TANGENT": buffer.floats(&tangents, "VEC4", 4),
            }),
            json!({
                "POSITION": buffer.floats(&flat(&source.displacements), "VEC3", 3),
                "NORMAL": buffer.floats(&flat(&source.normal_displacements), "VEC3", 3),
                "TANGENT": buffer.floats(&flat(&source.tangent_displacements), "VEC3", 3),
            }),
            json!({"mesh": 0}),
            0.,
        ),
        Some(format) => {
            let base_largest = largest(format.component);
            let low = source
                .positions
                .iter()
                .fold(Vec3::INFINITY, |a, &p| a.min(p));
            let high = source
                .positions
                .iter()
                .fold(Vec3::NEG_INFINITY, |a, &p| a.max(p));
            // Positions span [-1, 1] about the centre for a signed format,
            // [0, 1] from the low corner for an unsigned one, before the
            // integers' own scale.
            let (offset, span) = if format.signed() {
                ((low + high) / 2., ((high - low) / 2.).max_element())
            } else {
                (low, (high - low).max_element())
            };
            let step = span / base_largest;
            let round = |values: &[Vec3], from: Vec3, unit: f32| -> Vec<i32> {
                values
                    .iter()
                    .flat_map(|v| ((*v - from) / unit).round().to_array())
                    .map(|c| c as i32)
                    .collect()
            };
            let mut positions = round(&source.positions, offset, step);
            edit(&mut positions);
            let direction = format.signed_component();
            let direction_largest = largest(direction);
            let directions = |values: &[Vec3]| round(values, Vec3::ZERO, 1. / direction_largest);
            let tangent_codes: Vec<i32> = directions(&source.tangents)
                .chunks(3)
                .flat_map(|t| [t[0], t[1], t[2], -(direction_largest as i32)])
                .collect();
            let uvs: Vec<i32> = format
                .uvs()
                .concat()
                .iter()
                .map(|&uv| {
                    (if format.normalized {
                        uv * base_largest
                    } else {
                        uv
                    })
                    .round() as i32
                })
                .collect();
            // Displacements take the base positions' scale: in a signed
            // normalized component's range, or in the base's steps.
            let displacement_unit = if format.normalized {
                span / direction_largest
            } else {
                step
            };
            let scale = if format.normalized { span } else { step };
            (
                json!({
                    "POSITION": buffer.integers(&positions, format.component, format.normalized, "VEC3", 3),
                    "NORMAL": buffer.integers(&directions(&source.normals), direction, true, "VEC3", 3),
                    "TANGENT": buffer.integers(&tangent_codes, direction, true, "VEC4", 4),
                    "TEXCOORD_0": buffer.integers(&uvs, format.component, format.normalized, "VEC2", 2),
                }),
                json!({
                    "POSITION": buffer.integers(&round(&source.displacements, Vec3::ZERO, displacement_unit), direction, format.normalized, "VEC3", 3),
                    "NORMAL": buffer.integers(&directions(&source.normal_displacements), direction, true, "VEC3", 3),
                    "TANGENT": buffer.integers(&directions(&source.tangent_displacements), direction, true, "VEC3", 3),
                }),
                json!({"mesh": 0, "translation": offset.to_array(), "scale": [scale, scale, scale]}),
                step,
            )
        }
    };
    let (scale, rotation, translation) = parent().to_scale_rotation_translation();
    let document = json!({
        "asset": {"version": "2.0"},
        "extensionsUsed": ["KHR_mesh_quantization"],
        "extensionsRequired": ["KHR_mesh_quantization"],
        "meshes": [{"primitives": [{"attributes": attributes, "indices": indices, "targets": [target]}], "weights": [0.5]}],
        "nodes": [
            {"translation": translation.to_array(), "rotation": rotation.to_array(), "scale": scale.to_array(), "children": [1]},
            leaf
        ],
        "scenes": [{"nodes": [0]}], "scene": 0
    });
    (buffer.glb(document), step)
}

/// Every POSITION and TEXCOORD storage `KHR_mesh_quantization` allows.
const FORMATS: [Format; 8] = [
    Format {
        component: 5120,
        normalized: true,
    },
    Format {
        component: 5120,
        normalized: false,
    },
    Format {
        component: 5121,
        normalized: true,
    },
    Format {
        component: 5121,
        normalized: false,
    },
    Format {
        component: 5122,
        normalized: true,
    },
    Format {
        component: 5122,
        normalized: false,
    },
    Format {
        component: 5123,
        normalized: true,
    },
    Format {
        component: 5123,
        normalized: false,
    },
];

// Plausible defects: an integer read as its bytes' float, a normalized one
// left unnormalized or an unnormalized one normalized, a signed one read
// unsigned, the dequantising node skipped or applied twice, a quantized
// normal or tangent not normalized or transformed as a point, a tangent's
// handedness read as ±127, a morph displacement left at its integer scale.
// The oracles are the float values each document was quantized from, by
// the extension's float-to-int rounding, under the parent's non-uniform
// transform: world positions and normals by glTF's node and normal
// matrices; UVs as authored (an unnormalized integer is its value); the
// morph target as the float document's, which the unchanged float path
// loads. Tolerances are the source's rounding: half a step an axis, carried
// through the transform's largest scale (3) for positions and its
// condition (6) for directions.
#[wasm_bindgen_test(unsupported = test)]
fn every_quantized_format_loads_as_its_float_source_under_a_non_uniform_transform() {
    let source = source();
    let float = load_slice(&built(None, |_| {}).0).unwrap();
    let float = &float.meshes[0];
    let parent = parent();
    let normal_matrix = Mat3::from_mat4(parent).inverse().transpose();
    for format in FORMATS {
        let (bytes, step) = built(Some(format), |_| {});
        let asset = load_slice(&bytes).unwrap_or_else(|error| panic!("{format:?}: {error}"));
        let [mesh] = &asset.meshes[..] else {
            panic!("{format:?}: one mesh");
        };
        let direction_step = 0.5 / largest(format.signed_component()) * 3f32.sqrt();
        let direction_error = CONDITION * direction_step.asin() + 1e-5;
        let position_error = 3. * step / 2. * 3f32.sqrt() + 1e-5;
        for (i, vertex) in mesh.vertices.iter().enumerate() {
            let position = parent.transform_point3(source.positions[i]);
            assert!(
                Vec3::from(vertex.position).distance(position) <= position_error,
                "{format:?} vertex {i}: {:?}, not {position}",
                vertex.position
            );
            let normal = normal_matrix * source.normals[i];
            assert!(
                angle(vertex.normal.into(), normal) <= direction_error,
                "{format:?} vertex {i}: normal {:?}, not {normal}",
                vertex.normal
            );
            let normal = normal.normalize();
            let tangent = Mat3::from_mat4(parent) * source.tangents[i];
            let tangent = tangent - normal * normal.dot(tangent);
            let [x, y, z, w] = vertex.tangent;
            assert!(
                angle(Vec3::new(x, y, z), tangent) <= direction_error && w == -1.,
                "{format:?} vertex {i}: tangent {:?}, not {tangent}",
                vertex.tangent
            );
            let uv = format.uvs()[i];
            let uv_error = if format.normalized {
                0.5 / largest(format.component) + 1e-6
            } else {
                0.
            };
            assert!(
                (vertex.uv[0] - uv[0]).abs() <= uv_error
                    && (vertex.uv[1] - uv[1]).abs() <= uv_error,
                "{format:?} vertex {i}: uv {:?}, not {uv:?}",
                vertex.uv
            );
        }
        assert_eq!(mesh.indices, float.indices, "{format:?}");
        // The displacement's own rounding, the base's step or a signed
        // normalized step of its span, through the transform's largest
        // scale; a direction displacement's, divided by its transformed
        // normal's length (at least 1/6 of the largest), whose own rounding
        // rescales the displacement (at most 0.4 long) too.
        let displacement_error = 3.
            * (step * largest(format.component) / largest(format.signed_component())).max(step)
            / 2.
            * 3f32.sqrt()
            + 1e-5;
        let direction_displacement_error = CONDITION * direction_step * (1. + CONDITION * 0.4);
        let quantized = &mesh.deformation.morph_targets[0].deltas;
        for (i, (delta, expected)) in quantized
            .iter()
            .zip(&float.deformation.morph_targets[0].deltas)
            .enumerate()
        {
            let position = Vec3::from(delta.position).distance(expected.position.into());
            let normal = Vec3::from(delta.normal).distance(expected.normal.into());
            let tangent = Vec3::from(delta.tangent).distance(expected.tangent.into());
            assert!(
                position <= displacement_error
                    && normal <= direction_displacement_error
                    && tangent <= direction_displacement_error,
                "{format:?} vertex {i}: displacement {delta:?}, not {expected:?}"
            );
        }
    }
}

// Defect: a normalized signed component's most negative integer, -128 or
// -32768, read as below -1. Oracle: the extension's int-to-float equations,
// `max(c / 127.0, -1.0)` and `max(c / 32767.0, -1.0)`, which give it the
// value of -127 or -32767.
#[wasm_bindgen_test(unsupported = test)]
fn the_most_negative_normalized_integer_is_minus_one() {
    for component in [5120, 5122] {
        let format = Format {
            component,
            normalized: true,
        };
        let largest = largest(component) as i32;
        let position = |code: i32| {
            let (bytes, _) = built(Some(format), |positions| positions[0] = code);
            load_slice(&bytes).unwrap().meshes[0].vertices[0].position
        };
        assert_eq!(position(-largest - 1), position(-largest), "{format:?}");
    }
}

// Plausible defects: a skinned mesh's quantized positions read as floats or
// normalized, or given its node's transform; the skin's inverse bind
// matrices, which hold the dequantisation, left out or applied twice. The
// oracle is glTF's skinning of the float source: each vertex at the
// weighted sum of its joints' world transforms times their float inverse
// bind matrices, posed by hand; normals by that sum's normal matrix.
// Tolerances: half an UNSIGNED_SHORT step an axis (a blend of rigid
// joints scales nothing up), and half a normalized SHORT step a component,
// doubled for the blend's slight skew.
#[wasm_bindgen_test(unsupported = test)]
fn a_posed_skinned_mesh_dequantizes_through_its_inverse_bind_matrices() {
    let positions = [
        Vec3::new(0.2, 0.5, 0.1),
        Vec3::new(-0.3, 1.6, 0.2),
        Vec3::new(0.4, 1.1, -0.2),
    ];
    let normals = [
        Vec3::new(0.3, 0.2, 0.9),
        Vec3::new(-0.6, 0.1, 0.7),
        Vec3::new(0.2, -0.4, 0.8),
    ]
    .map(Vec3::normalize);
    let low = positions.iter().fold(Vec3::INFINITY, |a, &p| a.min(p));
    let high = positions.iter().fold(Vec3::NEG_INFINITY, |a, &p| a.max(p));
    let step = (high - low).max_element() / 65535.;
    let dequantize = Mat4::from_translation(low) * Mat4::from_scale(Vec3::splat(step));
    // Each joint's inverse transform at bind: the hip at (0, 1, 0), the
    // knee 1 above it.
    let inverse_binds = [
        Mat4::from_translation(Vec3::new(0., -1., 0.)),
        Mat4::from_translation(Vec3::new(0., -2., 0.)),
    ];
    let mut buffer = Buffer::default();
    let codes: Vec<i32> = positions
        .iter()
        .flat_map(|p| ((*p - low) / step).round().to_array())
        .map(|c| c as i32)
        .collect();
    let normal_codes: Vec<i32> = normals
        .iter()
        .flat_map(|n| (*n * 32767.).round().to_array())
        .map(|c| c as i32)
        .collect();
    let position_accessor = buffer.integers(&codes, 5123, false, "VEC3", 3);
    let normal_accessor = buffer.integers(&normal_codes, 5122, true, "VEC3", 3);
    let joints = buffer.accessor(&[0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0], 5121, "VEC4", 3);
    let weights = buffer.floats(
        &[1., 0., 0., 0., 0., 1., 0., 0., 0.5, 0.5, 0., 0.],
        "VEC4",
        4,
    );
    let quantized_binds: Vec<f32> = inverse_binds
        .iter()
        .flat_map(|m| (*m * dequantize).to_cols_array())
        .collect();
    let binds = buffer.floats(&quantized_binds, "MAT4", 16);
    let bytes = buffer.glb(json!({
        "asset": {"version": "2.0"},
        "extensionsUsed": ["KHR_mesh_quantization"],
        "extensionsRequired": ["KHR_mesh_quantization"],
        "meshes": [{"primitives": [{"attributes": {"POSITION": position_accessor, "NORMAL": normal_accessor, "JOINTS_0": joints, "WEIGHTS_0": weights}}]}],
        "nodes": [
            {"name": "hip", "translation": [0, 1, 0], "children": [1]},
            {"name": "knee", "translation": [0, 1, 0]},
            {"mesh": 0, "skin": 0, "scale": [4, 4, 4]}
        ],
        "skins": [{"joints": [0, 1], "inverseBindMatrices": binds}],
        "scenes": [{"nodes": [0, 2]}], "scene": 0
    }));
    let asset = load_slice(&bytes).unwrap();
    let mesh = &asset.meshes[0];
    let rig = &asset.rig;
    // The hip turned a quarter about Z and the knee 0.4 about X.
    let hip = Mat4::from_translation(Vec3::Y) * Mat4::from_rotation_z(std::f32::consts::FRAC_PI_2);
    let knee = Mat4::from_translation(Vec3::Y) * Mat4::from_rotation_x(0.4);
    let matrices = rig.joint_matrices(&[hip, knee, Mat4::IDENTITY]);
    let globals = [hip, hip * knee];
    for (i, vertex) in mesh.vertices.iter().enumerate() {
        let influence = &mesh.deformation.influences[i];
        let blend = |joint: &dyn Fn(usize) -> Mat4| {
            (0..4).fold(Mat4::ZERO, |sum, k| {
                sum + joint(influence.joints[k] as usize) * influence.weights[k]
            })
        };
        let skin = blend(&|joint| matrices[joint]);
        let expected = blend(&|joint| globals[joint] * inverse_binds[joint]);
        let position = skin.transform_point3(vertex.position.into());
        let float_position = expected.transform_point3(positions[i]);
        assert!(
            position.distance(float_position) <= step / 2. * 3f32.sqrt() + 1e-6,
            "vertex {i}: {position}, not {float_position}"
        );
        let normal = Mat3::from_mat4(skin).inverse().transpose() * Vec3::from(vertex.normal);
        let float_normal = Mat3::from_mat4(expected).inverse().transpose() * normals[i];
        assert!(
            angle(normal, float_normal) <= 2. * (0.5 / 32767. * 3f32.sqrt()).asin() + 1e-5,
            "vertex {i}: normal {normal}, not {float_normal}"
        );
    }
}
