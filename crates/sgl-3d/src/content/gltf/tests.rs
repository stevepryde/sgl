//! The glTF loader against hand-built documents.
use super::*;
use glam::Vec3;
use std::{
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};

struct Fixture {
    directory: PathBuf,
}

impl Fixture {
    fn new(source: &[u8], values: &[f32]) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let directory = std::env::temp_dir().join(format!(
            "sgl-gltf-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        std::fs::create_dir(&directory).unwrap();
        std::fs::write(directory.join("fixture.gltf"), source).unwrap();
        let bytes: Vec<_> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
        std::fs::write(directory.join("fixture.bin"), bytes).unwrap();
        Self { directory }
    }

    fn embedded(&self) -> Vec<u8> {
        let mut document: serde_json::Value =
            serde_json::from_slice(&std::fs::read(self.path()).unwrap()).unwrap();
        document["buffers"][0]
            .as_object_mut()
            .unwrap()
            .remove("uri");
        let mut json = serde_json::to_vec(&document).unwrap();
        while !json.len().is_multiple_of(4) {
            json.push(b' ');
        }
        let bin = std::fs::read(self.directory.join("fixture.bin")).unwrap();
        let mut bytes = Vec::new();
        for value in [
            0x46546c67u32,
            2,
            (28 + json.len() + bin.len()) as u32,
            json.len() as u32,
            0x4e4f534a,
        ] {
            bytes.extend(value.to_le_bytes());
        }
        bytes.extend(json);
        bytes.extend((bin.len() as u32).to_le_bytes());
        bytes.extend(0x004e4942u32.to_le_bytes());
        bytes.extend(bin);
        bytes
    }

    fn path(&self) -> PathBuf {
        self.directory.join("fixture.gltf")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

fn change_document(fixture: &Fixture, change: impl FnOnce(&mut serde_json::Value)) {
    let mut document = serde_json::from_slice(&std::fs::read(fixture.path()).unwrap()).unwrap();
    change(&mut document);
    std::fs::write(fixture.path(), serde_json::to_vec(&document).unwrap()).unwrap();
}

// Defect: a URI reads a real file outside its asset directory, including
// escaped parent segments and absolute/scheme forms. Independent oracle:
// the same outside files contain a valid mesh/PNG that loads beside the glTF,
// but S3D-2 forbids resolving them through an escaping URI.
#[test]
fn buffer_and_image_uris_are_confined_to_the_asset_directory() {
    let outside = extension_fixture(serde_json::json!({}), &[], &[]);
    image::RgbaImage::from_pixel(1, 1, image::Rgba([10, 20, 30, 255]))
        .save(outside.directory.join("outside.png"))
        .unwrap();
    let outside_name = outside.directory.file_name().unwrap().to_str().unwrap();
    for is_image in [false, true] {
        let file = if is_image {
            "outside.png"
        } else {
            "fixture.bin"
        };
        let absolute = outside.directory.join(file).to_str().unwrap().to_owned();
        let uris = [
            format!("../{outside_name}/{file}"),
            format!("%2e%2e%2f{outside_name}/{file}"),
            absolute.clone(),
            format!("file://{absolute}"),
            format!("file%3a{absolute}"),
            format!("C:\\{file}"),
            format!("%5c%5cserver%5c{file}"),
        ];
        for uri in uris {
            let fixture = if is_image {
                textured_triangle(
                    serde_json::json!({"pbrMetallicRoughness":{"baseColorTexture":{"index":0}}}),
                    serde_json::json!([{"uri": uri}]),
                    serde_json::json!([{"source":0}]),
                    &[],
                )
            } else {
                let fixture = extension_fixture(serde_json::json!({}), &[], &[]);
                change_document(&fixture, |document| {
                    document["buffers"][0]["uri"] = uri.clone().into()
                });
                fixture
            };
            let error = load(&fixture.path()).err().unwrap().to_string();
            assert!(
                error.contains("URI") && error.contains("relative path"),
                "{error}"
            );
            // Errors identify the authored URI; JSON-style escapes in the
            // diagnostic preserve Windows separators unambiguously.
            assert!(error.contains(&format!("{uri:?}")), "{error}");
        }
    }
}

#[test]
fn external_sources_refuse_oversized_files_before_reading() {
    let fixture = extension_fixture(serde_json::json!({}), &[], &[]);
    // A sparse file exceeds the production cap without allocating gigabytes.
    std::fs::OpenOptions::new()
        .write(true)
        .open(fixture.directory.join("fixture.bin"))
        .unwrap()
        .set_len(MAX_LOAD_BYTES + 1)
        .unwrap();
    let error = load(&fixture.path()).err().unwrap().to_string();
    assert!(
        error.contains("fixture.bin") && error.contains("remaining") && error.contains("budget"),
        "{error}"
    );

    let fixture = textured_triangle(
        serde_json::json!({"pbrMetallicRoughness":{"baseColorTexture":{"index":0}}}),
        serde_json::json!([{"uri":"huge.png"}]),
        serde_json::json!([{"source":0}]),
        &[],
    );
    // This image fits the cap on its own, but not after the document and
    // mesh buffer: removing either source's charge must fail this regression.
    let already_read = std::fs::metadata(fixture.path()).unwrap().len()
        + std::fs::metadata(fixture.directory.join("fixture.bin"))
            .unwrap()
            .len();
    std::fs::File::create(fixture.directory.join("huge.png"))
        .unwrap()
        .set_len(MAX_LOAD_BYTES - already_read + 1)
        .unwrap();
    let error = load(&fixture.path()).err().unwrap().to_string();
    assert!(
        error.contains("huge.png") && error.contains("remaining") && error.contains("budget"),
        "{error}"
    );
}

#[test]
fn buffer_file_length_must_match_its_declaration() {
    for length in [71, 73] {
        let fixture = extension_fixture(serde_json::json!({}), &[], &[]);
        std::fs::OpenOptions::new()
            .write(true)
            .open(fixture.directory.join("fixture.bin"))
            .unwrap()
            .set_len(length)
            .unwrap();
        let error = load(&fixture.path()).err().unwrap().to_string();
        assert!(
            error.contains("fixture.bin") && error.contains("byteLength"),
            "{error}"
        );
    }
}

#[cfg(unix)]
#[test]
fn external_sources_refuse_symlink_files_and_directories() {
    use std::os::unix::fs::symlink;
    let outside = extension_fixture(serde_json::json!({}), &[], &[]);
    let fixture = extension_fixture(serde_json::json!({}), &[], &[]);
    symlink(
        outside.directory.join("fixture.bin"),
        fixture.directory.join("linked.bin"),
    )
    .unwrap();
    symlink(&outside.directory, fixture.directory.join("linked")).unwrap();
    for uri in ["linked.bin", "linked/fixture.bin"] {
        change_document(&fixture, |document| {
            document["buffers"][0]["uri"] = uri.into()
        });
        let error = load(&fixture.path()).err().unwrap().to_string();
        assert!(error.contains(uri) && error.contains("symlink"), "{error}");
    }
}

// Defects: URI confinement also refuses legitimate percent-encoded nested
// files, or moving ownership of reads loses GLB/data-URI buffer bytes.
// Oracle: the authored triangle's three positions and a PNG's known texel.
#[test]
fn nested_percent_encoded_files_glb_and_data_uris_still_load() {
    let fixture = textured_triangle(
        serde_json::json!({"pbrMetallicRoughness":{"baseColorTexture":{"index":0}}}),
        serde_json::json!([{"uri":"nested/pixel%20image.png"}]),
        serde_json::json!([{"source":0}]),
        &[],
    );
    std::fs::create_dir(fixture.directory.join("nested")).unwrap();
    std::fs::copy(
        fixture.directory.join("fixture.bin"),
        fixture.directory.join("nested/mesh data.bin"),
    )
    .unwrap();
    image::RgbaImage::from_pixel(1, 1, image::Rgba([10, 20, 30, 255]))
        .save(fixture.directory.join("nested/pixel image.png"))
        .unwrap();
    change_document(&fixture, |document| {
        document["buffers"][0]["uri"] = "nested/mesh%20data.bin".into()
    });
    let asset = load(&fixture.path()).unwrap();
    assert_eq!(
        asset.meshes[0]
            .vertices
            .iter()
            .map(|vertex| vertex.position)
            .collect::<Vec<_>>(),
        [[0., 0., 0.], [1., 0., 0.], [0., 1., 0.]]
    );
    let Image::Rgba8(image) = &asset.images[0] else {
        panic!("expected decoded PNG")
    };
    assert_eq!(image.get_pixel(0, 0).0, [10, 20, 30, 255]);

    let fixture = extension_fixture(serde_json::json!({}), &[], &[]);
    let glb = load_slice(&fixture.embedded()).unwrap();
    assert_eq!(glb.meshes[0].vertices[1].position, [1., 0., 0.]);
    // An unused extra byte in the declared BIN data needs three bytes of
    // alignment padding in its chunk; glTF permits those padding bytes.
    change_document(&fixture, |document| {
        document["buffers"][0]["byteLength"] = 73.into()
    });
    std::fs::OpenOptions::new()
        .write(true)
        .open(fixture.directory.join("fixture.bin"))
        .unwrap()
        .set_len(76)
        .unwrap();
    let padded = load_slice(&fixture.embedded()).unwrap();
    assert_eq!(padded.meshes[0].vertices[2].position, [0., 1., 0.]);
    change_document(&fixture, |document| {
        document["buffers"][0]["byteLength"] = 72.into()
    });
    // Three positions and three +Z normals, little-endian f32, encoded as
    // glTF's standard base64 buffer. This exercises actual accessor reads.
    change_document(&fixture, |document| {
        document["buffers"][0]["uri"] =
        "data:application/octet-stream;base64,AAAAAAAAAAAAAAAAAACAPwAAAAAAAAAAAAAAAAAAgD8AAAAAAAAAAAAAAAAAAIA/AAAAAAAAAAAAAIA/AAAAAAAAAAAAAIA/".into()
    });
    let data = load_slice(&std::fs::read(fixture.path()).unwrap()).unwrap();
    assert_eq!(data.meshes[0].vertices[2].position, [0., 1., 0.]);
    assert_eq!(data.meshes[0].vertices[1].normal, [0., 0., 1.]);
}

fn anisotropy_fixture(
    extension: serde_json::Value,
    tangent: Option<[f32; 4]>,
    mirrored: bool,
) -> Fixture {
    let mut document = serde_json::json!({
        "asset": {"version": "2.0"},
        "extensionsUsed": ["KHR_materials_anisotropy"],
        "buffers": [{"uri": "fixture.bin", "byteLength": 72}],
        "bufferViews": [{"buffer": 0, "byteLength": 36}, {"buffer": 0, "byteOffset": 36, "byteLength": 36}],
        "accessors": [
            {"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3", "min": [0,0,0], "max": [1,1,1]},
            {"bufferView": 1, "componentType": 5126, "count": 3, "type": "VEC3"}
        ],
        "materials": [{"name": "brushed", "extensions": {"KHR_materials_anisotropy": extension}}],
        "meshes": [{"primitives": [{"attributes": {"POSITION": 0, "NORMAL": 1}, "material": 0}]}],
        "nodes": [{"mesh": 0, "matrix": [2,0,0,0, 1,3,0,0, 0,0,4,0, 0,0,0,1]}],
        "scenes": [{"nodes": [0]}], "scene": 0
    });
    if mirrored {
        document["nodes"][0]["matrix"][0] = (-2).into();
        document["nodes"][0]["matrix"][4] = (-1).into();
    }
    let q = std::f32::consts::FRAC_1_SQRT_2;
    let mut values = vec![
        0., 0., 0., 1., 0., 0., 0., 1., 1., 0., -q, q, 0., -q, q, 0., -q, q,
    ];
    if let Some(tangent) = tangent {
        document["buffers"][0]["byteLength"] = 120.into();
        document["bufferViews"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({"buffer":0,"byteOffset":72,"byteLength":48}));
        document["accessors"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({"bufferView":2,"componentType":5126,"count":3,"type":"VEC4"}));
        document["meshes"][0]["primitives"][0]["attributes"]["TANGENT"] = 2.into();
        values.extend(tangent.repeat(3));
    }
    Fixture::new(&serde_json::to_vec(&document).unwrap(), &values)
}

#[test]
fn anisotropic_frames_follow_shear_reflection_and_uv_handedness() {
    // x' = +/- (2x+y), y'=3y, z'=4z on y=z. The tangent
    // (1,1,1)/sqrt(3) becomes (+/-3,3,4)/sqrt(34), independently
    // of the inverse-transpose normal (0,-4,3)/5. UV handedness
    // and a reflected node each reverse B once, never the tangent itself.
    let q = 1.0 / 3.0_f32.sqrt();
    for mirrored in [false, true] {
        for handedness in [-1.0, 1.0] {
            let fixture = anisotropy_fixture(
                serde_json::json!({"anisotropyStrength":0.7}),
                Some([q, q, q, handedness]),
                mirrored,
            );
            let asset = load(&fixture.path()).unwrap();
            let mesh = &asset.meshes[0];
            let sign = if mirrored { -1.0 } else { 1.0 };
            let expected_t = Vec3::new(sign * 3.0, 3.0, 4.0) / 34.0_f32.sqrt();
            let expected_b = handedness * Vec3::new(-sign * 5.0, 1.8, 2.4) / 34.0_f32.sqrt();
            for vertex in &mesh.vertices {
                let n = Vec3::from_array(vertex.normal);
                let t = Vec3::from_slice(&vertex.tangent[..3]);
                let b = n.cross(t) * vertex.tangent[3];
                assert!((n - Vec3::new(0., -0.8, 0.6)).length() < 1e-6);
                assert!((t - expected_t).length() < 1e-6);
                assert!((b - expected_b).length() < 1e-6);
            }
            let p: Vec<_> = mesh
                .indices
                .iter()
                .map(|&i| Vec3::from_array(mesh.vertices[i as usize].position))
                .collect();
            assert!(
                (p[1] - p[0])
                    .cross(p[2] - p[0])
                    .dot(Vec3::new(0., -0.8, 0.6))
                    > 0.0
            );
        }
    }
}

/// Tangents along the fixture's normal in all but rounding, so their
/// projection off it is noise: four ulps off the authored normal (0, -q, q),
/// and (8, -16, 9) four ulps off, which the node's shear (columns (2,0,0),
/// (1,3,0), (0,0,4)) takes to (0, -48, 36), along the normal (0, -0.8, 0.6)
/// it leaves, though far off the authored one.
fn tangents_ulps_off_the_normal(handedness: f32) -> [[f32; 4]; 2] {
    let q = std::f32::consts::FRAC_1_SQRT_2;
    let near = |x: f32| f32::from_bits(x.to_bits() + 4);
    [
        [0., -q, near(q), handedness],
        [8., -16., near(9.), handedness],
    ]
}

#[test]
fn anisotropic_loader_rejects_missing_or_degenerate_authored_frames() {
    let invalid = [
        None,
        Some([0., 0., 0., 1.]),
        Some([0., -1., 1., 1.]),
        Some([1., 0., 0., 0.]),
        Some([f32::NAN, 0., 0., 1.]),
    ];
    let near = tangents_ulps_off_the_normal(1.).map(Some);
    for tangent in invalid.into_iter().chain(near) {
        let fixture = anisotropy_fixture(
            serde_json::json!({"anisotropyStrength":0.7}),
            tangent,
            false,
        );
        let error = load(&fixture.path()).err().unwrap().to_string();
        assert!(
            error.contains("TANGENT") && error.contains("export"),
            "{error}"
        );
    }
    // A legacy primitive with no tangent remains importable at zero strength.
    let fixture = anisotropy_fixture(serde_json::json!({}), None, false);
    assert!(load(&fixture.path()).is_ok());
}

// Defects: a tangent along its normal, valid glTF (unit XYZ, w = ±1), fails
// the load of a material that never builds a frame from it; one a few ulps
// off it has its rounding normalised into an arbitrary tangent. The oracle
// is `Vertex::tangent`'s contract: zero means absent. An anisotropic
// material still refuses both (above).
#[test]
fn a_tangent_along_its_normal_is_absent_without_anisotropy() {
    let q = std::f32::consts::FRAC_1_SQRT_2;
    for tangent in [[0., -q, q, 1.]]
        .into_iter()
        .chain(tangents_ulps_off_the_normal(-1.))
    {
        let fixture = anisotropy_fixture(serde_json::json!({}), Some(tangent), false);
        let asset = load(&fixture.path()).unwrap_or_else(|error| panic!("{error}"));
        for vertex in &asset.meshes[0].vertices {
            assert_eq!(vertex.tangent, [0.; 4], "{tangent:?}");
        }
    }
}

// Defects: a valid file refused over a vertex attribute nothing SGL3D draws
// reads (a third UV set, a second colour set, an application-specific
// one), one left out without being listed in `Asset::ignored`, or COLOR_1
// read as the vertex colour. The oracle is glTF 2.0 3.7.2.1 (loaders need
// not support more than two UV sets and one colour set; `_`-prefixed
// attributes are application-specific) and the documented fallback.
#[test]
fn unread_vertex_attributes_are_listed_not_refused() {
    let document = serde_json::json!({
        "asset": {"version": "2.0"},
        "buffers": [{"uri": "fixture.bin", "byteLength": 72}],
        "bufferViews": [{"buffer": 0, "byteLength": 36}, {"buffer": 0, "byteOffset": 36, "byteLength": 36}],
        "accessors": [
            {"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3", "min": [0,0,0], "max": [1,1,0]},
            {"bufferView": 1, "componentType": 5126, "count": 3, "type": "VEC3"},
            {"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC2"}
        ],
        "meshes": [{"primitives": [{"attributes": {
            "POSITION": 0, "NORMAL": 1, "TEXCOORD_2": 2, "COLOR_1": 1, "_TEMPERATURE": 0
        }}]}],
        "nodes": [{"mesh": 0}],
        "scenes": [{"nodes": [0]}], "scene": 0
    });
    let values = [
        0., 0., 0., 1., 0., 0., 0., 1., 0., 0., 0., 1., 0., 0., 1., 0., 0., 1.,
    ];
    let fixture = Fixture::new(&serde_json::to_vec(&document).unwrap(), &values);
    let asset = load(&fixture.path()).unwrap_or_else(|error| panic!("{error}"));
    let mut names: Vec<_> = asset
        .ignored
        .iter()
        .map(|ignored| match ignored {
            Ignored::Attribute {
                mesh: 0,
                primitive: 0,
                name,
            } => name.as_str(),
            other => panic!("{other:?}"),
        })
        .collect();
    names.sort_unstable();
    assert_eq!(names, ["COLOR_1", "TEXCOORD_2", "_TEMPERATURE"]);
    for vertex in &asset.meshes[0].vertices {
        assert_eq!(vertex.color, [1.; 4], "COLOR_1 read as the vertex colour");
    }
}

#[test]
fn anisotropy_import_resolves_texture_image_and_rejects_unsupported_inputs() {
    let fixture = anisotropy_fixture(
        serde_json::json!({
            "anisotropyStrength":0.7, "anisotropyRotation":-1.25,
            "anisotropyTexture":{"index":1}
        }),
        Some([1., 0., 0., 1.]),
        false,
    );
    let mut source: serde_json::Value =
        serde_json::from_slice(&std::fs::read(fixture.path()).unwrap()).unwrap();
    source["images"] = serde_json::json!([{"uri":"direction.png"},{"uri":"other.png"}]);
    source["textures"] = serde_json::json!([{"source":1},{"source":0}]);
    // Read the material boundary directly to isolate texture index -> image
    // indirection; image decoding and material GPU sampling have separate tests.
    let document = gltf::Gltf::from_slice(&serde_json::to_vec(&source).unwrap())
        .unwrap()
        .document;
    let material = read_material(
        document.materials().next().unwrap(),
        &document,
        0,
        &mut Vec::new(),
    )
    .unwrap();
    assert_eq!(material.anisotropy_texture, Some(0));
    assert!((material.anisotropy_rotation + 1.25).abs() < 1e-6);
    assert!((material.anisotropy_strength - 0.7).abs() < 1e-6);
    for extension in [
        serde_json::json!({"anisotropyStrength":-0.1}),
        serde_json::json!({"anisotropyStrength":-1e-100}),
        serde_json::json!({"anisotropyStrength":1.1}),
        serde_json::json!({"anisotropyRotation":"bad"}),
        serde_json::json!({"anisotropyTexture":{"index":4}}),
        serde_json::json!({"anisotropyTexture":{"index":0,"texCoord":1}}),
    ] {
        source["materials"][0]["extensions"]["KHR_materials_anisotropy"] = extension;
        let document = gltf::Gltf::from_slice(&serde_json::to_vec(&source).unwrap())
            .unwrap()
            .document;
        assert!(
            read_material(
                document.materials().next().unwrap(),
                &document,
                0,
                &mut Vec::new()
            )
            .is_err()
        );
    }
}

#[test]
fn required_anisotropy_is_supported_without_disabling_core_validation() {
    let fixture = anisotropy_fixture(
        serde_json::json!({"anisotropyStrength":0.7}),
        Some([1., 0., 0., 1.]),
        false,
    );
    let mut source: serde_json::Value =
        serde_json::from_slice(&std::fs::read(fixture.path()).unwrap()).unwrap();
    source["extensionsRequired"] = serde_json::json!(["KHR_materials_anisotropy"]);
    std::fs::write(fixture.path(), serde_json::to_vec(&source).unwrap()).unwrap();
    assert!(load(&fixture.path()).is_ok());
    assert!(load_slice(&fixture.embedded()).is_ok());
    // A malformed accessor remains rejected by the native glTF validator.
    source["accessors"][0]["bufferView"] = 99.into();
    std::fs::write(fixture.path(), serde_json::to_vec(&source).unwrap()).unwrap();
    assert!(load(&fixture.path()).is_err());
    source["accessors"][0]["bufferView"] = 0.into();
    source["extensionsRequired"] = serde_json::json!(["KHR_texture_transform"]);
    std::fs::write(fixture.path(), serde_json::to_vec(&source).unwrap()).unwrap();
    assert!(load(&fixture.path()).is_err());
}

#[test]
fn generated_assets_cannot_activate_anisotropy_without_valid_frames() {
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    let mut scene = crate::Scene::new(&device, &queue);
    let fixture = anisotropy_fixture(serde_json::json!({}), Some([1., 0., 0., 1.]), false);
    let mut asset = load(&fixture.path()).unwrap();
    asset.materials[0].anisotropy_strength = 0.7;
    // Absent, or a few ulps off its normal (along it in all but rounding).
    let [x, y, z] = asset.meshes[0].vertices[0].normal;
    for tangent in [[0.; 4], [x, y, f32::from_bits(z.to_bits() + 4), 1.]] {
        let mut missing = asset.meshes[0].clone();
        missing.vertices[0].tangent = tangent;
        asset.meshes.push(missing);
        assert!(
            matches!(
                scene.add_asset(&device, &queue, asset.clone()),
                Err(crate::SceneError::MissingAnisotropyTangents)
            ),
            "{tangent:?}"
        );
        asset.meshes.pop();
    }
    assert!(scene.add_asset(&device, &queue, asset.clone()).is_ok());
    asset.materials[0].anisotropy_rotation = f32::INFINITY;
    assert!(matches!(
        scene.add_asset(&device, &queue, asset),
        Err(crate::SceneError::InvalidAnisotropy)
    ));
}

#[test]
fn hierarchical_reflection_preserves_surface_orientation() {
    // Authored triangle on y=z, with a translated child under a reflected,
    // nonuniformly scaled parent. Expected world coordinates are independent
    // of the loader's matrix implementation.
    let source = br#"{
      "asset":{"version":"2.0"},
      "buffers":[{"uri":"fixture.bin","byteLength":72}],
      "bufferViews":[{"buffer":0,"byteOffset":0,"byteLength":36},{"buffer":0,"byteOffset":36,"byteLength":36}],
      "accessors":[
        {"bufferView":0,"componentType":5126,"count":3,"type":"VEC3","min":[0,0,0],"max":[1,1,1]},
        {"bufferView":1,"componentType":5126,"count":3,"type":"VEC3"}
      ],
      "meshes":[{"primitives":[{"attributes":{"POSITION":0,"NORMAL":1}}]}],
      "nodes":[{"translation":[10,20,30],"scale":[-2,3,4],"children":[1]},{"translation":[1,2,3],"mesh":0}],
      "scenes":[{"nodes":[0]}],"scene":0
    }"#;
    let component = std::f32::consts::FRAC_1_SQRT_2;
    let values: [f32; 18] = [
        0., 0., 0., 1., 0., 0., 0., 1., 1., 0., -component, component, 0., -component, component,
        0., -component, component,
    ];
    let fixture = Fixture::new(source, &values);
    let asset = load(&fixture.path()).unwrap();
    let mesh = &asset.meshes[0];
    for (vertex, expected) in
        mesh.vertices
            .iter()
            .zip([[8., 26., 42.], [6., 26., 42.], [8., 29., 46.]])
    {
        assert!((Vec3::from_array(vertex.position) - Vec3::from_array(expected)).length() < 1e-5);
        assert!((Vec3::from_array(vertex.normal) - Vec3::new(0., -0.8, 0.6)).length() < 1e-5);
    }
    let triangle: Vec<_> = mesh
        .indices
        .iter()
        .map(|i| Vec3::from_array(mesh.vertices[*i as usize].position))
        .collect();
    let face_normal = (triangle[1] - triangle[0])
        .cross(triangle[2] - triangle[0])
        .normalize();
    assert!(face_normal.dot(Vec3::from_array(mesh.vertices[0].normal)) > 0.999);
}

#[test]
fn a_tiny_uniform_scale_loads_and_a_collapsed_axis_is_refused() {
    // A uniform scale of 3e-4 (determinant 2.7e-11) is invertible and must
    // load at its scaled positions; a zero scale on one axis is singular.
    let fixture_scaled = |scale: [f32; 3]| {
        let source = serde_json::json!({
            "asset": {"version": "2.0"},
            "buffers": [{"uri": "fixture.bin", "byteLength": 72}],
            "bufferViews": [{"buffer": 0, "byteLength": 36}, {"buffer": 0, "byteOffset": 36, "byteLength": 36}],
            "accessors": [
                {"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3", "min": [0,0,0], "max": [1,1,0]},
                {"bufferView": 1, "componentType": 5126, "count": 3, "type": "VEC3"}
            ],
            "meshes": [{"primitives": [{"attributes": {"POSITION": 0, "NORMAL": 1}}]}],
            "nodes": [{"scale": scale, "mesh": 0}],
            "scenes": [{"nodes": [0]}], "scene": 0
        });
        let values: [f32; 18] = [
            0., 0., 0., 1., 0., 0., 0., 1., 0., 0., 0., 1., 0., 0., 1., 0., 0., 1.,
        ];
        Fixture::new(&serde_json::to_vec(&source).unwrap(), &values)
    };
    let tiny = fixture_scaled([3e-4; 3]);
    let asset = load(&tiny.path()).unwrap();
    let mesh = &asset.meshes[0];
    for (vertex, expected) in
        mesh.vertices
            .iter()
            .zip([[0., 0., 0.], [3e-4, 0., 0.], [0., 3e-4, 0.]])
    {
        assert!((Vec3::from_array(vertex.position) - Vec3::from_array(expected)).length() < 1e-9);
        assert!((Vec3::from_array(vertex.normal) - Vec3::Z).length() < 1e-5);
    }
    let collapsed = fixture_scaled([1., 1., 0.]);
    let Err(error) = load(&collapsed.path()) else {
        panic!("a zero-scale axis loaded");
    };
    assert!(error.to_string().contains("singular"), "{error}");
}

#[test]
fn emitted_strength_is_preserved_unless_the_caller_caps_it() {
    // KHR_materials_emissive_strength multiplies the authored color. The
    // application option limits strength, not the resulting color channels.
    let source = br#"{
      "asset":{"version":"2.0"},
      "extensionsUsed":["KHR_materials_emissive_strength"],
      "buffers":[{"uri":"fixture.bin","byteLength":72}],
      "bufferViews":[{"buffer":0,"byteOffset":0,"byteLength":36},{"buffer":0,"byteOffset":36,"byteLength":36}],
      "accessors":[
        {"bufferView":0,"componentType":5126,"count":3,"type":"VEC3","min":[0,0,0],"max":[1,1,0]},
        {"bufferView":1,"componentType":5126,"count":3,"type":"VEC3"}
      ],
      "materials":[{"emissiveFactor":[0.25,0.5,1.0],"extensions":{"KHR_materials_emissive_strength":{"emissiveStrength":4.0}}}],
      "meshes":[{"primitives":[{"attributes":{"POSITION":0,"NORMAL":1},"material":0}]}],
      "nodes":[{"mesh":0}],"scenes":[{"nodes":[0]}],"scene":0
    }"#;
    let values = [
        0., 0., 0., 1., 0., 0., 0., 1., 0., 0., 0., 1., 0., 0., 1., 0., 0., 1.,
    ];
    let fixture = Fixture::new(source, &values);
    let authored = load(&fixture.path()).unwrap();
    let adapted = load_with_options(
        &fixture.path(),
        LoadOptions {
            emissive_strength_cap: Some(1.5),
            ..LoadOptions::default()
        },
    )
    .unwrap();
    let embedded = load_slice_with_options(
        &fixture.embedded(),
        LoadOptions {
            emissive_strength_cap: Some(1.5),
            ..LoadOptions::default()
        },
    )
    .unwrap();
    assert_eq!(authored.materials[0].emissive, [1.0, 2.0, 4.0]);
    assert_eq!(adapted.materials[0].emissive, [0.375, 0.75, 1.5]);
    assert_eq!(embedded.materials[0].emissive, [0.375, 0.75, 1.5]);
}
#[test]
fn embedded_part_selection_keeps_ancestors_and_excludes_other_mesh_nodes() {
    let source = br#"{
      "asset":{"version":"2.0"},
      "buffers":[{"uri":"fixture.bin","byteLength":72}],
      "bufferViews":[{"buffer":0,"byteLength":36},{"buffer":0,"byteOffset":36,"byteLength":36}],
      "accessors":[
        {"bufferView":0,"componentType":5126,"count":3,"type":"VEC3","min":[0,0,0],"max":[1,1,0]},
        {"bufferView":1,"componentType":5126,"count":3,"type":"VEC3"}
      ],
      "meshes":[{"primitives":[{"attributes":{"POSITION":0,"NORMAL":1}}]}],
      "nodes":[
        {"name":"root","translation":[10,20,30],"scale":[-2,3,4],"mesh":0,"children":[1,2]},
        {"name":"head","translation":[1,2,3],"mesh":0,"children":[3]},
        {"name":"body","translation":[100,0,0],"mesh":0},
        {"name":"hat","mesh":0}
      ],
      "scenes":[{"nodes":[0]}],"scene":0
    }"#;
    let values = [
        0., 0., 0., 1., 0., 0., 0., 1., 0., 0., 0., 1., 0., 0., 1., 0., 0., 1.,
    ];
    let fixture = Fixture::new(source, &values);
    let bytes = fixture.embedded();
    let head = |name: Option<&str>| name == Some("head");
    fn selecting<'a>(nodes: &'a (dyn Fn(Option<&str>) -> bool + Sync)) -> LoadOptions<'a> {
        LoadOptions {
            nodes: Some(nodes),
            ..LoadOptions::default()
        }
    }
    let part = load_slice_with_options(&bytes, selecting(&head)).unwrap();
    let mesh = &part.meshes[0];
    assert_eq!(mesh.indices.len(), 3);
    for (v, expected) in mesh
        .vertices
        .iter()
        .zip([[8., 26., 42.], [6., 26., 42.], [8., 29., 42.]])
    {
        assert!((Vec3::from(v.position) - Vec3::from(expected)).length() < 1e-5);
    }
    let triangle: Vec<_> = mesh
        .indices
        .iter()
        .map(|i| Vec3::from(mesh.vertices[*i as usize].position))
        .collect();
    assert!(
        (triangle[1] - triangle[0])
            .cross(triangle[2] - triangle[0])
            .z
            > 0.
    );
    // Full embedded loading includes the ancestor and unselected descendants.
    assert_eq!(
        load_slice(&bytes)
            .unwrap()
            .meshes
            .iter()
            .map(|m| m.indices.len())
            .sum::<usize>(),
        12
    );
    assert!(load_slice_with_options(&bytes, selecting(&|_| false)).is_err());
    // A file selects as its bytes do.
    let file = load_with_options(&fixture.path(), selecting(&head)).unwrap();
    assert_eq!(file.meshes.len(), 1);
    assert_eq!(file.meshes[0].indices.len(), 3);
}

