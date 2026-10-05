//! Rust mirror of culling.wgsl: the GPU draw lists' layouts and caps (the
//! architecture's "GPU draw lists and occlusion culling"). The scene keeps
//! each instance's draw candidates, the level chains of meshes with
//! alternatives and the sets they draw in (`scene::candidates`); each
//! GPU-built view keeps its lists, its dispatch, its cluster regions and its
//! draws (`view::draw_list::gpu`); the cull stage writes the view's from the
//! scene's (`stages::cull`).

/// The most alternatives a mesh may register (`Scene::set_mesh_lods`): the
/// instance cull's walk of a level chain is bounded by it (AR-12), and its
/// chain record holds that many levels.
pub const MAX_MESH_LODS: usize = 8;
/// The most sections a mesh holds: `PreparedModel::new` refuses a mesh past
/// it (8,388,608 triangles), so a section cull's stride over a mesh's
/// sections is bounded (AR-12).
pub(crate) const MAX_MESH_SECTIONS: u32 = 65_536;
/// A section's most vertices, three to a triangle: the leaves of a mesh's
/// range hierarchy (`scene::mesh_ranges`), at most 128 triangles each in the
/// mesh's own order, and the vertices of every GPU-built draw's instance,
/// past its triangles a dummy (Bevy 9d12036's meshlet raster draws 128
/// triangles an instance, crates/bevy_pbr/src/meshlet/
/// visibility_buffer_raster_node.rs).
pub(crate) const SECTION_VERTICES: u32 = 384;
/// Invocations in a cull workgroup, as Bevy's mesh preprocessing
/// dispatches 64 (crates/bevy_pbr/src/render/mesh_preprocess.wesl): the
/// instance cull's candidates, the finalize's single workgroup, and the
/// section cull's stride over a mesh's sections.
pub(crate) const CULL_WORKGROUP: u32 = 64;
/// The most workgroups an indirect dispatch takes along x: WebGPU's
/// default `maxComputeWorkgroupsPerDimension`. Past it a dispatch is
/// remapped to two dimensions, as Bevy's `remap_1d_to_2d_dispatch.wesl`
/// remaps it, in integers.
pub(crate) const CULL_MAX_WORKGROUPS: u32 = 65_535;

/// `DrawCandidate::draw_set` of a free slot, which the cull skips.
pub(crate) const NO_SET: u32 = u32::MAX;
/// `DrawCandidate::chain` of a mesh without alternatives.
pub(crate) const NO_CHAIN: u32 = u32::MAX;

/// `DrawSet::flags`: its material casts the directional shadow.
pub(crate) const SET_CASTS_DIRECTIONAL_SHADOW: u32 = 1;

/// `CullView::flags`: the view is the camera, whose population is the
/// `visible` instances and the materials whose group the mask enables;
/// otherwise a directional cascade's, the `capture_visible` instances and
/// the materials that cast in an enabled group.
pub(crate) const CULL_CAMERA: u32 = 1;
/// `CullView::flags`: candidates and sections are tested against the view's
/// clip volume; without it every one passes (the `culling` layer).
pub(crate) const CULL_FRUSTUM: u32 = 2;
/// `CullView::flags`: the clip volume has a near plane. A cascade's has
/// none, since a caster between the light and the cascade casts into it.
pub(crate) const CULL_NEAR: u32 = 4;
/// `CullView::flags`: the view chooses a level of detail (the camera with
/// pixels); otherwise it draws level 0.
pub(crate) const CULL_LOD: u32 = 8;
/// `CullView::flags`: each candidate's appended sections and triangles are
/// counted at `CullView::candidate_statistics` (diagnostics, the camera).
pub(crate) const CULL_CANDIDATE_STATISTICS: u32 = 16;
/// `CullView::flags`: a section whose triangles pair
/// (`scene::rays::model::SECTION_PAIRED`) is appended to its set's paired
/// region, which the view draws indexed over `PAIRED_INDICES`: a cascade's
/// casters. The camera never draws indexed (`GeometryPass::pulled`).
pub(crate) const CULL_PAIRED: u32 = 32;

/// The indices a paired draw's instance draws, over its slots: four a pair
/// of its section's triangles, (a, b, c) then (a, c, d), the slots
/// `paired_corner` in caster.wgsl takes to the section's corners, so the
/// post-transform cache shades the corners a pair shares once, as an
/// indexed draw of the section's own indices would.
pub(crate) const PAIRED_INDICES: [u32; SECTION_VERTICES as usize] = {
    let mut indices = [0; SECTION_VERTICES as usize];
    let mut at = 0;
    while at < indices.len() {
        let pair = (at / 6) as u32;
        indices[at] = 4 * pair + [0, 1, 2, 0, 2, 3][at % 6];
        at += 1;
    }
    indices
};

