//! Meshopt-compressed buffer views through the public loader: files
//! compressed by meshoptimizer's own tools (gltfpack 1.3 and its npm
//! encoder) against their uncompressed sources, and documents broken here.
//! The fixtures' provenance and commands are in their READMEs.
use super::attributes_tests::{
    CubeError, angle, assert_cube_matches, assert_cube_within, embedded,
};
use super::*;
use crate::animation::{Pose, Wrap};
use crate::asset::LoadBudget;
use glam::{Mat3, Mat4, Vec3};
use serde_json::Value;
use wasm_bindgen_test::wasm_bindgen_test;

macro_rules! fixture {
    ($path:literal) => {
        include_bytes!(concat!("../../../tests/fixtures/gltf/", $path))
    };
}

const CUBE_FLOAT: (&[u8], &[u8]) = (
    fixture!("AnimatedMorphCube/glTF/AnimatedMorphCube.gltf"),
    fixture!("AnimatedMorphCube/glTF/AnimatedMorphCube.bin"),
);
const CUBE_EXT: (&[u8], &[u8]) = (
    fixture!("AnimatedMorphCube/glTF-Meshopt/AnimatedMorphCube.gltf"),
    fixture!("AnimatedMorphCube/glTF-Meshopt/AnimatedMorphCube.bin"),
);
const CUBE_KHR: (&[u8], &[u8]) = (
    fixture!("AnimatedMorphCube/glTF-Meshopt-KHR/AnimatedMorphCube.gltf"),
    fixture!("AnimatedMorphCube/glTF-Meshopt-KHR/AnimatedMorphCube.bin"),
);
const RIG: (&[u8], &[u8]) = (
    fixture!("RiggedSimple/glTF/RiggedSimple.gltf"),
    fixture!("RiggedSimple/glTF/RiggedSimple0.bin"),
);
const RIG_KHR: (&[u8], &[u8]) = (
    fixture!("RiggedSimple/glTF-Meshopt-KHR/RiggedSimple.gltf"),
    fixture!("RiggedSimple/glTF-Meshopt-KHR/RiggedSimple.bin"),
);
const RIG_FALLBACK: (&[u8], &[u8]) = (
    fixture!("RiggedSimple/glTF-Meshopt-Fallback/RiggedSimple.gltf"),
    fixture!("RiggedSimple/glTF-Meshopt-Fallback/RiggedSimple.bin"),
);
const GRID: &[u8] = fixture!("MeshoptColorGrid/MeshoptColorGrid.glb");

/// Each of `asset`'s clips sampled at `times`: the pose's morph weights.
fn weights(asset: &Asset, times: &[f32]) -> Vec<Vec<f32>> {
    let rig = &asset.rig;
    let mut pose = Pose::rest(rig);
    times
        .iter()
        .map(|&time| {
            pose.sample(rig, &rig.clips[0], time, Wrap::Clamp);
            pose.morph_weights.clone()
        })
        .collect()
}

/// Times across `asset`'s first clip, keyframes and between them.
fn times(asset: &Asset) -> Vec<f32> {
    let duration = asset.rig.clips[0].duration();
    (0..=40).map(|step| duration * step as f32 / 40.).collect()
}

// Plausible defects: the file refused for requiring EXT_meshopt_compression;
// its fallback buffer read (it has no data) or left unfilled; a mode
// decoded as another; the decoded bytes landing outside their view, so
// quantized attributes, indices, morph targets or animation keys read
// wrong. Oracle: Khronos' float cube, within the quantization gltfpack
// declares (`-c` is the same quantization as the glTF-Quantized fixture,
// and morph weights are 8-bit unorm keys), the combined
// KHR_mesh_quantization and EXT_meshopt_compression file #855 asked for.
#[wasm_bindgen_test(unsupported = test)]
fn quantized_ext_meshopt_cube_loads_as_the_float_cube() {
    let compressed = load_slice(&embedded(CUBE_EXT)).unwrap();
    let float = load_slice(&embedded(CUBE_FLOAT)).unwrap();
    assert_cube_matches(&compressed, &float);
    let at = times(&float);
    for (sampled, expected) in weights(&compressed, &at).iter().zip(weights(&float, &at)) {
        for (weight, expected) in sampled.iter().zip(&expected) {
            assert!(
                (weight - expected).abs() <= 0.5 / 255. + 1e-6,
                "{sampled:?} {expected:?}"
            );
        }
    }
}