// Defects: the loader reads or decodes an image the game supplies, puts a
// supplied or decoded image at another index than the materials address,
// tells the game the wrong image (a data URI is embedded, not a file), or
// cannot decode a data URI from bytes (#107).
// Oracle: an image whose file does not exist loads only while supplied, and
// a written PNG's and an embedded one's known texels.
#[test]
fn supplied_images_are_never_read_and_the_rest_decode() {
    let source = br#"{
      "asset":{"version":"2.0"},
      "buffers":[{"uri":"fixture.bin","byteLength":96}],
      "bufferViews":[
        {"buffer":0,"byteLength":36},
        {"buffer":0,"byteOffset":36,"byteLength":36},
        {"buffer":0,"byteOffset":72,"byteLength":24}
      ],
      "accessors":[
        {"bufferView":0,"componentType":5126,"count":3,"type":"VEC3","min":[0,0,0],"max":[1,1,0]},
        {"bufferView":1,"componentType":5126,"count":3,"type":"VEC3"},
        {"bufferView":2,"componentType":5126,"count":3,"type":"VEC2"}
      ],
      "images":[
        {"uri":"missing.png","name":"albedo"},
        {"uri":"glow.png"},
        {"uri":"data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGNwC4gCAAHQAPElUIcnAAAAAElFTkSuQmCC"}
      ],
      "textures":[{"source":0},{"source":1},{"source":2}],
      "materials":[{
        "pbrMetallicRoughness":{"baseColorTexture":{"index":0}},
        "emissiveTexture":{"index":1},"emissiveFactor":[1,1,1],
        "normalTexture":{"index":2}
      }],
      "meshes":[{"primitives":[{"attributes":{"POSITION":0,"NORMAL":1,"TEXCOORD_0":2},"material":0}]}],
      "nodes":[{"mesh":0}],"scenes":[{"nodes":[0]}],"scene":0
    }"#;
    let values = [
        0., 0., 0., 1., 0., 0., 0., 1., 0., 0., 0., 1., 0., 0., 1., 0., 0., 1., 0., 0., 1., 0., 0.,
        1.,
    ];
    let fixture = Fixture::new(source, &values);
    image::RgbImage::from_raw(2, 1, vec![10, 20, 30, 40, 50, 60])
        .unwrap()
        .save(fixture.directory.join("glow.png"))
        .unwrap();
    let supplied = image::RgbaImage::from_raw(1, 1, vec![1, 2, 3, 4]).unwrap();
    let seen = std::sync::Mutex::new(Vec::new());
    let sources = |image: GltfImage<'_>| -> Result<ImageSource> {
        seen.lock().unwrap().push((
            image.index,
            image.name.map(String::from),
            image.uri.map(String::from),
        ));
        Ok(match image.uri {
            Some("missing.png") => ImageSource::Supplied(Image::Rgba8(supplied.clone())),
            _ => ImageSource::Decode,
        })
    };
    let texels = |image: &Image| match image {
        Image::Rgba8(image) => image.as_raw().clone(),
        Image::Compressed(_) => panic!("expected RGBA8"),
    };
    assert!(
        load(&fixture.path()).is_err(),
        "decoding reads the missing image"
    );
    let asset = load_with_options(
        &fixture.path(),
        LoadOptions {
            images: Some(&sources),
            ..LoadOptions::default()
        },
    )
    .unwrap();
    assert_eq!(texels(&asset.images[0]), [1, 2, 3, 4]);
    assert_eq!(texels(&asset.images[1]), [10, 20, 30, 255, 40, 50, 60, 255]);
    assert_eq!(texels(&asset.images[2]), [70, 80, 90, 255]);
    assert_eq!(asset.materials[0].base_texture, Some(0));
    assert_eq!(asset.materials[0].emissive_texture, Some(1));
    assert_eq!(
        *seen.lock().unwrap(),
        [
            (0, Some("albedo".into()), Some("missing.png".into())),
            (1, None, Some("glow.png".into())),
            (2, None, None),
        ]
    );
    // Bytes resolve no external file: supplying the files loads them, and
    // the data URI decodes as the file's did.
    let supply_files = |image: GltfImage<'_>| -> Result<ImageSource> {
        Ok(match image.uri {
            Some(_) => ImageSource::Supplied(Image::Rgba8(supplied.clone())),
            None => ImageSource::Decode,
        })
    };
    let embedded = load_slice_with_options(
        &fixture.embedded(),
        LoadOptions {
            images: Some(&supply_files),
            ..LoadOptions::default()
        },
    )
    .unwrap();
    assert_eq!(texels(&embedded.images[2]), [70, 80, 90, 255]);
    assert!(load_slice(&fixture.embedded()).is_err());
}

