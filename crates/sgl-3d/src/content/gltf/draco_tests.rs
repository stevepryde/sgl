//! Draco-compressed primitives through the public loader: files compressed
//! by Google's encoder through Blender 5.2.1's exporter, gltf-pipeline 4.3.1
//! (draco3d 1.5.9) and Khronos' sample assets, against their uncompressed
//! sources, and documents and streams broken here. The fixtures' provenance
//! and commands are in their READMEs.
use super::attributes_tests::{angle, embedded};
use super::*;
use crate::asset::CpuMesh;
use glam::{Vec2, Vec3, Vec4};
use serde_json::Value;
use wasm_bindgen_test::wasm_bindgen_test;

macro_rules! fixture {
    ($path:literal) => {
        include_bytes!(concat!("../../../tests/fixtures/gltf/", $path))
    };
}

const SPHERE: (&[u8], &[u8]) = (
    fixture!("DracoSphere/glTF/DracoSphere.gltf"),
    fixture!("DracoSphere/glTF/DracoSphere.bin"),
);
const SPHERE_DRACO: (&[u8], &[u8]) = (
    fixture!("DracoSphere/glTF-Draco/DracoSphere.gltf"),
    fixture!("DracoSphere/glTF-Draco/DracoSphere.bin"),
);
const SPHERE_FALLBACK: (&[u8], &[u8]) = (
    fixture!("DracoSphere/glTF-Draco-Fallback/DracoSphere.gltf"),
    fixture!("DracoSphere/glTF-Draco-Fallback/DracoSphere-draco.bin"),
);
const RIG: (&[u8], &[u8]) = (
    fixture!("RiggedSimple/glTF/RiggedSimple.gltf"),
    fixture!("RiggedSimple/glTF/RiggedSimple0.bin"),
);
const RIG_DRACO: (&[u8], &[u8]) = (
    fixture!("RiggedSimple/glTF-Draco/RiggedSimple.gltf"),
    fixture!("RiggedSimple/glTF-Draco/RiggedSimple0.bin"),
);

/// One corner of a drawn triangle: what a vertex carries that Draco
/// encodes, the first morph target's displacement, and its skin weights
/// summed per joint (the fixtures' skins have at most four joints).
#[derive(Clone, Copy, Debug)]
struct Corner {
    position: Vec3,
    normal: Vec3,
    uv: Vec2,
    delta: Vec3,
    joints: [f32; 4],
}

/// How far a decoded corner may lie from its reference: a distance, an
/// angle in radians, a per-component UV error, a displacement distance and
/// a per-joint weight error.
struct Bounds {
    position: f32,
    normal: f32,
    uv: f32,
    delta: f32,
    weight: f32,
}

fn triangles(mesh: &CpuMesh) -> Vec<[Corner; 3]> {
    let corner = |index: u32| {
        let i = index as usize;
        let vertex = &mesh.vertices[i];
        Corner {
            position: vertex.position.into(),
            normal: vertex.normal.into(),
            uv: vertex.uv.into(),
            delta: mesh
                .deformation
                .morph_targets
                .first()
                .map_or(Vec3::ZERO, |target| target.deltas[i].position.into()),
            joints: mesh
                .deformation
                .influences
                .get(i)
                .map_or([0.; 4], |influence| {
                    let mut joints = [0.; 4];
                    for (joint, weight) in influence.joints.iter().zip(influence.weights) {
                        joints[*joint as usize] += weight;
                    }
                    joints
                }),
        }
    };
    mesh.indices
        .as_chunks::<3>()
        .0
        .iter()
        .map(|triangle| triangle.map(corner))
        .collect()
}

fn corner_within(a: &Corner, b: &Corner, bounds: &Bounds) -> bool {
    a.position.distance(b.position) <= bounds.position
        && angle(a.normal, b.normal) <= bounds.normal
        && (a.uv - b.uv).abs().max_element() <= bounds.uv
        && a.delta.distance(b.delta) <= bounds.delta
        && (Vec4::from(a.joints) - Vec4::from(b.joints))
            .abs()
            .max_element()
            <= bounds.weight
}

