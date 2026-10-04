//! Deformation's records in the scene source, mirrored by
//! `deformation.wgsl`: what the scene writes for deforming models and
//! instances (`scene::deformation`), the deform stage reads and writes, and
//! pulled raster passes read.
/// A skinned vertex's influences in the scene source (`Influence` in
/// deformation.wgsl's words): its four joints, then their normalized
/// weights.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct InfluenceRecord {
    pub joints: [u32; 4],
    pub weights: [f32; 4],
}

/// A morph target's displacement of one vertex in the scene source.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct MorphDeltaRecord {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub tangent: [f32; 3],
}

/// A joint matrix's words, column-major.
pub(crate) const JOINT_WORDS: u32 = 16;
/// A deforming instance's position of one vertex, in each of two slots: the
/// record shadow casters read (`CasterVertex`).
pub(crate) const DEFORMED_POSITION_WORDS: u32 = words::<crate::shading::vertex::CasterVertex>();
/// A deforming instance's normal and tangent with handedness of one vertex.
pub(crate) const DEFORMED_NORMAL_WORDS: u32 = 7;

/// `DeformDispatch` in deformation.wgsl: one mesh of one deforming instance.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct DeformDispatch {
    /// The mesh's first vertex record.
    pub vertices: u32,
    pub vertex_count: u32,
    /// Its influences' first word; zero when unskinned.
    pub influences: u32,
    /// Its morph targets' first word: their weight indices, then each
    /// target's displacement of every vertex.
    pub morph_targets: u32,
    pub morph_target_count: u32,
    /// The instance's first joint matrix and morph weight.
    pub joints: u32,
    pub weights: u32,
    /// Where the instance's deformed positions and normals of the mesh go.
    pub positions: u32,
    pub normals: u32,
}

/// Words of `T`.
pub(crate) const fn words<T>() -> u32 {
    (std::mem::size_of::<T>() / 4) as u32
}

#[cfg(test)]
pub(crate) fn constants() -> Vec<crate::shading::layout_tests::Constant> {
    use std::mem::offset_of;
    let word = |bytes: usize| naga::Literal::U32((bytes / 4) as u32);
    [
        (
            "INFLUENCE_WORDS",
            word(std::mem::size_of::<InfluenceRecord>()),
        ),
        (
            "INFLUENCE_WEIGHTS",
            word(offset_of!(InfluenceRecord, weights)),
        ),
        (
            "MORPH_DELTA_WORDS",
            word(std::mem::size_of::<MorphDeltaRecord>()),
        ),
        (
            "MORPH_DELTA_NORMAL",
            word(offset_of!(MorphDeltaRecord, normal)),
        ),
        (
            "MORPH_DELTA_TANGENT",
            word(offset_of!(MorphDeltaRecord, tangent)),
        ),
        ("JOINT_WORDS", naga::Literal::U32(JOINT_WORDS)),
        (
            "DEFORMED_POSITION_WORDS",
            naga::Literal::U32(DEFORMED_POSITION_WORDS),
        ),
        (
            "DEFORMED_NORMAL_WORDS",
            naga::Literal::U32(DEFORMED_NORMAL_WORDS),
        ),
    ]
    .into_iter()
    .map(|(name, value)| crate::shading::layout_tests::Constant::new("deform", name, value))
    .collect()
}

#[cfg(test)]
pub(crate) fn mirrors() -> [crate::shading::layout_tests::Mirror; 1] {
    [crate::shading::layout_tests::mirror!(
        "deform",
        "DeformDispatch",
        DeformDispatch,
        [
            vertices,
            vertex_count,
            influences,
            morph_targets,
            morph_target_count,
            joints,
            weights,
            positions,
            normals
        ]
    )]
}