// Defect: a GLB header declaring a total length below its own 12 bytes
// underflowed the gltf crate's unchecked subtraction, a panic under
// overflow checks, instead of failing the load. The oracle is the GLB
// layout (glTF 2.0 4.4.2): magic "glTF", version 2, then that length.
#[test]
fn a_glb_header_shorter_than_itself_fails_the_load() {
    assert!(load_slice(b"glTF\x02\0\0\0\0\0\0\0").is_err());
}

/// A triangle drawn with a material of `extensions`, in a document that
/// lists `used` and `required` and holds a light, as `KHR_lights_punctual`
/// adds one where a file uses it.
fn extension_fixture(extensions: serde_json::Value, used: &[&str], required: &[&str]) -> Fixture {
    let document = serde_json::json!({
        "asset": {"version": "2.0"},
        "extensionsUsed": used,
        "extensionsRequired": required,
        "extensions": {"KHR_lights_punctual": {"lights": [{"type": "point"}]}},
        "buffers": [{"uri": "fixture.bin", "byteLength": 72}],
        "bufferViews": [{"buffer": 0, "byteLength": 36}, {"buffer": 0, "byteOffset": 36, "byteLength": 36}],
        "accessors": [
            {"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3", "min": [0,0,0], "max": [1,1,0]},
            {"bufferView": 1, "componentType": 5126, "count": 3, "type": "VEC3"}
        ],
        "materials": [{"extensions": extensions}],
        "meshes": [{"primitives": [{"attributes": {"POSITION": 0, "NORMAL": 1}, "material": 0}]}],
        "nodes": [{"mesh": 0, "extensions": {"KHR_lights_punctual": {"light": 0}}}],
        "scenes": [{"nodes": [0]}], "scene": 0
    });
    let values = [
        0., 0., 0., 1., 0., 0., 0., 1., 0., 0., 0., 1., 0., 0., 1., 0., 0., 1.,
    ];
    Fixture::new(&serde_json::to_vec(&document).unwrap(), &values)
}

