//! AMD's FidelityFX shadow denoiser over the ray-traced shadow stage's
//! first four slots, as Wicked Engine runs it (2ff1d9e
//! `Postprocess_RTShadow`, wiRenderer.cpp 15629–15831): tile classification
//! against the previous frame's moments and the reprojected history, then
//! three edge-stopping filter passes at step sizes 1, 2 and 4, the last
//! recovering contrast. One dispatch a pass covers the four slots, a slot a
//! group's z, where Wicked dispatches each light apart. Its result replaces
//! those slots' temporal blend.
use super::{Pass, texture};
use crate::shading;
use crate::view::cached_group::CachedGroup;

/// The entry points the denoiser's pipelines are created with.
pub(crate) const TILE_CLASSIFICATION_ENTRY: &str = "traced_denoise_tile_classification";
pub(crate) const FILTER_ENTRY: &str = "traced_denoise_filter";
/// The slots the denoiser filters (`TRACED_DENOISED_SLOTS`): Wicked's first
/// four.
pub(super) const DENOISED_SLOTS: u32 = 4;
/// The filter passes' step sizes are 1 << pass.
const FILTER_PASSES: u32 = 3;

static UTIL: shading::Module = shading::Module {
    name: "ffx_denoiser_shadows_util",
    source: include_str!("ffx_denoiser_shadows_util.wgsl"),
    deps: &[],
};
static FFX_TILE_CLASSIFICATION: shading::Module = shading::Module {
    name: "ffx_denoiser_shadows_tileclassification",
    source: include_str!("ffx_denoiser_shadows_tileclassification.wgsl"),
    deps: &[&UTIL],
};
static FFX_FILTER: shading::Module = shading::Module {
    name: "ffx_denoiser_shadows_filter",
    source: include_str!("ffx_denoiser_shadows_filter.wgsl"),
    deps: &[&UTIL],
};
/// The passes' reading of the trace's half-resolution depth and normals for
/// AMD's callbacks.
static COMMON: shading::Module = shading::Module {
    name: "traced_denoise_common",
    source: include_str!("traced_denoise_common.wgsl"),
    deps: &[&super::COMMON],
};
pub(crate) static TILE_CLASSIFICATION: shading::Module = shading::Module {
    name: "traced_denoise_tileclassification",
    source: include_str!("traced_denoise_tileclassification.wgsl"),
    deps: &[
        &FFX_TILE_CLASSIFICATION,
        &shading::SHADOW_MASK_SLOTS,
        &COMMON,
    ],
};
pub(crate) static FILTER: shading::Module = shading::Module {
    name: "traced_denoise_filter",
    source: include_str!("traced_denoise_filter.wgsl"),
    deps: &[&FFX_FILTER, &COMMON],
};

/// The format of the tracing pixels' shading normals the trace writes for
/// the denoiser.
pub(super) const NORMAL_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// The denoiser's targets at one tracing size: each 8×4 tile's hit masks
/// and each tracing pixel's shading normal, which the trace writes; each
/// 8×8 group's metadata; the reprojected and filtered visibility and
/// variance, packed as two halves (A, then B, which keeps the first filter
/// pass's result for the next frame); the moments' pair, a layer a denoised
/// slot; and the denoised visibility, a word a tracing pixel, packed as the
/// trace packs its first word.
pub(super) struct Targets {
    pub tiles: wgpu::TextureView,
    pub normal: wgpu::TextureView,
    metadata: wgpu::TextureView,
    scratch: [wgpu::TextureView; 2],
    moments: [wgpu::TextureView; 2],
    pub denoised: wgpu::Buffer,
}

impl Targets {
    pub fn new(device: &wgpu::Device, reduced: [u32; 2]) -> Self {
        let [width, height] = reduced;
        let layers = |label, format| texture(device, label, reduced, DENOISED_SLOTS, format);
        Self {
            tiles: texture(
                device,
                "ray-traced shadow tiles",
                [width.div_ceil(8), height.div_ceil(4)],
                1,
                wgpu::TextureFormat::Rgba32Uint,
            ),
            metadata: texture(
                device,
                "ray-traced shadow tile metadata",
                [width.div_ceil(8), height.div_ceil(8)],
                DENOISED_SLOTS,
                wgpu::TextureFormat::R32Uint,
            ),
            scratch: [0, 1].map(|_| {
                layers(
                    "ray-traced shadow denoise scratch",
                    wgpu::TextureFormat::R32Uint,
                )
            }),
            moments: [0, 1].map(|_| {
                layers(
                    "ray-traced shadow moments",
                    wgpu::TextureFormat::Rgba16Float,
                )
            }),
            normal: texture(
                device,
                "ray-traced shadow normals",
                reduced,
                1,
                NORMAL_FORMAT,
            ),
            denoised: crate::counters::buffer(
                device,
                &wgpu::BufferDescriptor {
                    label: Some("ray-traced shadow denoised"),
                    size: u64::from(width) * u64::from(height) * 4,
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                },
            ),
        }
    }
}