// Defects: a version 1 attribute stream misdecoded (control or channel
// modes), the OCTAHEDRAL or EXPONENTIAL filter skipped or misapplied.
// Oracle: the float cube within the precision each filter declares for
// gltfpack's `-cc -vpf`: positions and their morph displacements are
// exponential with 15-bit mantissas sharing an exponent per component
// (`-vp 14` plus one), so each component errs by at most the stream's
// largest magnitude of that component over 2^14 - 1; normals are 8-bit
// octahedral: a half step of 1/127 in each coordinate moves the
// unnormalized vector by at most sqrt(6)/254, at least 1/sqrt(3) long,
// then the 8-bit output rounds each component by half a step.
#[wasm_bindgen_test(unsupported = test)]
fn khr_meshopt_cube_with_filters_loads_as_the_float_cube() {
    let compressed = load_slice(&embedded(CUBE_KHR)).unwrap();
    let float = load_slice(&embedded(CUBE_FLOAT)).unwrap();
    let [mesh] = &float.meshes[..] else {
        panic!("one mesh");
    };
    // In the world, under the node's scale and a rotation that mixes axes:
    // a component's largest magnitude is at most the largest length, 1 for
    // the cube's corners' components and that of the longest displacement.
    let exponential = |largest: f32| 3f32.sqrt() * largest / 16383. + 1e-6;
    let largest_delta = mesh
        .deformation
        .morph_targets
        .iter()
        .flat_map(|target| &target.deltas)
        .map(|delta| Vec3::from(delta.position).length())
        .fold(0f32, f32::max);
    let normal = 6f32.sqrt() / 254. * 3f32.sqrt() + 0.5 / 127. * 3f32.sqrt();
    assert_cube_within(
        &compressed,
        &float,
        &CubeError {
            position: exponential(1.),
            // Per axis: a component's error, the cube's corners being 1.
            bounds: 1. / 16383. + 1e-6,
            normal,
            delta_position: exponential(largest_delta),
            // Morph normal displacements are 8-bit snorm, unfiltered.
            delta_normal: 0.5 / 127. * 3f32.sqrt(),
        },
    );
}

/// Each vertex's position and normal posed by `pose` through its skin.
fn skinned(asset: &Asset, pose: &Pose) -> Vec<(Vec3, Vec3)> {
    let mut locals = Vec::new();
    pose.local_matrices(&mut locals);
    let matrices = asset.rig.joint_matrices(&locals);
    let [mesh] = &asset.meshes[..] else {
        panic!("one mesh");
    };
    mesh.vertices
        .iter()
        .zip(&mesh.deformation.influences)
        .map(|(vertex, influence)| {
            let skin = (0..4).fold(Mat4::ZERO, |sum, k| {
                sum + matrices[influence.joints[k] as usize] * influence.weights[k]
            });
            let normal = Mat3::from_mat4(skin).inverse().transpose() * Vec3::from(vertex.normal);
            (
                skin.transform_point3(vertex.position.into()),
                normal.normalize(),
            )
        })
        .collect()
}

/// `rig`'s node named `name`.
fn named(asset: &Asset, name: &str) -> usize {
    asset
        .rig
        .nodes
        .iter()
        .position(|node| node.name.as_deref() == Some(name))
        .unwrap_or_else(|| panic!("no node {name}"))
}

