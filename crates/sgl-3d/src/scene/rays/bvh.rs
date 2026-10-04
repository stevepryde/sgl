//! PBRT 4e EqualCounts primitive subdivision, flattened depth first with escape
//! links instead of a traversal stack. Each primitive occurs in exactly one
//! leaf. Node and leaf words are absolute addresses in the source. One builder
//! and node record serve both levels of the source: a model's BVH, whose leaf
//! records name its triangles, and the instance BVHs (`instances`), whose leaf
//! records name instance entries.
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
/// The most primitives a leaf holds.
const LEAF_PRIMITIVES: usize = 4;

/// Where a node of `primitives` splits them, or None for a leaf. The split
/// depends only on their count, so a tree's size does too.
fn split(primitives: usize) -> Option<usize> {
    (primitives > LEAF_PRIMITIVES).then_some(primitives / 2)
}

/// The words a BVH over `primitives` leaf records of `T` takes. It grows
/// with their count: one more primitive splits no fewer nodes.
pub(super) fn words<T>(primitives: usize) -> usize {
    fn nodes(primitives: usize) -> usize {
        match split(primitives) {
            Some(middle) => 1 + nodes(middle) + nodes(primitives - middle),
            None => 1,
        }
    }
    if primitives == 0 {
        return 0;
    }
    nodes(primitives) * NODE_WORDS + primitives * std::mem::size_of::<T>() / 4
}

/// The words `append` writes for a model of `triangles`.
pub(super) fn model_words(triangles: usize) -> usize {
    words::<LeafPrimitive>(triangles)
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

/// Appends the BVH of `meshes`' triangles to `words`, whose first word is
/// stored at source word `base`, and returns its root's address, zero when
/// there are no triangles. The scene validated the geometry: indices name
/// vertices and positions are finite.
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
    append_primitives(&mut primitives, words, base)
}

/// Appends the BVH of `primitives` to `words`, whose first word is stored at
/// source word `base`, and returns its root's address, zero when there are
/// none. Their bounds are finite.
pub(super) fn append_primitives<T: bytemuck::Pod>(
    primitives: &mut [Primitive<T>],
    words: &mut Vec<u32>,
    base: u32,
) -> u32 {
    if primitives.is_empty() {
        return 0;
    }
    let root = base + words.len() as u32;
    build(primitives, words, base);
    root
}

fn build<T: bytemuck::Pod>(primitives: &mut [Primitive<T>], words: &mut Vec<u32>, base: u32) {
    let at = words.len();
    words.resize(at + NODE_WORDS, 0);
    let min = primitives
        .iter()
        .fold(Vec3::splat(f32::INFINITY), |b, p| b.min(p.min));
    let max = primitives
        .iter()
        .fold(Vec3::splat(f32::NEG_INFINITY), |b, p| b.max(p.max));
    let (count, first) = if let Some(middle) = split(primitives.len()) {
        let extent = max - min;
        let axis = if extent.x >= extent.y && extent.x >= extent.z {
            0
        } else if extent.y >= extent.z {
            1
        } else {
            2
        };
        primitives.select_nth_unstable_by(middle, |a, b| {
            (a.min[axis] * 0.5 + a.max[axis] * 0.5)
                .total_cmp(&(b.min[axis] * 0.5 + b.max[axis] * 0.5))
        });
        let (left, right) = primitives.split_at_mut(middle);
        build(left, words, base);
        build(right, words, base);
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