/// What the denoiser reads beyond its own targets.
pub(super) struct Inputs<'a> {
    /// The G-buffer's depth, which the tile classification reprojects.
    pub depth: &'a wgpu::TextureView,
    pub motion: &'a wgpu::TextureView,
    /// The tracing resolution's linear depth this frame and last.
    pub half_depth: &'a wgpu::TextureView,
    pub previous_depth: &'a wgpu::TextureView,
    pub params: &'a wgpu::Buffer,
    pub slot_table: &'a wgpu::Buffer,
    pub reduced: [u32; 2],
    /// The halves of the moments' pair this frame writes and reads.
    pub current: usize,
}

pub(super) struct Denoiser {
    classification: Pass,
    filters: [Pass; FILTER_PASSES as usize],
}

impl Denoiser {
    pub fn new(device: &wgpu::Device) -> Self {
        Self {
            classification: Pass::new(device, &TILE_CLASSIFICATION, TILE_CLASSIFICATION_ENTRY, &[]),
            filters: std::array::from_fn(|pass| {
                Pass::new(
                    device,
                    &FILTER,
                    FILTER_ENTRY,
                    &[("filter_pass", pass as f64)],
                )
            }),
        }
    }

    /// Denoises the slots' traced visibility, whose tiles `targets` holds,
    /// into its denoised words.
    pub fn encode(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        timing: Option<&crate::timing::GpuTiming>,
        targets: &Targets,
        inputs: Inputs<'_>,
    ) {
        let resource = wgpu::BindingResource::TextureView;
        let (current, previous) = (inputs.current, 1 - inputs.current);
        let [width, height] = inputs.reduced;
        let groups = [width.div_ceil(8), height.div_ceil(8), DENOISED_SLOTS];
        let dispatch = |encoder: &mut wgpu::CommandEncoder,
                        pipeline: &wgpu::ComputePipeline,
                        group: &wgpu::BindGroup,
                        label: &'static str| {
            let mut compute = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some(label),
                timestamp_writes: timing.and_then(|t| t.compute_pass(label)),
            });
            compute.set_pipeline(pipeline);
            compute.set_bind_group(0, group, &[]);
            compute.dispatch_workgroups(groups[0], groups[1], groups[2]);
        };
        let [scratch_a, scratch_b] = &targets.scratch;
        let classification = self.classification.groups[current].get(
            device,
            "ray-traced shadow tile classification",
            &[
                (0, resource(inputs.depth)),
                (1, resource(&targets.normal)),
                (2, resource(&targets.tiles)),
                (3, resource(&targets.moments[previous])),
                (4, resource(scratch_b)),
                (5, resource(inputs.previous_depth)),
                (6, resource(inputs.motion)),
                (7, inputs.params.as_entire_binding()),
                (8, inputs.slot_table.as_entire_binding()),
                (9, resource(&targets.metadata)),
                (10, resource(scratch_a)),
                (11, resource(&targets.moments[current])),
                (12, resource(inputs.half_depth)),
            ],
        );
        dispatch(
            encoder,
            &self.classification.pipeline,
            classification,
            "ray-traced shadow tile classification",
        );
        // Pass 0 filters A into B, which the next frame's classification
        // reads as its history; pass 1 B into A; pass 2 A into the denoised
        // words, which each slot's group ORs its byte into, binding B,
        // which it does not write.
        encoder.clear_buffer(&targets.denoised, 0, None);
        for (pass, (input, history)) in self.filters.iter_mut().zip([
            (scratch_a, scratch_b),
            (scratch_b, scratch_a),
            (scratch_a, scratch_b),
        ]) {
            let group = filter_group(
                &mut pass.groups[current],
                device,
                [
                    &targets.normal,
                    &targets.metadata,
                    input,
                    inputs.half_depth,
                    history,
                ],
                inputs.params,
                &targets.denoised,
            );
            dispatch(encoder, &pass.pipeline, group, "ray-traced shadow filter");
        }
    }
}

/// A filter pass's group 0: the tracing pixels' normals, the tiles'
/// metadata, the pass's input and the tracing resolution's linear depth,
/// the parameters, and its history and denoised outputs.
fn filter_group<'a>(
    group: &'a mut CachedGroup,
    device: &wgpu::Device,
    [normal, metadata, input, half_depth, history]: [&wgpu::TextureView; 5],
    params: &wgpu::Buffer,
    denoised: &wgpu::Buffer,
) -> &'a wgpu::BindGroup {
    let resource = wgpu::BindingResource::TextureView;
    group.get(
        device,
        "ray-traced shadow filter",
        &[
            (1, resource(normal)),
            (2, resource(metadata)),
            (3, resource(input)),
            (4, params.as_entire_binding()),
            (5, resource(history)),
            (6, denoised.as_entire_binding()),
            (7, resource(half_depth)),
        ],
    )
}

#[cfg(test)]
pub(crate) fn constants() -> [crate::shading::layout_tests::Constant; 1] {
    [crate::shading::layout_tests::Constant::new(
        "traced_shadows_temporal",
        "TRACED_DENOISED_SLOTS",
        naga::Literal::U32(DENOISED_SLOTS),
    )]
}
