//! `EXT_mesh_gpu_instancing` through the public loader, against the same
//! scenes expanded into ordinary nodes by another implementation: Khronos's
//! SimpleInstancing sample (125 translated, rotated and scaled cubes) and
//! its expansion by Khronos glTF Blender I/O; that sample written again by
//! gltfpack 1.3 with Meshopt-compressed instance views and quantized
//! rotations; and an authored Blender scene with a turned, non-uniformly
//! scaled parent, mirrored instances and two nodes sharing a mesh, exported
//! with and without GPU instances. The extension defines an instanced node
//! as its expansion (`WorldPosition = NodeWorldXform * InstanceXform *
//! VertexPosition`), so each instance's triangles, placed by its transform,
//! must be the expanded file's. The fixtures' provenance and commands are
//! in their READMEs.
use super::attributes_tests::embedded;
use super::*;
use crate::asset::{LoadBudget, Vertex};
use glam::{Mat3, Mat4, Vec3};
use serde_json::{Value, json};
use wasm_bindgen_test::wasm_bindgen_test;

macro_rules! fixture {
    ($path:literal) => {
        include_bytes!(concat!("../../../tests/fixtures/gltf/", $path))
    };
}

const SIMPLE: (&[u8], &[u8]) = (
    fixture!("SimpleInstancing/glTF/SimpleInstancing.gltf"),
    fixture!("SimpleInstancing/glTF/SimpleInstancing.bin"),
);
const SIMPLE_EXPANDED: (&[u8], &[u8]) = (
    fixture!("SimpleInstancing/glTF-Expanded/SimpleInstancing.gltf"),
    fixture!("SimpleInstancing/glTF-Expanded/SimpleInstancing.bin"),
);
const SIMPLE_MESHOPT: (&[u8], &[u8]) = (
    fixture!("SimpleInstancing/glTF-Meshopt/SimpleInstancing.gltf"),
    fixture!("SimpleInstancing/glTF-Meshopt/SimpleInstancing.bin"),
);
const PROPS: (&[u8], &[u8]) = (
    fixture!("InstancedProps/glTF/InstancedProps.gltf"),
    fixture!("InstancedProps/glTF/InstancedProps.bin"),
);
const PROPS_EXPANDED: (&[u8], &[u8]) = (
    fixture!("InstancedProps/glTF-Expanded/InstancedProps.gltf"),
    fixture!("InstancedProps/glTF-Expanded/InstancedProps.bin"),
);

/// One triangle in the asset's space: its vertices' positions and unit
/// normals, in winding order, front faces counter-clockwise.
type Triangle = [(Vec3, Vec3); 3];

/// `asset`'s triangles in its own space: its ordinary meshes as baked, and
/// each instance of each instanced mesh placed by its transform, normals by
/// the transform's inverse transpose. A mirrored transform's raster winding
/// reverses (the scene's mirrored pose, `InstanceState::pose`), so its
/// triangles are listed in the order that is front-facing in the asset.
fn triangles(asset: &Asset) -> Vec<Triangle> {
    let mut triangles = Vec::new();
    let mut add = |mesh: &crate::asset::CpuMesh, transform: Mat4| {
        let normals = Mat3::from_mat4(transform).inverse().transpose();
        let mirrored = transform.determinant() < 0.;
        for triangle in mesh.indices.as_chunks::<3>().0 {
            let mut corners = triangle.map(|index| {
                let vertex = &mesh.vertices[index as usize];
                (
                    transform.transform_point3(Vec3::from(vertex.position)),
                    (normals * Vec3::from(vertex.normal)).normalize(),
                )
            });
            if mirrored {
                corners.swap(1, 2);
            }
            triangles.push(corners);
        }
    };
    for mesh in &asset.meshes {
        add(mesh, Mat4::IDENTITY);
    }
    for instanced in &asset.instanced {
        for node in &instanced.nodes {
            for &transform in &node.transforms {
                for mesh in &instanced.meshes {
                    add(mesh, transform);
                }
            }
        }
    }
    triangles
}

