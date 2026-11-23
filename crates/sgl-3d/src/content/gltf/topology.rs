//! A glTF primitive's topology as the triangle list the rest of SGL3D draws:
//! TRIANGLES as authored, TRIANGLE_STRIP and TRIANGLE_FAN expanded once at
//! import by glTF 2.0.1 3.7.2.1's equations (strip
//! `p_i = {v_i, v_{i+(1+i%2)}, v_{i+(2-i%2)}}`, fan `p_i = {v_{i+1}, v_{i+2},
//! v_0}`), the parity three.js r187's `toTrianglesDrawMode` also uses.
//! Unlike three.js, a triangle that repeats an index, such as the
//! degenerates that join strips, is left out: it covers nothing in raster,
//! shadow or ray paths, and its absence keeps the joined strips' triangles
//! adjacent. Parity counts from the authored position, so leaving one out
//! never changes a later triangle's winding.
use gltf::mesh::Mode;

/// How many indices a primitive of `mode` with `count` authored indices
/// (its vertices, when it has none) holds as a triangle list, before
/// repeated-index triangles are left out: what the decoded budget charges.
/// `None` for a mode SGL3D does not draw.
pub(super) fn triangle_list_len(mode: Mode, count: u64) -> Option<u64> {
    match mode {
        Mode::Triangles => Some(count),
        Mode::TriangleStrip | Mode::TriangleFan => Some(count.saturating_sub(2) * 3),
        Mode::Points | Mode::Lines | Mode::LineLoop | Mode::LineStrip => None,
    }
}

/// Refuses a mode SGL3D does not draw, before its accessors are read.
pub(super) fn check_mode(mode: Mode, label: &str) -> Result<(), String> {
    match triangle_list_len(mode, 0) {
        Some(_) => Ok(()),
        None => Err(format!(
            "{label}: {mode:?} primitives are not supported; export triangles, triangle strips or triangle fans"
        )),
    }
}

/// `indices`, authored in `mode`, as a triangle list. glTF requires a
/// multiple of three for TRIANGLES and at least three for strips and fans
/// (glTF 2.0.1 3.7.2.1); any other count is refused.
pub(super) fn triangle_list(
    mode: Mode,
    indices: Vec<u32>,
    label: &str,
) -> Result<Vec<u32>, String> {
    let count = indices.len();
    let valid = match mode {
        Mode::Triangles => count.is_multiple_of(3),
        _ => count >= 3,
    };
    if !valid {
        return Err(format!(
            "{label}: {count} indices cannot form {mode:?}; re-export valid glTF"
        ));
    }
    let triangle = |i: usize| match mode {
        Mode::TriangleStrip => [indices[i], indices[i + 1 + i % 2], indices[i + 2 - i % 2]],
        _ => [indices[i + 1], indices[i + 2], indices[0]],
    };
    Ok(match mode {
        Mode::Triangles => indices,
        _ => (0..count - 2)
            .map(triangle)
            .filter(|[a, b, c]| a != b && b != c && a != c)
            .flatten()
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    /// Each triangle rotated to start at its smallest index, which keeps
    /// its winding, so listings that differ only in starting vertex agree.
    fn canonical(list: &[u32]) -> Vec<[u32; 3]> {
        list.as_chunks::<3>()
            .0
            .iter()
            .map(|&[a, b, c]| {
                let mut t = [a, b, c];
                let first = (0..3).min_by_key(|&k| t[k]).unwrap();
                t.rotate_left(first);
                t
            })
            .collect()
    }

    // Defect: odd strip triangles keep the even winding (or the reverse),
    // a joining degenerate is kept, or one is dropped by shifting parity so
    // the next strip's winding flips. Oracle: two counter-clockwise quads
    // listed by hand, joined by the usual repeated indices 3 and 4.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_joined_strip_keeps_each_quads_winding_and_drops_its_joins() {
        let list = triangle_list(
            Mode::TriangleStrip,
            vec![0, 1, 2, 3, 3, 4, 4, 5, 6, 7],
            "strip",
        )
        .unwrap();
        assert_eq!(
            canonical(&list),
            [[0, 1, 2], [1, 3, 2], [4, 5, 6], [5, 7, 6]]
        );
    }

    // Defect: a fan pivots on the wrong vertex or reverses its winding.
    // Oracle: a counter-clockwise pentagon around index 9, listed by hand.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_fan_turns_about_its_first_index() {
        let list = triangle_list(Mode::TriangleFan, vec![9, 1, 2, 3, 4], "fan").unwrap();
        assert_eq!(canonical(&list), [[1, 2, 9], [2, 3, 9], [3, 4, 9]]);
    }

    // Defect: a count glTF 2.0.1 3.7.2.1 forbids (under three for a strip
    // or fan, not a multiple of three for triangles) loads, or underflows.
    #[wasm_bindgen_test(unsupported = test)]
    fn counts_gltf_forbids_are_refused() {
        for mode in [Mode::TriangleStrip, Mode::TriangleFan] {
            assert!(triangle_list(mode, vec![0, 1], "short").is_err());
        }
        assert!(triangle_list(Mode::Triangles, vec![0, 1, 2, 3], "list").is_err());
    }
}