/// Asserts that `compressed` poses as RiggedSimple's reference: its
/// skinned vertices at rest, their influences, and both joints' sampled
/// transforms at every time, within `rotation` radians for a sampled
/// rotation and `normal` for a normal.
fn assert_rig_matches(compressed: &Asset, reference: &Asset, rotation: f32, normal: f32) {
    assert!(compressed.ignored.is_empty(), "{:?}", compressed.ignored);
    // gltfpack's 14-bit positions: the step its mesh node's scale holds,
    // and up to two and a half 8-bit weight steps (four weights each
    // rounded by half a step, the sum's error moved into one of them, and
    // the loader's renormalization) times the joints' spread at rest.
    const STEP: f32 = 0.000_558_515_2;
    let weight = (4. * 0.5 + 0.5) / 255.;
    let rest = |asset: &Asset| skinned(asset, &Pose::rest(&asset.rig));
    let (vertices, expected) = (rest(compressed), rest(reference));
    let reference_mesh = &reference.meshes[0];
    let mut locals = Vec::new();
    Pose::rest(&reference.rig).local_matrices(&mut locals);
    let matrices = reference.rig.joint_matrices(&locals);
    let spread: Vec<f32> = reference_mesh
        .vertices
        .iter()
        .zip(&reference_mesh.deformation.influences)
        .map(|(vertex, influence)| {
            let posed = influence
                .joints
                .map(|j| matrices[j as usize].transform_point3(vertex.position.into()));
            posed
                .iter()
                .flat_map(|a| posed.iter().map(move |b| a.distance(*b)))
                .fold(0., f32::max)
        })
        .collect();
    let matched: Vec<usize> = vertices
        .iter()
        .map(|(position, direction)| {
            let found: Vec<usize> = (0..expected.len())
                .filter(|&r| {
                    let bound = STEP / 2. * 3f32.sqrt() + spread[r] * weight + 1e-5;
                    position.distance(expected[r].0) <= bound
                        && angle(*direction, expected[r].1) <= normal
                })
                .collect();
            assert!(!found.is_empty(), "{position} matched nothing");
            found[0]
        })
        .collect();
    assert_eq!(vertices.len(), expected.len());
    let mut seen = matched.clone();
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(
        seen.len(),
        expected.len(),
        "every reference vertex matched once"
    );
    // Influences: the same joints, each weight within its 8-bit rounding.
    let influences = &compressed.meshes[0].deformation.influences;
    for (vertex, &r) in matched.iter().enumerate() {
        let mut ours = [0f32; 2];
        let mut theirs = [0f32; 2];
        for k in 0..4 {
            ours[influences[vertex].joints[k] as usize] += influences[vertex].weights[k];
            let expected = &reference_mesh.deformation.influences[r];
            theirs[expected.joints[k] as usize] += expected.weights[k];
        }
        for (a, b) in ours.iter().zip(theirs) {
            assert!(
                (a - b).abs() <= weight + 1e-6,
                "vertex {vertex}: {ours:?} {theirs:?}"
            );
        }
    }
    // The animated joints, sampled at every time.
    let (mut ours, mut theirs) = (Pose::rest(&compressed.rig), Pose::rest(&reference.rig));
    for time in times(reference) {
        ours.sample(&compressed.rig, &compressed.rig.clips[0], time, Wrap::Clamp);
        theirs.sample(&reference.rig, &reference.rig.clips[0], time, Wrap::Clamp);
        for name in ["Bone", "Bone.001"] {
            let (a, b) = (
                ours.nodes[named(compressed, name)],
                theirs.nodes[named(reference, name)],
            );
            assert!(
                a.rotation.angle_between(b.rotation) <= rotation,
                "{name} at {time}: {a:?} {b:?}"
            );
            assert!(
                a.translation.abs_diff_eq(b.translation, 1e-5),
                "{name} at {time}"
            );
            assert!(a.scale.abs_diff_eq(b.scale, 1e-5), "{name} at {time}");
        }
    }
}

/// The angle a unit quaternion can turn by when its four components each
/// err by at most `e`: twice the error's length.
fn quaternion_error(e: [f32; 4]) -> f32 {
    2. * e.iter().map(|e| e * e).sum::<f32>().sqrt() + 1e-6
}

/// The octahedral normal bound (see the cube test): 8-bit coordinates and
/// output.
fn octahedral_normal() -> f32 {
    6f32.sqrt() / 254. * 3f32.sqrt() + 0.5 / 127. * 3f32.sqrt()
}

