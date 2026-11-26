//! Tangent frames the loader generates (`LoadOptions::tangents`): MikkTSpace
//! for a primitive whose material takes a vertex frame and that carries no
//! `TANGENT`, on Khronos assets and hand-built documents. Those that need
//! the generator run with the `mikktspace` feature; one checks the build
//! without it.
// The generator's helpers go unused without it.
#![cfg_attr(not(feature = "mikktspace"), allow(dead_code))]
use super::tests::Fixture;
use super::*;
use crate::asset::{Asset, Vertex};
use glam::Vec3;
use std::path::Path;

/// `path` loaded with `tangents`, every image supplied as one texel so
/// that only geometry is read.
fn load_frames(path: &Path, tangents: TangentFrames) -> Asset {
    let texel = |_: GltfImage<'_>| -> Result<ImageSource> {
        Ok(ImageSource::Supplied(crate::asset::Image::Rgba8(
            image::RgbaImage::from_pixel(1, 1, image::Rgba([128, 128, 255, 255])),
        )))
    };
    load_with_options(
        path,
        LoadOptions {
            tangents,
            images: Some(&texel),
            ..LoadOptions::default()
        },
    )
    .unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn khronos(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/gltf")
        .join(name)
}

/// Whether `vertex` carries a frame glTF 2.0 3.7.2.1 accepts: a unit tangent
/// perpendicular to its normal, with handedness +1 or -1.
fn framed(vertex: &Vertex) -> bool {
    let tangent = Vec3::from_slice(&vertex.tangent[..3]);
    (tangent.length() - 1.).abs() < 1e-5
        && tangent.dot(Vec3::from_array(vertex.normal)).abs() < 1e-5
        && matches!(vertex.tangent[3], -1. | 1.)
}

// The Khronos NormalTangentTest carries no TANGENT and has a normal map.
// Defects: generation skipped by default, run under `AuthoredOnly`, or
// leaving a vertex without a usable frame; geometry or triangles changed
// where no seam asks for a split. The oracle is the setting's contract;
// whether the frames shade right is `scene::rays::khronos_normal_tests`'.
#[cfg(feature = "mikktspace")]
#[test]
fn a_normal_mapped_primitive_without_tangents_gets_a_frame_on_every_vertex() {
    let path = khronos("NormalTangentTest/glTF/NormalTangentTest.gltf");
    let generated = load_frames(&path, TangentFrames::Generate);
    let authored = load_frames(&path, TangentFrames::AuthoredOnly);
    for (generated, authored) in generated.meshes.iter().zip(&authored.meshes) {
        assert!(generated.vertices.iter().all(framed));
        assert!(
            authored
                .vertices
                .iter()
                .all(|vertex| vertex.tangent == [0.; 4])
        );
        assert_eq!(generated.indices, authored.indices);
        assert_eq!(generated.vertices.len(), authored.vertices.len());
        for (a, b) in generated.vertices.iter().zip(&authored.vertices) {
            assert_eq!((a.position, a.normal, a.uv), (b.position, b.normal, b.uv));
        }
    }
}