// Defects: the loader refuses a file for an extension it only lists as
// used, drops one without listing it in `Asset::ignored`, loads a file that
// requires an extension SGL3D does not support, refuses one that requires a
// material extension SGL3D supports but the gltf crate's validation does
// not know (KHR_materials_ior and KHR_materials_specular), or misreads
// their factors. The oracle is glTF 2.0 5.17 (a loader may ignore an
// extension a file uses without requiring it; it must not load a file that
// requires one it does not support), KHR_materials_ior (an ior of 0 stands
// for an infinite one) and the authored values.
#[test]
fn used_extensions_are_listed_and_required_ones_honoured_or_refused() {
    let material = serde_json::json!({
        "KHR_materials_ior": {"ior": 1.33},
        "KHR_materials_specular": {"specularFactor": 0.5, "specularColorFactor": [1.0, 0.5, 2.0]}
    });
    let ours = ["KHR_materials_ior", "KHR_materials_specular"];
    let used = [&ours[..], &["KHR_lights_punctual"]].concat();
    let asset = load(&extension_fixture(material.clone(), &used, &[]).path()).unwrap();
    assert_eq!(
        asset.ignored,
        [Ignored::Extension("KHR_lights_punctual".into())]
    );
    let loaded = &asset.materials[0];
    assert_eq!(
        (loaded.ior, loaded.specular, loaded.specular_color),
        (1.33, 0.5, [1.0, 0.5, 2.0])
    );
    let asset = load(&extension_fixture(material.clone(), &used, &ours).path()).unwrap();
    assert_eq!(asset.materials[0].ior, 1.33, "required, and supported");
    let refused = load(&extension_fixture(material, &used, &["KHR_lights_punctual"]).path());
    assert!(
        refused
            .err()
            .is_some_and(|error| error.to_string().contains("KHR_lights_punctual")),
        "a file that requires an unsupported extension loaded"
    );
    let infinite = serde_json::json!({"KHR_materials_ior": {"ior": 0}});
    let asset = load(&extension_fixture(infinite, &ours[..1], &[]).path()).unwrap();
    assert_eq!(asset.materials[0].ior, f32::INFINITY);
}

