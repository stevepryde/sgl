//! Bounding volume hierarchies flattened depth first with escape links
//! instead of a traversal stack. Each primitive occurs in exactly one leaf.
//! Node and leaf words are absolute addresses in the source: a model's BVH
//! is built from zero when it is prepared and rebased where it is placed
//! (`rebase`). One builder and node record serve both levels of the source:
//! a model's BVH, whose leaf records name its triangles, and the instance
//! BVHs (`instances`), whose leaf records name instance entries.
//!
//! Each level splits as a ray-tracing API builds its level (`Split`): a
//! model's BVH, built once off the render thread, for fast traversal by the
//! binned surface area heuristic (Wald 2007, "On fast construction of
//! SAH-based bounding volume hierarchies"); the instance BVHs, built on the
//! render thread every traced frame or after a static edit, for fast
//! building by PBRT 4e's EqualCounts median split, as DXR and Vulkan
//! applications build a TLAS to build fast and a static BLAS to trace fast
//! (practice).
use super::RayMesh;
use glam::Vec3;

/// A node's record in the source. An interior node's first child follows it;
/// a leaf's primitive records start at `first`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Node {
    min: [f32; 3],
    /// The word after this node's subtree: its next sibling or an ancestor's.
    escape: u32,
    max: [f32; 3],
    /// Primitives in a leaf, zero for an interior node.
    count: u32,
    first: u32,
    padding: [u32; 3],
}

/// A node record's words.
const NODE_WORDS: usize = std::mem::size_of::<Node>() / 4;
/// The most primitives a leaf holds, which a walk on the GPU trusts no leaf
/// beyond (`SCENE_BVH_LEAF_PRIMITIVES`).
pub(super) const LEAF_PRIMITIVES: usize = 4;

/// How a BVH splits its nodes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Split {
    /// The binned surface area heuristic: a node's split is the cheapest
    /// among the boundaries of `SAH_BINS` equal bins of its primitives'
    /// centroid bounds along each axis, as the binned builder Godot ships
    /// through Embree evaluates all three (b130438
    /// `thirdparty/embree/kernels/builders/heuristic_binning.h`,
    /// Apache-2.0; practice), each costed as Embree costs one, a traversal
    /// as much as an intersection (`bvh_builder_sah.h` `travCost` and
    /// `intCost`). A node of more than `LEAF_PRIMITIVES` always splits; a
    /// smaller one is a leaf unless its split costs less. Primitives whose
    /// centroids coincide, and nodes deeper than `MOST_SAH_DEPTH`, split at
    /// their median. It replaced EqualCounts for models, whose halves
    /// overlap on long, thin triangles (sgl#187: a dynamic GI probe ray over
    /// Hyperdrive's route visited 204 nodes and tested 34 triangles on the
    /// CPU, 103 and 8 with this build).
    SurfaceArea,
    /// PBRT 4e's EqualCounts: the longest axis of the node's bounds, at the
    /// median by centroid. A tree's size depends only on its primitive
    /// count (`words`).
    EqualCounts,
}

/// The bins along each axis of a node's centroid bounds: Wald 2007's 16.
const SAH_BINS: usize = 16;
/// A node's traversal cost in primitive intersections: Embree's default.
const TRAVERSAL_COST: f32 = 1.;
/// The depth past which a surface area split takes the median instead, so
/// a tree's depth stays bounded however its primitives lie: twice Embree's
/// `maxDepth`, past which Embree makes leaves. Hyperdrive's 134,000-triangle
/// world reaches 25.
const MOST_SAH_DEPTH: usize = 64;

/// The words an EqualCounts BVH over `primitives` leaf records of `T`
/// takes. It grows with their count: one more primitive splits no fewer
/// nodes.
pub(super) fn words<T>(primitives: usize) -> usize {
    fn nodes(primitives: usize) -> usize {
        if primitives > LEAF_PRIMITIVES {
            1 + nodes(primitives / 2) + nodes(primitives - primitives / 2)
        } else {
            1
        }
    }
    if primitives == 0 {
        return 0;
    }
    nodes(primitives) * NODE_WORDS + primitives * std::mem::size_of::<T>() / 4
}

