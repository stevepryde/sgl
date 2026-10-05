//! Rust mirror of clusters.wgsl: a view's cluster grid, and the words of a
//! cluster's header.

/// A view's cluster grid (`ClusterGrid` in clusters.wgsl).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct ClusterGrid {
    /// Clusters along x, y and z.
    pub dimensions: [u32; 3],
    /// 1 for an orthographic view.
    pub orthographic: u32,
    /// Clusters per pixel along x and y, then the z slicing's factors.
    pub factors: [f32; 4],
}

impl ClusterGrid {
    /// One cluster: culled lists, whatever the point.
    pub const LIST: Self = Self {
        dimensions: [1; 3],
        orthographic: 0,
        factors: [0.; 4],
    };
}

/// A cluster's header in `Clusters::data`: its first item, then its live
/// lights', baked lights' and decals' counts.
pub(crate) const CLUSTER_HEADER_WORDS: usize = 4;
/// The most lights and decals one cluster lists (`CLUSTER_MOST_ITEMS`,
/// AR-12): the top of Godot's `rendering/limits/cluster_builder/
/// max_clustered_elements` range, the most elements its cluster builder
/// holds a view (servers/rendering/rendering_server.cpp, revision
/// ed1daf0bf001b61586d9930840f2f1394092c079; its default is 512). A single
/// cluster lists a probe capture's every light and decal in the scene, a
/// ray hit's every one reaching the view and a dynamic GI probe ray's every
/// one reaching the volume, so this is a scene's limit there.
/// Past it a cluster keeps its live lights, then its baked lights, then its
/// decals, in the scene's order.
pub(crate) const CLUSTER_MOST_ITEMS: usize = 8192;

#[cfg(test)]
pub(crate) fn constants() -> [crate::shading::layout_tests::Constant; 2] {
    use crate::shading::layout_tests::Constant;
    [
        ("CLUSTER_HEADER_WORDS", CLUSTER_HEADER_WORDS),
        ("CLUSTER_MOST_ITEMS", CLUSTER_MOST_ITEMS),
    ]
    .map(|(name, value)| Constant::new("geometry", name, naga::Literal::U32(value as u32)))
}

#[cfg(test)]
pub(crate) fn mirrors() -> [crate::shading::layout_tests::Mirror; 1] {
    use crate::shading::layout_tests::mirror;
    [mirror!(
        "geometry",
        "ClusterGrid",
        ClusterGrid,
        [dimensions, orthographic, factors]
    )]
}