// Defects: an occlusion map packed in the metallic-roughness image (the
// same image, through another texture) is taken for an image of its own,
// or one in an image of its own, on another UV set or a specular texture is
// left out without being listed in `Asset::ignored`; or its strength is
// lost; or one SGL3D leaves out still constrains the material's wrapping.
// The oracle is glTF 2.0's texture -> image indirection and the authored
// values.
#[test]
fn occlusion_and_specular_maps_sgl3d_does_not_sample_are_listed() {
    let source = |occlusion: serde_json::Value, specular: serde_json::Value| {
        serde_json::json!({
            "asset": {"version": "2.0"},
            "extensionsUsed": ["KHR_materials_specular"],
            "images": [{"uri": "orm.png"}, {"uri": "occlusion.png"}],
            "samplers": [{"wrapS": 33071, "wrapT": 33071}],
            "textures": [{"source": 0}, {"source": 0}, {"source": 1}, {"source": 0, "sampler": 0}],
            "materials": [{
                "pbrMetallicRoughness": {"metallicRoughnessTexture": {"index": 0}},
                "occlusionTexture": occlusion,
                "extensions": {"KHR_materials_specular": specular}
            }]
        })
    };
    let read = |document: serde_json::Value| {
        let document = gltf::Gltf::from_slice(&serde_json::to_vec(&document).unwrap())
            .unwrap()
            .document;
        let mut ignored = Vec::new();
        let material = read_material(
            document.materials().next().unwrap(),
            &document,
            0,
            &mut ignored,
        )
        .unwrap();
        (material, ignored)
    };
    let unlisted = serde_json::json!({});
    // Texture 1 is another texture of the metallic-roughness image.
    let (packed, ignored) = read(source(
        serde_json::json!({"index": 1, "strength": 0.6}),
        unlisted.clone(),
    ));
    assert_eq!(
        (packed.occlusion_texture, packed.occlusion_strength),
        (Some(0), 0.6)
    );
    assert!(packed.packed_occlusion());
    assert!(ignored.is_empty());
    let (separate, ignored) = read(source(serde_json::json!({"index": 2}), unlisted.clone()));
    assert_eq!(separate.occlusion_texture, Some(1));
    assert!(!separate.packed_occlusion());
    assert_eq!(ignored, [Ignored::OcclusionMap { material: 0 }]);
    // Texture 3 clamps the metallic-roughness image the material repeats.
    let (second_set, ignored) = read(source(
        serde_json::json!({"index": 3, "texCoord": 1}),
        unlisted,
    ));
    assert_eq!(second_set.occlusion_texture, None);
    assert_eq!(ignored, [Ignored::OcclusionMap { material: 0 }]);
    let (_, ignored) = read(source(
        serde_json::json!({"index": 0}),
        serde_json::json!({"specularColorTexture": {"index": 2}}),
    ));
    assert_eq!(ignored, [Ignored::SpecularMap { material: 0 }]);
}

// Defects: a whole file refused over an occlusion map SGL3D leaves out:
// its UV set's TEXCOORD_1 refused as an unsupported attribute (and not
// kept as the lightmap UV), or its nearest sampling refused although SGL3D
// never samples it; or, the other way, a map SGL3D samples loaded with
// sampling it does not support. The oracle is the documented fallback (a
// left-out map is listed in `Asset::ignored` and the rest loads) and the
// authored values.
#[test]
fn an_ignored_occlusion_map_constrains_neither_attributes_nor_sampling() {
    let uv0 = [0., 0., 1., 0., 0., 1.];
    let uv1 = [0.25, 0.5, 0.75, 0.5, 0.25, 0.75];
    let load_triangle = |occlusion: serde_json::Value, nearest: usize, lightmap_uv: bool| {
        let mut attributes = serde_json::json!({"POSITION": 0, "NORMAL": 1, "TEXCOORD_0": 2});
        if lightmap_uv {
            attributes["TEXCOORD_1"] = 3.into();
        }
        let mut document = serde_json::json!({
            "asset": {"version": "2.0"},
            "buffers": [{"uri": "fixture.bin", "byteLength": 120}],
            "bufferViews": [
                {"buffer": 0, "byteLength": 36}, {"buffer": 0, "byteOffset": 36, "byteLength": 36},
                {"buffer": 0, "byteOffset": 72, "byteLength": 24}, {"buffer": 0, "byteOffset": 96, "byteLength": 24}
            ],
            "accessors": [
                {"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3", "min": [0,0,0], "max": [1,1,0]},
                {"bufferView": 1, "componentType": 5126, "count": 3, "type": "VEC3"},
                {"bufferView": 2, "componentType": 5126, "count": 3, "type": "VEC2"},
                {"bufferView": 3, "componentType": 5126, "count": 3, "type": "VEC2"}
            ],
            "images": [{"uri": "orm.png"}, {"uri": "occlusion.png"}],
            "samplers": [{"magFilter": 9728, "minFilter": 9728}],
            "textures": [{"source": 0}, {"source": 1}],
            "materials": [{
                "pbrMetallicRoughness": {"metallicRoughnessTexture": {"index": 0}},
                "occlusionTexture": occlusion
            }],
            "meshes": [{"primitives": [{"attributes": attributes, "material": 0}]}],
            "nodes": [{"mesh": 0}],
            "scenes": [{"nodes": [0]}], "scene": 0
        });
        document["textures"][nearest]["sampler"] = 0.into();
        let values = [
            &[0., 0., 0., 1., 0., 0., 0., 1., 0.][..],
            &[0., 0., 1., 0., 0., 1., 0., 0., 1.],
            &uv0,
            &uv1,
        ]
        .concat();
        let fixture = Fixture::new(&serde_json::to_vec(&document).unwrap(), &values);
        let supplied = image::RgbaImage::new(1, 1);
        let supply = |_: GltfImage<'_>| Ok(ImageSource::Supplied(Image::Rgba8(supplied.clone())));
        load_with_options(
            &fixture.path(),
            LoadOptions {
                images: Some(&supply),
                ..Default::default()
            },
        )
    };
    // On TEXCOORD_1, which the mesh carries, and sampled nearest: its UVs
    // are the lightmap UVs.
    let asset = load_triangle(serde_json::json!({"index": 1, "texCoord": 1}), 1, true)
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(
        asset.ignored,
        [
            Ignored::OcclusionMap { material: 0 },
            Ignored::Image { image: 1 }
        ]
    );
    let vertices = &asset.meshes[0].vertices;
    let uvs: Vec<[f32; 2]> = vertices.iter().map(|vertex| vertex.uv).collect();
    let lightmap_uvs: Vec<[f32; 2]> = vertices.iter().map(|vertex| vertex.lightmap_uv).collect();
    assert_eq!(uvs.concat(), uv0);
    assert_eq!(lightmap_uvs.concat(), uv1);
    // An image of its own, sampled nearest, which SGL3D never samples.
    let asset = load_triangle(serde_json::json!({"index": 1}), 1, false)
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(
        asset.ignored,
        [
            Ignored::OcclusionMap { material: 0 },
            Ignored::Image { image: 1 }
        ]
    );
    assert_eq!(
        (
            asset.materials[0].mr_texture,
            asset.materials[0].occlusion_texture
        ),
        (Some(0), Some(1))
    );
    // The metallic-roughness map, which SGL3D samples, sampled nearest.
    let error = load_triangle(serde_json::json!({"index": 1}), 0, false)
        .err()
        .expect("a sampled map's nearest sampling was accepted")
        .to_string();
    assert!(error.contains("unsupported sampling"), "{error}");
}