/// Asserts that `actual`'s triangles are `expected`'s, as a multiset: each
/// matched by one of the other's with the same winding (a cyclic rotation
/// of its corners), positions within `position` metres and normals within
/// `normal` of each other.
fn assert_same_triangles(actual: &Asset, expected: &Asset, position: f32, normal: f32) {
    let actual = triangles(actual);
    let mut expected: Vec<Option<Triangle>> = triangles(expected).into_iter().map(Some).collect();
    assert_eq!(actual.len(), expected.len(), "triangle counts differ");
    let close = |a: &Triangle, b: &Triangle| {
        (0..3).any(|rotation| {
            (0..3).all(|corner| {
                let (pa, na) = a[corner];
                let (pb, nb) = b[(corner + rotation) % 3];
                pa.distance(pb) <= position && na.distance(nb) <= normal
            })
        })
    };
    for (index, triangle) in actual.iter().enumerate() {
        let found = expected
            .iter_mut()
            .find(|other| other.as_ref().is_some_and(|other| close(triangle, other)));
        assert!(
            found.is_some(),
            "triangle {index} {triangle:?} has no match in the expanded scene"
        );
        *found.unwrap() = None;
    }
}

fn load_embedded(files: (&[u8], &[u8])) -> Asset {
    load_slice(&embedded(files)).unwrap()
}

/// `files` with its document edited by `edit`, as an embedded GLB.
fn edited(files: (&[u8], &[u8]), edit: impl FnOnce(&mut Value)) -> Vec<u8> {
    let mut document: Value = serde_json::from_slice(files.0).unwrap();
    edit(&mut document);
    embedded((&serde_json::to_vec(&document).unwrap(), files.1))
}

fn error(bytes: &[u8]) -> String {
    match load_slice(bytes) {
        Ok(_) => panic!("the load should fail"),
        Err(error) => error.to_string(),
    }
}

/// The bytes `asset`'s meshes and instanced meshes retain for geometry.
fn geometry_bytes(asset: &Asset) -> usize {
    asset
        .meshes
        .iter()
        .chain(
            asset
                .instanced
                .iter()
                .flat_map(|instanced| &instanced.meshes),
        )
        .map(|mesh| {
            mesh.vertices.len() * std::mem::size_of::<Vertex>()
                + mesh.indices.len() * std::mem::size_of::<u32>()
        })
        .sum()
}

// Oracle: the extension's definition of an instance, against Khronos's
// sample expanded by Blender's importer and exporter.
#[wasm_bindgen_test(unsupported = test)]
fn the_khronos_sample_places_each_instance_as_its_expansion() {
    let instanced = load_embedded(SIMPLE);
    let expanded = load_embedded(SIMPLE_EXPANDED);
    assert!(instanced.meshes.is_empty());
    assert!(expanded.instanced.is_empty());
    assert_eq!(instanced.instanced.len(), 1);
    let nodes = &instanced.instanced[0].nodes;
    assert_eq!(nodes.len(), 1);
    // The sample's README: 125 instances in a 5×5×5 grid.
    assert_eq!(nodes[0].transforms.len(), 125);
    assert!(instanced.ignored.is_empty(), "{:?}", instanced.ignored);
    // Blender writes f32 TRS decomposed from its own matrices: within 10 µm
    // across the 12 m grid, and normals within 1e-4.
    assert_same_triangles(&instanced, &expanded, 1e-5, 1e-4);
}