// Defects: the QUATERNION filter's largest-component slot, scale or w
// wrong; a skin's joints, weights or inverse binds read from the wrong
// bytes; keyframe times misdecoded. Oracle: Khronos' RiggedSimple, within
// gltfpack `-cc`'s declared precision: 12-bit quaternion components stored
// times sqrt(2), so each stored one errs by half a 12-bit step over
// sqrt(2), and w, at least a half, by at most sqrt(6) times that, plus
// the 16-bit output's half step on each; 8-bit octahedral normals.
#[wasm_bindgen_test(unsupported = test)]
fn khr_meshopt_rig_with_quaternion_filter_poses_as_the_reference() {
    let compressed = load_slice(&embedded(RIG_KHR)).unwrap();
    let reference = load_slice(&embedded(RIG)).unwrap();
    let stored = 0.5 / 2047. / 2f32.sqrt();
    let output = 0.5 / 32767.;
    let rotation = quaternion_error([
        stored + output,
        stored + output,
        stored + output,
        6f32.sqrt() * stored + output,
    ]);
    assert_rig_matches(&compressed, &reference, rotation, octahedral_normal());
}

// Defects: an optional extension's fallback buffer read rather than
// decoded around (its file is absent), or the extension ignored because it
// is not required. Oracle: RiggedSimple within gltfpack `-c`'s precision:
// 16-bit snorm rotations and 8-bit snorm normals.
#[wasm_bindgen_test(unsupported = test)]
fn optional_ext_meshopt_loads_without_its_fallback_file() {
    let compressed = load_slice(&embedded(RIG_FALLBACK)).unwrap();
    let reference = load_slice(&embedded(RIG)).unwrap();
    let document: Value = serde_json::from_slice(RIG_FALLBACK.0).unwrap();
    assert!(
        document
            .get("extensionsRequired")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e != "EXT_meshopt_compression")
    );
    assert!(document["buffers"][1]["uri"].is_string());
    assert_rig_matches(
        &compressed,
        &reference,
        quaternion_error([0.5 / 32767.; 4]),
        0.5 / 127. * 3f32.sqrt() + 1e-6,
    );
}

// The same through the file entry point, which resolves the compressed
// buffer beside the glTF and must not open the absent fallback file.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn file_entry_point_decodes_meshopt_without_reading_the_fallback() {
    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/gltf");
    let fallback = fixtures.join("RiggedSimple/glTF-Meshopt-Fallback");
    assert!(!fallback.join("RiggedSimple.fallback.bin").exists());
    let compressed = load(&fallback.join("RiggedSimple.gltf")).unwrap();
    let reference = load(&fixtures.join("RiggedSimple/glTF/RiggedSimple.gltf")).unwrap();
    assert_rig_matches(
        &compressed,
        &reference,
        quaternion_error([0.5 / 32767.; 4]),
        0.5 / 127. * 3f32.sqrt() + 1e-6,
    );
    let cube = fixtures.join("AnimatedMorphCube");
    assert_cube_matches(
        &load(&cube.join("glTF-Meshopt/AnimatedMorphCube.gltf")).unwrap(),
        &load(&cube.join("glTF/AnimatedMorphCube.gltf")).unwrap(),
    );
}

/// The colour grid's authored values (`encode.mjs`): positions, colours
/// and triangle-list indices.
fn grid() -> (Vec<Vec3>, Vec<[f32; 4]>, Vec<u32>) {
    let mut positions = Vec::new();
    let mut colours = Vec::new();
    for y in 0..3 {
        for x in 0..3 {
            let (x, y) = (x as f32, y as f32);
            positions.push(Vec3::new(x, y, 0.));
            colours.push([x / 2., y / 2., (x + y) / 4., 1. - x / 4.]);
        }
    }
    let mut indices = Vec::new();
    for y in 0..2 {
        for x in 0..2 {
            let a = y * 3 + x;
            indices.extend([a, a + 1, a + 4, a, a + 4, a + 3]);
        }
    }
    (positions, colours, indices)
}

/// The sRGB transfer function (IEC 61966-2-1), encoded to linear.
fn linear(srgb: f32) -> f32 {
    if srgb <= 0.04045 {
        srgb / 12.92
    } else {
        ((srgb + 0.055) / 1.055).powf(2.4)
    }
}

// Defects: the COLOR filter's YCoCg reconstruction or alpha scale wrong;
// the INDICES mode's baselines misread; KHR's version 1 stream rejected.
// Oracle: the values `encode.mjs` authored before meshoptimizer's encoder
// compressed them. Positions are exact; indices name the same triangles; a
// colour component errs by at most two 8-bit steps (Y, Co and Cg each
// rounded by half a step and summed, then the output's half step), plus
// half the widest step of the 8-bit sRGB the loader keeps colour in.
#[wasm_bindgen_test(unsupported = test)]
fn color_filter_and_index_sequence_decode_to_the_authored_grid() {
    assert_grid(&load_slice(GRID).unwrap());
}

