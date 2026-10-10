//! Skins, morph targets and clips imported from a hand-built document.
use super::*;
use crate::deformation::{ChannelValues, Interpolation};
use glam::{Quat, Vec3};
use serde_json::{Value, json};
use wasm_bindgen_test::wasm_bindgen_test;

/// A document's binary chunk and accessors, built one accessor at a time.
#[derive(Default)]
struct Buffer {
    bytes: Vec<u8>,
    views: Vec<Value>,
    accessors: Vec<Value>,
}

impl Buffer {
    fn accessor(&mut self, bytes: &[u8], component: u32, kind: &str, count: usize) -> usize {
        self.views.push(json!({
            "buffer": 0, "byteOffset": self.bytes.len(), "byteLength": bytes.len()
        }));
        self.bytes.extend_from_slice(bytes);
        let mut accessor = json!({
            "bufferView": self.views.len() - 1, "componentType": component, "count": count, "type": kind
        });
        if kind == "VEC3" && component == 5126 {
            let values: &[f32] = bytemuck::cast_slice(bytes);
            let (mut min, mut max) = ([f32::MAX; 3], [f32::MIN; 3]);
            for v in values.chunks(3) {
                for axis in 0..3 {
                    min[axis] = min[axis].min(v[axis]);
                    max[axis] = max[axis].max(v[axis]);
                }
            }
            accessor["min"] = json!(min);
            accessor["max"] = json!(max);
        }
        self.accessors.push(accessor);
        self.accessors.len() - 1
    }

    fn floats(&mut self, values: &[f32], kind: &str, width: usize) -> usize {
        self.accessor(
            bytemuck::cast_slice(values),
            5126,
            kind,
            values.len() / width,
        )
    }

