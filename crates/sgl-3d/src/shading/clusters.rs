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

#[cfg(test)]
pub(crate) fn constants() -> [crate::shading::layout_tests::Constant; 1] {
    use crate::shading::layout_tests::Constant;
    [Constant::new(
        "geometry",
        "CLUSTER_HEADER_WORDS",
        naga::Literal::U32(CLUSTER_HEADER_WORDS as u32),
    )]
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