/// Asserts that `decoded` draws `reference`'s triangles, each once, with
/// the same winding, whatever order the decoder put its vertices and
/// triangles in, every corner within `bounds`: the triangle surface and,
/// across UV seams, each triangle's own UVs.
fn assert_surface_matches(decoded: &CpuMesh, reference: &CpuMesh, bounds: &Bounds) {
    let (ours, theirs) = (triangles(decoded), triangles(reference));
    assert_eq!(ours.len(), theirs.len(), "triangle count");
    let mut used = vec![false; ours.len()];
    for (r, expected) in theirs.iter().enumerate() {
        let found = (0..ours.len()).find(|&t| {
            !used[t]
                && (0..3).any(|rotation| {
                    (0..3)
                        .all(|k| corner_within(&ours[t][(k + rotation) % 3], &expected[k], bounds))
                })
        });
        let Some(found) = found else {
            panic!("reference triangle {r} {expected:?} matched no decoded triangle");
        };
        used[found] = true;
    }
}

/// The largest extent along any axis of `values`: the range Draco
/// quantizes an attribute over.
fn range<const N: usize>(values: impl Iterator<Item = [f32; N]>) -> f32 {
    let mut low = [f32::MAX; N];
    let mut high = [f32::MIN; N];
    for value in values {
        for k in 0..N {
            low[k] = low[k].min(value[k]);
            high[k] = high[k].max(value[k]);
        }
    }
    (0..N).map(|k| high[k] - low[k]).fold(0., f32::max)
}

/// The bounds for `reference` encoded at Blender's default quantization:
/// positions at 14 bits and UVs at 12 over each attribute's range, so each
/// component rounds by half of range / (2^bits - 1); normals at 10-bit
/// octahedral coordinates, which Draco spans over 2^10 - 2 steps on
/// [-1, 1] (`OctahedronToolBox`: center (2^10 - 2) / 2), so each of the two
/// coordinates rounds by at most 1 / 1022, moving the unnormalized vector
/// by at most sqrt(6) / 1022, at least 1 / sqrt(3) long. Morph targets are
/// not compressed: the same displacement, for the same vertex.
fn blender_bounds(reference: &CpuMesh) -> Bounds {
    let positions = range(reference.vertices.iter().map(|v| v.position));
    let uvs = range(reference.vertices.iter().map(|v| v.uv));
    Bounds {
        position: 3f32.sqrt() * positions / 16383. / 2. + 1e-6,
        normal: 6f32.sqrt() / 1022. * 3f32.sqrt() + 1e-5,
        uv: uvs / 4095. / 2. + 1e-6,
        delta: 1e-6,
        weight: 0.,
    }
}

// Plausible defects: Edgebreaker (valence) or sequential connectivity
// misdecoded, a seam's split corners given one UV, quantized or
// octahedral attributes left undequantized, the indices accessor filled
// in the wrong component type, a sequential stream's vertex order changed
// so the uncompressed morph targets land on other vertices, or the file
// refused for requiring the extension. Oracle: the same Blender scene
// exported uncompressed, within the quantization the exporter declares.
#[wasm_bindgen_test(unsupported = test)]
fn blender_draco_export_draws_its_uncompressed_export() {
    let decoded = load_slice(&embedded(SPHERE_DRACO)).unwrap();
    let reference = load_slice(&embedded(SPHERE)).unwrap();
    assert!(decoded.ignored.is_empty(), "{:?}", decoded.ignored);
    assert_eq!(decoded.meshes.len(), 2, "the sphere, then the morphed cube");
    assert_eq!(reference.meshes.len(), 2);
    for (ours, theirs) in decoded.meshes.iter().zip(&reference.meshes) {
        assert_surface_matches(ours, theirs, &blender_bounds(theirs));
    }
}