// Oracle: the same expansion; gltfpack's quantization bounds the error. Its
// positions are 14-bit over the unit cube, its instance translations and
// scales 15-bit exponential floats and its rotations 16-bit quaternions
// (meshoptimizer `-cc` defaults), each under 1e-4 of its range, so a
// vertex of a cube at most 2 m across and 17 m from the origin errs below
// 5 mm and a normal below 5e-3.
#[wasm_bindgen_test(unsupported = test)]
fn meshopt_compressed_quantized_instances_match_their_expansion() {
    let document: Value = serde_json::from_slice(SIMPLE_MESHOPT.0).unwrap();
    let rotation = &document["accessors"][4];
    assert_eq!(
        (
            rotation["componentType"].as_u64(),
            rotation["normalized"].as_bool()
        ),
        (Some(5122), Some(true)),
        "the fixture's ROTATION is normalized SHORT"
    );
    let instanced = load_embedded(SIMPLE_MESHOPT);
    assert_eq!(instanced.instanced[0].nodes[0].transforms.len(), 125);
    assert_same_triangles(&instanced, &load_embedded(SIMPLE_EXPANDED), 5e-3, 5e-3);
}

// Oracle: Blender's exports of one scene with and without GPU instances: a
// parent turned and scaled unevenly, instances translated, rotated, scaled
// and mirrored, two instanced nodes of one mesh and an ordinary ground.
#[wasm_bindgen_test(unsupported = test)]
fn mirrored_instances_under_a_scaled_parent_match_their_expansion() {
    let instanced = load_embedded(PROPS);
    let expanded = load_embedded(PROPS_EXPANDED);
    assert_eq!(instanced.instanced.len(), 1, "two nodes share one mesh");
    let names: Vec<_> = instanced.instanced[0]
        .nodes
        .iter()
        .map(|node| (node.name.as_deref().unwrap(), node.transforms.len()))
        .collect();
    assert_eq!(names, [("Copse", 3), ("Grove", 6)]);
    // export.py mirrors Grove.3, Grove.5 and Copse.1 (one or three negative
    // scales); Grove.4's two negative scales are a rotation.
    let mirrored = instanced.instanced[0]
        .nodes
        .iter()
        .flat_map(|node| &node.transforms)
        .filter(|transform| transform.determinant() < 0.)
        .count();
    assert_eq!(mirrored, 3);
    assert_eq!(instanced.meshes.len(), 1, "the ground is an ordinary node");
    assert_same_triangles(&instanced, &expanded, 1e-5, 1e-4);
}

// Oracle: the fixture's accessor counts. The instanced asset holds the
// prop's 48 vertices and 72 indices once, where its expansion bakes nine.
#[wasm_bindgen_test(unsupported = test)]
fn instanced_geometry_is_held_once_however_many_instances() {
    let document: Value = serde_json::from_slice(PROPS.0).unwrap();
    let prop = &document["meshes"][0]["primitives"][0];
    let count = |accessor: &Value| {
        document["accessors"][accessor.as_u64().unwrap() as usize]["count"]
            .as_u64()
            .unwrap() as usize
    };
    let (vertices, indices) = (
        count(&prop["attributes"]["POSITION"]),
        count(&prop["indices"]),
    );
    let instanced = load_embedded(PROPS);
    let meshes = &instanced.instanced[0].meshes;
    assert_eq!(
        meshes.iter().map(|mesh| mesh.vertices.len()).sum::<usize>(),
        vertices
    );
    assert_eq!(
        meshes.iter().map(|mesh| mesh.indices.len()).sum::<usize>(),
        indices
    );
    let prop_bytes = vertices * std::mem::size_of::<Vertex>() + indices * 4;
    let ground_bytes = geometry_bytes(&instanced) - prop_bytes;
    assert_eq!(
        geometry_bytes(&load_embedded(PROPS_EXPANDED)),
        ground_bytes + 9 * prop_bytes
    );
}

// Oracle: the byte and file entry points share one decoder.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn a_file_with_instancing_loads_from_a_path() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/gltf/InstancedProps/glTF/InstancedProps.gltf");
    let asset = load(&path).unwrap();
    let embedded = load_embedded(PROPS);
    assert_eq!(asset.instanced[0].nodes, embedded.instanced[0].nodes);
    assert_eq!(geometry_bytes(&asset), geometry_bytes(&embedded));
}