    /// `document` with this buffer, as a GLB.
    fn glb(self, mut document: Value) -> Vec<u8> {
        document["buffers"] = json!([{"byteLength": self.bytes.len()}]);
        document["bufferViews"] = json!(self.views);
        document["accessors"] = json!(self.accessors);
        let mut json = serde_json::to_vec(&document).unwrap();
        while !json.len().is_multiple_of(4) {
            json.push(b' ');
        }
        let mut bin = self.bytes;
        while !bin.len().is_multiple_of(4) {
            bin.push(0);
        }
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
}

/// A rig: a root with two bones, a mesh skinned to the second of two skins
/// under a node whose transform glTF ignores, and a rigid, scaled mesh with a
/// morph target; with a clip turning the second bone and stepping the
/// morph weight. Each `edit` changes the document before it is encoded.
fn rigged(edit: impl FnOnce(&mut Value, &mut Buffer)) -> Vec<u8> {
    let mut buffer = Buffer::default();
    let triangle = [0., 0., 0., 1., 0., 0., 0., 1., 0.];
    let up = [0., 0., 1., 0., 0., 1., 0., 0., 1.];
    let positions = buffer.floats(&triangle, "VEC3", 3);
    let normals = buffer.floats(&up, "VEC3", 3);
    let joints: [[u16; 4]; 3] = [[0, 0, 0, 0], [1, 0, 0, 0], [0, 1, 0, 0]];
    let joints = buffer.accessor(bytemuck::cast_slice(&joints), 5123, "VEC4", 3);
    let weights = buffer.floats(
        &[1., 0., 0., 0., 1., 0., 0., 0., 0.5, 0.5, 0., 0.],
        "VEC4",
        4,
    );
    let rigid_positions = buffer.floats(&triangle, "VEC3", 3);
    let rigid_normals = buffer.floats(&up, "VEC3", 3);
    let lift = buffer.floats(&up, "VEC3", 3);
    let tilt = buffer.floats(&[0.1, 0., 0., 0.1, 0., 0., 0.1, 0., 0.], "VEC3", 3);
    let along = buffer.floats(&[1., 0., 0., 1., 1., 0., 0., 1., 1., 0., 0., 1.], "VEC4", 4);
    let twist = buffer.floats(&[0., 0.1, 0., 0., 0.1, 0., 0., 0.1, 0.], "VEC3", 3);
    // Each bone's inverse rest transform in the asset's space.
    let inverse_binds: Vec<f32> = [Vec3::new(-1., -1., 0.), Vec3::new(-1., -2., 0.)]
        .iter()
        .flat_map(|&t| Mat4::from_translation(t).to_cols_array())
        .collect();
    let inverse_binds = buffer.floats(&inverse_binds, "MAT4", 16);
    let times = buffer.floats(&[0., 1.], "SCALAR", 1);
    let quarter = Quat::from_rotation_z(std::f32::consts::FRAC_PI_2).to_array();
    let turns = buffer.floats(
        &[
            0., 0., 0., 1., quarter[0], quarter[1], quarter[2], quarter[3],
        ],
        "VEC4",
        4,
    );
    let steps = buffer.floats(&[0., 1.], "SCALAR", 1);
    let mut document = json!({
        "asset": {"version": "2.0"},
        "meshes": [
            {"primitives": [{"attributes": {"POSITION": positions, "NORMAL": normals, "JOINTS_0": joints, "WEIGHTS_0": weights}}]},
            {"primitives": [{"attributes": {"POSITION": rigid_positions, "NORMAL": rigid_normals, "TANGENT": along},
                "targets": [{"POSITION": lift, "NORMAL": tilt, "TANGENT": twist}]}], "weights": [0.5]}
        ],
        "nodes": [
            {"name": "root", "translation": [1, 0, 0], "children": [1, 3]},
            {"name": "upper", "translation": [0, 1, 0], "children": [2]},
            {"name": "lower", "translation": [0, 1, 0]},
            {"name": "body", "mesh": 0, "skin": 1, "translation": [5, 5, 5]},
            {"name": "lid", "mesh": 1, "scale": [2, 2, 2], "weights": [0.25]}
        ],
        "skins": [{"joints": [2]}, {"joints": [1, 2], "inverseBindMatrices": inverse_binds}],
        "animations": [{"name": "wave",
            "samplers": [{"input": times, "output": turns}, {"input": times, "output": steps, "interpolation": "STEP"}],
            "channels": [{"sampler": 0, "target": {"node": 2, "path": "rotation"}},
                         {"sampler": 1, "target": {"node": 4, "path": "weights"}}]}],
        "scenes": [{"nodes": [0, 4]}], "scene": 0
    });
    edit(&mut document, &mut buffer);
    buffer.glb(document)
}

// Plausible defects: a skinned mesh baked with its node's transform, which
// glTF ignores; joint indices left relative to their skin; skins' joints or
// inverse bind matrices misordered; a rigid node's morph displacements left
// unscaled; rest weights taken from the mesh instead of its node; parents or
// clips misread. The oracles are the document's authored values, and glTF's
// joint matrix, which is the identity at rest when each inverse bind matrix
// is its joint's inverse rest transform.
#[wasm_bindgen_test(unsupported = test)]
fn skins_morph_targets_and_clips_import_as_plain_data() {
    let asset = load_slice(&rigged(|_, _| {})).unwrap();
    let rig = &asset.rig;
    let parents: Vec<_> = rig.nodes.iter().map(|node| node.parent).collect();
    assert_eq!(parents, [None, Some(0), Some(1), Some(0), None]);
    assert_eq!(rig.nodes[1].name.as_deref(), Some("upper"));
    // Skin 0's one joint, then skin 1's.
    let joints: Vec<_> = rig.joints.iter().map(|joint| joint.node).collect();
    assert_eq!(joints, [2, 1, 2]);
    assert_eq!(rig.joints[0].inverse_bind, Mat4::IDENTITY);
    let rest: Vec<Mat4> = rig.nodes.iter().map(|node| node.rest()).collect();
    for matrix in &rig.joint_matrices(&rest)[1..] {
        assert!(matrix.abs_diff_eq(Mat4::IDENTITY, 1e-6), "{matrix}");
    }
    // Turning the upper bone turns the lower one's matrix about the upper.
    let mut posed = rest.clone();
    posed[1] *= Mat4::from_rotation_z(std::f32::consts::FRAC_PI_2);
    let lower = rig.joint_matrices(&posed)[2];
    assert!(
        lower
            .transform_point3(Vec3::new(1., 2., 0.))
            .abs_diff_eq(Vec3::new(0., 1., 0.), 1e-6)
    );

    let skinned = asset
        .meshes
        .iter()
        .find(|mesh| !mesh.deformation.influences.is_empty())
        .unwrap();
    let positions: Vec<_> = skinned.vertices.iter().map(|v| v.position).collect();
    assert_eq!(positions, [[0., 0., 0.], [1., 0., 0.], [0., 1., 0.]]);
    let influences: Vec<_> = skinned
        .deformation
        .influences
        .iter()
        .map(|i| (i.joints, i.weights))
        .collect();
    assert_eq!(
        influences,
        [
            ([1, 1, 1, 1], [1., 0., 0., 0.]),
            ([2, 1, 1, 1], [1., 0., 0., 0.]),
            ([1, 2, 1, 1], [0.5, 0.5, 0., 0.])
        ]
    );
    assert!(skinned.deformation.morph_targets.is_empty());

    let lid = asset
        .meshes
        .iter()
        .find(|mesh| !mesh.deformation.morph_targets.is_empty())
        .unwrap();
    assert!(lid.deformation.influences.is_empty());
    assert_eq!(lid.vertices[1].position, [2., 0., 0.]);
    let target = &lid.deformation.morph_targets[0];
    assert_eq!(target.weight, 0);
    for delta in &target.deltas {
        // Positions scaled with the node; normals by the inverse transpose
        // and tangents by the node, each kept at the scale of its normalized
        // baked vector.
        assert_eq!(delta.position, [0., 0., 2.]);
        assert!(Vec3::from(delta.normal).abs_diff_eq(Vec3::new(0.1, 0., 0.), 1e-6));
        assert!(Vec3::from(delta.tangent).abs_diff_eq(Vec3::new(0., 0.1, 0.), 1e-6));
    }
    assert_eq!(rig.morph_weights.len(), 1);
    assert_eq!(
        (rig.morph_weights[0].node, rig.morph_weights[0].rest),
        (4, 0.25)
    );

    let clip = &rig.clips[0];
    assert_eq!(clip.name.as_deref(), Some("wave"));
    let turn = &clip.channels[0];
    assert_eq!((turn.node, turn.interpolation), (2, Interpolation::Linear));
    assert_eq!(turn.times, [0., 1.]);
    match &turn.values {
        ChannelValues::Rotation(values) => {
            assert!(
                values[1].abs_diff_eq(Quat::from_rotation_z(std::f32::consts::FRAC_PI_2), 1e-6)
            );
        }
        other => panic!("rotation read as {other:?}"),
    }
    let step = &clip.channels[1];
    assert_eq!((step.node, step.interpolation), (4, Interpolation::Step));
    assert_eq!(step.values, ChannelValues::MorphWeights(vec![0., 1.]));

    // A static document has no rig.
    let mut static_asset = load_slice(&rigged(|document, _| {
        document.as_object_mut().unwrap().remove("skins");
        document.as_object_mut().unwrap().remove("animations");
        document["meshes"] = json!([document["meshes"][0].clone()]);
        let attributes = document["meshes"][0]["primitives"][0]["attributes"]
            .as_object_mut()
            .unwrap();
        attributes.remove("JOINTS_0");
        attributes.remove("WEIGHTS_0");
        document["nodes"][3].as_object_mut().unwrap().remove("skin");
        document["nodes"][4].as_object_mut().unwrap().remove("mesh");
    }))
    .unwrap();
    assert_eq!(static_asset.rig, Default::default());
    assert!(static_asset.meshes.pop().unwrap().deformation.is_rigid());
}

// Plausible defects: a node hierarchy that is not a set of trees walked as
// one: the scene's walk recursing forever through a node that is its own
// child; a cycle the scene never reaches loaded, so posing a joint on it
// recurses forever later; a node with two parents read under both.
#[wasm_bindgen_test(unsupported = test)]
fn node_hierarchies_that_are_not_trees_are_rejected() {
    type Edit = fn(&mut Value, &mut Buffer);
    let cases: [(&str, Edit); 3] = [
        ("a joint its own child", |document, _| {
            document["nodes"][2]["children"] = json!([2]);
        }),
        ("a joint on a cycle outside the scene", |document, _| {
            let nodes = document["nodes"].as_array_mut().unwrap();
            nodes.push(json!({"children": [6]}));
            nodes.push(json!({"children": [5]}));
            document["skins"][0]["joints"] = json!([6]);
        }),
        ("a joint with two parents", |document, _| {
            document["nodes"][0]["children"] = json!([1, 3, 2]);
        }),
    ];
    for (label, edit) in cases {
        let error = load_slice(&rigged(edit))
            .err()
            .unwrap_or_else(|| panic!("{label} was accepted"))
            .to_string();
        assert!(error.contains("node hierarchy"), "{label}: {error}");
    }
}

// Plausible defect: a game-built rig whose parents form a cycle evaluated
// by recursing up the parents forever, overflowing the stack, or a cycle
// corrupting another joint's matrix. The oracle for the tree's joint is
// glTF's: its parent's transform times its own.
#[wasm_bindgen_test(unsupported = test)]
fn a_built_rig_with_a_parent_cycle_still_evaluates() {
    let node = |parent, x: f32| crate::deformation::Node {
        name: None,
        parent,
        translation: Vec3::new(x, 0., 0.),
        rotation: Quat::IDENTITY,
        scale: Vec3::ONE,
    };
    let joint = |node| crate::deformation::Joint {
        node,
        inverse_bind: Mat4::IDENTITY,
    };
    let rig = crate::deformation::Rig {
        nodes: vec![
            node(Some(1), 1.),
            node(Some(0), 2.),
            node(None, 3.),
            node(Some(2), 4.),
        ],
        joints: vec![joint(0), joint(3)],
        ..Default::default()
    };
    let locals: Vec<Mat4> = rig.nodes.iter().map(|node| node.rest()).collect();
    let matrices = rig.joint_matrices(&locals);
    assert_eq!(matrices.len(), 2);
    assert!(matrices[0].is_finite());
    assert_eq!(matrices[1], locals[2] * locals[3]);
}

// Plausible defects: a fifth influence silently dropped; a skinned
// primitive without influences, with an out-of-range joint or with
// weightless vertices loaded as partial geometry or left for the scene to
// refuse without the primitive's label.
#[wasm_bindgen_test(unsupported = test)]
fn unsupported_skins_are_rejected() {
    let fifth = |document: &mut Value, buffer: &mut Buffer| {
        let joints = buffer.accessor(bytemuck::cast_slice(&[[0u16; 4]; 3]), 5123, "VEC4", 3);
        let weights = buffer.floats(&[0.; 12], "VEC4", 4);
        let attributes = &mut document["meshes"][0]["primitives"][0]["attributes"];
        attributes["JOINTS_1"] = json!(joints);
        attributes["WEIGHTS_1"] = json!(weights);
    };
    let unweighted = |document: &mut Value, _: &mut Buffer| {
        let attributes = document["meshes"][0]["primitives"][0]["attributes"]
            .as_object_mut()
            .unwrap();
        attributes.remove("WEIGHTS_0");
    };
    let beyond = |document: &mut Value, _: &mut Buffer| {
        // Skin 0 has one joint; the primitive names a second.
        document["nodes"][3]["skin"] = json!(0);
    };
    let weightless = |document: &mut Value, buffer: &mut Buffer| {
        let weights = buffer.floats(
            &[0., 0., 0., 0., 1., 0., 0., 0., 0.5, 0.5, 0., 0.],
            "VEC4",
            4,
        );
        document["meshes"][0]["primitives"][0]["attributes"]["WEIGHTS_0"] = json!(weights);
    };
    // Skin 0 grows to 65536 joints, so skin 1's start past the most an
    // asset may have.
    let crowded = |document: &mut Value, _: &mut Buffer| {
        let nodes = document["nodes"].as_array_mut().unwrap();
        let first = nodes.len();
        nodes.extend((0..65536).map(|_| json!({})));
        document["skins"][0]["joints"] = json!((first..first + 65536).collect::<Vec<_>>());
    };
    for (label, error, says) in [
        ("fifth influence", load_slice(&rigged(fifth)).err(), "four"),
        (
            "missing weights",
            load_slice(&rigged(unweighted)).err(),
            "WEIGHTS_0",
        ),
        (
            "joint beyond its skin",
            load_slice(&rigged(beyond)).err(),
            "its skin lacks",
        ),
        (
            "weights of zero sum",
            load_slice(&rigged(weightless)).err(),
            "positive sum",
        ),
        (
            "joint beyond 65535",
            load_slice(&rigged(crowded)).err(),
            "beyond the 65536",
        ),
    ] {
        let error = error
            .unwrap_or_else(|| panic!("{label} was accepted"))
            .to_string();
        assert!(
            error.contains("mesh 0 primitive 0") && error.contains(says),
            "{label}: {error}"
        );
    }
}

// Plausible defect: an accessor whose component type or shape glTF does not
// allow for its use, with no elements, or reaching past its buffer view
// handed to the gltf crate's readers, which panic on it (an `unreachable!`,
// a slice assertion or an arithmetic overflow) or misread it, taking the
// game down instead of returning an error.
#[wasm_bindgen_test(unsupported = test)]
fn malformed_accessors_are_rejected() {
    fn attribute(document: &Value, semantic: &str) -> usize {
        document["meshes"][0]["primitives"][0]["attributes"][semantic]
            .as_u64()
            .unwrap() as usize
    }
    // A 32-bit target's gltf refuses a size beyond its `usize` itself.
    let (past_view, overflowing_view) = if cfg!(target_pointer_width = "64") {
        (
            "mesh 0 primitive 0: POSITION accessor",
            "mesh 0 primitive 0: indices accessor",
        )
    } else {
        ("exceeds system limits", "exceeds system limits")
    };
    type Edit = fn(&mut Value, &mut Buffer);
    let cases: [(&str, Edit, &str); 15] = [
        (
            "FLOAT indices",
            |document, buffer| {
                let indices = buffer.floats(&[0., 1., 2.], "SCALAR", 1);
                document["meshes"][0]["primitives"][0]["indices"] = json!(indices);
            },
            "mesh 0 primitive 0: indices accessor",
        ),
        (
            "FLOAT JOINTS_0",
            |document, buffer| {
                let joints = attribute(document, "JOINTS_0");
                buffer.accessors[joints]["componentType"] = json!(5126);
            },
            "mesh 0 primitive 0: JOINTS_0 accessor",
        ),
        (
            "VEC2 COLOR_0",
            |document, buffer| {
                let colors = buffer.floats(&[1.; 6], "VEC2", 2);
                document["meshes"][0]["primitives"][0]["attributes"]["COLOR_0"] = json!(colors);
            },
            "mesh 0 primitive 0: COLOR_0 accessor",
        ),
        (
            "UNSIGNED_INT TEXCOORD_0",
            |document, buffer| {
                let uvs = buffer.accessor(bytemuck::cast_slice(&[0u32; 6]), 5125, "VEC2", 3);
                document["meshes"][0]["primitives"][0]["attributes"]["TEXCOORD_0"] = json!(uvs);
            },
            "mesh 0 primitive 0: TEXCOORD_0 accessor",
        ),
        (
            "UNSIGNED_SHORT POSITION",
            |document, buffer| {
                let positions = attribute(document, "POSITION");
                buffer.accessors[positions]["componentType"] = json!(5123);
            },
            "mesh 0 primitive 0: POSITION accessor",
        ),
        (
            "VEC4 NORMAL",
            |document, buffer| {
                let normals = buffer.floats(&[0., 0., 1., 0.].repeat(3), "VEC4", 4);
                document["meshes"][0]["primitives"][0]["attributes"]["NORMAL"] = json!(normals);
            },
            "mesh 0 primitive 0: NORMAL accessor",
        ),
        (
            "POSITION of no elements",
            |document, buffer| {
                let positions = attribute(document, "POSITION");
                buffer.accessors[positions]["count"] = json!(0);
            },
            "mesh 0 primitive 0: POSITION accessor",
        ),
        (
            "POSITION past its view",
            |document, buffer| {
                let positions = attribute(document, "POSITION");
                buffer.accessors[positions]["count"] = json!(1u64 << 62);
            },
            past_view,
        ),
        (
            "POSITION stride under an element",
            |document, buffer| {
                let positions = attribute(document, "POSITION");
                let view = buffer.accessors[positions]["bufferView"].as_u64().unwrap() as usize;
                buffer.views[view]["byteStride"] = json!(4);
            },
            "mesh 0 primitive 0: POSITION accessor",
        ),
        (
            "indices in a view whose end overflows",
            |document, buffer| {
                let indices =
                    buffer.accessor(bytemuck::cast_slice(&[0u16, 1, 2]), 5123, "SCALAR", 3);
                let view = buffer.accessors[indices]["bufferView"].as_u64().unwrap() as usize;
                buffer.views[view]["byteOffset"] = json!(u64::MAX - 2);
                document["meshes"][0]["primitives"][0]["indices"] = json!(indices);
            },
            overflowing_view,
        ),
        (
            // Read as no indices, which draws the vertices in order.
            "indices in a view past its buffer",
            |document, buffer| {
                let indices =
                    buffer.accessor(bytemuck::cast_slice(&[0u16, 1, 2]), 5123, "SCALAR", 3);
                let view = buffer.accessors[indices]["bufferView"].as_u64().unwrap() as usize;
                buffer.views[view]["byteOffset"] = json!(buffer.bytes.len() + 64);
                document["meshes"][0]["primitives"][0]["indices"] = json!(indices);
            },
            "mesh 0 primitive 0: indices accessor",
        ),
        (
            "morph target of no sparse elements",
            |document, buffer| {
                let lift = document["meshes"][1]["primitives"][0]["targets"][0]["POSITION"]
                    .as_u64()
                    .unwrap() as usize;
                let view = buffer.accessors[lift]["bufferView"].clone();
                buffer.accessors[lift]["sparse"] = json!({
                    "count": 0,
                    "indices": {"bufferView": view, "componentType": 5123},
                    "values": {"bufferView": view}
                });
            },
            "mesh 1 primitive 0: morph target 0 accessor",
        ),
        (
            "MAT3 inverse binds",
            |document, buffer| {
                let binds = document["skins"][1]["inverseBindMatrices"]
                    .as_u64()
                    .unwrap() as usize;
                buffer.accessors[binds]["type"] = json!("MAT3");
            },
            "skin 1: inverse bind matrices accessor",
        ),
        (
            "UNSIGNED_BYTE keyframe times",
            |document, buffer| {
                let times = document["animations"][0]["samplers"][0]["input"]
                    .as_u64()
                    .unwrap() as usize;
                buffer.accessors[times]["componentType"] = json!(5121);
            },
            "animation 0: keyframe times accessor",
        ),
        (
            "UNSIGNED_INT rotations",
            |document, buffer| {
                let turns = document["animations"][0]["samplers"][0]["output"]
                    .as_u64()
                    .unwrap() as usize;
                buffer.accessors[turns]["componentType"] = json!(5125);
            },
            "animation 0: keyframe values accessor",
        ),
    ];
    for (label, edit, says) in cases {
        let error = load_slice(&rigged(edit))
            .err()
            .unwrap_or_else(|| panic!("{label} was accepted"))
            .to_string();
        assert!(error.contains(says), "{label}: {error}");
    }
}

// Plausible defect: an embedded image's buffer view reaching past its
// buffer handed to gltf's image decoding, which slices the buffer with it
// unchecked and panics.
#[wasm_bindgen_test(unsupported = test)]
fn an_image_view_past_its_buffer_is_rejected() {
    let bytes = rigged(|document, buffer| {
        buffer
            .views
            .push(json!({"buffer": 0, "byteOffset": buffer.bytes.len(), "byteLength": 64}));
        document["images"] =
            json!([{"bufferView": buffer.views.len() - 1, "mimeType": "image/png"}]);
    });
    let error = load_slice(&bytes)
        .err()
        .expect("an image past its buffer was accepted")
        .to_string();
    assert!(
        error.contains("image 0") && error.contains("buffer"),
        "{error}"
    );
}