/// Asserts that `asset` is the authored grid (see the test above).
fn assert_grid(asset: &Asset) {
    let (positions, colours, indices) = grid();
    let [mesh] = &asset.meshes[..] else {
        panic!("one mesh");
    };
    let order: Vec<usize> = mesh
        .vertices
        .iter()
        .map(|vertex| {
            positions
                .iter()
                .position(|p| *p == Vec3::from(vertex.position))
                .unwrap_or_else(|| panic!("{:?} is not authored", vertex.position))
        })
        .collect();
    let colour = 2. / 255. + (1. - linear(254. / 255.)) / 2.;
    for (vertex, &authored) in mesh.vertices.iter().zip(&order) {
        assert_eq!(vertex.normal, [0., 0., 1.]);
        for (c, (value, expected)) in vertex.color.iter().zip(colours[authored]).enumerate() {
            assert!(
                (value - expected).abs() <= colour,
                "vertex {authored} component {c}: {value} not {expected}"
            );
        }
    }
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
        triangles.sort_unstable();
        triangles
    };
    assert_eq!(
        triangles(&mesh.indices, &|i| order[i as usize] as u32),
        triangles(&indices, &|i| i)
    );
}

/// The grid's JSON and BIN chunk.
fn grid_parts() -> (Value, Vec<u8>) {
    let json_length = u32::from_le_bytes(GRID[12..16].try_into().unwrap()) as usize;
    let json = serde_json::from_slice(&GRID[20..20 + json_length]).unwrap();
    (json, GRID[28 + json_length..].to_vec())
}

fn grid_error(change: impl FnOnce(&mut Value, &mut Vec<u8>), options: LoadOptions<'_>) -> String {
    let (mut json, mut bin) = grid_parts();
    change(&mut json, &mut bin);
    let bytes = embedded((&serde_json::to_vec(&json).unwrap(), &bin));
    match load_slice_with_options(&bytes, options) {
        Ok(_) => panic!("the changed grid loaded"),
        Err(error) => error.to_string(),
    }
}

// Defect: a decoded buffer allocated at its declared size before the
// decoded budget is charged, so a file declaring a huge view allocates it.
// Oracle: a view declared far beyond the 1 GiB decoded budget must be
// refused, naming the budget. Natively it is 2^46 bytes, an allocation
// that fails and aborts if made first; on wasm32 3 GiB, beyond its memory.
#[wasm_bindgen_test(unsupported = test)]
fn a_declared_decoded_size_above_the_budget_is_refused_before_allocating() {
    let huge: u64 = if cfg!(target_pointer_width = "64") {
        12 << 42
    } else {
        3 << 30
    };
    let error = grid_error(
        |json, _| {
            json["bufferViews"][0]["byteLength"] = huge.into();
            json["bufferViews"][0]["extensions"]["KHR_meshopt_compression"]["count"] =
                (huge / 12).into();
            json["buffers"][1]["byteLength"] = (huge + 300).into();
        },
        LoadOptions {
            budget: LoadBudget::new(1 << 20, 1 << 30).unwrap(),
            ..LoadOptions::default()
        },
    );
    assert!(error.contains("decoded load budget exceeded"), "{error}");
}