fn centroid<T>(primitive: &Primitive<T>) -> Vec3 {
    primitive.min * 0.5 + primitive.max * 0.5
}

/// Half the surface area of a box, which the SAH compares.
fn half_area(min: Vec3, max: Vec3) -> f32 {
    let extent = (max - min).max(Vec3::ZERO);
    extent.x * extent.y + extent.y * extent.z + extent.z * extent.x
}

/// Splits `primitives` at their median by centroid along the longest axis
/// of `min` to `max` (EqualCounts), or None for a leaf.
fn split_median<T>(primitives: &mut [Primitive<T>], min: Vec3, max: Vec3) -> Option<usize> {
    if primitives.len() <= LEAF_PRIMITIVES {
        return None;
    }
    let middle = primitives.len() / 2;
    let axis = (max - min).max_position();
    primitives.select_nth_unstable_by(middle, |a, b| {
        centroid(a)[axis].total_cmp(&centroid(b)[axis])
    });
    Some(middle)
}

/// Where a node of `primitives`, bounded by `min` and `max`, splits them by
/// the surface area heuristic, ordered so the first that many go left; None
/// for a leaf.
fn split_surface_area<T>(primitives: &mut [Primitive<T>], min: Vec3, max: Vec3) -> Option<usize> {
    let count = primitives.len();
    if count <= 1 {
        return None;
    }
    let (low, high) = primitives.iter().fold(
        (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY)),
        |(low, high), p| (low.min(centroid(p)), high.max(centroid(p))),
    );
    let extent = high - low;
    // Each centroid's bin along `axis`, the first at `low`, the last at
    // `high`.
    let bin = |p: &Primitive<T>, axis: usize| {
        let at = (centroid(p)[axis] - low[axis]) / extent[axis] * SAH_BINS as f32;
        (at as usize).min(SAH_BINS - 1)
    };
    let empty = (
        0,
        Vec3::splat(f32::INFINITY),
        Vec3::splat(f32::NEG_INFINITY),
    );
    // Every axis's bins in one pass over the primitives.
    let mut bins = [[empty; SAH_BINS]; 3];
    for p in primitives.iter() {
        for (axis, bins) in bins.iter_mut().enumerate() {
            if extent[axis] > 0. {
                let b = &mut bins[bin(p, axis)];
                *b = (b.0 + 1, b.1.min(p.min), b.2.max(p.max));
            }
        }
    }
    // The cheapest boundary: (cost, axis, the first bin to the right).
    let mut best: Option<(f32, usize, usize)> = None;
    for axis in (0..3).filter(|&axis| extent[axis] > 0.) {
        let bins = &bins[axis];
        // Each boundary's left count and area, then its right ones.
        let mut left = [(0, 0.); SAH_BINS];
        let mut sweep = empty;
        for (at, b) in bins.iter().enumerate().take(SAH_BINS - 1) {
            sweep = (sweep.0 + b.0, sweep.1.min(b.1), sweep.2.max(b.2));
            left[at + 1] = (sweep.0, half_area(sweep.1, sweep.2));
        }
        let mut sweep = empty;
        for at in (1..SAH_BINS).rev() {
            let b = bins[at];
            sweep = (sweep.0 + b.0, sweep.1.min(b.1), sweep.2.max(b.2));
            let (left_count, left_area) = left[at];
            if left_count == 0 || sweep.0 == 0 {
                continue;
            }
            let cost = left_area * left_count as f32 + half_area(sweep.1, sweep.2) * sweep.0 as f32;
            if best.is_none_or(|(least, ..)| cost < least) {
                best = Some((cost, axis, at));
            }
        }
    }
    // Coincident centroids: no boundary separates them.
    let Some((cost, axis, first_right)) = best else {
        return split_median(primitives, min, max);
    };
    let area = half_area(min, max);
    let split_cost = TRAVERSAL_COST + if area > 0. { cost / area } else { 0. };
    if count <= LEAF_PRIMITIVES && count as f32 <= split_cost {
        return None;
    }
    // Partition by the same bins the costs counted.
    let mut middle = 0;
    for at in 0..count {
        if bin(&primitives[at], axis) < first_right {
            primitives.swap(middle, at);
            middle += 1;
        }
    }
    Some(middle)
}