// Defects: the loader misreads KHR_materials_transmission,
// KHR_materials_volume or KHR_materials_dispersion (a channel or texture
// swapped, a texture's image not resolved through its texture), takes
// other defaults than the extensions define, or loads a value they rule
// out. The oracle is the extensions' schemas (Khronos glTF acfcbe65:
// transmissionFactor in 0..1, default 0; thicknessFactor nonnegative,
// default 0; attenuationDistance positive, default infinity;
// attenuationColor in 0..1, default white; dispersion nonnegative, default
// 0) and the authored values.
#[test]
fn transmission_volume_and_dispersion_load_as_their_extensions_define() {
    let read = |extensions: serde_json::Value, unlit: bool| {
        let mut extensions = extensions;
        if unlit {
            extensions["KHR_materials_unlit"] = serde_json::json!({});
        }
        let document = serde_json::json!({
            "asset": {"version": "2.0"},
            "images": [{"uri": "transmission.png"}, {"uri": "thickness.png"}],
            "textures": [{"source": 0}, {"source": 1}, {"source": 1}],
            "materials": [{"extensions": extensions}]
        });
        let document = gltf::Gltf::from_slice(&serde_json::to_vec(&document).unwrap())
            .unwrap()
            .document;
        read_material(
            document.materials().next().unwrap(),
            &document,
            0,
            &mut Vec::new(),
        )
    };
    let authored = serde_json::json!({
        "KHR_materials_transmission": {"transmissionFactor": 0.8, "transmissionTexture": {"index": 0}},
        "KHR_materials_volume": {
            "thicknessFactor": 0.3,
            "thicknessTexture": {"index": 2},
            "attenuationDistance": 2.5,
            "attenuationColor": [0.9, 0.5, 0.2]
        },
        "KHR_materials_dispersion": {"dispersion": 0.33}
    });
    let material = read(authored, false).unwrap();
    assert_eq!(
        (
            material.transmission,
            material.transmission_texture,
            material.thickness,
            material.thickness_texture,
        ),
        (0.8, Some(0), 0.3, Some(1))
    );
    assert_eq!(
        (
            material.attenuation_distance,
            material.attenuation_color,
            material.dispersion
        ),
        (2.5, [0.9, 0.5, 0.2], 0.33)
    );
    // Each default, with the extensions present but empty, and absent.
    let defaults = serde_json::json!({
        "KHR_materials_transmission": {},
        "KHR_materials_volume": {},
        "KHR_materials_dispersion": {}
    });
    for extensions in [defaults, serde_json::json!({})] {
        let material = read(extensions.clone(), false).unwrap();
        assert_eq!(
            (
                material.transmission,
                material.thickness,
                material.attenuation_distance,
                material.attenuation_color,
                material.dispersion
            ),
            (0., 0., f32::INFINITY, [1.; 3], 0.),
            "{extensions}"
        );
    }
    for refused in [
        serde_json::json!({"KHR_materials_transmission": {"transmissionFactor": 1.5}}),
        serde_json::json!({"KHR_materials_volume": {"thicknessFactor": -0.1}}),
        serde_json::json!({"KHR_materials_volume": {"attenuationDistance": 0}}),
        serde_json::json!({"KHR_materials_volume": {"attenuationColor": [1.0, 1.2, 0.5]}}),
        serde_json::json!({"KHR_materials_volume": {"thicknessTexture": {"index": 1, "texCoord": 1}}}),
        serde_json::json!({"KHR_materials_dispersion": {"dispersion": -1}}),
        serde_json::json!({"KHR_materials_volume": {"thicknessScale": 1}}),
    ] {
        assert!(read(refused.clone(), false).is_err(), "{refused} loaded");
    }
    let unlit = serde_json::json!({"KHR_materials_transmission": {"transmissionFactor": 1.0}});
    assert!(
        read(unlit, true).is_err(),
        "an unlit material took transmission"
    );
}

/// `material` read from a document of images `a`, `b` and `c` and textures
/// 0, 1 and 2 of images `c`, `b` and `a`.
fn read_textured_material(material: serde_json::Value) -> Result<Material> {
    let source = serde_json::json!({
        "asset": {"version": "2.0"},
        "extensionsUsed": [
            "KHR_materials_clearcoat",
            "KHR_materials_diffuse_transmission",
            "KHR_materials_iridescence",
            "KHR_materials_sheen"
        ],
        "images": [{"uri": "a.png"}, {"uri": "b.png"}, {"uri": "c.png"}],
        "textures": [{"source": 2}, {"source": 1}, {"source": 0}],
        "materials": [material]
    });
    let document = gltf::Gltf::from_slice(&serde_json::to_vec(&source).unwrap())
        .unwrap()
        .document;
    read_material(
        document.materials().next().unwrap(),
        &document,
        0,
        &mut Vec::new(),
    )
}

// Defects: a bump map without bumpFactor read as factor 0, so it changed
// nothing, or a factor above 1 refused. The oracle is EXT_materials_bump's
// schema (KhronosGroup/glTF#2339) and three.js's GLTFMaterialsBumpExtension:
// bumpFactor defaults to 1, in 0..100.
#[test]
fn a_bump_factor_defaults_to_one_and_may_exceed_one() {
    let bump = |extension: serde_json::Value| {
        read_textured_material(serde_json::json!({
            "extensions": {"EXT_materials_bump": extension}
        }))
    };
    let plain = bump(serde_json::json!({"bumpTexture": {"index": 0}})).unwrap();
    assert_eq!((plain.bump_texture, plain.bump_scale), (Some(2), 1.));
    let strong = bump(serde_json::json!({"bumpTexture": {"index": 0}, "bumpFactor": 2.0})).unwrap();
    assert_eq!(strong.bump_scale, 2.);
    for refused in [-1.0, 101.0] {
        assert!(
            bump(serde_json::json!({"bumpTexture": {"index": 0}, "bumpFactor": refused})).is_err(),
            "bumpFactor {refused} loaded"
        );
    }
    // glTF textureInfo's texCoord is an integer; one that is not cannot
    // stand for TEXCOORD_0.
    assert!(
        bump(serde_json::json!({"bumpTexture": {"index": 0, "texCoord": "1"}})).is_err(),
        "a non-integer bump texCoord loaded"
    );
}

// Defect: a core factor outside glTF's bounds loaded, to fail later in
// `Scene::add_materials` with an error naming neither file nor material.
// The oracle is glTF 2.0's material schema (baseColorFactor in 0..1,
// emissiveFactor at least 0) and KHR_materials_emissive_strength's
// (emissiveStrength at least 0).
#[test]
fn core_factors_outside_gltfs_bounds_fail_the_load() {
    for material in [
        serde_json::json!({"extensions": {"KHR_materials_emissive_strength": {"emissiveStrength": -2}}}),
        serde_json::json!({"emissiveFactor": [-1, 0, 0]}),
        serde_json::json!({"pbrMetallicRoughness": {"baseColorFactor": [2, 1, 1, 1]}}),
    ] {
        let label = material.to_string();
        assert!(read_textured_material(material).is_err(), "{label} loaded");
    }
}

// Defects: a clearcoat or iridescence texture's index taken for its image
// (glTF's texture -> image indirection lost), or one map's texture read for
// another's; a KHR default misread; the clearcoat normal map's scale lost;
// or a map off TEXCOORD_0, an unknown property or a value outside the
// schema accepted. The oracle is the extensions' schemas: clearcoatFactor
// and clearcoatRoughnessFactor in 0..1, default 0, clearcoatNormalTexture's
// scale default 1; iridescenceFactor in 0..1, default 0, iridescenceIor at
// least 1, default 1.3, iridescenceThicknessMinimum and Maximum at least 0,
// default 100 and 400 nm, a minimum above the maximum allowed.
#[test]
fn clearcoat_maps_and_iridescence_load_with_their_defaults() {
    let extensions = |clearcoat: serde_json::Value, iridescence: serde_json::Value| {
        serde_json::json!({"extensions": {
            "KHR_materials_clearcoat": clearcoat,
            "KHR_materials_iridescence": iridescence
        }})
    };
    let empty = serde_json::json!({});
    let plain = read_textured_material(extensions(empty.clone(), empty.clone())).unwrap();
    assert_eq!(
        (
            plain.clearcoat,
            plain.coat_roughness,
            plain.clearcoat_texture,
            plain.coat_roughness_texture,
            plain.coat_normal_texture,
            plain.coat_normal_scale
        ),
        (0., 0., None, None, None, 1.)
    );
    assert_eq!(
        (
            plain.iridescence,
            plain.iridescence_ior,
            plain.iridescence_thickness,
            plain.iridescence_texture,
            plain.iridescence_thickness_texture
        ),
        (0., 1.3, [100., 400.], None, None)
    );
    let authored = read_textured_material(extensions(
        serde_json::json!({
            "clearcoatFactor": 0.5, "clearcoatRoughnessFactor": 0.25,
            "clearcoatTexture": {"index": 0}, "clearcoatRoughnessTexture": {"index": 1},
            "clearcoatNormalTexture": {"index": 2, "scale": 0.5}
        }),
        serde_json::json!({
            "iridescenceFactor": 0.75, "iridescenceIor": 1.8,
            "iridescenceThicknessMinimum": 500, "iridescenceThicknessMaximum": 50,
            "iridescenceTexture": {"index": 1}, "iridescenceThicknessTexture": {"index": 0}
        }),
    ))
    .unwrap();
    assert_eq!(
        (
            authored.clearcoat,
            authored.coat_roughness,
            authored.clearcoat_texture,
            authored.coat_roughness_texture,
            authored.coat_normal_texture,
            authored.coat_normal_scale
        ),
        (0.5, 0.25, Some(2), Some(1), Some(0), 0.5)
    );
    assert_eq!(
        (
            authored.iridescence,
            authored.iridescence_ior,
            authored.iridescence_thickness,
            authored.iridescence_texture,
            authored.iridescence_thickness_texture
        ),
        (0.75, 1.8, [500., 50.], Some(1), Some(2))
    );
    for (clearcoat, iridescence) in [
        (serde_json::json!({"clearcoatFactor": 1.5}), empty.clone()),
        (
            serde_json::json!({"clearcoatTexture": {"index": 0, "texCoord": 1}}),
            empty.clone(),
        ),
        (
            serde_json::json!({"clearcoatNormalTexture": {"index": 3}}),
            empty.clone(),
        ),
        (
            serde_json::json!({"clearcoatNormalTexture": {"index": 0, "scale": "half"}}),
            empty.clone(),
        ),
        (serde_json::json!({"clearcoatTint": 1}), empty.clone()),
        (empty.clone(), serde_json::json!({"iridescenceFactor": 1.5})),
        (empty.clone(), serde_json::json!({"iridescenceIor": 0.9})),
        (
            empty.clone(),
            serde_json::json!({"iridescenceThicknessMinimum": -1}),
        ),
        (
            empty.clone(),
            serde_json::json!({"iridescenceThicknessTexture": {"index": 0, "texCoord": 1}}),
        ),
        (empty.clone(), serde_json::json!({"iridescenceSpread": 1})),
    ] {
        let label = format!("{clearcoat} {iridescence}");
        assert!(
            read_textured_material(extensions(clearcoat, iridescence)).is_err(),
            "{label} loaded"
        );
    }
}