// Oracle: LoadOptions::nodes asks once per mesh node, instanced ones too.
#[wasm_bindgen_test(unsupported = test)]
fn node_selection_applies_to_instanced_nodes() {
    let only_copse = |name: Option<&str>| name != Some("Grove");
    let asset = load_slice_with_options(
        &embedded(PROPS),
        LoadOptions {
            nodes: Some(&only_copse),
            ..LoadOptions::default()
        },
    )
    .unwrap();
    let names: Vec<_> = asset.instanced[0]
        .nodes
        .iter()
        .map(|node| node.name.clone())
        .collect();
    assert_eq!(names, [Some("Copse".to_owned())]);
}

/// SimpleInstancing's node 0 with `extension` as its
/// `EXT_mesh_gpu_instancing`, the extension required or only used.
fn with_extension(extension: Value, required: bool) -> Vec<u8> {
    edited(SIMPLE, |document| {
        document["nodes"][0]["extensions"]["EXT_mesh_gpu_instancing"] = extension;
        if required {
            document["extensionsRequired"] = json!(["EXT_mesh_gpu_instancing"]);
        }
    })
}

// Oracle: glTF 2.0 5.17: a required extension SGL3D cannot honour fails the
// load; a used one falls back to the core meaning, listed as ignored.
#[wasm_bindgen_test(unsupported = test)]
fn a_morphed_instanced_node_fails_when_required_and_loads_once_otherwise() {
    let morphed = |required| {
        edited(SIMPLE, |document| {
            // The cube's normals as a morph target's displacements: 24 VEC3.
            document["meshes"][0]["primitives"][0]["targets"] = json!([{ "POSITION": 2 }]);
            if required {
                document["extensionsRequired"] = json!(["EXT_mesh_gpu_instancing"]);
            }
        })
    };
    assert!(error(&morphed(true)).contains("skinned or morphed"));
    let asset = load_slice(&morphed(false)).unwrap();
    assert!(asset.instanced.is_empty());
    assert_eq!(
        asset
            .meshes
            .iter()
            .map(|mesh| mesh.indices.len())
            .sum::<usize>(),
        36
    );
    assert_eq!(asset.ignored, [Ignored::Instancing { node: 0 }]);
}

// Oracle: glTF 2.0 5.17, as for a morphed node; and the extension leaves a
// skin under instancing undefined (a skin ignores the node transform the
// instances compose with).
#[wasm_bindgen_test(unsupported = test)]
fn a_skinned_instanced_node_fails_when_required_and_loads_once_otherwise() {
    let skinned = |required| {
        let mut document: Value = serde_json::from_slice(SIMPLE.0).unwrap();
        // The cube's 24 vertices bound wholly to joint 0: JOINTS_0 zeros
        // (UNSIGNED_BYTE) and WEIGHTS_0 (1, 0, 0, 0), after the buffer.
        let mut bin = SIMPLE.1.to_vec();
        let joints = bin.len();
        bin.extend([0u8; 24 * 4]);
        let weights = bin.len();
        for _ in 0..24 {
            for weight in [1f32, 0., 0., 0.] {
                bin.extend(weight.to_le_bytes());
            }
        }
        document["buffers"][0]["byteLength"] = json!(bin.len());
        let views = document["bufferViews"].as_array_mut().unwrap();
        views.push(json!({ "buffer": 0, "byteOffset": joints, "byteLength": 96 }));
        views.push(json!({ "buffer": 0, "byteOffset": weights, "byteLength": 384 }));
        let accessors = document["accessors"].as_array_mut().unwrap();
        accessors
            .push(json!({ "bufferView": 6, "componentType": 5121, "count": 24, "type": "VEC4" }));
        accessors
            .push(json!({ "bufferView": 7, "componentType": 5126, "count": 24, "type": "VEC4" }));
        let attributes = &mut document["meshes"][0]["primitives"][0]["attributes"];
        attributes["JOINTS_0"] = json!(6);
        attributes["WEIGHTS_0"] = json!(7);
        document["nodes"]
            .as_array_mut()
            .unwrap()
            .push(json!({ "name": "Joint" }));
        document["nodes"][0]["skin"] = json!(0);
        document["skins"] = json!([{ "joints": [1] }]);
        document["scenes"][0]["nodes"] = json!([0, 1]);
        if required {
            document["extensionsRequired"] = json!(["EXT_mesh_gpu_instancing"]);
        }
        embedded((&serde_json::to_vec(&document).unwrap(), &bin))
    };
    assert!(error(&skinned(true)).contains("skinned or morphed"));
    let asset = load_slice(&skinned(false)).unwrap();
    assert!(asset.instanced.is_empty());
    assert_eq!(
        asset
            .meshes
            .iter()
            .map(|mesh| mesh.indices.len())
            .sum::<usize>(),
        36
    );
    assert!(
        asset.meshes[0].deformation.influences.len() == 24,
        "it loads skinned"
    );
    assert_eq!(asset.ignored, [Ignored::Instancing { node: 0 }]);
}

