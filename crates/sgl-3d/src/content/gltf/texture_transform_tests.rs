//! Each glTF texture slot's UV set and `KHR_texture_transform`
//! (specs/sgl3d-architecture.md, Material map coordinates), as the reader
//! takes them. Authorities: glTF 2.0 3.9.2 (`textureInfo.texCoord`),
//! KhronosGroup/glTF 08e10b3 `KHR_texture_transform/README.md`, and the
//! Khronos TextureTransformTest asset, whose marker quads show where each
//! transform must put its arrow.
use super::material::read_material;
use super::*;
use crate::asset::{Material, MaterialTexture, UvSet, UvTransform};
use crate::shading::map_uvs::MapUv;
use std::path::Path;

/// The material `material`, of a document with three textures.
fn read(material: serde_json::Value) -> Result<Material> {
    let source = serde_json::json!({
        "asset": {"version": "2.0"},
        "extensionsUsed": ["KHR_texture_transform", "KHR_materials_clearcoat", "KHR_materials_sheen"],
        "images": [{"uri": "a.png"}, {"uri": "b.png"}, {"uri": "c.png"}],
        "textures": [{"source": 0}, {"source": 1}, {"source": 2}],
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

fn transform(value: serde_json::Value) -> serde_json::Value {
    serde_json::json!({"KHR_texture_transform": value})
}

// Plausible defects: a slot's texCoord dropped or shared between slots, the
// extension's texCoord not overriding the textureInfo's, a transform read
// from another slot or shared by the slots of one image, a default other
// than the extension's (no offset, no rotation, scale 1), or an extension
// textureInfo (clearcoat, sheen) losing what a core one keeps. The oracle
// is the authored values: three slots sample one texture, each its own way.
#[test]
fn each_slot_keeps_its_own_uv_set_and_transform() {
    let material = read(serde_json::json!({
        "pbrMetallicRoughness": {
            "baseColorTexture": {"index": 0, "texCoord": 1},
            "metallicRoughnessTexture": {
                "index": 0,
                "extensions": transform(serde_json::json!({
                    "offset": [0.25, -0.5], "rotation": 0.75, "scale": [2.0, -3.0]
                }))
            }
        },
        // texCoord 0 on the textureInfo, 1 from the extension, which wins.
        "normalTexture": {
            "index": 0, "texCoord": 0,
            "extensions": transform(serde_json::json!({"texCoord": 1, "scale": [4.0, 4.0]}))
        },
        // The reverse: the extension names the first set.
        "emissiveTexture": {
            "index": 1, "texCoord": 1,
            "extensions": transform(serde_json::json!({"texCoord": 0, "rotation": -1.5}))
        },
        "extensions": {
            "KHR_materials_clearcoat": {
                "clearcoatFactor": 1.0,
                "clearcoatTexture": {
                    "index": 2, "texCoord": 1,
                    "extensions": transform(serde_json::json!({"offset": [0.5, 0.5]}))
                }
            },
            "KHR_materials_sheen": {
                "sheenColorFactor": [1.0, 1.0, 1.0],
                "sheenColorTexture": {"index": 2}
            }
        }
    }))
    .unwrap_or_else(|error| panic!("{error}"));
    let slot = |texture: Option<MaterialTexture>| {
        let texture = texture.unwrap();
        (texture.image, texture.uv_set, texture.transform)
    };
    let identity = UvTransform::IDENTITY;
    assert_eq!(slot(material.base_texture), (0, UvSet::Uv1, identity));
    assert_eq!(
        slot(material.mr_texture),
        (
            0,
            UvSet::Uv0,
            UvTransform {
                offset: [0.25, -0.5],
                rotation: 0.75,
                scale: [2., -3.]
            }
        )
    );
    assert_eq!(
        slot(material.normal_texture),
        (
            0,
            UvSet::Uv1,
            UvTransform {
                scale: [4., 4.],
                ..identity
            }
        )
    );
    assert_eq!(
        slot(material.emissive_texture),
        (
            1,
            UvSet::Uv0,
            UvTransform {
                rotation: -1.5,
                ..identity
            }
        )
    );
    assert_eq!(
        slot(material.clearcoat_texture),
        (
            2,
            UvSet::Uv1,
            UvTransform {
                offset: [0.5, 0.5],
                ..identity
            }
        )
    );
    assert_eq!(
        slot(material.sheen_color_texture),
        (2, UvSet::Uv0, identity)
    );
}

// An extension object that changes nothing (empty, or its defaults written
// out) is the identity, so its material keeps a UV0-only material's
// pipelines: a plausible defect is taking the extension's presence for a
// transform. Oracle: the extension's defaults.
#[test]
fn an_extension_object_of_defaults_is_no_transform() {
    for object in [
        serde_json::json!({}),
        serde_json::json!({"offset": [0.0, 0.0], "rotation": 0.0, "scale": [1.0, 1.0], "texCoord": 0}),
    ] {
        let material = read(serde_json::json!({
            "pbrMetallicRoughness": {
                "baseColorTexture": {"index": 0, "extensions": transform(object)}
            }
        }))
        .unwrap();
        assert_eq!(material.base_texture, Some(MaterialTexture::new(0)));
    }
}

// The extension's values are numbers of fixed shape and SGL3D keeps two UV
// sets: a malformed value or a third set fails the load, naming the slot,
// rather than loading something else. An occlusion map on a third set is
// instead left out and listed (`tests.rs`).
#[test]
fn malformed_transforms_and_a_third_uv_set_are_refused() {
    for (info, names) in [
        (serde_json::json!({"index": 0, "texCoord": 2}), "TEXCOORD_2"),
        (
            serde_json::json!({"index": 0, "extensions": transform(serde_json::json!({"texCoord": 3}))}),
            "TEXCOORD_3",
        ),
        (
            serde_json::json!({"index": 0, "extensions": transform(serde_json::json!({"offset": [0.0]}))}),
            "offset",
        ),
        (
            serde_json::json!({"index": 0, "extensions": transform(serde_json::json!({"scale": [1.0, "2"]}))}),
            "scale",
        ),
        (
            serde_json::json!({"index": 0, "extensions": transform(serde_json::json!({"rotation": [1.0]}))}),
            "rotation",
        ),
        (
            serde_json::json!({"index": 0, "extensions": transform(serde_json::json!({"texCoord": -1}))}),
            "texCoord",
        ),
        (
            serde_json::json!({"index": 0, "extensions": transform(serde_json::json!([]))}),
            "KHR_texture_transform",
        ),
    ] {
        let error = read(serde_json::json!({"emissiveTexture": info.clone()}))
            .err()
            .unwrap_or_else(|| panic!("{info} loaded"))
            .to_string();
        assert!(
            error.contains("emissiveTexture") && error.contains(names),
            "{info}: {error}"
        );
    }
}

/// A triangle with both UV sets, drawn with `material`, loaded with its
/// images supplied: positions (0,0,0), (1,0,0), (0,1,0), `TEXCOORD_0`
/// (0,0), (1,0), (0,1) and `TEXCOORD_1` (0.25,0.5), (0.75,0.5),
/// (0.25,0.75). `second` keeps `TEXCOORD_1` in the primitive.
fn load_triangle(material: serde_json::Value, second: bool) -> Result<Asset> {
    let mut attributes = serde_json::json!({"POSITION": 0, "NORMAL": 1, "TEXCOORD_0": 2});
    if second {
        attributes["TEXCOORD_1"] = 3.into();
    }
    let document = serde_json::json!({
        "asset": {"version": "2.0"},
        "extensionsUsed": ["KHR_texture_transform"],
        "extensionsRequired": ["KHR_texture_transform"],
        "buffers": [{"byteLength": 120}],
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
        "images": [{"uri": "a.png"}],
        "textures": [{"source": 0}],
        "materials": [material],
        "meshes": [{"primitives": [{"attributes": attributes, "material": 0}]}],
        "nodes": [{"mesh": 0}],
        "scenes": [{"nodes": [0]}], "scene": 0
    });
    let values: [f32; 30] = [
        0., 0., 0., 1., 0., 0., 0., 1., 0., // positions
        0., 0., 1., 0., 0., 1., 0., 0., 1., // normals
        0., 0., 1., 0., 0., 1., // TEXCOORD_0
        0.25, 0.5, 0.75, 0.5, 0.25, 0.75, // TEXCOORD_1
    ];
    let supplied = image::RgbaImage::new(1, 1);
    let supply = |_: GltfImage<'_>| Ok(ImageSource::Supplied(Image::Rgba8(supplied.clone())));
    load_slice_with_options(
        &glb(&document, &bytes(&values)),
        LoadOptions {
            images: Some(&supply),
            ..Default::default()
        },
    )
}

/// `values`' little-endian bytes.
fn bytes(values: &[f32]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

/// A GLB of `document` and its one buffer, `bin`.
fn glb(document: &serde_json::Value, bin: &[u8]) -> Vec<u8> {
    let mut json = serde_json::to_vec(document).unwrap();
    while !json.len().is_multiple_of(4) {
        json.push(b' ');
    }
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

// Plausible defects: a file that requires the extension refused; the second
// set not reaching the vertices a map samples it on, or reaching every
// mesh (so content that only lightmaps on TEXCOORD_1 pays for a set no map
// reads); the lightmap UV changed by any of it; or a map on a set its
// primitive lacks loading with zeros. The oracle is the buffer's values.
#[test]
fn texcoord_1_fills_the_second_set_only_where_a_map_samples_it() {
    let second = [[0.25, 0.5], [0.75, 0.5], [0.25, 0.75]];
    let uvs = |asset: &Asset| -> Vec<([f32; 2], [f32; 2], [f32; 2])> {
        asset.meshes[0]
            .vertices
            .iter()
            .map(|vertex| (vertex.uv, vertex.uv1, vertex.lightmap_uv))
            .collect()
    };
    // The emission map on TEXCOORD_1: the second set is the buffer's, and
    // the lightmap UV still is too.
    let sampled = load_triangle(
        serde_json::json!({
            "pbrMetallicRoughness": {"baseColorTexture": {"index": 0}},
            "emissiveTexture": {"index": 0, "texCoord": 1}
        }),
        true,
    )
    .unwrap_or_else(|error| panic!("{error}"));
    let first = [[0., 0.], [1., 0.], [0., 1.]];
    assert_eq!(
        uvs(&sampled),
        [0, 1, 2].map(|i| (first[i], second[i], second[i]))
    );
    // No map on it: no second set, the lightmap UV as before.
    let unsampled = load_triangle(
        serde_json::json!({"pbrMetallicRoughness": {"baseColorTexture": {"index": 0}}}),
        true,
    )
    .unwrap();
    assert_eq!(
        uvs(&unsampled),
        [0, 1, 2].map(|i| (first[i], [0.; 2], second[i]))
    );
    // An occlusion map on TEXCOORD_1 that is not the metallic-roughness
    // map, which SGL3D leaves out, asks for no second set.
    let ignored = load_triangle(
        serde_json::json!({
            "pbrMetallicRoughness": {"baseColorTexture": {"index": 0}},
            "occlusionTexture": {"index": 0, "texCoord": 1}
        }),
        false,
    )
    .unwrap_or_else(|error| panic!("{error}"));
    assert!(
        ignored.meshes[0]
            .vertices
            .iter()
            .all(|vertex| vertex.uv1 == [0.; 2])
    );
    // A map on a set the primitive lacks (glTF 2.0 3.9.2).
    let error = load_triangle(
        serde_json::json!({"emissiveTexture": {"index": 0, "texCoord": 1}}),
        false,
    )
    .err()
    .expect("a map on a missing TEXCOORD_1 loaded")
    .to_string();
    assert!(error.contains("TEXCOORD_1"), "{error}");
}

/// The quad-local position of a TextureTransformTest marker as the texture
/// coordinate its quad has there: the asset's quads span -0.5..0.5 with
/// `TEXCOORD_0` (0,0) at their upper left corner (-0.5, 0.5) and (1,1) at
/// their lower right (its `Positions` and `UV0` accessors).
fn quad_uv([x, y]: [f32; 2]) -> [f32; 2] {
    [x + 0.5, 0.5 - y]
}

// The Khronos TextureTransformTest (glTF-Sample-Assets e124e8d): each of its
// Rotation, Scale and All quads draws an arrow texture through a transform
// and carries marker quads: "Correct" where the arrow must point, "Not
// Supported" where it points untransformed, and for the rotation "Error"
// where it points if "the rotation was applied in the opposite direction"
// (its README). So the texture point the arrow points at is the Not
// Supported marker's own coordinate, and a correct transform takes the
// Correct marker's coordinate to that point. The All quad's marker is
// placed to two decimals, within 0.035 of it. Plausible defects: the
// rotation turned the other way (the extension README's GLSL listing read
// as column-major), scale applied after rotation or offset before them, or
// the reader dropping a property.
#[test]
fn khronos_texture_transform_test_points_each_arrow_at_its_correct_marker() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/gltf/TextureTransformTest/glTF/TextureTransformTest.gltf");
    let document = gltf::Gltf::from_slice(&std::fs::read(path).unwrap())
        .unwrap()
        .document;
    let material = |name: &str| {
        let material = document
            .materials()
            .find(|material| material.name() == Some(name))
            .unwrap();
        let index = material.index().unwrap();
        read_material(material, &document, index, &mut Vec::new())
            .unwrap_or_else(|error| panic!("{error}"))
    };
    let marker = |name: &str| {
        let node = document
            .nodes()
            .find(|node| node.name() == Some(name))
            .unwrap_or_else(|| panic!("no node {name}"));
        let (translation, _, _) = node.transform().decomposed();
        quad_uv([translation[0], translation[1]])
    };
    let distance = |a: [f32; 2], b: [f32; 2]| (a[0] - b[0]).hypot(a[1] - b[1]);
    // Where the arrow points in its texture.
    let pointed = marker("Rotation - Not Supported");
    for (quad, tolerance) in [("Rotation", 1e-4), ("Scale", 1e-4), ("All", 0.035)] {
        let slot = material(quad).base_texture.unwrap();
        assert_eq!(slot.uv_set, UvSet::Uv0);
        let through = MapUv::of(&slot.transform);
        let correct = through.apply(marker(&format!("{quad} - Correct")));
        assert!(
            distance(correct, pointed) < tolerance,
            "{quad}: its Correct marker reads {correct:?}, the arrow points at {pointed:?}"
        );
    }
    // The opposite rotation's marker is elsewhere.
    let rotation = MapUv::of(&material("Rotation").base_texture.unwrap().transform);
    assert!(distance(rotation.apply(marker("Rotation - Error")), pointed) > 0.4);
    // Its offsets, as its README states them, on quads whose coordinates
    // run 0 to 0.5.
    for (quad, offset) in [
        ("Offset U", [0.5, 0.]),
        ("Offset V", [0., 0.5]),
        ("Offset UV", [0.5, 0.5]),
    ] {
        assert_eq!(
            material(quad).base_texture.unwrap().transform,
            UvTransform {
                offset,
                ..UvTransform::IDENTITY
            }
        );
    }
}

// KHR_mesh_quantization stores UVs as integers and exporters put their
// dequantisation in KHR_texture_transform (gltfpack, glTF-Transform): a
// plausible defect is the stored range reaching the sampler. Normalized
// UNSIGNED_SHORT UVs 0, 32767 and 65535 through offset (2, -1) and scale
// (4, 8) are the authored coordinates 2, about 4 and 6 in U and -1, about
// 3 and 7 in V: 65535 / 65535 x 4 + 2 = 6 and 32767 / 65535 x 8 - 1 =
// 2.99994, by hand.
#[test]
fn quantized_uvs_reach_their_authored_range_through_the_slots_transform() {
    let document = serde_json::json!({
        "asset": {"version": "2.0"},
        "extensionsUsed": ["KHR_mesh_quantization", "KHR_texture_transform"],
        "extensionsRequired": ["KHR_mesh_quantization", "KHR_texture_transform"],
        "buffers": [{"byteLength": 84}],
        "bufferViews": [
            {"buffer": 0, "byteLength": 36}, {"buffer": 0, "byteOffset": 36, "byteLength": 36},
            {"buffer": 0, "byteOffset": 72, "byteLength": 12}
        ],
        "accessors": [
            {"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3", "min": [0,0,0], "max": [1,1,0]},
            {"bufferView": 1, "componentType": 5126, "count": 3, "type": "VEC3"},
            {"bufferView": 2, "componentType": 5123, "normalized": true, "count": 3, "type": "VEC2"}
        ],
        "images": [{"uri": "a.png"}],
        "textures": [{"source": 0}],
        "materials": [{"pbrMetallicRoughness": {"baseColorTexture": {
            "index": 0,
            "extensions": transform(serde_json::json!({"offset": [2.0, -1.0], "scale": [4.0, 8.0]}))
        }}}],
        "meshes": [{"primitives": [{"attributes": {"POSITION": 0, "NORMAL": 1, "TEXCOORD_0": 2}, "material": 0}]}],
        "nodes": [{"mesh": 0}],
        "scenes": [{"nodes": [0]}], "scene": 0
    });
    let floats: [f32; 18] = [
        0., 0., 0., 1., 0., 0., 0., 1., 0., 0., 0., 1., 0., 0., 1., 0., 0., 1.,
    ];
    let mut bin = bytes(&floats);
    for value in [0u16, 0, 32767, 32767, 65535, 65535] {
        bin.extend(value.to_le_bytes());
    }
    let supplied = image::RgbaImage::new(1, 1);
    let supply = |_: GltfImage<'_>| Ok(ImageSource::Supplied(Image::Rgba8(supplied.clone())));
    let asset = load_slice_with_options(
        &glb(&document, &bin),
        LoadOptions {
            images: Some(&supply),
            ..Default::default()
        },
    )
    .unwrap_or_else(|error| panic!("{error}"));
    let through = MapUv::of(&asset.materials[0].base_texture.unwrap().transform);
    for (vertex, expected) in
        asset.meshes[0]
            .vertices
            .iter()
            .zip([[2., -1.], [3.99994, 2.99988], [6., 7.]])
    {
        let sampled = through.apply(vertex.uv);
        assert!(
            (sampled[0] - expected[0]).abs() < 1e-4 && (sampled[1] - expected[1]).abs() < 1e-4,
            "{:?} samples {sampled:?}, authored {expected:?}",
            vertex.uv
        );
    }
}
