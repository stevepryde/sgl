//! Tangent frames for a triangle list that needs one and carries none:
//! MikkTSpace's, as glTF 2.0 3.7.2.1 defines absent tangents ("the tangent
//! vectors are calculated using default MikkTSpace algorithms with the
//! vertex positions, normals …, and texture coordinates associated with the
//! normal texture"), through Bevy's `bevy_mikktspace` 1.0, a safe Rust port
//! of Morten Mikkelsen's mikktspace.c (Zlib AND (MIT OR Apache-2.0); its
//! notice is in DISTRIBUTION_NOTICES.txt), with its corrected edge sorting
//! and vertex welding, so not byte-identical to the C original
//! ([D-88](../../../../specs/decisions.md)). MikkTSpace gives
//! each triangle corner its own frame; a vertex whose corners take different
//! frames (a mirrored island meeting another at a seam whose coordinates
//! coincide) is split, so each keeps its own (specs/sgl3d-architecture.md,
//! Material map coordinates). The generator is the `mikktspace` feature,
//! on by default; without it `generate` frames nothing, so each such
//! triangle list keeps the frame of each triangle's coordinates.
#[cfg(feature = "mikktspace")]
use bevy_mikktspace::{Geometry, TangentSpace};
#[cfg(feature = "mikktspace")]
use rustc_hash::FxHashMap;
#[cfg(feature = "mikktspace")]
use std::collections::HashMap;

/// The most triangles that may share one vertex of a triangle list that
/// MikkTSpace frames, counting as one the corners it welds (equal position,
/// normal and coordinates); a list in which more do is not framed. Its
/// grouping compares the triangles around each vertex pairwise, which
/// `bevy_mikktspace` keeps although no angular threshold splits groups
/// here: a fan of 8 000 triangles around one vertex took 331 ms, about
/// 331 ms / 8 000² ≈ 5.2 ns a pair, so a vertex at this cap costs about
/// 5.2 ns × 1 024² ≈ 5.5 ms (a fan of 1 000 took 6 ms), while a 256-segment
/// disc's centre or a cone's tip (256) still generates. An upstream change
/// skipping that comparison when no threshold is set would make the cap
/// unnecessary (#902, D-88).
pub(crate) const MOST_TRIANGLES_AT_A_VERTEX: usize = 1024;

/// A triangle list's corners, read by MikkTSpace, and the frame it gives
/// each corner.
#[cfg(feature = "mikktspace")]
struct Corners<'a> {
    positions: &'a [[f32; 3]],
    normals: &'a [[f32; 3]],
    uvs: &'a [[f32; 2]],
    triangles: &'a [u32],
    frames: Vec<Option<[f32; 4]>>,
}

#[cfg(feature = "mikktspace")]
impl Corners<'_> {
    fn vertex(&self, face: usize, vert: usize) -> usize {
        self.triangles[face * 3 + vert] as usize
    }
}

#[cfg(feature = "mikktspace")]
impl Geometry for Corners<'_> {
    fn num_faces(&self) -> usize {
        self.triangles.len() / 3
    }

    fn num_vertices_of_face(&self, _: usize) -> usize {
        3
    }

    fn position(&self, face: usize, vert: usize) -> [f32; 3] {
        self.positions[self.vertex(face, vert)]
    }

    fn normal(&self, face: usize, vert: usize) -> [f32; 3] {
        self.normals[self.vertex(face, vert)]
    }

    // MikkTSpace takes V up its image, as the tools that bake against it
    // (Blender among them) hold their coordinates; glTF's V runs down
    // (glTF 2.0 3.9.3), so a frame's sign comes out as glTF's
    // B = (N x T) w toward decreasing V. Bevy instead negates every sign
    // it gets (bevy_mesh `generate_tangents_for_mesh`).
    fn tex_coord(&self, face: usize, vert: usize) -> [f32; 2] {
        let [u, v] = self.uvs[self.vertex(face, vert)];
        [u, 1. - v]
    }

    fn set_tangent(&mut self, frame: Option<TangentSpace>, face: usize, vert: usize) {
        self.frames[face * 3 + vert] = frame.map(|frame| frame.tangent_encoded());
    }
}