// Oracle: the extension's tables and its shared-count rule.
#[wasm_bindgen_test(unsupported = test)]
fn instance_attributes_outside_the_extension_fail_the_load() {
    for (extension, fragment) in [
        // Every attribute of a node has the same count; accessor 0 is the
        // cube's 36 indices.
        (
            json!({ "attributes": { "TRANSLATION": 3, "ROTATION": 4, "SCALE": 5, "_ID": 0 } }),
            "has 36 elements",
        ),
        // TRANSLATION is a FLOAT VEC3: accessor 4 is a VEC4.
        (
            json!({ "attributes": { "TRANSLATION": 4 } }),
            "TRANSLATION accessor 4 holds",
        ),
        (
            json!({ "attributes": { "TRANSLATION": 99 } }),
            "names no accessor",
        ),
        (json!({ "attributes": {} }), "no attributes"),
        (json!({ "attributes": [3] }), "not an object"),
    ] {
        let message = error(&with_extension(extension.clone(), true));
        assert!(message.contains(fragment), "{extension}: {message}");
    }
}

// Oracle: the extension allows ROTATION as normalized integers only.
#[wasm_bindgen_test(unsupported = test)]
fn an_unnormalized_integer_rotation_fails_the_load() {
    let mut document: Value = serde_json::from_slice(SIMPLE_MESHOPT.0).unwrap();
    document["accessors"][4]["normalized"] = json!(false);
    let bytes = embedded((&serde_json::to_vec(&document).unwrap(), SIMPLE_MESHOPT.1));
    assert!(error(&bytes).contains("unnormalized"));
}

// Oracle: a zero quaternion is no rotation, and a zero scale no invertible
// pose (`InstanceState::pose`).
#[wasm_bindgen_test(unsupported = test)]
fn a_zero_rotation_or_scale_fails_the_load() {
    for (attribute, shape, size, fragment) in [
        ("ROTATION", "VEC4", 16, "rotation is zero"),
        ("SCALE", "VEC3", 12, "singular"),
    ] {
        let bytes = edited(SIMPLE, |document| {
            // 125 zero elements in a second, embedded buffer, whole
            // base64 groups of three zero bytes.
            let length: usize = 125 * size;
            let groups = length.div_ceil(3);
            document["buffers"].as_array_mut().unwrap().push(json!({
                "byteLength": groups * 3,
                "uri": format!("data:application/octet-stream;base64,{}", "AAAA".repeat(groups)),
            }));
            let views = document["bufferViews"].as_array_mut().unwrap();
            views.push(json!({ "buffer": 1, "byteLength": length }));
            let view = views.len() - 1;
            let accessors = document["accessors"].as_array_mut().unwrap();
            accessors.push(
                json!({ "bufferView": view, "componentType": 5126, "count": 125, "type": shape }),
            );
            let zeros = accessors.len() - 1;
            document["nodes"][0]["extensions"]["EXT_mesh_gpu_instancing"]["attributes"]
                [attribute] = json!(zeros);
        });
        let message = error(&bytes);
        assert!(
            message.contains(fragment) && message.contains("instance 0"),
            "{message}"
        );
    }
}

