//! Skins, morph targets and clips imported from a hand-built document.
use super::*;
use crate::content::identity::Identity;
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

/// One rigid triangle instanced by the supplied scene nodes, fully in memory.
fn mesh_nodes(nodes: Value, roots: Value) -> Vec<u8> {
    let mut buffer = Buffer::default();
    let positions = buffer.floats(&[0., 0., 0., 1., 0., 0., 0., 1., 0.], "VEC3", 3);
    let normals = buffer.floats(&[0., 0., 1., 0., 0., 1., 0., 0., 1.], "VEC3", 3);
    buffer.glb(json!({
        "asset": {"version": "2.0"},
        "meshes": [{"primitives": [{"attributes": {"POSITION": positions, "NORMAL": normals}}]}],
        "nodes": nodes,
        "scenes": [{"nodes": roots}], "scene": 0
    }))
}

// Regression: recursive import aborts on this valid hierarchy. The terminal
// triangle's translated coordinates independently expose lost ancestor transforms.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn a_twenty_thousand_node_chain_loads_on_a_small_stack() {
    let mut nodes: Vec<Value> = (0..20_000)
        .map(|index| json!({"translation": [1, 0, 0], "children": [index + 1]}))
        .collect();
    nodes[19_999] = json!({"translation": [1, 0, 0], "mesh": 0});
    let bytes = mesh_nodes(json!(nodes), json!([0]));
    std::thread::Builder::new()
        .stack_size(1 << 20)
        .spawn(move || {
            let asset = load_slice(&bytes).unwrap();
            let triangle: Vec<_> = asset.meshes[0]
                .indices
                .iter()
                .map(|&index| asset.meshes[0].vertices[index as usize].position)
                .collect();
            assert_eq!(
                triangle,
                [[20_000., 0., 0.], [20_001., 0., 0.], [20_000., 1., 0.]]
            );
        })
        .unwrap()
        .join()
        .unwrap();
}

