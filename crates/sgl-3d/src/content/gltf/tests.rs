//! The glTF loader against hand-built documents.
use super::*;
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

#[test]
fn anisotropic_loader_rejects_missing_or_degenerate_authored_frames() {
    let invalid = [
        None,
        Some([0., 0., 0., 1.]),
        Some([0., -1., 1., 1.]),
        Some([1., 0., 0., 0.]),
        Some([f32::NAN, 0., 0., 1.]),
    ];
    for tangent in invalid {
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
    let material = read_material(document.materials().next().unwrap(), &document).unwrap();
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
        serde_json::json!({"anisotropyTexture":{"index":0,"extensions":{"KHR_texture_transform":{"rotation":1.0}}}}),
    ] {
        source["materials"][0]["extensions"]["KHR_materials_anisotropy"] = extension;
        let document = gltf::Gltf::from_slice(&serde_json::to_vec(&source).unwrap())
            .unwrap()
            .document;
        assert!(read_material(document.materials().next().unwrap(), &document).is_err());
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
    source["extensionsRequired"] = serde_json::json!(["KHR_materials_transmission"]);
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
    let mut missing = asset.meshes[0].clone();
    missing.vertices[0].tangent = [0.; 4];
    asset.meshes.push(missing);
    asset.materials[0].anisotropy_strength = 0.7;
    assert!(matches!(
        scene.add_asset(&device, &queue, asset.clone()),
        Err(crate::SceneError::MissingAnisotropyTangents)
    ));
    asset.meshes.pop();
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
      "textures":[{"source":0},{"source":1}],
      "materials":[{
        "pbrMetallicRoughness":{"baseColorTexture":{"index":0}},
        "emissiveTexture":{"index":1},"emissiveFactor":[1,1,1]
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