/// Adds `by` to every word of the BVH `words` holds, as `append` laid it
/// out from its first word, that addresses the source: each node's escape
/// and a leaf's first primitive. Nodes follow each other depth first, a
/// leaf's primitive records after it, so one pass over the words visits
/// every node once.
pub(super) fn rebase(words: &mut [u32], by: u32) {
    let leaf_words = std::mem::size_of::<LeafPrimitive>() / 4;
    let mut at = 0;
    while at < words.len() {
        let node: &mut Node =
            bytemuck::from_bytes_mut(bytemuck::cast_slice_mut(&mut words[at..at + NODE_WORDS]));
        node.escape += by;
        let count = node.count as usize;
        if count > 0 {
            node.first += by;
        }
        at += NODE_WORDS + count * leaf_words;
    }
}

/// A leaf primitive's record in the source: a triangle of one of the model's
/// meshes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct LeafPrimitive {
    mesh: u32,
    triangle: u32,
}

/// A primitive a BVH bounds, and its leaf record.
pub(crate) struct Primitive<T> {
    pub min: Vec3,
    pub max: Vec3,
    pub leaf: T,
}

/// Appends the BVH of `meshes`' triangles to `words` by the surface area
/// heuristic, whose first word is stored at source word `base`, and returns
/// its root's address, zero when there are no triangles. The scene
/// validated the geometry: indices name vertices and positions are finite.
pub(super) fn append(meshes: &[RayMesh<'_>], words: &mut Vec<u32>, base: u32) -> u32 {
    let mut primitives = Vec::new();
    for (mesh_id, mesh) in meshes.iter().enumerate() {
        for (triangle, indices) in mesh.indices.chunks_exact(3).enumerate() {
            let p =
                [0, 1, 2].map(|i| Vec3::from_array(mesh.vertices[indices[i] as usize].position));
            primitives.push(Primitive {
                min: p[0].min(p[1]).min(p[2]),
                max: p[0].max(p[1]).max(p[2]),
                leaf: LeafPrimitive {
                    mesh: mesh_id as u32,
                    triangle: triangle as u32,
                },
            });
        }
    }
    append_primitives(&mut primitives, Split::SurfaceArea, words, base)
}

/// Appends the BVH of `primitives`, split as `split` says, to `words`,
/// whose first word is stored at source word `base`, and returns its root's
/// address, zero when there are none. Their bounds are finite.
pub(super) fn append_primitives<T: bytemuck::Pod>(
    primitives: &mut [Primitive<T>],
    split: Split,
    words: &mut Vec<u32>,
    base: u32,
) -> u32 {
    if primitives.is_empty() {
        return 0;
    }
    let root = base + words.len() as u32;
    build(primitives, (split, 0), words, base);
    root
}

/// Builds the subtree over `primitives`, `depth` below the root.
fn build<T: bytemuck::Pod>(
    primitives: &mut [Primitive<T>],
    (split, depth): (Split, usize),
    words: &mut Vec<u32>,
    base: u32,
) {
    let at = words.len();
    words.resize(at + NODE_WORDS, 0);
    let min = primitives
        .iter()
        .fold(Vec3::splat(f32::INFINITY), |b, p| b.min(p.min));
    let max = primitives
        .iter()
        .fold(Vec3::splat(f32::NEG_INFINITY), |b, p| b.max(p.max));
    let middle = match split {
        Split::SurfaceArea if depth < MOST_SAH_DEPTH => split_surface_area(primitives, min, max),
        _ => split_median(primitives, min, max),
    };
    let (count, first) = if let Some(middle) = middle {
        let (left, right) = primitives.split_at_mut(middle);
        build(left, (split, depth + 1), words, base);
        build(right, (split, depth + 1), words, base);
        (0, 0)
    } else {
        let first = base + words.len() as u32;
        for p in primitives.iter() {
            words.extend_from_slice(bytemuck::cast_slice(&[p.leaf]));
        }
        (primitives.len() as u32, first)
    };
    let node = Node {
        // Outward rounded bounds compensate representational rounding only;
        // they never enlarge the actual triangle or the caller's query interval.
        min: min.to_array().map(|v| v.next_down().max(f32::MIN)),
        max: max.to_array().map(|v| v.next_up().min(f32::MAX)),
        // Exclusive subtree end is also the next sibling/ancestor sibling. A
        // root's escape is the traversal bound, so there is no fixed-depth
        // stack.
        escape: base + words.len() as u32,
        count,
        first,
        padding: [0; 3],
    };
    words[at..at + NODE_WORDS].copy_from_slice(bytemuck::cast_slice(&[node]));
}

/// The WGSL twins of the node and leaf primitive records, in bytes.
#[cfg(test)]
pub(super) fn layout() -> [(&'static str, usize); 9] {
    use std::mem::{offset_of, size_of};
    [
        ("SCENE_BVH_NODE_WORDS", size_of::<Node>()),
        ("SCENE_BVH_NODE_MIN", offset_of!(Node, min)),
        ("SCENE_BVH_NODE_ESCAPE", offset_of!(Node, escape)),
        ("SCENE_BVH_NODE_MAX", offset_of!(Node, max)),
        ("SCENE_BVH_NODE_COUNT", offset_of!(Node, count)),
        ("SCENE_BVH_NODE_FIRST", offset_of!(Node, first)),
        ("SCENE_BVH_PRIMITIVE_WORDS", size_of::<LeafPrimitive>()),
        ("SCENE_BVH_PRIMITIVE_MESH", offset_of!(LeafPrimitive, mesh)),
        (
            "SCENE_BVH_PRIMITIVE_TRIANGLE",
            offset_of!(LeafPrimitive, triangle),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::{
        LEAF_PRIMITIVES, NODE_WORDS, Node, Primitive, RayMesh, Split, append, append_primitives,
        rebase,
    };
    use crate::asset::Vertex;
    use glam::Vec3;
    use wasm_bindgen_test::wasm_bindgen_test;

    /// Checks the subtree at word `at` of `words` lies within `bounds` and
    /// counts each leaf record of `primitives` it names in `seen`; returns
    /// the word after it.
    fn check_subtree(
        words: &[u32],
        at: usize,
        bounds: [Vec3; 2],
        primitives: &[[Vec3; 2]],
        seen: &mut [u32],
    ) -> usize {
        let node: Node = *bytemuck::from_bytes(bytemuck::cast_slice(&words[at..at + NODE_WORDS]));
        let [min, max] = [Vec3::from(node.min), Vec3::from(node.max)];
        assert!(
            min.cmpge(bounds[0]).all() && max.cmple(bounds[1]).all(),
            "node at {at} lies within its parent"
        );
        let escape = node.escape as usize;
        assert!(escape > at, "node at {at} escapes forward");
        if node.count == 0 {
            let right = check_subtree(words, at + NODE_WORDS, [min, max], primitives, seen);
            let end = check_subtree(words, right, [min, max], primitives, seen);
            assert_eq!(end, escape, "node at {at} escapes past its children");
            return escape;
        }
        let count = node.count as usize;
        assert!(count <= LEAF_PRIMITIVES, "leaf at {at} holds {count}");
        for &index in &words[node.first as usize..node.first as usize + count] {
            let [p_min, p_max] = primitives[index as usize];
            assert!(
                p_min.cmpge(min).all() && p_max.cmple(max).all(),
                "leaf at {at} bounds primitive {index}"
            );
            seen[index as usize] += 1;
        }
        assert_eq!(
            escape,
            at + NODE_WORDS + count,
            "leaf at {at} escapes past its records"
        );
        escape
    }

    // Plausible defects: a split's partition that drops or repeats a
    // primitive, or puts none on one side; a leaf of more than
    // LEAF_PRIMITIVES, which the GPU walk does not trust, as a surface area
    // split that keeps coincident centroids together would make; a node
    // whose bounds miss what lies beneath it, which hides it from a ray; an
    // escape that does not lead past its subtree; and an EqualCounts BVH
    // larger than `words` says, which would write an instance BVH past its
    // range. The oracle is the input: each primitive's own bounds, named
    // once. Long, thin primitives as a road's, small ones, and a cluster of
    // coincident centroids no bin separates. CPU only: nothing reaches a
    // GPU walk.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_bvh_names_each_primitive_once_within_its_bounds() {
        let mut seed = 0x2468_ace1_u32;
        let mut random = move || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) as f32 / (1 << 24) as f32
        };
        for (split, count) in [Split::SurfaceArea, Split::EqualCounts]
            .into_iter()
            .flat_map(|split| {
                [1, 2, 3, 4, 5, 6, 7, 8, 9, 17, 64, 1000, 5000].map(|count| (split, count))
            })
        {
            let bounds: Vec<[Vec3; 2]> = (0..count)
                .map(|index| {
                    let at = Vec3::new(random(), random(), random()) * 200. - 100.;
                    let size = match index % 4 {
                        // Along x, as a road's strip.
                        0 => Vec3::new(60. * random(), 0.01, 0.4),
                        1 => Vec3::new(0.2, 8. * random(), 30. * random()),
                        2 => Vec3::splat(0.05),
                        // Coincident centroids.
                        _ => {
                            let half = random();
                            return [Vec3::splat(3. - half), Vec3::splat(3. + half)];
                        }
                    };
                    [at, at + size]
                })
                .collect();
            let mut primitives: Vec<Primitive<u32>> = bounds
                .iter()
                .enumerate()
                .map(|(index, &[min, max])| Primitive {
                    min,
                    max,
                    leaf: index as u32,
                })
                .collect();
            let mut words = Vec::new();
            let root = append_primitives(&mut primitives, split, &mut words, 0);
            assert_eq!(root, 0);
            if split == Split::EqualCounts {
                assert_eq!(words.len(), super::words::<u32>(count), "{count}");
            }
            let mut seen = vec![0; count];
            let everything = [Vec3::splat(f32::MIN), Vec3::splat(f32::MAX)];
            let end = check_subtree(&words, 0, everything, &bounds, &mut seen);
            assert_eq!(
                end,
                words.len(),
                "{split:?} {count}: the root's subtree is the BVH"
            );
            assert!(seen.iter().all(|&n| n == 1), "{split:?} {count}: {seen:?}");
        }
    }

    // Plausible defects: a rebase that misses a node's escape or a leaf's
    // first primitive, adds to an interior node's unused first word, or
    // steps over a leaf's records by the wrong stride and so rebases
    // primitive records as nodes. The oracle is the builder itself, given
    // the final base where the scene used to build models: the words built
    // from zero and rebased must be the words built at the base. CPU only:
    // no rebased words reach a GPU traversal here.
    #[wasm_bindgen_test(unsupported = test)]
    fn rebased_words_match_words_built_in_place() {
        let mut seed = 0x1357_9bdf_u32;
        let mut random = move || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) as f32 / (1 << 24) as f32
        };
        let meshes: Vec<(Vec<Vertex>, Vec<u32>)> = [1usize, 3, 7, 61, 200]
            .into_iter()
            .map(|triangles| {
                let vertices = (0..triangles * 3)
                    .map(|_| Vertex {
                        position: [random() * 16., random() * 16., random() * 16.],
                        ..bytemuck::Zeroable::zeroed()
                    })
                    .collect();
                (vertices, (0..triangles as u32 * 3).collect())
            })
            .collect();
        let rays: Vec<_> = meshes
            .iter()
            .map(|(vertices, indices)| RayMesh { vertices, indices })
            .collect();
        for base in [1, 4096, 0x00ab_cdef] {
            let mut in_place = Vec::new();
            let root = append(&rays, &mut in_place, base);
            let mut rebased = Vec::new();
            let from_zero = append(&rays, &mut rebased, 0);
            rebase(&mut rebased, base);
            assert_eq!(from_zero + base, root);
            assert_eq!(rebased, in_place, "base {base}");
        }
    }
}