// Defects: a truncated or corrupt stream read past its end, panicking, or
// decoded into garbage that loads; an extension's rules not enforced
// (COLOR is KHR's, a fallback buffer an uncompressed view reads, a view
// with both extensions). Oracle: the specifications' validity rules.
#[wasm_bindgen_test(unsupported = test)]
fn malformed_compressed_views_fail_the_load_naming_the_view() {
    let options = LoadOptions::default();
    let khr = "KHR_meshopt_compression";
    // One byte short of the stream.
    let error = grid_error(
        |json, _| {
            let length = &mut json["bufferViews"][0]["extensions"][khr]["byteLength"];
            *length = (length.as_u64().unwrap() - 1).into();
        },
        options,
    );
    assert!(
        error.contains("buffer view 0") && error.contains(khr),
        "{error}"
    );
    // A corrupt header byte.
    let error = grid_error(|_, bin| bin[0] = 0xb1, options);
    assert!(
        error.contains("buffer view 0") && error.contains("header"),
        "{error}"
    );
    // The index sequence's header replaced by the triangle stream's.
    let error = grid_error(
        |json, bin| {
            let offset = json["bufferViews"][3]["extensions"][khr]["byteOffset"]
                .as_u64()
                .unwrap();
            bin[offset as usize] = 0xe1;
        },
        options,
    );
    assert!(error.contains("buffer view 3"), "{error}");
    // COLOR under the EXT extension, which does not define it.
    let error = grid_error(
        |json, _| {
            json["extensionsUsed"] = serde_json::json!([khr, "EXT_meshopt_compression"]);
            let view = json["bufferViews"][2]["extensions"]
                .as_object_mut()
                .unwrap();
            let object = view.remove(khr).unwrap();
            view.insert("EXT_meshopt_compression".into(), object);
        },
        options,
    );
    assert!(
        error.contains("buffer view 2") && error.contains("Color"),
        "{error}"
    );
    // Both extensions on one view.
    let error = grid_error(
        |json, _| {
            json["extensionsUsed"] = serde_json::json!([khr, "EXT_meshopt_compression"]);
            let object = json["bufferViews"][0]["extensions"][khr].clone();
            json["bufferViews"][0]["extensions"]["EXT_meshopt_compression"] = object;
        },
        options,
    );
    assert!(
        error.contains("buffer view 0") && error.contains("both"),
        "{error}"
    );
    // A fallback buffer an uncompressed view reads.
    let error = grid_error(
        |json, _| {
            json["bufferViews"][3]["extensions"]
                .as_object_mut()
                .unwrap()
                .remove(khr);
        },
        options,
    );
    assert!(
        error.contains("buffer 1") && error.contains("fallback"),
        "{error}"
    );
    // A view whose count and stride do not make its length.
    let error = grid_error(
        |json, _| json["bufferViews"][0]["extensions"][khr]["count"] = 8.into(),
        options,
    );
    assert!(
        error.contains("buffer view 0") && error.contains("count"),
        "{error}"
    );
}

// Defect: the decoder treats a file that lists neither extension
// differently, or a decode-only buffer leaks into one that has none.
// Oracle: the float cube loads to the same asset with and without an
// unrelated buffer view extension object, since neither extension is used.
#[wasm_bindgen_test(unsupported = test)]
fn an_unlisted_extension_object_is_not_decoded() {
    let plain = load_slice(&embedded(CUBE_FLOAT)).unwrap();
    let mut json: Value = serde_json::from_slice(CUBE_FLOAT.0).unwrap();
    json["bufferViews"][0]["extensions"] = serde_json::json!({
        "EXT_meshopt_compression": {"buffer": 0, "byteLength": 1, "byteStride": 4, "count": 1, "mode": "ATTRIBUTES"}
    });
    let marked = load_slice(&embedded((
        &serde_json::to_vec(&json).unwrap(),
        CUBE_FLOAT.1,
    )))
    .unwrap();
    let positions = |asset: &Asset| -> Vec<[f32; 3]> {
        asset
            .meshes
            .iter()
            .flat_map(|mesh| mesh.vertices.iter().map(|v| v.position))
            .collect()
    };
    assert_eq!(positions(&plain), positions(&marked));
    assert_eq!(plain.meshes[0].indices, marked.meshes[0].indices);
}

// Defect: a buffer tagged as a fallback that nothing references, without a
// URI, is read (and fails as a second BIN chunk) or refused. Oracle: both
// specifications let a fallback buffer have only compressed references, of
// which none is one; such a buffer has nothing to read.
#[wasm_bindgen_test(unsupported = test)]
fn an_unreferenced_fallback_buffer_is_never_read() {
    let (mut json, bin) = grid_parts();
    json["buffers"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({"byteLength": 64, "extensions": {"KHR_meshopt_compression": {"fallback": true}}}));
    let asset = load_slice(&embedded((&serde_json::to_vec(&json).unwrap(), &bin))).unwrap();
    assert_eq!(asset.meshes[0].indices.len(), 24);
}

