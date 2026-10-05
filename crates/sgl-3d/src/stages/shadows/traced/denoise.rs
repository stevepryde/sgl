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
pub(crate) static TILE_CLASSIFICATION: shading::Module = shading::Module {
    name: "traced_denoise_tileclassification",
    source: include_str!("traced_denoise_tileclassification.wgsl"),
    deps: &[
        &FFX_TILE_CLASSIFICATION,
        &shading::GBUFFER,
        &shading::SHADOW_MASK_SLOTS,
        &super::COMMON,
    ],
};
pub(crate) static FILTER: shading::Module = shading::Module {
    name: "traced_denoise_filter",
    source: include_str!("traced_denoise_filter.wgsl"),
    deps: &[&FFX_FILTER, &shading::GBUFFER, &super::COMMON],
};

/// The denoiser's targets at one tracing size: each 8×4 tile's hit masks,
/// which the trace writes; each 8×8 group's metadata; the reprojected and
/// filtered visibility and variance, packed as two halves (A, then B,
/// which keeps the first filter pass's result for the next frame); the
/// moments' pair; and the denoised visibility. A layer a denoised slot.
pub(super) struct Targets {
    pub tiles: wgpu::TextureView,
    metadata: wgpu::TextureView,
    scratch: [wgpu::TextureView; 2],
    moments: [wgpu::TextureView; 2],
    pub denoised: wgpu::TextureView,
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
            denoised: layers("ray-traced shadow denoised", wgpu::TextureFormat::R32Float),
        }
    }
}

/// What the denoiser reads beyond its own targets.
pub(super) struct Inputs<'a> {
    pub depth: &'a wgpu::TextureView,
    pub normal: &'a wgpu::TextureView,
    pub motion: &'a wgpu::TextureView,
    /// The tracing resolution's linear depth last frame.
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
            classification: Pass::new(
                device,
                &TILE_CLASSIFICATION,
                "traced_denoise_tile_classification",
                &[],
            ),
            filters: std::array::from_fn(|pass| {
                Pass::new(
                    device,
                    &FILTER,
                    "traced_denoise_filter",
                    &[("filter_pass", pass as f64)],
                )
            }),
        }
    }

    /// Denoises the slots' traced visibility, whose tiles `targets` holds,
    /// into its denoised layers.
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
                (1, resource(inputs.normal)),
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
        // layers, binding B, which it does not write.
        for (pass, (input, history)) in self
            .filters
            .iter_mut()
            .zip([(scratch_a, scratch_b), (scratch_b, scratch_a), (scratch_a, scratch_b)])
        {
            let group = filter_group(
                &mut pass.groups[0],
                device,
                [inputs.depth, inputs.normal, &targets.metadata, input],
                inputs.params,
                [history, &targets.denoised],
            );
            dispatch(encoder, &pass.pipeline, group, "ray-traced shadow filter");
        }
    }
}

/// A filter pass's group 0: the G-buffer's depth and normals, the tiles'
/// metadata and the pass's input, the parameters, and its history and
/// denoised outputs.
fn filter_group<'a>(
    group: &'a mut CachedGroup,
    device: &wgpu::Device,
    [depth, normal, metadata, input]: [&wgpu::TextureView; 4],
    params: &wgpu::Buffer,
    [history, denoised]: [&wgpu::TextureView; 2],
) -> &'a wgpu::BindGroup {
    let resource = wgpu::BindingResource::TextureView;
    group.get(
        device,
        "ray-traced shadow filter",
        &[
            (0, resource(depth)),
            (1, resource(normal)),
            (2, resource(metadata)),
            (3, resource(input)),
            (4, params.as_entire_binding()),
            (5, resource(history)),
            (6, resource(denoised)),
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