// Defects: an optional extension's fallback buffer read (its file is
// absent, and bytes cannot resolve it), the extension ignored because it
// is not required, or Edgebreaker's standard traversal misdecoded.
// Oracle: Blender's uncompressed sphere, within gltf-pipeline's default
// quantization (positions 11 bits, normals 8, UVs 10), which it applies
// over the same ranges: the sphere's own.
#[wasm_bindgen_test(unsupported = test)]
fn optional_draco_loads_without_its_fallback_buffer() {
    let document: Value = serde_json::from_slice(SPHERE_FALLBACK.0).unwrap();
    assert!(document.get("extensionsRequired").is_none());
    assert!(document["buffers"][1]["uri"].is_string());
    let decoded = load_slice(&embedded(SPHERE_FALLBACK)).unwrap();
    let reference = load_slice(&embedded(SPHERE)).unwrap();
    assert!(decoded.ignored.is_empty(), "{:?}", decoded.ignored);
    let sphere = &reference.meshes[0];
    let positions = range(sphere.vertices.iter().map(|v| v.position));
    let uvs = range(sphere.vertices.iter().map(|v| v.uv));
    let bounds = Bounds {
        position: 3f32.sqrt() * positions / 2047. / 2. + 1e-6,
        normal: 6f32.sqrt() / 254. * 3f32.sqrt() + 1e-5,
        uv: uvs / 1023. / 2. + 1e-6,
        delta: 0.,
        weight: 0.,
    };
    let [mesh] = &decoded.meshes[..] else {
        panic!("one mesh");
    };
    assert_surface_matches(mesh, sphere, &bounds);
}

// Defects: attributes taken by their position in the stream rather than
// by unique id (this stream holds ids 0, 2, 3, 1 in that order), JOINTS_0
// (UNSIGNED_SHORT) or WEIGHTS_0 taken from the wrong Draco attribute or
// point, or a stream whose points share values through
// its point-to-value map (96 positions for 160 points here) read as if
// they did not, so the skin binds other joints or the surface tears.
// Oracle: Khronos' uncompressed RiggedSimple, in bind space; its
// glTF-Draco variant's positions lie on an 11-bit grid over their 9.15 m
// range and its normals are 8-bit octahedral, gltf-pipeline's defaults;
// JOINTS_0 and WEIGHTS_0 are stored unquantized.
#[wasm_bindgen_test(unsupported = test)]
fn khronos_draco_rig_binds_as_the_reference() {
    let decoded = load_slice(&embedded(RIG_DRACO)).unwrap();
    let reference = load_slice(&embedded(RIG)).unwrap();
    assert!(decoded.ignored.is_empty(), "{:?}", decoded.ignored);
    let ([ours], [theirs]) = (&decoded.meshes[..], &reference.meshes[..]) else {
        panic!("one mesh each");
    };
    let positions = range(theirs.vertices.iter().map(|v| v.position));
    let bounds = Bounds {
        position: 3f32.sqrt() * positions / 2047. / 2. + 1e-5,
        normal: 6f32.sqrt() / 254. * 3f32.sqrt() + 1e-5,
        uv: 0.,
        delta: 0.,
        weight: 1e-6,
    };
    assert_surface_matches(ours, theirs, &bounds);
}

/// Loads `parts` after `change` edits its JSON and BIN, returning the
/// error; a load that succeeds fails the test.
fn error_of(
    parts: (&[u8], &[u8]),
    change: impl FnOnce(&mut Value, &mut Vec<u8>),
    options: LoadOptions<'_>,
) -> String {
    let mut json: Value = serde_json::from_slice(parts.0).unwrap();
    let mut bin = parts.1.to_vec();
    change(&mut json, &mut bin);
    let bytes = embedded((&serde_json::to_vec(&json).unwrap(), &bin));
    match load_slice_with_options(&bytes, options) {
        Ok(_) => panic!("the changed file loaded"),
        Err(error) => error.to_string(),
    }
}