/// `CullOcclusion::flags`: the early phase tests the camera's candidates
/// and sections against the last submitted frame's depth pyramid.
pub(crate) const OCCLUSION_EARLY: u32 = 1;

/// A view's dispatch buffer: the indirect dispatches the finalizes write
/// (`wgpu::util::DispatchIndirectArgs`, three words each), at these words:
/// the early section cull's, the late instance cull's and the late section
/// cull's.
pub(crate) const DISPATCH_EARLY_SECTIONS: u32 = 0;
pub(crate) const DISPATCH_LATE_INSTANCES: u32 = 3;
pub(crate) const DISPATCH_LATE_SECTIONS: u32 = 6;
pub(crate) const CULL_DISPATCH_WORDS: u32 = 9;

/// One instance's mesh, which a GPU-built view may draw (`DrawCandidate` in
/// culling.wgsl): its bounds in its model's space (the mesh's, or a
/// deforming instance's deformed ones), its object record's index, its
/// mesh's record word in the ray source, its set, its level chain and its
/// mesh's first vertex in its set's positions slab (`scene::geometry`), or
/// `NO_POSITIONS` for a mesh without slab positions.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct DrawCandidate {
    pub bounds_min: [f32; 3],
    pub object: u32,
    pub bounds_max: [f32; 3],
    pub mesh: u32,
    pub draw_set: u32,
    pub chain: u32,
    pub positions: u32,
    pub padding: u32,
}

impl DrawCandidate {
    /// A free slot.
    pub const FREE: Self = Self {
        bounds_min: [0.; 3],
        object: 0,
        bounds_max: [0.; 3],
        mesh: 0,
        draw_set: NO_SET,
        chain: NO_CHAIN,
        positions: crate::shading::vertex::NO_POSITIONS,
        padding: 0,
    };
}

/// One alternative of a mesh (`ChainLevel` in culling.wgsl): its bounds in
/// the base mesh's space, its error bound in metres and its mesh's record
/// word.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct ChainLevel {
    pub bounds_min: [f32; 3],
    pub error: f32,
    pub bounds_max: [f32; 3],
    pub mesh: u32,
}

/// A mesh's alternatives, detailed to coarse (`LodChain` in culling.wgsl).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct LodChain {
    pub levels: [ChainLevel; MAX_MESH_LODS],
    pub count: u32,
    pub padding: [u32; 3],
}

/// What draws with one pipeline and one material (`DrawSet` in
/// culling.wgsl): its region of each GPU-built view's cluster list, in draw
/// instances, its material's visibility group and `SET_*` bits.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct DrawSet {
    pub region: u32,
    pub capacity: u32,
    pub visibility_group: u32,
    pub flags: u32,
}

/// One GPU-built view's cull for a frame (`CullView` in culling.wgsl): what
/// Bevy pushes as immediates, in a uniform, so the browser runs it.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct CullView {
    /// The clip volume's planes in world space (`view::culling::Frustum::planes`),
    /// left, right, bottom, top, far and near: a point `p` lies inside where
    /// `dot(plane, p) >= 0`.
    pub planes: [[f32; 4]; 6],
    /// Each plane's tolerance row, which the cull scales by the pose's
    /// absolute values and the bounds' largest coordinates.
    pub plane_errors: [[f32; 4]; 6],
    /// The camera's unjittered projection times its view, and the product
    /// of their absolute values, which the level of detail's bound takes.
    pub lod_clip_from_world: [[f32; 4]; 4],
    pub lod_magnitude: [[f32; 4]; 4],
    /// The render size the bound counts pixels in.
    pub lod_size: [f32; 2],
    /// The candidate slots the early instance cull runs over, and its
    /// dispatch's workgroups along x.
    pub candidates: u32,
    pub candidate_side: u32,
    /// The frame's enabled visibility groups.
    pub visibility_mask: u32,
    /// `CULL_*` bits.
    pub flags: u32,
    /// Where each candidate's appended sections and triangles start in the
    /// view's draws, in words (`CULL_CANDIDATE_STATISTICS`).
    pub candidate_statistics: u32,
    /// The index of the view's first late command in its draws: set `s`'s
    /// late draw is command `late_command + s`.
    pub late_command: u32,
    /// The late section queue's entries, zero for a view without a late
    /// phase.
    pub queue_capacity: u32,
    /// The index of the view's first paired command in its draws, and where
    /// its cluster list's paired regions start, in draw instances
    /// (`CULL_PAIRED`): set `s`'s paired draw is command `paired_command +
    /// s`, over the region at `paired_region` past the set's.
    pub paired_command: u32,
    pub paired_region: u32,
    pub padding: u32,
}

