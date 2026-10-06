//! The stage's buffers as its shaders lay them out (`common.wgsl` and
//! `allocate.wgsl`, tied by the layout test), the constants it shares with
//! them, and the frame's ray budget.

/// Workgroups to a row of a two-dimensional dispatch (`DDGI_GROUP_ROW`).
pub(super) const GROUP_ROW: u32 = 32768;

/// A workgroup per probe of `count`, in rows of `GROUP_ROW`.
pub(super) fn dispatch(count: u32) -> [u32; 2] {
    [count.min(GROUP_ROW), count.div_ceil(GROUP_ROW)]
}

/// `DdgiVolume` in common.wgsl.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct VolumeUniform {
    pub origin: [f32; 3],
    pub max_distance: f32,
    pub spacing: [f32; 3],
    pub max_rays: u32,
    pub probes: [u32; 3],
    pub probe_count: u32,
    pub rotation: [[f32; 4]; 3],
    pub frustum: [[f32; 4]; 6],
    pub eye: [f32; 3],
    pub frame: u32,
    pub rays: u32,
    pub budget: u32,
    pub traced: u32,
    pub padding: u32,
    pub scroll: [u32; 3],
    pub padding_scroll: u32,
    pub scrolled: [i32; 3],
    pub moving_count: u32,
    pub changed: u32,
    pub padding_changed: [u32; 3],
}

/// `DdgiBounds` in common.wgsl.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct BoundsUniform {
    pub min: [f32; 3],
    pub padding_min: f32,
    pub max: [f32; 3],
    pub padding_max: f32,
}
/// The most moving instances' bounds a frame takes, the nearest the camera
/// (`DDGI_MOST_MOVING_BOUNDS`): the cap on the allocation's walk over them
/// (AR-12), generously above the moving instances a volume's cells hold in
/// a game's frame.
pub(super) const MOST_MOVING_BOUNDS: u32 = 256;

/// The allocation's buffer (`DdgiAllocation` in allocate.wgsl): the
/// trace's indirect dispatch and ray count, the blends' dispatch and the
/// count of probes that trace, the starting probes' words and rays, whether
/// the volume paused, the stride the frame's periods take, the rays
/// reserved against the budget, the rays the shortened turns may take and
/// have taken, the blended probes' requests under each stride, and the
/// starting probes' bins. The trace and the blends read the counts in the
/// volume's uniform.
#[repr(C)]
pub(super) struct Allocation {
    pub groups: [u32; 3],
    pub rays: u32,
    pub blend_groups: [u32; 3],
    pub traced: u32,
    pub ramp_bins: u32,
    pub ramp_room: u32,
    pub ramp_taken: u32,
    pub unblended: u32,
    pub unblended_rays: u32,
    pub paused: u32,
    pub stride: u32,
    pub reserved: u32,
    pub spare: u32,
    pub spare_taken: u32,
    pub demand: [u32; STRIDES as usize],
    pub bins: [u32; RAMP_BINS as usize],
}
pub(super) const ALLOCATION_RAYS: u64 = std::mem::offset_of!(Allocation, rays) as u64;
pub(super) const ALLOCATION_BLEND_GROUPS: u64 =
    std::mem::offset_of!(Allocation, blend_groups) as u64;
pub(super) const ALLOCATION_TRACED: u64 = std::mem::offset_of!(Allocation, traced) as u64;
pub(super) const ALLOCATION_BYTES: u64 = std::mem::size_of::<Allocation>() as u64;