/// An edit to a fixture's JSON and BIN.
type Change = Box<dyn FnOnce(&mut Value, &mut Vec<u8>)>;

/// The Blender sphere primitive's extension object.
fn sphere_extension(json: &mut Value) -> &mut Value {
    &mut json["meshes"][1]["primitives"][0]["extensions"]["KHR_draco_mesh_compression"]
}

// Defects: required data that is absent or does not match the stream
// loaded as zeros, as another attribute or as a partial mesh, or a
// panic indexing what is not there. Oracle: the extension's requirements:
// an attribute is found by its unique id in the stream, the extension's
// attributes are a subset of the primitive's, the accessors "must match
// the decompressed data", the mode is TRIANGLES or TRIANGLE_STRIP, and
// the schema requires `bufferView` and `attributes`; the decoded
// triangles need an indices accessor to fill.
#[wasm_bindgen_test(unsupported = test)]
fn missing_or_mismatched_required_data_fails_the_load() {
    let options = LoadOptions::default();
    let cases: [(&str, Change); 9] = [
        (
            "no attribute with id 9",
            Box::new(|json, _| sphere_extension(json)["attributes"]["NORMAL"] = 9.into()),
        ),
        (
            "which its primitive does not have",
            Box::new(|json, _| sphere_extension(json)["attributes"]["COLOR_0"] = 1.into()),
        ),
        (
            "bufferView is missing",
            Box::new(|json, _| {
                sphere_extension(json)
                    .as_object_mut()
                    .unwrap()
                    .remove("bufferView");
            }),
        ),
        (
            "bufferView 40 is out of range",
            Box::new(|json, _| sphere_extension(json)["bufferView"] = 40.into()),
        ),
        (
            "no indices accessor",
            Box::new(|json, _| {
                json["meshes"][1]["primitives"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("indices");
            }),
        ),
        (
            "neither TRIANGLES nor TRIANGLE_STRIP",
            Box::new(|json, _| json["meshes"][1]["primitives"][0]["mode"] = 6.into()),
        ),
        (
            "holds 558 elements where the stream decodes 559 points",
            Box::new(|json, _| json["accessors"][7]["count"] = 558.into()),
        ),
        (
            "holds 2883 indices where the stream decodes 960 triangles",
            Box::new(|json, _| json["accessors"][9]["count"] = 2883.into()),
        ),
        (
            "decodes 3 Float32 components",
            Box::new(|json, _| json["accessors"][7]["componentType"] = 5122.into()),
        ),
    ];
    for (expected, change) in cases {
        let error = error_of(SPHERE_DRACO, change, options);
        assert!(error.contains(expected), "{expected}: {error}");
    }
}

// Defect: an accessor two Draco streams name filled by whichever decodes
// last. Oracle: an accessor holds one set of data; the sphere's NORMAL
// pointed at the cube's NORMAL accessor, which the cube's stream fills.
#[wasm_bindgen_test(unsupported = test)]
fn an_accessor_two_streams_fill_fails_the_load() {
    let error = error_of(
        SPHERE_DRACO,
        |json, _| json["meshes"][1]["primitives"][0]["attributes"]["NORMAL"] = 1.into(),
        LoadOptions::default(),
    );
    assert!(
        error.contains("also filled from another Draco stream"),
        "{error}"
    );
}

// Defects: the decoded accessors allocated at their declared size before
// the decoded budget is charged, so a file declaring a huge count
// allocates it; the decoder's connectivity, which grows with the
// triangles an indices accessor declares (the stream's own count could
// claim up to eight a byte), left outside the budget; or the decoder left
// free to produce attribute values beyond what remains of it. Oracle: a
// POSITION count far beyond the 1 GiB decoded budget is refused naming the
// budget (natively 2^42 points, 48 TiB, an allocation that aborts if made
// first; on wasm32 3 GiB of them); so are 3 x 10^8 u16 indices, whose
// 0.6 GB fit the budget but whose 6.4 GB of connectivity does not; and of
// budgets that hold the accessors exactly, one a byte short of the
// largest stream's connectivity (the sphere's 960 triangles at 64 bytes)
// is refused by the budget, and one 1000 bytes over it by the decoder's
// ceiling on the sphere's 17,888 bytes of attribute values.
#[wasm_bindgen_test(unsupported = test)]
fn decoded_allocations_beyond_the_budget_are_refused() {
    let huge: u64 = if cfg!(target_pointer_width = "64") {
        1 << 42
    } else {
        (3 << 30) / 12
    };
    let budget = |decoded| LoadOptions {
        budget: LoadBudget::new(1 << 30, decoded).unwrap(),
        ..LoadOptions::default()
    };
    let error = error_of(
        SPHERE_DRACO,
        |json, _| json["accessors"][6]["count"] = huge.into(),
        budget(1 << 30),
    );
    assert!(error.contains("decoded load budget exceeded"), "{error}");
    let error = error_of(
        SPHERE_DRACO,
        |json, _| json["accessors"][9]["count"] = 300_000_000.into(),
        budget(1 << 30),
    );
    assert!(
        error.contains("the decoder's connectivity: decoded load budget exceeded"),
        "{error}"
    );
    // The two primitives' accessors, each 4-byte aligned: the cube's 24
    // vertices (two VEC3 and a VEC2 of floats) and 36 u16 indices, and the
    // sphere's 559 vertices and 2880 indices.
    let accessors = 24 * 32 + 72 + 559 * 32 + 2880 * 2;
    let connectivity = 960 * 64;
    let error = error_of(
        SPHERE_DRACO,
        |_, _| {},
        budget(accessors + connectivity - 1),
    );
    assert!(error.contains("decoded load budget exceeded"), "{error}");
    let error = error_of(
        SPHERE_DRACO,
        |_, _| {},
        budget(accessors + connectivity + 1000),
    );
    assert!(error.contains("ceiling"), "{error}");
}

// Defect: an accessor a Draco primitive names and another primitive also
// reads repointed for both, so the uncompressed primitive draws the
// decoded, reordered and quantized data, or its view's buffer is left
// unread. Oracle: the extension ignores the view "when loading each
// accessor" of the Draco primitive only. Blender's uncompressed sphere,
// whose accessors match the Draco sphere's counts, with the Draco sphere's
// stream appended and the extension added to its primitive, and a second
// mesh drawing the same accessors uncompressed: that mesh is the
// reference exactly, and the Draco one within Blender's quantization.
#[wasm_bindgen_test(unsupported = test)]
fn an_accessor_another_primitive_reads_keeps_its_data_for_it() {
    let (view, stream) = sphere_stream();
    let draco: Value = serde_json::from_slice(SPHERE_DRACO.0).unwrap();
    let mut json: Value = serde_json::from_slice(SPHERE.0).unwrap();
    let mut bin = SPHERE.1.to_vec();
    bin.resize(bin.len().next_multiple_of(4), 0);
    let offset = bin.len();
    bin.extend_from_slice(&SPHERE_DRACO.1[stream.clone()]);
    json["buffers"][0]["byteLength"] = bin.len().into();
    let views = json["bufferViews"].as_array_mut().unwrap();
    views.push(serde_json::json!({"buffer": 0, "byteOffset": offset, "byteLength": stream.len()}));
    let added = views.len() - 1;
    let mut extension = draco["meshes"][1]["primitives"][0]["extensions"].clone();
    extension["KHR_draco_mesh_compression"]["bufferView"] = added.into();
    assert_eq!(draco["bufferViews"][view]["byteLength"], stream.len());
    let sphere = json["meshes"][1]["primitives"][0].clone();
    json["meshes"][1]["primitives"][0]["extensions"] = extension;
    json["meshes"][1]["primitives"][0]["material"] = 0.into();
    let mut plain = sphere;
    plain["material"] = 1.into();
    json["meshes"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({"primitives": [plain]}));
    json["nodes"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({"mesh": 2}));
    let node = json["nodes"].as_array().unwrap().len() - 1;
    json["scenes"][0]["nodes"]
        .as_array_mut()
        .unwrap()
        .push(node.into());
    json["materials"] = serde_json::json!([{}, {}]);
    json["extensionsUsed"] = serde_json::json!(["KHR_draco_mesh_compression"]);
    let asset = load_slice(&embedded((&serde_json::to_vec(&json).unwrap(), &bin))).unwrap();
    let reference = load_slice(&embedded(SPHERE)).unwrap();
    let sphere = &reference.meshes[0];
    assert_eq!(
        asset.meshes.len(),
        3,
        "the Draco sphere, the plain one, the cube"
    );
    assert_surface_matches(&asset.meshes[0], sphere, &blender_bounds(sphere));
    let exact = Bounds {
        position: 0.,
        normal: 1e-6,
        uv: 0.,
        delta: 0.,
        weight: 0.,
    };
    assert_surface_matches(&asset.meshes[1], sphere, &exact);
}

/// The Blender sphere's stream: its buffer view, and that view's byte
/// range in the export's BIN.
fn sphere_stream() -> (usize, std::ops::Range<usize>) {
    let mut json: Value = serde_json::from_slice(SPHERE_DRACO.0).unwrap();
    let index = sphere_extension(&mut json)["bufferView"].as_u64().unwrap() as usize;
    let view = &json["bufferViews"][index];
    let start = view["byteOffset"].as_u64().unwrap() as usize;
    (
        index,
        start..start + view["byteLength"].as_u64().unwrap() as usize,
    )
}

// Defects: a truncated or corrupted stream panics, hangs, reads out of
// bounds or allocates without bound in the decoder or in the step that
// copies its attributes into the accessors. Oracle: every truncation of
// the sphere's Edgebreaker stream fails the load, and every byte of it
// inverted in turn returns, loaded or refused, without panicking.
#[wasm_bindgen_test(unsupported = test)]
fn truncated_and_corrupted_streams_return_without_panicking() {
    let (view, stream) = sphere_stream();
    for length in (0..stream.len()).step_by(7) {
        let error = error_of(
            SPHERE_DRACO,
            |json, _| json["bufferViews"][view]["byteLength"] = length.into(),
            LoadOptions::default(),
        );
        assert!(
            error.contains("KHR_draco_mesh_compression stream"),
            "{length}: {error}"
        );
    }
    for at in stream.step_by(3) {
        let mut bin = SPHERE_DRACO.1.to_vec();
        bin[at] ^= 0xff;
        let _ = load_slice(&embedded((SPHERE_DRACO.0, &bin)));
    }
}

// The file entry point resolves each buffer beside the glTF, and must not
// open the absent fallback file.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn file_entry_point_decodes_draco_without_reading_the_fallback() {
    let fixtures =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/gltf/DracoSphere");
    let fallback = fixtures.join("glTF-Draco-Fallback");
    assert!(!fallback.join("DracoSphere-uncompressed.bin").exists());
    let reference = load(&fixtures.join("glTF/DracoSphere.gltf")).unwrap();
    let decoded = load(&fixtures.join("glTF-Draco/DracoSphere.gltf")).unwrap();
    for (ours, theirs) in decoded.meshes.iter().zip(&reference.meshes) {
        assert_surface_matches(ours, theirs, &blender_bounds(theirs));
    }
    let optional = load(&fallback.join("DracoSphere.gltf")).unwrap();
    assert_eq!(optional.meshes.len(), 1);
    assert_eq!(
        optional.meshes[0].indices,
        load_slice(&embedded(SPHERE_FALLBACK)).unwrap().meshes[0].indices
    );
}