/// The camera's occlusion test for a frame (`CullOcclusion` in
/// culling.wgsl), which the cull stage writes: the last submitted frame's
/// view-projection with its jitter (`CameraFrame::jittered_view_projection`),
/// through which the early phase projects each object at its previous
/// pose, this frame's, through which the late phase projects it at its
/// pose, the pyramid's levels the test may read, and `OCCLUSION_*` bits.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct CullOcclusion {
    pub previous: [[f32; 4]; 4],
    pub current: [[f32; 4]; 4],
    pub levels: u32,
    pub flags: u32,
    pub padding: [u32; 2],
}

/// The head of a view's lists (`CullLists` in culling.wgsl): for each list,
/// the appends its producer made, the count its consumer runs over, which a
/// finalize clamped to the list's capacity, and that dispatch's workgroups
/// along x, from which the consumer linearises its index: the early visible
/// list (the early section cull's), the late list (the late instance
/// cull's), the late visible list and the late section queue (the late
/// section cull's, one dispatch over both). The entries follow, two words
/// each: of a view's candidate count N, the early visible list at
/// [0, N), the late list at [N, 2N), the late visible list at [2N, 3N) and
/// the queue after them (`late_entries`).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct CullListsHeader {
    pub visible_count: u32,
    pub visible_dispatched: u32,
    pub visible_side: u32,
    pub late_count: u32,
    pub late_dispatched: u32,
    pub late_side: u32,
    pub late_visible_count: u32,
    pub late_visible_dispatched: u32,
    pub queue_count: u32,
    pub queue_dispatched: u32,
    pub late_sections_side: u32,
    pub padding: u32,
}

/// The entries of the lists of a view with `candidates` candidates and a
/// late section queue of `queue` entries, when it culls a late phase: its
/// early visible list, late list and late visible list, a slot for every
/// candidate each, then the queue.
pub(crate) fn late_entries(candidates: u32, queue: u32) -> u64 {
    3 * u64::from(candidates) + u64::from(queue)
}

/// A view's statistics words, at the start of its draws: the sections the
/// section cull appended and their triangles, by mobility.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct CullStatistics {
    pub static_sections: u32,
    pub static_triangles: u32,
    pub moving_sections: u32,
    pub moving_triangles: u32,
}

/// One set's indirect draw in a view's draws, after its statistics, whose
/// instance count the section cull adds its appended sections to: a pulled
/// draw's `wgpu::util::DrawIndirectArgs` (its first four words), or a
/// paired one's `DrawIndexedIndirectArgs`, so one layout serves both.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct DrawCommand {
    /// Vertices of a pulled draw's instance, indices of a paired one's.
    pub count: u32,
    pub instance_count: u32,
    /// A pulled draw's first vertex, a paired one's first index.
    pub first: u32,
    /// A pulled draw's first instance, a paired one's base vertex.
    pub base: u32,
    /// A paired draw's first instance.
    pub first_instance: u32,
}

impl DrawCommand {
    /// A set's command as each frame starts: `SECTION_VERTICES` vertices,
    /// or `PAIRED_INDICES`' as many indices, of no instance, from the first
    /// vertex, index and instance, since the set's region is bound as the
    /// draw-instance buffer.
    pub const RESET: Self = Self {
        count: SECTION_VERTICES,
        instance_count: 0,
        first: 0,
        base: 0,
        first_instance: 0,
    };
}

/// A candidate's words in a view's per-candidate statistics: its appended
/// sections and their triangles.
pub(crate) const CANDIDATE_STATISTICS_WORDS: u32 = 2;

/// Words of `T`.
pub(crate) const fn words<T>() -> u32 {
    (std::mem::size_of::<T>() / 4) as u32
}

/// The workgroups along x and y of a dispatch of `workgroups` in all, past
/// `CULL_MAX_WORKGROUPS` remapped to two dimensions, as the finalize remaps
/// the indirect ones.
pub(crate) fn dispatch_side(workgroups: u32) -> [u32; 2] {
    [
        workgroups.min(CULL_MAX_WORKGROUPS),
        workgroups.div_ceil(CULL_MAX_WORKGROUPS),
    ]
}