// Defects: a sheen or diffuse transmission texture's index taken for its
// image, or one map's texture read for another's; a KHR default misread
// (the transmission colour white, the rest 0); or a map off TEXCOORD_0, an
// unknown property, a value outside the schemas or either extension beside
// KHR_materials_unlit accepted. The oracle is the extensions' schemas:
// sheenColorFactor three numbers in 0..1, default black; sheenRoughnessFactor
// in 0..1, default 0; diffuseTransmissionFactor in 0..1, default 0;
// diffuseTransmissionColorFactor three numbers of at least 0, default white;
// and KHR_materials_unlit, which takes the place of every lit model.
#[test]
fn sheen_and_diffuse_transmission_load_with_their_defaults() {
    let extensions = |sheen: serde_json::Value, transmission: serde_json::Value| {
        serde_json::json!({"extensions": {
            "KHR_materials_sheen": sheen,
            "KHR_materials_diffuse_transmission": transmission
        }})
    };
    let empty = serde_json::json!({});
    let plain = read_textured_material(extensions(empty.clone(), empty.clone())).unwrap();
    assert_eq!(
        (
            plain.sheen_color,
            plain.sheen_roughness,
            plain.sheen_color_texture,
            plain.sheen_roughness_texture
        ),
        ([0.; 3], 0., None, None)
    );
    assert_eq!(
        (
            plain.diffuse_transmission,
            plain.diffuse_transmission_color,
            plain.diffuse_transmission_texture,
            plain.diffuse_transmission_color_texture
        ),
        (0., [1.; 3], None, None)
    );
    let authored = read_textured_material(extensions(
        serde_json::json!({
            "sheenColorFactor": [0.25, 0.5, 0.75], "sheenRoughnessFactor": 0.5,
            "sheenColorTexture": {"index": 0}, "sheenRoughnessTexture": {"index": 1}
        }),
        serde_json::json!({
            "diffuseTransmissionFactor": 0.75, "diffuseTransmissionColorFactor": [1.5, 0.5, 0.],
            "diffuseTransmissionTexture": {"index": 2}, "diffuseTransmissionColorTexture": {"index": 1}
        }),
    ))
    .unwrap();
    assert_eq!(
        (
            authored.sheen_color,
            authored.sheen_roughness,
            authored.sheen_color_texture,
            authored.sheen_roughness_texture
        ),
        ([0.25, 0.5, 0.75], 0.5, Some(2), Some(1))
    );
    assert_eq!(
        (
            authored.diffuse_transmission,
            authored.diffuse_transmission_color,
            authored.diffuse_transmission_texture,
            authored.diffuse_transmission_color_texture
        ),
        (0.75, [1.5, 0.5, 0.], Some(0), Some(1))
    );
    for (sheen, transmission) in [
        (
            serde_json::json!({"sheenColorFactor": [1.5, 0., 0.]}),
            empty.clone(),
        ),
        (
            serde_json::json!({"sheenColorFactor": [0.5, 0.5]}),
            empty.clone(),
        ),
        (
            serde_json::json!({"sheenRoughnessFactor": -0.5}),
            empty.clone(),
        ),
        (
            serde_json::json!({"sheenColorTexture": {"index": 0, "texCoord": 1}}),
            empty.clone(),
        ),
        (serde_json::json!({"sheenTint": 1}), empty.clone()),
        (
            empty.clone(),
            serde_json::json!({"diffuseTransmissionFactor": 1.5}),
        ),
        (
            empty.clone(),
            serde_json::json!({"diffuseTransmissionColorFactor": [-0.5, 0., 0.]}),
        ),
        (
            empty.clone(),
            serde_json::json!({"diffuseTransmissionTexture": {"index": 0, "texCoord": 1}}),
        ),
        (
            empty.clone(),
            serde_json::json!({"diffuseTransmissionSpread": 1}),
        ),
    ] {
        let label = format!("{sheen} {transmission}");
        assert!(
            read_textured_material(extensions(sheen, transmission)).is_err(),
            "{label} loaded"
        );
    }
    for unlit in [
        serde_json::json!({"KHR_materials_unlit": {}, "KHR_materials_sheen": {}}),
        serde_json::json!({"KHR_materials_unlit": {}, "KHR_materials_diffuse_transmission": {}}),
    ] {
        assert!(
            read_textured_material(serde_json::json!({ "extensions": unlit })).is_err(),
            "{unlit} loaded"
        );
    }
}

// Defects: a material extension object carrying its own `extensions` (or
// `extras`) is refused, or its authored values are lost. The oracle is
// glTF 2.0's glTFProperty schema, from which every KHR material extension's
// schema derives, so each may hold both; and the authored values.
#[test]
fn material_extensions_with_their_own_extensions_load() {
    let nested = |mut lobe: serde_json::Value| {
        lobe["extensions"] = serde_json::json!({"EXT_vendor_note": {"note": 1}});
        lobe["extras"] = serde_json::json!({"author": "fixture"});
        lobe
    };
    let material = read_textured_material(serde_json::json!({"extensions": {
        "KHR_materials_anisotropy": nested(serde_json::json!({"anisotropyStrength": 0.5})),
        "KHR_materials_clearcoat": nested(serde_json::json!({"clearcoatFactor": 0.25})),
        "KHR_materials_iridescence": nested(serde_json::json!({"iridescenceFactor": 0.75})),
        "KHR_materials_sheen": nested(serde_json::json!({"sheenRoughnessFactor": 0.5})),
        "KHR_materials_diffuse_transmission": nested(serde_json::json!({"diffuseTransmissionFactor": 0.25})),
        "KHR_materials_transmission": nested(serde_json::json!({"transmissionFactor": 0.75})),
        "KHR_materials_volume": nested(serde_json::json!({"thicknessFactor": 0.5})),
        "KHR_materials_dispersion": nested(serde_json::json!({"dispersion": 0.25}))
    }}))
    .unwrap();
    assert_eq!(
        (
            material.anisotropy_strength,
            material.clearcoat,
            material.iridescence,
            material.sheen_roughness,
        ),
        (0.5, 0.25, 0.75, 0.5)
    );
    assert_eq!(
        (
            material.diffuse_transmission,
            material.transmission,
            material.thickness,
            material.dispersion,
        ),
        (0.25, 0.75, 0.5, 0.25)
    );
}

// Defects: a primitive without TEXCOORD_0 loads though its material samples
// a map, which then reads one texel everywhere (the check covered the base,
// metallic-roughness, occlusion and anisotropy maps alone). The oracle is
// glTF's textureInfo: every map SGL3D samples lies on TEXCOORD_0, so a
// primitive whose material has any map needs it; one with none loads.
#[test]
fn a_primitive_without_texcoord_0_is_refused_for_any_map() {
    // A 1×1 PNG, embedded, so the image decodes without a file.
    let image = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGNwC4gCAAHQAPElUIcnAAAAAElFTkSuQmCC";
    let texture = serde_json::json!({"index": 0});
    let maps = [
        serde_json::json!({"emissiveTexture": texture}),
        serde_json::json!({"normalTexture": texture}),
        serde_json::json!({"extensions": {"EXT_materials_bump": {"bumpTexture": texture}}}),
        serde_json::json!({"extensions": {"KHR_materials_clearcoat": {"clearcoatNormalTexture": texture}}}),
        serde_json::json!({"extensions": {"KHR_materials_iridescence": {"iridescenceThicknessTexture": texture}}}),
        serde_json::json!({"extensions": {"KHR_materials_sheen": {"sheenRoughnessTexture": texture}}}),
        serde_json::json!({"extensions": {"KHR_materials_diffuse_transmission": {"diffuseTransmissionColorTexture": texture}}}),
    ];
    let used = [
        "EXT_materials_bump",
        "KHR_materials_clearcoat",
        "KHR_materials_diffuse_transmission",
        "KHR_materials_iridescence",
        "KHR_materials_sheen",
        "KHR_lights_punctual",
    ];
    for material in maps.into_iter().chain([serde_json::json!({})]) {
        let fixture = extension_fixture(serde_json::json!({}), &used, &[]);
        let mut source: serde_json::Value =
            serde_json::from_slice(&std::fs::read(fixture.path()).unwrap()).unwrap();
        source["images"] = serde_json::json!([{"uri": image}]);
        source["textures"] = serde_json::json!([{"source": 0}]);
        source["materials"][0] = material.clone();
        std::fs::write(fixture.path(), serde_json::to_vec(&source).unwrap()).unwrap();
        let loaded = load(&fixture.path());
        if material == serde_json::json!({}) {
            assert!(
                loaded.is_ok(),
                "an untextured primitive: {:?}",
                loaded.err()
            );
        } else {
            assert!(
                loaded
                    .err()
                    .is_some_and(|error| error.to_string().contains("TEXCOORD_0")),
                "{material} loaded without TEXCOORD_0"
            );
        }
    }
}