/// A triangle list with a frame for each vertex.
#[derive(Debug, PartialEq)]
#[cfg_attr(not(feature = "mikktspace"), allow(dead_code))]
pub(crate) struct Framed {
    /// Each vertex's source vertex: the source's own first, in order, then
    /// one for each split, each a copy of the vertex it names.
    pub source: Vec<u32>,
    /// Each vertex's tangent and handedness (glTF's TANGENT), `None` where
    /// MikkTSpace gave none (a vertex of degenerate triangles only, or of
    /// none).
    pub frames: Vec<Option<[f32; 4]>>,
    /// The triangles over these vertices, in the source's order.
    pub triangles: Vec<u32>,
}

/// MikkTSpace's frames for `triangles` (three vertex indices a triangle) over
/// `positions`, `normals` and the coordinates `uvs` of the normal map, one
/// of each a vertex, in the triangles' own winding and space; none where
/// more than `MOST_TRIANGLES_AT_A_VERTEX` triangles share one vertex.
#[cfg(feature = "mikktspace")]
pub(crate) fn generate(
    positions: &[[f32; 3]],
    normals: &[[f32; 3]],
    uvs: &[[f32; 2]],
    triangles: &[u32],
) -> Option<Framed> {
    // One pass over the corners, keyed as MikkTSpace welds them: by the
    // values it is given (`Corners`), compared by `total_cmp`, the bits.
    let mut shared: FxHashMap<[u32; 8], usize> = FxHashMap::default();
    for &vertex in triangles {
        let at = vertex as usize;
        let [x, y, z] = positions[at].map(f32::to_bits);
        let [nx, ny, nz] = normals[at].map(f32::to_bits);
        let [u, v] = uvs[at];
        let key = [x, y, z, nx, ny, nz, u.to_bits(), (1. - v).to_bits()];
        let count = shared.entry(key).or_default();
        *count += 1;
        if *count > MOST_TRIANGLES_AT_A_VERTEX {
            return None;
        }
    }
    let mut corners = Corners {
        positions,
        normals,
        uvs,
        triangles,
        frames: vec![None; triangles.len()],
    };
    // The crate reports no error today; one it adds leaves every corner
    // without a frame, as a mesh MikkTSpace cannot frame has none.
    if bevy_mikktspace::generate_tangents(&mut corners).is_err() {
        corners.frames.fill(None);
    }
    Some(split(positions.len(), triangles, &corners.frames))
}

/// None: without the `mikktspace` feature there is no generator.
#[cfg(not(feature = "mikktspace"))]
pub(crate) fn generate(
    _positions: &[[f32; 3]],
    _normals: &[[f32; 3]],
    _uvs: &[[f32; 2]],
    _triangles: &[u32],
) -> Option<Framed> {
    None
}

/// `triangles` over `vertices` source vertices with each corner's frame
/// `corners`: a vertex keeps its first corner's frame, and a corner with
/// another frame takes a copy of it with that frame, one copy a distinct
/// frame.
#[cfg(feature = "mikktspace")]
fn split(vertices: usize, triangles: &[u32], corners: &[Option<[f32; 4]>]) -> Framed {
    let key = |frame: Option<[f32; 4]>| frame.map(|frame| frame.map(f32::to_bits));
    let mut source: Vec<u32> = (0..vertices as u32).collect();
    let mut frames: Vec<Option<[f32; 4]>> = vec![None; vertices];
    let mut assigned = vec![false; vertices];
    let mut copies: HashMap<(u32, Option<[u32; 4]>), u32> = HashMap::new();
    let triangles = triangles
        .iter()
        .zip(corners)
        .map(|(&vertex, &frame)| {
            let at = vertex as usize;
            if !assigned[at] {
                assigned[at] = true;
                frames[at] = frame;
                return vertex;
            }
            if key(frames[at]) == key(frame) {
                return vertex;
            }
            *copies.entry((vertex, key(frame))).or_insert_with(|| {
                source.push(vertex);
                frames.push(frame);
                (source.len() - 1) as u32
            })
        })
        .collect();
    Framed {
        source,
        frames,
        triangles,
    }
}