// Regression: a LIFO walk reverses siblings or visits them before descendants;
// filtering an ancestor must not discard its children or its transform.
#[wasm_bindgen_test(unsupported = test)]
fn mesh_nodes_keep_declared_depth_first_order_and_unselected_ancestor_transforms() {
    let bytes = mesh_nodes(
        json!([
            {"name": "root", "mesh": 0, "translation": [10, 0, 0], "scale": [2, 1, 1], "children": [2, 1]},
            {"name": "second", "mesh": 0, "translation": [1, 0, 0]},
            {"name": "first", "mesh": 0, "translation": [3, 0, 0], "children": [3]},
            {"name": "grandchild", "mesh": 0, "translation": [4, 0, 0]},
            {"name": "other_root", "mesh": 0, "translation": [50, 0, 0]}
        ]),
        json!([0, 4]),
    );
    let visited = std::sync::Mutex::new(Vec::new());
    let select = |name: Option<&str>| {
        visited.lock().unwrap().push(name.unwrap().to_owned());
        name != Some("root")
    };
    let asset = load_slice_with_options(
        &bytes,
        LoadOptions {
            nodes: Some(&select),
            ..LoadOptions::default()
        },
    )
    .unwrap();
    assert_eq!(
        visited.into_inner().unwrap(),
        ["root", "first", "grandchild", "second", "other_root"]
    );
    let origins: Vec<_> = asset.meshes[0]
        .indices
        .chunks_exact(3)
        .map(|triangle| asset.meshes[0].vertices[triangle[0] as usize].position)
        .collect();
    // Root x=10, scale x=2: first x=16, its child x=24, second x=12.
    // The independent scene root keeps x=50 instead of inheriting x=10.
    assert_eq!(
        origins,
        [[16., 0., 0.], [24., 0., 0.], [12., 0., 0.], [50., 0., 0.]]
    );
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

// Plausible defects: a morph target's tangent delta divided by the length
// of a tangent along its normal: zero, so the delta is not finite and
// preparing the asset refuses the file the loader accepted; or a few ulps,
// so a vertex with no tangent gets a huge one. The oracle is
// `Vertex::tangent`'s contract (zero means absent, so nothing displaces it)
// and the lid's third vertex, whose frame keeps its delta.
#[wasm_bindgen_test(unsupported = test)]
fn morph_targets_leave_absent_tangents_undisplaced() {
    let lid_along_its_normal = |document: &mut Value, buffer: &mut Buffer| {
        let off = 4. * f32::EPSILON;
        let tangents = buffer.floats(
            &[0., 0., 1., 1., off, 0., 1., 1., 1., 0., 0., 1.],
            "VEC4",
            4,
        );
        document["meshes"][1]["primitives"][0]["attributes"]["TANGENT"] = json!(tangents);
    };
    let asset = load_slice(&rigged(lid_along_its_normal)).unwrap_or_else(|error| panic!("{error}"));
    let lid = asset
        .meshes
        .iter()
        .find(|mesh| !mesh.deformation.morph_targets.is_empty())
        .unwrap();
    let tangents: Vec<_> = lid.vertices.iter().map(|vertex| vertex.tangent).collect();
    assert_eq!(tangents, [[0.; 4], [0.; 4], [1., 0., 0., 1.]]);
    let deltas = &lid.deformation.morph_targets[0].deltas;
    assert_eq!(deltas[0].tangent, [0.; 3]);
    assert_eq!(deltas[1].tangent, [0.; 3]);
    assert!(Vec3::from(deltas[2].tangent).abs_diff_eq(Vec3::new(0., 0.1, 0.), 1e-6));
    let meshes = asset
        .meshes
        .into_iter()
        .map(|mesh| crate::ModelMesh {
            vertices: mesh.vertices,
            indices: mesh.indices,
            material: crate::MaterialId::issue(0, 0),
            deformation: mesh.deformation,
        })
        .collect();
    crate::PreparedModel::new(meshes).unwrap_or_else(|error| panic!("{error}"));
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

// Plausible defect: a joint whose matrix has a zero-scale axis loaded with
// the NaN rotation its decomposition divides out, which posing spreads to
// every joint beneath it. Mesh nodes refuse such a singular transform; a
// joint, which no mesh node's check sees, must be refused too; a helper
// node no joint descends from, which poses nothing, must still load.
#[wasm_bindgen_test(unsupported = test)]
fn a_joint_with_a_zero_scale_matrix_is_rejected() {
    let collapsed = json!([0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 1, 0, 1]);
    let error = load_slice(&rigged(|document, _| {
        document["nodes"][1] =
            json!({"name": "upper", "children": [2], "matrix": collapsed.clone()});
    }))
    .err()
    .expect("a zero-scale joint loaded")
    .to_string();
    assert!(error.contains("node 1 (upper)"), "{error}");
    let helper = load_slice(&rigged(|document, _| {
        document["nodes"][0]["children"] = json!([1, 3, 5]);
        let nodes = document["nodes"].as_array_mut().unwrap();
        nodes.push(json!({"name": "helper", "matrix": collapsed.clone()}));
    }));
    assert!(helper.is_ok(), "a zero-scale leaf helper was refused");
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

// Plausible defects: a mesh's primitive with another nonzero number of
// morph targets than its others loaded against the node's weights, its
// extra targets driven by weights past the node's; or a primitive without
// targets, which glTF (3.7.2.2, KhronosGroup/glTF#2650) loads unmorphed,
// refused, or taken for the mesh's count when it comes first, so the node's
// weights and their animation are lost.
#[wasm_bindgen_test(unsupported = test)]
fn a_mesh_s_morphed_primitives_share_one_target_count() {
    // The rigged lid's one primitive (one target), with a copy of `targets`
    // targets inserted at `at`.
    let with_copy = |targets: usize, at: usize| {
        rigged(|document, _| {
            let lid = &document["meshes"][1]["primitives"][0];
            let mut copy = json!({"attributes": lid["attributes"].clone()});
            if targets > 0 {
                copy["targets"] = json!(vec![lid["targets"][0].clone(); targets]);
            }
            document["meshes"][1]["primitives"]
                .as_array_mut()
                .unwrap()
                .insert(at, copy);
        })
    };
    let error = load_slice(&with_copy(2, 1))
        .err()
        .expect("a primitive of 2 targets beside one of 1 was accepted")
        .to_string();
    assert!(
        error.contains("mesh 1 primitive 1") && error.contains("morph targets"),
        "{error}"
    );
    for at in [1, 0] {
        let asset = load_slice(&with_copy(0, at)).unwrap_or_else(|error| panic!("{error}"));
        let morphed: Vec<_> = asset
            .meshes
            .iter()
            .filter(|mesh| !mesh.deformation.morph_targets.is_empty())
            .collect();
        assert_eq!(morphed.len(), 1);
        let targets = &morphed[0].deformation.morph_targets;
        assert_eq!(targets.len(), 1);
        assert_eq!((targets[0].weight, targets[0].deltas.len()), (0, 3));
        assert!(
            asset
                .meshes
                .iter()
                .any(|mesh| mesh.deformation.is_rigid() && mesh.vertices.len() == 3),
            "the primitive without targets did not load unmorphed"
        );
        assert_eq!(
            asset.rig.clips[0].channels[1].values,
            ChannelValues::MorphWeights(vec![0., 1.])
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
    // Makes `accessor` sparse without a buffer view and declares 2^20
    // elements: more than the file's buffers hold or its use's count
    // allows, but within the load limit, which a load checks first.
    fn viewless(buffer: &mut Buffer, accessor: usize) {
        sparse_without_view(buffer, accessor, 1 << 20);
    }
    // A 32-bit target's gltf refuses a size beyond its `usize` itself.
    let overflowing_view = if cfg!(target_pointer_width = "64") {
        "mesh 0 primitive 0: indices accessor"
    } else {
        "exceeds system limits"
    };
    type Edit = fn(&mut Value, &mut Buffer);
    let cases: [(&str, Edit, &str); 20] = [
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
                buffer.accessors[positions]["count"] = json!(4);
            },
            "mesh 0 primitive 0: POSITION accessor",
        ),
        (
            // No data, but gltf's readers allocate every element up front.
            "POSITION of countless elements without a view",
            |document, buffer| viewless(buffer, attribute(document, "POSITION")),
            "mesh 0 primitive 0: POSITION accessor",
        ),
        (
            "NORMAL of countless elements without a view",
            |document, buffer| viewless(buffer, attribute(document, "NORMAL")),
            "mesh 0 primitive 0: NORMAL accessor",
        ),
        (
            "inverse binds of countless elements without a view",
            |document, buffer| {
                let binds = document["skins"][1]["inverseBindMatrices"]
                    .as_u64()
                    .unwrap() as usize;
                viewless(buffer, binds);
            },
            "skin 1: inverse bind matrices accessor",
        ),
        (
            "keyframe times of countless elements without a view",
            |document, buffer| {
                let times = document["animations"][0]["samplers"][0]["input"]
                    .as_u64()
                    .unwrap() as usize;
                viewless(buffer, times);
            },
            "animation 0: keyframe times accessor",
        ),
        (
            "keyframe values of countless elements without a view",
            |document, buffer| {
                let turns = document["animations"][0]["samplers"][0]["output"]
                    .as_u64()
                    .unwrap() as usize;
                viewless(buffer, turns);
            },
            "animation 0: keyframe values accessor",
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

/// Two sparse POSITION substitutions in a GLB. Both the view's offset
/// and the sparse indices' offset are nonzero, and the ignored prefix is
/// deliberately invalid as an index. The sparse values are authored mesh
/// positions, not output generated by the loader under test.
fn sparse_positions(component: u32, indices: [u32; 2], count: usize, base: bool) -> Vec<u8> {
    let mut buffer = Buffer::default();
    let triangle = [[0., 0., 0.], [1., 0., 0.], [0., 1., 0.]];
    let positions: Vec<f32> = triangle
        .iter()
        .cycle()
        .take(count)
        .flatten()
        .copied()
        .collect();
    let positions = buffer.floats(&positions, "VEC3", 3);
    let normals = buffer.floats(&[0., 0., 1.].repeat(count), "VEC3", 3);
    let mut bytes = vec![255; 4];
    for index in indices {
        match component {
            5121 => bytes.push(u8::try_from(index).unwrap()),
            5123 => bytes.extend(u16::try_from(index).unwrap().to_le_bytes()),
            5125 => bytes.extend(index.to_le_bytes()),
            _ => unreachable!(),
        }
    }
    let indices_view = buffer.views.len();
    buffer.views.push(json!({
        "buffer": 0, "byteOffset": buffer.bytes.len(), "byteLength": bytes.len()
    }));
    buffer.bytes.extend(bytes);
    while !buffer.bytes.len().is_multiple_of(4) {
        buffer.bytes.push(0);
    }
    let values = buffer.floats(&[2., 3., 4., 5., 6., 7.], "VEC3", 3);
    let values_view = buffer.accessors[values]["bufferView"].clone();
    let accessor = buffer.accessors[positions].as_object_mut().unwrap();
    if !base {
        accessor.remove("bufferView");
    }
    accessor.insert("min".into(), json!([0, 0, 0]));
    accessor.insert("max".into(), json!([5, 6, 7]));
    accessor.insert(
        "sparse".into(),
        json!({
            "count": 2,
            "indices": {"bufferView": indices_view, "byteOffset": 4, "componentType": component},
            "values": {"bufferView": values_view}
        }),
    );
    buffer.glb(json!({
        "asset": {"version": "2.0"},
        "meshes": [{"primitives": [{"attributes": {"POSITION": positions, "NORMAL": normals}}]}],
        "nodes": [{"mesh": 0}], "scenes": [{"nodes": [0]}], "scene": 0
    }))
}

// Plausible defect: gltf's sparse reader silently discards a descending,
// duplicate or out-of-range index and any subsequent substitutions. The
// independent oracle is glTF 2.0 3.6.2.3's required rejection of each
// malformed sequence, exercised through the complete asset import.
#[wasm_bindgen_test(unsupported = test)]
fn malformed_sparse_indices_fail_the_load() {
    for component in [5121, 5123, 5125] {
        for indices in [[5, 2], [2, 2], [1, 6], [1, 255]] {
            for base in [false, true] {
                let error = load_slice(&sparse_positions(component, indices, 6, base))
                    .err()
                    .unwrap_or_else(|| panic!("accepted {component}: {indices:?}, base {base}"))
                    .to_string();
                assert!(
                    error.contains("mesh 0 primitive 0: POSITION accessor")
                        && error.contains("sparse indices"),
                    "{component}: {indices:?}, base {base}: {error}"
                );
            }
        }
    }
    // A decoder reading only the low byte/word would mistake these for 1/0.
    for (component, indices) in [(5123, [0, 257]), (5125, [0, 65_536])] {
        let error = load_slice(&sparse_positions(component, indices, 6, true))
            .err()
            .unwrap_or_else(|| panic!("accepted {component}: {indices:?}"))
            .to_string();
        assert!(error.contains("sparse indices"), "{error}");
    }
}

// Plausible defects: rejecting valid sparse indices, reading the ignored
// prefix rather than byteOffset, or truncating u16/u32 indices. Authored
// positions and glTF's zero-initialisation rule provide the output oracle.
#[wasm_bindgen_test(unsupported = test)]
fn sparse_index_widths_and_offsets_preserve_authored_positions() {
    for (component, count, last) in [(5121, 6, 5), (5123, 300, 258), (5125, 65_541, 65_538)] {
        for base in [false, true] {
            let asset = load_slice(&sparse_positions(component, [1, last], count, base)).unwrap();
            let vertices = &asset.meshes[0].vertices;
            assert_eq!(vertices[1].position, [2., 3., 4.]);
            assert_eq!(vertices[last as usize].position, [5., 6., 7.]);
            assert_eq!(
                vertices[2].position,
                if base { [0., 1., 0.] } else { [0.; 3] }
            );
        }
    }
}

// Plausible defect: the refusal of accessors without a buffer view that
// declare more elements than their data backs also refusing glTF's usual
// sparse morph target, which holds as many elements as the mesh has
// vertices and substitutes only the moved ones.
#[wasm_bindgen_test(unsupported = test)]
fn a_sparse_morph_target_without_a_view_loads() {
    let bytes = rigged(|document, buffer| {
        let lift = document["meshes"][1]["primitives"][0]["targets"][0]["POSITION"]
            .as_u64()
            .unwrap() as usize;
        let indices = buffer.accessor(bytemuck::cast_slice(&[1u16]), 5123, "SCALAR", 1);
        let values = buffer.floats(&[0., 0., 1.], "VEC3", 3);
        let indices = buffer.accessors[indices]["bufferView"].clone();
        let values = buffer.accessors[values]["bufferView"].clone();
        let accessor = buffer.accessors[lift].as_object_mut().unwrap();
        accessor.remove("bufferView");
        accessor.insert(
            "sparse".into(),
            json!({
                "count": 1,
                "indices": {"bufferView": indices, "componentType": 5123},
                "values": {"bufferView": values}
            }),
        );
    });
    let asset = load_slice(&bytes).unwrap_or_else(|error| panic!("{error}"));
    let lid = asset
        .meshes
        .iter()
        .find(|mesh| !mesh.deformation.morph_targets.is_empty())
        .unwrap();
    // Vertex 1 alone moves, one unit up at the node's scale of 2.
    let lifts: Vec<_> = lid.deformation.morph_targets[0]
        .deltas
        .iter()
        .map(|delta| delta.position)
        .collect();
    assert_eq!(lifts, [[0., 0., 0.], [0., 0., 2.], [0., 0., 0.]]);
}

// Plausible defect: the bound on an accessor without a buffer view that
// nothing else counts refusing valid glTF in which its zero repeats, here
// a sparse POSITION with two vertices at the origin.
#[wasm_bindgen_test(unsupported = test)]
fn a_sparse_position_without_a_view_repeating_the_origin_loads() {
    let bytes = rigged(|document, buffer| {
        let positions = document["meshes"][1]["primitives"][0]["attributes"]["POSITION"]
            .as_u64()
            .unwrap() as usize;
        let indices = buffer.accessor(bytemuck::cast_slice(&[1u16]), 5123, "SCALAR", 1);
        let values = buffer.floats(&[1., 0., 0.], "VEC3", 3);
        let indices = buffer.accessors[indices]["bufferView"].clone();
        let values = buffer.accessors[values]["bufferView"].clone();
        let accessor = buffer.accessors[positions].as_object_mut().unwrap();
        accessor.remove("bufferView");
        accessor.insert(
            "sparse".into(),
            json!({
                "count": 1,
                "indices": {"bufferView": indices, "componentType": 5123},
                "values": {"bufferView": values}
            }),
        );
    });
    let asset = load_slice(&bytes).unwrap_or_else(|error| panic!("{error}"));
    let lid = asset
        .meshes
        .iter()
        .find(|mesh| !mesh.deformation.morph_targets.is_empty())
        .unwrap();
    // Vertex 1 alone leaves the origin, at the node's scale of 2.
    let positions: Vec<_> = lid.vertices.iter().map(|vertex| vertex.position).collect();
    assert_eq!(positions, [[0., 0., 0.], [2., 0., 0.], [0., 0., 0.]]);
}

// Plausible defect: two keyframes at one time, which glTF forbids (a
// sampler's input is strictly increasing), loaded, so a game interpolating
// over the pair divides by a zero span.
#[wasm_bindgen_test(unsupported = test)]
fn equal_keyframe_times_are_rejected() {
    let error = load_slice(&rigged(|document, buffer| {
        let times = buffer.floats(&[1., 1.], "SCALAR", 1);
        document["animations"][0]["samplers"][0]["input"] = json!(times);
    }))
    .err()
    .expect("equal keyframe times were accepted")
    .to_string();
    assert!(error.contains("animation 0"), "{error}");
    assert!(error.contains("strictly increasing"), "{error}");
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
        // A material samples it, so the load reads it.
        document["textures"] = json!([{"source": 0}]);
        document["materials"] =
            json!([{"pbrMetallicRoughness": {"baseColorTexture": {"index": 0}}}]);
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

/// Makes `accessor` sparse without a buffer view, of `count` elements, one
/// substitution for element 0 (64 zero bytes, enough for any element).
fn sparse_without_view(buffer: &mut Buffer, accessor: usize, count: usize) {
    let indices = buffer.accessor(bytemuck::cast_slice(&[0u16]), 5123, "SCALAR", 1);
    let values = buffer.accessor(&[0; 64], 5121, "SCALAR", 64);
    let indices = buffer.accessors[indices]["bufferView"].clone();
    let values = buffer.accessors[values]["bufferView"].clone();
    let accessor = buffer.accessors[accessor].as_object_mut().unwrap();
    accessor.remove("bufferView");
    accessor.insert("count".into(), json!(count));
    accessor.insert(
        "sparse".into(),
        json!({
            "count": 1,
            "indices": {"bufferView": indices, "componentType": 5123},
            "values": {"bufferView": values}
        }),
    );
}

// Plausible defects: a scene listing a root twice, or a node that has a
// parent among its roots, loaded, reading the subtree's meshes once each
// time it is listed, which glTF forbids so that a scene is a set of trees.
#[wasm_bindgen_test(unsupported = test)]
fn scene_roots_that_repeat_or_have_a_parent_are_rejected() {
    for (label, roots) in [
        ("a root listed twice", json!([0, 4, 4])),
        ("a child listed as a root", json!([0, 4, 3])),
    ] {
        let error = load_slice(&rigged(|document, _| {
            document["scenes"][0]["nodes"] = roots;
        }))
        .err()
        .unwrap_or_else(|| panic!("{label} was accepted"))
        .to_string();
        assert!(error.contains("scene 0"), "{label}: {error}");
    }
}

// Plausible defect: morph-weight keyframe values, which glTF requires a
// key and target each (three a key for a cubic spline), read without a
// bound on their count: 65536 cubic keys for 8192 targets need 1.6 billion
// values (6.4 GB), declared by a sparse accessor without a buffer view in
// a file of under 1 MB.
#[wasm_bindgen_test(unsupported = test)]
fn morph_weight_keyframes_beyond_the_load_limit_are_rejected() {
    const TARGETS: usize = 8192;
    const KEYS: usize = 65536;
    let bytes = rigged(|document, buffer| {
        let primitive = &mut document["meshes"][1]["primitives"][0];
        primitive["targets"] = json!(vec![primitive["targets"][0].clone(); TARGETS]);
        document["meshes"][1]["weights"] = json!(vec![0.5; TARGETS]);
        document["nodes"][4]["weights"] = json!(vec![0.25; TARGETS]);
        let times: Vec<f32> = (0..KEYS).map(|key| key as f32).collect();
        let times = buffer.floats(&times, "SCALAR", 1);
        let values = buffer.floats(&[0.], "SCALAR", 1);
        sparse_without_view(buffer, values, KEYS * 3 * TARGETS);
        document["animations"][0]["samplers"][1] =
            json!({"input": times, "output": values, "interpolation": "CUBICSPLINE"});
    });
    let error = load_slice(&bytes)
        .err()
        .expect("6.4 GB of keyframe values were read")
        .to_string();
    assert!(error.contains("bytes one load may"), "{error}");
}

// Plausible defect: a mesh read once for each node that uses it without a
// bound on the total: 1024 nodes using a mesh of 65535 vertices produce 67
// million vertices (6.2 GB as `Vertex`es and their indices) from a file of
// under 1 MB, though only 134 million accessor elements.
#[wasm_bindgen_test(unsupported = test)]
fn instancing_beyond_the_load_limit_is_rejected() {
    const NODES: usize = 1024;
    let mut buffer = Buffer::default();
    let positions = buffer.floats(&[1.; 3 * 65535], "VEC3", 3);
    let document = json!({
        "asset": {"version": "2.0"},
        "meshes": [{"primitives": [{"attributes": {"POSITION": positions, "NORMAL": positions}}]}],
        "nodes": vec![json!({"mesh": 0}); NODES],
        "scenes": [{"nodes": (0..NODES).collect::<Vec<_>>()}], "scene": 0
    });
    let error = load_slice(&buffer.glb(document))
        .err()
        .expect("67 million vertices were read")
        .to_string();
    assert!(error.contains("bytes one load may"), "{error}");
}

// Plausible defect: work for each morph-weight channel that walks its
// mesh's primitives, which costs channels times primitives: 30000 channels
// on nodes outside the scene, all using a mesh of 60000 primitives without
// targets before its morphed one, a file of about 4 MB. A per-channel walk
// visits 1.8 billion primitives, over a minute in a test build; counting
// each mesh's targets once reads it in well under half a second there. The
// 5 s bound sits an order of magnitude from each, so a slow, shared runner
// neither fails the fixed import nor passes the quadratic one.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn many_morph_weight_channels_on_a_mesh_of_many_primitives_import_without_quadratic_time() {
    const PRIMITIVES: usize = 60_000;
    const CHANNELS: usize = 30_000;
    let mut buffer = Buffer::default();
    let positions = buffer.floats(&[0., 0., 0., 1., 0., 0., 0., 1., 0.], "VEC3", 3);
    let normals = buffer.floats(&[0., 0., 1., 0., 0., 1., 0., 0., 1.], "VEC3", 3);
    let times = buffer.floats(&[0.], "SCALAR", 1);
    let weights = buffer.floats(&[1.], "SCALAR", 1);
    let mut primitives = vec![json!({"attributes": {"POSITION": positions}}); PRIMITIVES];
    primitives.push(
        json!({"attributes": {"POSITION": positions, "NORMAL": normals},
        "targets": [{"POSITION": normals}]}),
    );
    let mut nodes = vec![json!({"mesh": 0})];
    nodes.extend(vec![json!({"mesh": 1}); CHANNELS]);
    let channels: Vec<Value> = (1..=CHANNELS)
        .map(|node| json!({"sampler": 0, "target": {"node": node, "path": "weights"}}))
        .collect();
    let document = json!({
        "asset": {"version": "2.0"},
        "meshes": [
            {"primitives": [{"attributes": {"POSITION": positions, "NORMAL": normals}}]},
            {"primitives": primitives}
        ],
        "nodes": nodes,
        "animations": [{"samplers": [{"input": times, "output": weights}], "channels": channels}],
        "scenes": [{"nodes": [0]}], "scene": 0
    });
    let bytes = buffer.glb(document);
    let start = std::time::Instant::now();
    let asset = load_slice(&bytes).unwrap();
    let elapsed = start.elapsed();
    assert_eq!(asset.rig.clips[0].channels.len(), CHANNELS);
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "a {} byte file took {elapsed:?} to import",
        bytes.len()
    );
}

// gltf-json leaves these document references/source variants unchecked; the
// public loader must return an error rather than panic on malformed content.
#[wasm_bindgen_test(unsupported = test)]
fn malformed_primitive_accessor_references_are_rejected() {
    for morph in [false, true] {
        let bytes = rigged(|document, buffer| {
            let missing = json!(buffer.accessors.len());
            if morph {
                document["meshes"][1]["primitives"][0]["targets"][0]["POSITION"] = missing;
            } else {
                document["meshes"][0]["primitives"][0]["attributes"]["POSITION"] = missing;
            }
        });
        let error = load_slice(&bytes)
            .err()
            .expect("missing accessor loaded")
            .to_string();
        assert!(
            error.contains("mesh") && error.contains("accessor"),
            "{error}"
        );
    }
}

#[wasm_bindgen_test(unsupported = test)]
fn malformed_animation_target_nodes_are_rejected() {
    let bytes = rigged(|document, _| {
        document["animations"][0]["channels"][0]["target"]["node"] = json!(9);
    });
    let error = load_slice(&bytes)
        .err()
        .expect("missing target node loaded")
        .to_string();
    assert!(
        error.contains("animation 0 channel 0") && error.contains("node"),
        "{error}"
    );
}

#[wasm_bindgen_test(unsupported = test)]
fn malformed_image_sources_are_rejected() {
    for image in [
        json!({"bufferView": 0}),
        json!({}),
        json!({"bufferView": 0, "mimeType": "image/png", "uri": "image.png"}),
    ] {
        let bytes = rigged(|document, _| {
            document["images"] = json!([image]);
            document["textures"] = json!([{"source": 0}]);
            document["materials"] =
                json!([{"pbrMetallicRoughness": {"baseColorTexture": {"index": 0}}}]);
        });
        let error = load_slice(&bytes)
            .err()
            .expect("invalid image source loaded")
            .to_string();
        assert!(error.contains("image 0"), "{error}");
    }
}

#[wasm_bindgen_test(unsupported = test)]
fn malformed_animation_target_paths_are_rejected() {
    let bytes = rigged(|document, _| {
        document["animations"][0]["channels"][0]["target"]["path"] = json!("pointer");
    });
    let error = load_slice(&bytes)
        .err()
        .expect("unknown target path loaded")
        .to_string();
    assert!(
        error.contains("animation 0 channel 0") && error.contains("path"),
        "{error}"
    );
}
