//! Each mesh's retained hierarchy of triangle ranges: consecutive groups of
//! at most 128 triangles, in the mesh's own primitive order, with their
//! bounds. Camera culling (`view::culling`) walks it against a view.
use crate::asset::Vertex;
use glam::Vec3;
use std::ops::Range;

/// The indices of a leaf's triangles.
pub(crate) const INDICES_PER_LEAF: usize = 128 * 3;

pub(crate) struct Node {
    pub bounds: [Vec3; 2],
    pub indices: Range<u32>,
    /// First node after this subtree; a leaf points to its immediate successor.
    pub end: usize,
}

pub(crate) struct MeshRanges {
    pub nodes: Vec<Node>,
}

impl MeshRanges {
    pub fn bounds(&self) -> Option<[Vec3; 2]> {
        self.nodes.first().map(|node| node.bounds)
    }
    pub fn new(vertices: &[Vertex], indices: &[u32]) -> Self {
        let mut result = Self { nodes: Vec::new() };
        let leaves: Vec<_> = indices[..indices.len() / 3 * 3]
            .chunks(INDICES_PER_LEAF)
            .map(|indices| {
                indices.iter().fold(
                    [Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY)],
                    |bounds, &index| {
                        let p = Vec3::from_array(vertices[index as usize].position);
                        [bounds[0].min(p), bounds[1].max(p)]
                    },
                )
            })
            .collect();
        if !leaves.is_empty() {
            result.build(&leaves, 0, indices.len() as u32 / 3 * 3);
        }
        result
    }

    fn build(&mut self, bounds: &[[Vec3; 2]], start: u32, end: u32) {
        let at = self.nodes.len();
        self.nodes.push(Node {
            bounds: bounds[0],
            indices: start..end,
            end: at + 1,
        });
        if bounds.len() > 1 {
            let split = bounds.len() / 2;
            let middle = start + (split * INDICES_PER_LEAF) as u32;
            self.build(&bounds[..split], start, middle);
            let right = self.nodes.len();
            self.build(&bounds[split..], middle, end);
            self.nodes[at].bounds = [
                self.nodes[at + 1].bounds[0].min(self.nodes[right].bounds[0]),
                self.nodes[at + 1].bounds[1].max(self.nodes[right].bounds[1]),
            ];
            self.nodes[at].end = self.nodes.len();
        }
    }
}