/// The grid with its views' parent bytes moved into the end of the BIN
/// chunk, which also holds the compressed bytes, and the fallback buffer
/// removed.
fn grid_in_one_buffer() -> (Value, Vec<u8>) {
    let (mut json, mut bin) = grid_parts();
    let base = bin.len().next_multiple_of(4);
    let fallback = json["buffers"][1]["byteLength"].as_u64().unwrap() as usize;
    bin.resize(base + fallback, 0);
    for view in json["bufferViews"].as_array_mut().unwrap() {
        view["buffer"] = 0.into();
        let offset = view["byteOffset"].as_u64().unwrap_or(0) as usize;
        view["byteOffset"] = (base + offset).into();
    }
    json["buffers"] = serde_json::json!([{"byteLength": bin.len()}]);
    (json, bin)
}

// Defects: a view decoded into the buffer that also holds compressed
// bytes overwrites another view's input before it is decoded, or loses
// its output; a read buffer that an uncompressed view also uses is not
// decoded into. Oracle: the authored grid, which both layouts hold.
#[wasm_bindgen_test(unsupported = test)]
fn views_decode_into_buffers_that_are_also_read() {
    // Parents and compressed bytes in one buffer, which is therefore read.
    let (json, bin) = grid_in_one_buffer();
    assert_grid(&load_slice(&embedded((&serde_json::to_vec(&json).unwrap(), &bin))).unwrap());
    // The same, with the parents overlapping the compressed bytes they
    // and the views after them decode from: every view must read its
    // input before any output lands.
    let (mut json, bin) = grid_parts();
    json["buffers"] = serde_json::json!([{"byteLength": bin.len().max(300)}]);
    let mut bin = bin;
    bin.resize(bin.len().max(300), 0);
    for view in json["bufferViews"].as_array_mut().unwrap() {
        view["buffer"] = 0.into();
    }
    assert_grid(&load_slice(&embedded((&serde_json::to_vec(&json).unwrap(), &bin))).unwrap());
    // A fallback buffer with data that an uncompressed view also names,
    // so it is read and the compressed views decode into it.
    let (mut json, bin) = grid_parts();
    let zeros = "A".repeat(400);
    json["buffers"][1] = serde_json::json!({
        "byteLength": 300,
        "uri": format!("data:application/octet-stream;base64,{zeros}")
    });
    json["bufferViews"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({"buffer": 1, "byteLength": 4}));
    assert_grid(&load_slice(&embedded((&serde_json::to_vec(&json).unwrap(), &bin))).unwrap());
}

// Defects: rules of one extension not enforced: a fallback buffer that a
// compressed view takes bytes from, EXT's parent byteStride that differs
// from its own, and KHR attributes narrower than 4 bytes. Oracle: the
// specifications' validity rules.
#[wasm_bindgen_test(unsupported = test)]
fn extension_specific_rules_are_enforced() {
    let options = LoadOptions::default();
    let khr = "KHR_meshopt_compression";
    let error = grid_error(
        |json, _| json["bufferViews"][3]["extensions"][khr]["buffer"] = 1.into(),
        options,
    );
    assert!(
        error.contains("buffer 1") && error.contains("fallback"),
        "{error}"
    );
    // The normals view under EXT, with a parent stride of 16 against 12.
    let error = grid_error(
        |json, _| {
            json["extensionsUsed"] = serde_json::json!([khr, "EXT_meshopt_compression"]);
            let view = &mut json["bufferViews"][1];
            view["byteStride"] = 16.into();
            let object = view["extensions"]
                .as_object_mut()
                .unwrap()
                .remove(khr)
                .unwrap();
            view["extensions"]["EXT_meshopt_compression"] = object;
        },
        options,
    );
    assert!(
        error.contains("buffer view 1") && error.contains("differs"),
        "{error}"
    );
    // KHR attributes with a 2-byte stride: the index view as ATTRIBUTES.
    let error = grid_error(
        |json, _| json["bufferViews"][3]["extensions"][khr]["mode"] = "ATTRIBUTES".into(),
        options,
    );
    assert!(
        error.contains("buffer view 3") && error.contains("Attributes"),
        "{error}"
    );
}
