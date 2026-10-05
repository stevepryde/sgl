//! Local-light shadows' static casters: a mesh's triangles grouped once, in
//! its model's space, into 16 m cells, so a shadow face draws only the part
//! of a static instance's mesh within its light's range. Bounds enclose the
//! actual triangles, including triangles that extend beyond their
//! centroid's cell. Cells are ordered by 64 m group, so a face tests a
//! group's bounds before its cells'. A static instance keeps its clusters'
//! and groups' world bounds (`posed`), so a face tests them without
//! transforming them. Their indices live in the scene's geometry buffers
//! (`geometry`).
use super::geometry::GeometryRange;
use super::static_edits::posed_bounds;
use crate::asset::Vertex;
use glam::{Mat4, Vec3};
use std::{collections::BTreeMap, ops::Range};

/// A cell's side in metres.
const CELL: f32 = 16.;
/// Cells along each side of a group.
const GROUP_CELLS: i32 = 4;

pub(crate) struct Cluster {
    pub bounds: [Vec3; 2],
    pub indices: Range<u32>,
}

/// One mesh's clusters: indices over its vertices, cell by cell, cells in
/// group order, placed in the scene's geometry buffers.
pub(crate) struct MeshClusters {
    pub indices: GeometryRange,
    pub clusters: Vec<Cluster>,
    /// Each group's clusters: consecutive ranges of `clusters`.
    pub groups: Vec<Range<usize>>,
}

/// One mesh's clusters before they are placed: their indices, and the
/// clusters and groups that name ranges of them.
pub(crate) struct ClusteredIndices {
    pub indices: Vec<u32>,
    pub clusters: Vec<Cluster>,
    pub groups: Vec<Range<usize>>,
}

/// A mesh's clusters' and groups' world bounds at one pose, in their order.
#[derive(Default)]
pub(crate) struct PosedClusters {
    pub clusters: Vec<[Vec3; 2]>,
    pub groups: Vec<[Vec3; 2]>,
}

impl MeshClusters {
    /// Each cluster's and group's world bounds at `pose`.
    pub fn posed(&self, pose: Mat4) -> PosedClusters {
        let clusters: Vec<_> = self
            .clusters
            .iter()
            .map(|cluster| posed_bounds(cluster.bounds, pose))
            .collect();
        let groups = self
            .groups
            .iter()
            .map(|group| {
                clusters[group.clone()].iter().fold(
                    [Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY)],
                    |bounds, cluster| [bounds[0].min(cluster[0]), bounds[1].max(cluster[1])],
                )
            })
            .collect();
        PosedClusters { clusters, groups }
    }
}

impl ClusteredIndices {
    /// None for a mesh without triangles.
    pub fn new(vertices: &[Vertex], indices: &[u32]) -> Option<Self> {
        // Cells by group, then by cell.
        let mut cells = BTreeMap::<([i32; 3], [i32; 3]), Vec<([u32; 3], [Vec3; 2])>>::new();
        for triangle in indices.chunks_exact(3) {
            let ids = [triangle[0], triangle[1], triangle[2]];
            let p = ids.map(|id| Vec3::from_array(vertices[id as usize].position));
            let key = ((p[0] + p[1] + p[2]) / (3. * CELL))
                .floor()
                .as_ivec3()
                .to_array();
            let group = key.map(|cell| cell.div_euclid(GROUP_CELLS));
            cells
                .entry((group, key))
                .or_default()
                .push((ids, [p[0].min(p[1]).min(p[2]), p[0].max(p[1]).max(p[2])]));
        }
        let mut clustered = Vec::<u32>::with_capacity(indices.len());
        let mut clusters = Vec::with_capacity(cells.len());
        let mut groups: Vec<Range<usize>> = Vec::new();
        let mut current = None;
        for ((group, _), triangles) in cells {
            if current != Some(group) {
                current = Some(group);
                groups.push(clusters.len()..clusters.len());
            }
            let start = clustered.len() as u32;
            let mut bounds = [Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY)];
            for (ids, triangle_bounds) in triangles {
                clustered.extend(ids);
                bounds[0] = bounds[0].min(triangle_bounds[0]);
                bounds[1] = bounds[1].max(triangle_bounds[1]);
            }
            clusters.push(Cluster {
                bounds,
                indices: start..clustered.len() as u32,
            });
            groups.last_mut().expect("a group per cluster").end = clusters.len();
        }
        (!clustered.is_empty()).then_some(Self {
            indices: clustered,
            clusters,
            groups,
        })
    }
}