// Authored tangents stay authoritative and content that needs no frame pays
// nothing. Defects: generation run over an authored TANGENT, or on a
// material without a normal map, clearcoat normal map or anisotropy. The
// oracle is the contract: such primitives load to the same vertices and
// indices whatever the setting.
#[test]
fn authored_tangents_and_frameless_materials_load_unchanged() {
    let path = khronos("NormalTangentMirrorTest/glTF/NormalTangentMirrorTest.gltf");
    let generated = load_frames(&path, TangentFrames::Generate);
    let authored = load_frames(&path, TangentFrames::AuthoredOnly);
    let bytes = |asset: &Asset| {
        asset
            .meshes
            .iter()
            .map(|mesh| {
                (
                    bytemuck::cast_slice::<_, u8>(&mesh.vertices).to_vec(),
                    mesh.indices.clone(),
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(bytes(&generated), bytes(&authored));
    let plain = seam(false);
    assert_eq!(
        bytes(&load_frames(&plain.path(), TangentFrames::Generate)),
        bytes(&load_frames(&plain.path(), TangentFrames::AuthoredOnly)),
    );
    assert!(
        load_frames(&plain.path(), TangentFrames::Generate).meshes[0]
            .vertices
            .iter()
            .all(|vertex| vertex.tangent == [0.; 4])
    );
}

const PNG: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGNwC4gCAAHQAPElUIcnAAAAAElFTkSuQmCC";

/// A word's bits as the `f32` the fixture writes, for integer accessors.
fn bits(low: u16, high: u16) -> f32 {
    f32::from_bits(u32::from(low) | (u32::from(high) << 16))
}

/// Two triangles facing +Z that meet along x = 0, their coordinates
/// mirrored across it (U = 1 − |x| rises toward the seam from both sides,
/// V = (1 − y) / 2 runs down), sharing the seam's two vertices; skinned to
/// two joints and morphed by one target, each vertex's influence and
/// displacement its own. Drawn with a normal map where `normal_mapped`.
fn seam(normal_mapped: bool) -> Fixture {
    seam_at(normal_mapped, |x, y| [1. - f32::abs(x), (1. - y) / 2.])
}

/// `seam`'s triangles with coordinates `uv` of each vertex's position.
fn seam_at(normal_mapped: bool, uv: impl Fn(f32, f32) -> [f32; 2]) -> Fixture {
    let (document, values) = seam_parts(normal_mapped, uv);
    Fixture::new(&serde_json::to_vec(&document).unwrap(), &values)
}

/// `seam_at`'s document and buffer.
fn seam_parts(
    normal_mapped: bool,
    uv: impl Fn(f32, f32) -> [f32; 2],
) -> (serde_json::Value, Vec<f32>) {
    let mut material = serde_json::json!({"name": "seam"});
    if normal_mapped {
        material["normalTexture"] = serde_json::json!({"index": 0});
    }
    let document = serde_json::json!({
        "asset": {"version": "2.0"},
        "buffers": [{"uri": "fixture.bin", "byteLength": 288}],
        "bufferViews": [
            {"buffer": 0, "byteOffset": 0, "byteLength": 48},
            {"buffer": 0, "byteOffset": 48, "byteLength": 48},
            {"buffer": 0, "byteOffset": 96, "byteLength": 32},
            {"buffer": 0, "byteOffset": 128, "byteLength": 32},
            {"buffer": 0, "byteOffset": 160, "byteLength": 64},
            {"buffer": 0, "byteOffset": 224, "byteLength": 48},
            {"buffer": 0, "byteOffset": 272, "byteLength": 12}
        ],
        "accessors": [
            {"bufferView": 0, "componentType": 5126, "count": 4, "type": "VEC3", "min": [-1,-1,0], "max": [1,1,0]},
            {"bufferView": 1, "componentType": 5126, "count": 4, "type": "VEC3"},
            {"bufferView": 2, "componentType": 5126, "count": 4, "type": "VEC2"},
            {"bufferView": 3, "componentType": 5123, "count": 4, "type": "VEC4"},
            {"bufferView": 4, "componentType": 5126, "count": 4, "type": "VEC4"},
            {"bufferView": 5, "componentType": 5126, "count": 4, "type": "VEC3", "min": [0.1,0,0], "max": [0.4,0,0]},
            {"bufferView": 6, "componentType": 5123, "count": 6, "type": "SCALAR"}
        ],
        "images": [{"uri": PNG}],
        "textures": [{"source": 0}],
        "materials": [material],
        "meshes": [{
            "primitives": [{
                "attributes": {"POSITION": 0, "NORMAL": 1, "TEXCOORD_0": 2, "JOINTS_0": 3, "WEIGHTS_0": 4},
                "targets": [{"POSITION": 5}],
                "indices": 6,
                "material": 0
            }],
            "weights": [0.5]
        }],
        "skins": [{"joints": [1, 2]}],
        "nodes": [{"mesh": 0, "skin": 0}, {"name": "a"}, {"name": "b"}],
        "scenes": [{"nodes": [0, 1, 2]}], "scene": 0
    });
    let mut values = Vec::new();
    let positions = [[-1., 0., 0.], [0., -1., 0.], [0., 1., 0.], [1., 0., 0.]];
    for p in positions {
        values.extend(p);
    }
    for _ in positions {
        values.extend([0., 0., 1.]);
    }
    for [x, y, _] in positions {
        values.extend(uv(x, y));
    }
    for _ in positions {
        values.extend([bits(0, 1), bits(0, 0)]);
    }
    for weights in [
        [1., 0., 0., 0.],
        [0.75, 0.25, 0., 0.],
        [0.5, 0.5, 0., 0.],
        [0.25, 0.75, 0., 0.],
    ] {
        values.extend(weights);
    }
    for delta in [0.1, 0.2, 0.3, 0.4] {
        values.extend([delta, 0., 0.]);
    }
    values.extend([bits(0, 1), bits(2, 3), bits(2, 1), 0.]);
    (document, values)
}

/// Asserts glTF 2.0 3.7.2.1's frame of `seam`'s coordinates on each
/// triangle of `mesh`, as it lies in its space: U rises toward the seam,
/// so on the triangle left of it (−X) T is +X and B toward decreasing V is
/// +Y = N x T, handedness +1; on the right T is −X and B is +Y = −(N x T),
/// handedness −1.
fn assert_seam_frames(mesh: &crate::asset::CpuMesh) {
    for triangle in mesh.indices.as_chunks::<3>().0 {
        let corners = triangle.map(|i| mesh.vertices[i as usize]);
        let left = corners.iter().any(|corner| corner.position[0] < -0.5);
        let expected = if left {
            [1., 0., 0., 1.]
        } else {
            [-1., 0., 0., -1.]
        };
        for corner in corners {
            assert!(
                Vec3::from_slice(&corner.tangent[..3]).distance(Vec3::from_slice(&expected[..3]))
                    < 1e-5
                    && corner.tangent[3] == expected[3],
                "{} triangle: {:?}",
                if left { "left" } else { "right" },
                corner.tangent
            );
        }
    }
}

// A mirrored seam: MikkTSpace frames the two triangles with opposite
// handedness, so the vertices they share take two frames and must be
// split, each copy keeping its source's skin and morph displacement.
// Defects: a shared vertex keeping one triangle's frame for both, a split
// copy losing its influence or displacement (the deformed copies tear
// apart), the frames' sign or handedness wrong. The oracle is glTF 2.0
// 3.7.2.1's frame of each triangle's coordinates, worked out from the
// fixture: on the left U rises along +X, so T is +X, and B toward
// decreasing V is +Y = N x T, handedness +1; on the right U rises along
// −X, T is −X and B is +Y = −(N x T), handedness −1; and the unsplit
// load's own streams, matched by position.
#[cfg(feature = "mikktspace")]
#[test]
fn a_mirrored_seam_splits_its_vertices_with_their_skin_and_morphs() {
    let fixture = seam(true);
    let generated = load_frames(&fixture.path(), TangentFrames::Generate);
    let unsplit = load_frames(&fixture.path(), TangentFrames::AuthoredOnly);
    let (mesh, source) = (&generated.meshes[0], &unsplit.meshes[0]);
    assert_eq!(source.vertices.len(), 4);
    assert_eq!(mesh.vertices.len(), 6, "the seam's two vertices split");
    let from = |vertex: &Vertex| {
        source
            .vertices
            .iter()
            .position(|other| other.position == vertex.position)
            .unwrap()
    };
    for (i, vertex) in mesh.vertices.iter().enumerate() {
        let s = from(vertex);
        assert_eq!(
            (vertex.normal, vertex.uv),
            (source.vertices[s].normal, source.vertices[s].uv)
        );
        assert_eq!(
            mesh.deformation.influences[i],
            source.deformation.influences[s]
        );
        assert_eq!(
            mesh.deformation.morph_targets[0].deltas[i],
            source.deformation.morph_targets[0].deltas[s]
        );
    }
    assert_seam_frames(mesh);
}

// Triangles whose coordinates are all one point: glTF's tangent space is
// undefined there and MikkTSpace gives them none. Defects: a NaN or zero
// tangent reaching a vertex, or the mesh left without frames, which would
// take its whole material off its vertex frames. The oracle is the
// contract: a frame glTF accepts, in the normal's plane.
#[cfg(feature = "mikktspace")]
#[test]
fn degenerate_coordinates_take_a_frame_in_the_normals_plane() {
    let fixture = seam_at(true, |_, _| [0.5, 0.5]);
    let mesh = &load_frames(&fixture.path(), TangentFrames::Generate).meshes[0];
    assert!(mesh.vertices.iter().all(framed));
    assert!(crate::asset::tangent_frames(&mesh.vertices));
}

// One mesh used by two nodes, the second mirrored (scale −1 on X): its
// frames are generated once, in the mesh's space, and each node carries
// them through its own transform. Defects: the second node reusing the
// first's frames untransformed, or a mirror's handedness not reversed
// (glTF's B = (N x T) w is measured in the node's space). The oracle is
// glTF's frame of each node's coordinates as they lie after its
// transform (`assert_seam_frames`); the seam is symmetric about x = 0, so
// both nodes' triangles left of it take +X with handedness +1.
#[cfg(feature = "mikktspace")]
#[test]
fn a_mesh_two_nodes_use_takes_each_nodes_frame() {
    let (mut document, values) = seam_parts(true, |x, y| [1. - f32::abs(x), (1. - y) / 2.]);
    let primitive = &mut document["meshes"][0]["primitives"][0];
    let attributes = primitive["attributes"].as_object_mut().unwrap();
    attributes.remove("JOINTS_0");
    attributes.remove("WEIGHTS_0");
    document.as_object_mut().unwrap().remove("skins");
    document["nodes"] = serde_json::json!([{"mesh": 0}, {"mesh": 0, "scale": [-1, 1, 1]}]);
    document["scenes"] = serde_json::json!([{"nodes": [0, 1]}]);
    let fixture = Fixture::new(&serde_json::to_vec(&document).unwrap(), &values);
    let asset = load_frames(&fixture.path(), TangentFrames::Generate);
    assert_eq!(asset.meshes.len(), 2, "one mesh a morphed node");
    for mesh in &asset.meshes {
        assert_eq!(mesh.vertices.len(), 6);
        assert_seam_frames(mesh);
    }
}

/// A disc facing +Z of `triangles` triangles around one centre vertex,
/// its coordinates the disc mapped onto the unit square, with a normal map.
fn fan(triangles: u32) -> Fixture {
    let rim = triangles;
    let vertices = rim + 1;
    let document = serde_json::json!({
        "asset": {"version": "2.0"},
        "buffers": [{"uri": "fixture.bin", "byteLength": vertices * 32 + triangles * 12}],
        "bufferViews": [
            {"buffer": 0, "byteOffset": 0, "byteLength": vertices * 12},
            {"buffer": 0, "byteOffset": vertices * 12, "byteLength": vertices * 12},
            {"buffer": 0, "byteOffset": vertices * 24, "byteLength": vertices * 8},
            {"buffer": 0, "byteOffset": vertices * 32, "byteLength": triangles * 12}
        ],
        "accessors": [
            {"bufferView": 0, "componentType": 5126, "count": vertices, "type": "VEC3", "min": [-1,-1,0], "max": [1,1,0]},
            {"bufferView": 1, "componentType": 5126, "count": vertices, "type": "VEC3"},
            {"bufferView": 2, "componentType": 5126, "count": vertices, "type": "VEC2"},
            {"bufferView": 3, "componentType": 5125, "count": triangles * 3, "type": "SCALAR"}
        ],
        "images": [{"uri": PNG}],
        "textures": [{"source": 0}],
        "materials": [{"normalTexture": {"index": 0}}],
        "meshes": [{"primitives": [{"attributes": {"POSITION": 0, "NORMAL": 1, "TEXCOORD_0": 2}, "indices": 3, "material": 0}]}],
        "nodes": [{"mesh": 0}],
        "scenes": [{"nodes": [0]}], "scene": 0
    });
    let corner = |k: u32| {
        let angle = k as f32 / rim as f32 * std::f32::consts::TAU;
        (angle.cos(), angle.sin())
    };
    let mut values = vec![0., 0., 0.];
    for k in 0..rim {
        let (x, y) = corner(k);
        values.extend([x, y, 0.]);
    }
    for _ in 0..vertices {
        values.extend([0., 0., 1.]);
    }
    values.extend([0.5, 0.5]);
    for k in 0..rim {
        let (x, y) = corner(k);
        values.extend([0.5 + 0.5 * x, 0.5 - 0.5 * y]);
    }
    for k in 0..triangles {
        values.extend([0, 1 + k, 1 + (k + 1) % rim].map(f32::from_bits));
    }
    Fixture::new(&serde_json::to_vec(&document).unwrap(), &values)
}

// MikkTSpace's grouping costs the square of the triangles that share one
// vertex, so a primitive in which more than
// `tangents::MOST_TRIANGLES_AT_A_VERTEX` do keeps the frame of each
// triangle's coordinates (its tangents absent) and is listed. Defects: the
// cap off by one, a crowded primitive generating anyway (a 20 000-triangle
// fan holds the load for seconds) or failing the load, or its skip not
// reported. The oracle is the contract at the cap and either side of it,
// and the load's time: a 20 000-triangle fan generated takes over a second
// in a release build, several times more in a debug one, and loads in a
// small part of that when skipped.
#[cfg(feature = "mikktspace")]
#[test]
fn a_vertex_too_many_triangles_share_keeps_the_triangle_frame() {
    let at_cap = tangents_cap();
    let under = load_frames(&fan(at_cap).path(), TangentFrames::Generate);
    assert!(under.meshes[0].vertices.iter().all(framed));
    assert!(under.ignored.is_empty(), "{:?}", under.ignored);
    for triangles in [at_cap + 1, 20_000] {
        let fixture = fan(triangles);
        let start = std::time::Instant::now();
        let over = load_frames(&fixture.path(), TangentFrames::Generate);
        let seconds = start.elapsed().as_secs_f64();
        assert!(
            over.meshes[0]
                .vertices
                .iter()
                .all(|vertex| vertex.tangent == [0.; 4]),
            "{triangles} triangles: generated"
        );
        assert_eq!(
            over.ignored,
            [Ignored::TangentFrames {
                mesh: 0,
                primitive: 0
            }]
        );
        assert!(seconds < 0.5, "{triangles} triangles: {seconds} s");
    }
}

// Without the `mikktspace` feature there is no generator: a primitive that
// needs a frame and carries no TANGENT loads under the default setting as
// under `AuthoredOnly`, keeping the frame of each triangle's coordinates,
// and is listed, as one over the cap is; an anisotropic one fails the load,
// naming the feature. Defects: such a primitive given a placeholder frame
// (which would take its material off the triangle frame), split, or left
// unlisted; one listed where the game asked for no generation; an
// anisotropic one loaded without a frame, or refused as if over the cap.
// The oracle is the contract (`TangentFrames::Generate`,
// `Ignored::TangentFrames`).
#[cfg(not(feature = "mikktspace"))]
#[test]
fn without_mikktspace_a_primitive_without_tangents_keeps_the_triangle_frame() {
    let fixture = seam(true);
    let generated = load_frames(&fixture.path(), TangentFrames::Generate);
    let authored = load_frames(&fixture.path(), TangentFrames::AuthoredOnly);
    assert_eq!(
        generated.ignored,
        [Ignored::TangentFrames {
            mesh: 0,
            primitive: 0
        }]
    );
    assert!(authored.ignored.is_empty(), "{:?}", authored.ignored);
    for asset in [&generated, &authored] {
        assert_eq!(asset.meshes.len(), 1);
        let vertices = &asset.meshes[0].vertices;
        assert_eq!(vertices.len(), 4, "a split");
        assert!(vertices.iter().all(|vertex| vertex.tangent == [0.; 4]));
    }
    let (mut document, values) = seam_parts(true, |x, y| [1. - f32::abs(x), (1. - y) / 2.]);
    document["extensionsUsed"] = serde_json::json!(["KHR_materials_anisotropy"]);
    document["materials"][0]["extensions"] =
        serde_json::json!({"KHR_materials_anisotropy": {"anisotropyStrength": 0.7}});
    let fixture = Fixture::new(&serde_json::to_vec(&document).unwrap(), &values);
    let error = load_with_options(&fixture.path(), LoadOptions::default())
        .err()
        .unwrap()
        .to_string();
    assert!(
        error.contains("mikktspace feature") && error.contains("TANGENT"),
        "{error}"
    );
}

/// The cap the loader applies.
fn tangents_cap() -> u32 {
    crate::content::tangents::MOST_TRIANGLES_AT_A_VERTEX as u32
}