/// A UV-mapped triangle drawn with `material`, in a document of `images`
/// and `textures` that lists `used`.
fn textured_triangle(
    material: serde_json::Value,
    images: serde_json::Value,
    textures: serde_json::Value,
    used: &[&str],
) -> Fixture {
    let document = serde_json::json!({
        "asset": {"version": "2.0"},
        "extensionsUsed": used,
        "buffers": [{"uri": "fixture.bin", "byteLength": 96}],
        "bufferViews": [
            {"buffer": 0, "byteLength": 36},
            {"buffer": 0, "byteOffset": 36, "byteLength": 36},
            {"buffer": 0, "byteOffset": 72, "byteLength": 24}
        ],
        "accessors": [
            {"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3", "min": [0,0,0], "max": [1,1,0]},
            {"bufferView": 1, "componentType": 5126, "count": 3, "type": "VEC3"},
            {"bufferView": 2, "componentType": 5126, "count": 3, "type": "VEC2"}
        ],
        "images": images,
        "textures": textures,
        "materials": [material],
        "meshes": [{"primitives": [{"attributes": {"POSITION": 0, "NORMAL": 1, "TEXCOORD_0": 2}, "material": 0}]}],
        "nodes": [{"mesh": 0}],
        "scenes": [{"nodes": [0]}], "scene": 0
    });
    let values = [
        0., 0., 0., 1., 0., 0., 0., 1., 0., 0., 0., 1., 0., 0., 1., 0., 0., 1., 0., 0., 1., 0., 0.,
        1.,
    ];
    Fixture::new(&serde_json::to_vec(&document).unwrap(), &values)
}

// Defects: an image only an unsupported extension's texture source names
// is decoded, failing the load over a KTX2 image a loader without
// KHR_texture_basisu never needs; or the core source it falls back to is
// not decoded; or the skipped image is not listed. The oracle is glTF 2.0
// 5.17.1 and KHR_texture_basisu (a client without the extension that the
// file does not require uses the texture's core `source`), and the
// embedded PNG's known texel.
#[test]
fn an_unsupported_texture_extensions_image_is_left_out_and_its_fallback_decodes() {
    let png = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGNwC4gCAAHQAPElUIcnAAAAAElFTkSuQmCC";
    // A KTX2 file identifier and nothing more, which no loader decodes.
    let ktx2 = "data:image/ktx2;base64,q0tUWCAyMLsNChoK";
    let fixture = textured_triangle(
        serde_json::json!({"pbrMetallicRoughness": {"baseColorTexture": {"index": 0}}}),
        serde_json::json!([{"uri": png}, {"uri": ktx2}]),
        serde_json::json!([{"source": 0, "extensions": {"KHR_texture_basisu": {"source": 1}}}]),
        &["KHR_texture_basisu"],
    );
    let asset = load_slice(&fixture.embedded()).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(asset.materials[0].base_texture, Some(0));
    let Image::Rgba8(base) = &asset.images[0] else {
        panic!("expected RGBA8");
    };
    assert_eq!(base.as_raw(), &[70, 80, 90, 255]);
    assert_eq!(
        asset.ignored,
        [
            Ignored::Extension("KHR_texture_basisu".into()),
            Ignored::Image { image: 1 }
        ]
    );
}

// Defects: a 16-bit PNG, which SGL3D does not decode, fails the load though
// only a map SGL3D leaves out (an occlusion map in an image of its own, a
// KHR_materials_specular texture) uses it, or the game is asked for it; or,
// the other way, the same image loads unnoticed where a map SGL3D samples
// uses it. The oracle is the documented fallback (a left-out map constrains
// nothing and is listed in `Asset::ignored`) and the error for an image in
// effect that cannot decode.
#[test]
fn an_image_only_ignored_maps_use_is_not_decoded() {
    let images = serde_json::json!([{"uri": "orm.png"}, {"uri": "deep.png"}]);
    let textures = serde_json::json!([{"source": 0}, {"source": 1}]);
    let load_material = |material: serde_json::Value| {
        let fixture = textured_triangle(
            material,
            images.clone(),
            textures.clone(),
            &["KHR_materials_specular"],
        );
        image::RgbImage::new(1, 1)
            .save(fixture.directory.join("orm.png"))
            .unwrap();
        image::ImageBuffer::<image::Rgb<u16>, _>::from_raw(1, 1, vec![1000u16, 2000, 3000])
            .unwrap()
            .save(fixture.directory.join("deep.png"))
            .unwrap();
        let asked = std::sync::Mutex::new(Vec::new());
        let ask = |image: GltfImage<'_>| {
            asked.lock().unwrap().push(image.index);
            Ok(ImageSource::Decode)
        };
        let options = LoadOptions {
            images: Some(&ask),
            ..LoadOptions::default()
        };
        let asset = load_with_options(&fixture.path(), options);
        (asset, asked.into_inner().unwrap())
    };
    let mr = serde_json::json!({"metallicRoughnessTexture": {"index": 0}});
    let (asset, asked) = load_material(serde_json::json!({
        "pbrMetallicRoughness": mr,
        "occlusionTexture": {"index": 1}
    }));
    let asset = asset.unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(
        asset.ignored,
        [
            Ignored::OcclusionMap { material: 0 },
            Ignored::Image { image: 1 }
        ]
    );
    assert_eq!(asked, [0]);
    let (asset, asked) = load_material(serde_json::json!({
        "pbrMetallicRoughness": mr,
        "extensions": {"KHR_materials_specular": {"specularTexture": {"index": 1}}}
    }));
    let asset = asset.unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(
        asset.ignored,
        [
            Ignored::SpecularMap { material: 0 },
            Ignored::Image { image: 1 }
        ]
    );
    assert_eq!(asked, [0]);
    let (asset, _) = load_material(serde_json::json!({
        "pbrMetallicRoughness": {"baseColorTexture": {"index": 1}}
    }));
    let error = asset
        .err()
        .expect("a sampled 16-bit image loaded")
        .to_string();
    assert!(
        error.contains("image 1") && error.contains("unsupported Rgb16"),
        "{error}"
    );
}

// Defect: a compressed image header reaches pixel decoding before the fixed
// texture-side limit is checked. These headers are valid but the 1x1 payload
// cannot supply their declared pixels: the limit diagnostic must win over a
// truncated-pixels diagnostic, without constructing a giant decoded fixture.
#[test]
fn oversized_png_and_jpeg_headers_fail_before_pixels_with_image_index() {
    use image::ImageEncoder;
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(&[0], 1, 1, image::ExtendedColorType::L8)
        .unwrap();
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new(&mut jpeg)
        .encode(&[0, 0, 0], 1, 1, image::ExtendedColorType::Rgb8)
        .unwrap();
    // Baseline JPEG SOF0: precision, big-endian height and width.
    let sof = jpeg
        .windows(2)
        .position(|bytes| bytes == [0xff, 0xc0])
        .unwrap();
    for [width, height] in [[20000u32, 1], [1, 20000], [20000, 20000]] {
        let mut png = png.clone();
        // PNG IHDR dimensions, followed by its IEEE CRC32 (PNG section 5.5).
        png[16..20].copy_from_slice(&width.to_be_bytes());
        png[20..24].copy_from_slice(&height.to_be_bytes());
        let mut crc = !0u32;
        for &byte in &png[12..29] {
            crc ^= u32::from(byte);
            for _ in 0..8 {
                crc = (crc >> 1) ^ (0xedb88320 & 0u32.wrapping_sub(crc & 1));
            }
        }
        png[29..33].copy_from_slice(&(!crc).to_be_bytes());
        let mut jpeg = jpeg.clone();
        jpeg[sof + 5..sof + 7].copy_from_slice(&(height as u16).to_be_bytes());
        jpeg[sof + 7..sof + 9].copy_from_slice(&(width as u16).to_be_bytes());
        for (name, bytes) in [("large.png", png), ("large.jpg", jpeg)] {
            let fixture = textured_triangle(
                serde_json::json!({"pbrMetallicRoughness":{"baseColorTexture":{"index":0}}}),
                serde_json::json!([{"uri":name}]),
                serde_json::json!([{"source":0}]),
                &[],
            );
            std::fs::write(fixture.directory.join(name), bytes).unwrap();
            let error = load(&fixture.path()).err().unwrap().to_string();
            assert!(
                error.contains("image 0") && error.contains("16384"),
                "{error}"
            );
        }
    }
}

// Defect: image bytes receive a fresh budget instead of sharing the scene's
// budget. Published layout: 46,684,425 unskinned vertices and implicit indices
// take 4,294,967,100 bytes, leaving 196 bytes of the native 4 GiB cap; this
// 64x1 image requires another 256 RGBA bytes. Its tiny mesh buffer cannot
// satisfy the accessor: the combined-budget error must occur before that
// accessor is read, so the fixture never needs a multi-gigabyte allocation.
#[test]
fn decoded_images_and_scene_data_share_the_load_budget() {
    let fixture = textured_triangle(
        serde_json::json!({"pbrMetallicRoughness":{"baseColorTexture":{"index":0}}}),
        serde_json::json!([{"uri":"budget.png"}]),
        serde_json::json!([{"source":0}]),
        &[],
    );
    image::GrayImage::new(64, 1)
        .save(fixture.directory.join("budget.png"))
        .unwrap();
    change_document(&fixture, |document| {
        document["accessors"][0]["count"] = 46_684_425.into();
    });
    let error = load(&fixture.path()).err().unwrap().to_string();
    assert!(
        error.contains("image 0") && error.contains("budget"),
        "{error}"
    );
}