/// The constants with WGSL twins.
#[cfg(test)]
pub(crate) fn constants() -> Vec<crate::shading::layout_tests::Constant> {
    use crate::shading::layout_tests::Constant;
    use naga::Literal::U32;
    [
        ("MAX_MESH_LODS", MAX_MESH_LODS as u32),
        ("MAX_MESH_SECTIONS", MAX_MESH_SECTIONS),
        ("SECTION_VERTICES", SECTION_VERTICES),
        ("CULL_WORKGROUP", CULL_WORKGROUP),
        ("CULL_MAX_WORKGROUPS", CULL_MAX_WORKGROUPS),
        ("NO_SET", NO_SET),
        ("NO_CHAIN", NO_CHAIN),
        ("SET_CASTS_DIRECTIONAL_SHADOW", SET_CASTS_DIRECTIONAL_SHADOW),
        ("CULL_CAMERA", CULL_CAMERA),
        ("CULL_FRUSTUM", CULL_FRUSTUM),
        ("CULL_NEAR", CULL_NEAR),
        ("CULL_LOD", CULL_LOD),
        ("CULL_CANDIDATE_STATISTICS", CULL_CANDIDATE_STATISTICS),
        ("CULL_PAIRED", CULL_PAIRED),
        ("OCCLUSION_EARLY", OCCLUSION_EARLY),
        ("DISPATCH_EARLY_SECTIONS", DISPATCH_EARLY_SECTIONS),
        ("DISPATCH_LATE_INSTANCES", DISPATCH_LATE_INSTANCES),
        ("DISPATCH_LATE_SECTIONS", DISPATCH_LATE_SECTIONS),
        ("CULL_DISPATCH_WORDS", CULL_DISPATCH_WORDS),
        ("CULL_STATISTICS_WORDS", words::<CullStatistics>()),
        (
            "CULL_STATIC_SECTIONS",
            (std::mem::offset_of!(CullStatistics, static_sections) / 4) as u32,
        ),
        (
            "CULL_STATIC_TRIANGLES",
            (std::mem::offset_of!(CullStatistics, static_triangles) / 4) as u32,
        ),
        (
            "CULL_MOVING_SECTIONS",
            (std::mem::offset_of!(CullStatistics, moving_sections) / 4) as u32,
        ),
        (
            "CULL_MOVING_TRIANGLES",
            (std::mem::offset_of!(CullStatistics, moving_triangles) / 4) as u32,
        ),
        ("DRAW_COMMAND_WORDS", words::<DrawCommand>()),
        (
            "DRAW_COMMAND_INSTANCE_COUNT",
            (std::mem::offset_of!(DrawCommand, instance_count) / 4) as u32,
        ),
        ("CANDIDATE_STATISTICS_WORDS", CANDIDATE_STATISTICS_WORDS),
    ]
    .into_iter()
    .map(|(name, value)| Constant::new("cull", name, U32(value)))
    .collect()
}

#[cfg(test)]
pub(crate) fn mirrors() -> [crate::shading::layout_tests::Mirror; 7] {
    use crate::shading::layout_tests::mirror;
    [
        mirror!(
            "cull",
            "DrawCandidate",
            DrawCandidate,
            [
                bounds_min, object, bounds_max, mesh, draw_set, chain, positions
            ]
        ),
        mirror!(
            "cull",
            "ChainLevel",
            ChainLevel,
            [bounds_min, error, bounds_max, mesh]
        ),
        mirror!("cull", "LodChain", LodChain, [levels, count]),
        mirror!(
            "cull",
            "DrawSet",
            DrawSet,
            [region, capacity, visibility_group, flags]
        ),
        mirror!(
            "cull",
            "CullView",
            CullView,
            [
                planes,
                plane_errors,
                lod_clip_from_world,
                lod_magnitude,
                lod_size,
                candidates,
                candidate_side,
                visibility_mask,
                flags,
                candidate_statistics,
                late_command,
                queue_capacity,
                paired_command,
                paired_region,
            ]
        ),
        mirror!(
            "cull",
            "CullOcclusion",
            CullOcclusion,
            [previous, current, levels, flags]
        ),
        mirror!(
            "cull",
            "CullLists",
            CullListsHeader,
            [
                visible_count,
                visible_dispatched,
                visible_side,
                late_count,
                late_dispatched,
                late_side,
                late_visible_count,
                late_visible_dispatched,
                queue_count,
                queue_dispatched,
                late_sections_side,
            ]
        ),
    ]
}