#[cfg(all(test, feature = "mikktspace"))]
mod tests {
    use super::*;
    use glam::Vec3;
    use wasm_bindgen_test::wasm_bindgen_test;

    // Defects: a vertex whose corners take different frames keeps only
    // one; corners that agree are split anyway, or a split forgets its
    // source. The oracle is the contract: a vertex keeps its first corner's
    // frame, each distinct other frame takes one copy, and every copy names
    // its source.
    #[wasm_bindgen_test(unsupported = test)]
    fn corners_with_another_frame_take_a_copy_of_their_vertex() {
        let left = Some([1., 0., 0., 1.]);
        let right = Some([-1., 0., 0., -1.]);
        // Two triangles over four vertices, sharing 1 and 2; then a third
        // triangle on 1 and 2 again with the left frame, and a vertex no
        // triangle uses.
        let triangles = [0, 1, 2, 3, 2, 1, 2, 1, 0];
        let corners = [left, left, left, right, right, right, left, left, left];
        let framed = split(5, &triangles, &corners);
        assert_eq!(framed.source, [0, 1, 2, 3, 4, 2, 1]);
        assert_eq!(framed.frames, [left, left, left, right, None, right, right]);
        assert_eq!(framed.triangles, [0, 1, 2, 3, 5, 6, 2, 1, 0]);
    }

    // The Khronos NormalTangentMirrorTest (glTF-Sample-Assets e124e8d)
    // carries the tangents Blender's exporter computed with MikkTSpace from
    // the same positions, normals and coordinates. Defects: V taken down
    // the image (every handedness reversed), a sign or axis lost, a mirrored
    // island's handedness not followed, a seam split that MikkTSpace does
    // not make. Bound: Blender runs MikkTSpace on its polygons before the
    // exporter triangulates them, with its own (V-up) copies of the
    // coordinates, so the directions are not bit-identical; they must agree
    // within one 8-bit step of a normal map's unit component (2/255, about
    // 0.45°), the precision of the map the frame serves, and every
    // handedness must be equal.
    #[wasm_bindgen_test(unsupported = test)]
    fn frames_match_blenders_mikktspace_on_the_khronos_mirror_test() {
        let json = include_bytes!(
            "../../tests/fixtures/gltf/NormalTangentMirrorTest/glTF/NormalTangentMirrorTest.gltf"
        );
        let bin: &[u8] = include_bytes!(
            "../../tests/fixtures/gltf/NormalTangentMirrorTest/glTF/NormalTangentMirrorTest.bin"
        );
        let document = gltf::Gltf::from_slice(json).unwrap().document;
        let primitive = document
            .meshes()
            .next()
            .unwrap()
            .primitives()
            .next()
            .unwrap();
        let reader = primitive.reader(|_| Some(bin));
        let positions: Vec<[f32; 3]> = reader.read_positions().unwrap().collect();
        let normals: Vec<[f32; 3]> = reader.read_normals().unwrap().collect();
        let uvs: Vec<[f32; 2]> = reader.read_tex_coords(0).unwrap().into_f32().collect();
        let authored: Vec<[f32; 4]> = reader.read_tangents().unwrap().collect();
        let triangles: Vec<u32> = reader.read_indices().unwrap().into_u32().collect();
        let framed = generate(&positions, &normals, &uvs, &triangles).unwrap();
        assert_eq!(
            framed.source.len(),
            positions.len(),
            "a split Blender did not make"
        );
        let bound = (2. / 255f32).asin().to_degrees();
        for (vertex, frame) in framed.frames.iter().enumerate() {
            let frame = frame.unwrap_or_else(|| panic!("vertex {vertex} has no frame"));
            let blender = authored[vertex];
            assert_eq!(frame[3], blender[3], "vertex {vertex}: handedness");
            let degrees = Vec3::from_slice(&frame[..3])
                .angle_between(Vec3::from_slice(&blender[..3]))
                .to_degrees();
            assert!(
                degrees <= bound,
                "vertex {vertex}: {degrees}° from Blender's tangent"
            );
        }
    }
}