/// `DdgiConvergence` in common.wgsl: the frame's sums of the active probes'
/// variability and their longest period, which the allocation and the
/// blends find and each frame clears, and the windows the settle pass
/// averages them over.
#[repr(C)]
pub(super) struct Convergence {
    pub variability: u32,
    pub probes: u32,
    pub longest: u32,
    pub average: f32,
    pub window_sum: f32,
    pub window_updates: f32,
    pub window_turns: f32,
    pub previous: f32,
    pub converged: u32,
}
pub(super) const CONVERGENCE_SUMS: u64 = std::mem::offset_of!(Convergence, average) as u64;
pub(super) const CONVERGENCE_BYTES: u64 = std::mem::size_of::<Convergence>() as u64;
/// The starting probes' bins of distance (`RAMP_BINS` in allocate.wgsl).
pub(super) const RAMP_BINS: u32 = 1024;
/// The lengthenings of every period the allocation weighs (`DDGI_STRIDES`).
pub(super) const STRIDES: u32 = 7;
/// The frame's most rays, fixed rays included, in probes at the tier's
/// most: 32,768 at High, 16,384 at Low. Wicked's surfel GI traces at most
/// 100,000 a frame (4323a33c `SURFEL_RAY_BUDGET`) on hardware ray tracing;
/// on SGL3D's portable walk this many cost Hyperdrive's 3,179-probe course
/// about 3.4 ms a frame in motion on an Apple M5 (#196). A restart starts
/// as many probes as the budget holds at their starting rays, nearest
/// first: 128 a frame within a spacing of the camera, at the tier's most
/// rays, and more farther out, where a probe starts with fewer.
pub(super) const BUDGET_PROBES: u32 = 128;

/// The frame's most rays, fixed rays included, at the tier's `max_rays`:
/// the ray list holds them.
pub(super) fn budget(max_rays: u32) -> u32 {
    max_rays * BUDGET_PROBES
}
pub(super) const UNIFORM_RAYS: u64 = std::mem::offset_of!(VolumeUniform, rays) as u64;
pub(super) const UNIFORM_TRACED: u64 = std::mem::offset_of!(VolumeUniform, traced) as u64;

/// The layouts this module mirrors.
#[cfg(test)]
pub(super) fn mirrors() -> Vec<crate::shading::layout_tests::Mirror> {
    use crate::shading::layout_tests::mirror;
    vec![
        mirror!(
            "dynamic_gi_trace",
            "DdgiVolume",
            VolumeUniform,
            [
                origin,
                max_distance,
                spacing,
                max_rays,
                probes,
                probe_count,
                rotation,
                frustum,
                eye,
                frame,
                rays,
                budget,
                traced,
                scroll,
                scrolled,
                moving_count,
                changed,
            ]
        ),
        mirror!(
            "dynamic_gi_allocate",
            "DdgiConvergence",
            Convergence,
            [
                variability,
                probes,
                longest,
                average,
                window_sum,
                window_updates,
                window_turns,
                previous,
                converged,
            ]
        ),
        mirror!(
            "dynamic_gi_allocate",
            "DdgiBounds",
            BoundsUniform,
            [min, max]
        ),
        mirror!(
            "dynamic_gi_allocate",
            "DdgiAllocation",
            Allocation,
            [
                groups,
                rays,
                blend_groups,
                traced,
                ramp_bins,
                ramp_room,
                ramp_taken,
                unblended,
                unblended_rays,
                paused,
                stride,
                reserved,
                spare,
                spare_taken,
                demand,
                bins,
            ]
        ),
    ]
}

/// The constants this module shares with the shaders.
#[cfg(test)]
pub(super) fn constants() -> Vec<crate::shading::layout_tests::Constant> {
    use crate::shading::layout_tests::Constant;
    vec![
        Constant::new(
            "dynamic_gi_trace",
            "DDGI_GROUP_ROW",
            naga::Literal::U32(GROUP_ROW),
        ),
        Constant::new(
            "dynamic_gi_allocate",
            "RAMP_BINS",
            naga::Literal::U32(RAMP_BINS),
        ),
        Constant::new(
            "dynamic_gi_allocate",
            "DDGI_STRIDES",
            naga::Literal::U32(STRIDES),
        ),
        Constant::new(
            "dynamic_gi_allocate",
            "DDGI_MOST_MOVING_BOUNDS",
            naga::Literal::U32(MOST_MOVING_BOUNDS),
        ),
    ]
}