// Oracle: a custom attribute is application-specific (the extension); the
// loader lists what it does not read (`Ignored`), and the instances load.
#[wasm_bindgen_test(unsupported = test)]
fn a_custom_attribute_is_listed_and_the_instances_load() {
    let asset = load_slice(&with_extension(
        json!({ "attributes": { "TRANSLATION": 3, "ROTATION": 4, "SCALE": 5, "_ID": 3 } }),
        true,
    ))
    .unwrap();
    assert_eq!(asset.instanced[0].nodes[0].transforms.len(), 125);
    assert_eq!(
        asset.ignored,
        [Ignored::InstanceAttribute {
            node: 0,
            name: "_ID".to_owned()
        }]
    );
}

// Oracle: the extension defines nothing for a node without a mesh, so the
// load reads the rest of the scene.
#[wasm_bindgen_test(unsupported = test)]
fn the_extension_on_a_node_without_a_mesh_is_left_alone() {
    let bytes = edited(SIMPLE, |document| {
        document["nodes"]
            .as_array_mut()
            .unwrap()
            .push(json!({ "extensions": { "EXT_mesh_gpu_instancing": { "attributes": {} } } }));
        document["scenes"][0]["nodes"] = json!([0, 1]);
    });
    assert_eq!(load_slice(&bytes).unwrap().instanced[0].nodes.len(), 1);
}

// Oracle: the decoded budget is charged for every instance's transform from
// the declared count before anything is read or allocated: ten million
// instances of a sparse accessor, which holds one substitution and no
// buffer view, would take 640 MB of transforms.
#[wasm_bindgen_test(unsupported = test)]
fn the_instance_count_is_charged_before_allocation() {
    let bytes = edited(SIMPLE, |document| {
        let accessors = document["accessors"].as_array_mut().unwrap();
        // Index 0 from the cube's indices, and one translation.
        accessors.push(json!({
            "componentType": 5126,
            "count": 10_000_000,
            "type": "VEC3",
            "sparse": {
                "count": 1,
                "indices": { "bufferView": 0, "componentType": 5125 },
                "values": { "bufferView": 3 },
            },
        }));
        let empty = accessors.len() - 1;
        document["nodes"][0]["extensions"]["EXT_mesh_gpu_instancing"]["attributes"] =
            json!({ "TRANSLATION": empty });
    });
    let budget = LoadBudget::new(1 << 20, 64 << 20).unwrap();
    let message = load_slice_with_options(
        &bytes,
        LoadOptions {
            budget,
            ..LoadOptions::default()
        },
    )
    .err()
    .unwrap()
    .to_string();
    assert!(message.contains("budget"), "{message}");
}

// Oracle: the README's budget rule: an instanced mesh is charged once
// however many nodes instance it, and each instance 64 bytes. A second node
// instancing the sample's cube 125 more times raises the least decoded
// budget the load needs by those instances alone, not by another cube.
#[wasm_bindgen_test(unsupported = test)]
fn a_shared_instanced_mesh_is_charged_once() {
    let second = edited(SIMPLE, |document| {
        let node = document["nodes"][0].clone();
        document["nodes"].as_array_mut().unwrap().push(node);
        document["scenes"][0]["nodes"] = json!([0, 1]);
    });
    let least = |bytes: &[u8]| {
        let loads = |decoded| {
            let budget = LoadBudget::new(1 << 20, decoded).unwrap();
            load_slice_with_options(
                bytes,
                LoadOptions {
                    budget,
                    ..LoadOptions::default()
                },
            )
            .is_ok()
        };
        let (mut low, mut high) = (0, 1 << 20);
        assert!(loads(high));
        while high - low > 1 {
            let middle = (low + high) / 2;
            if loads(middle) {
                high = middle
            } else {
                low = middle
            }
        }
        high
    };
    assert_eq!(least(&second) - least(&embedded(SIMPLE)), 125 * 64);
}
