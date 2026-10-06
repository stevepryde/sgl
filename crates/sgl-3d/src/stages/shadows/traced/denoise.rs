//! AMD's FidelityFX shadow denoiser over the ray-traced shadow stage's
//! first slots, as Wicked Engine runs it over its first four lights
//! (2ff1d9e `Postprocess_RTShadow`, wiRenderer.cpp 15629–15831): tile
//! classification against the previous frame's moments and the
//! reprojected history, then edge-stopping filter passes at step sizes 1,
//! 2 and 4, the last recovering contrast. One invocation a pixel covers the
//! slots it filters, a slot a lane, sharing the depth, normals and their
//! weights, where Wicked dispatches each light apart; with none of them
//! held it does not run. Its result replaces those slots' temporal blend.
//! Which slots and how many passes is the frame's `Shape`, from
//! `Settings::ray_traced_shadow_quality`; one port serves every shape, its
//! lanes the program's (traced_denoise_lanes_*.wgsl) and its passes
//! pipeline constants.
use super::{Pass, texture};
use crate::settings::RayTracedShadowQuality;
use crate::shading::{self, shadow_mask};
use crate::view::cached_group::CachedGroup;

/// The entry points the denoiser's pipelines are created with.
pub(crate) const TILE_CLASSIFICATION_ENTRY: &str = "traced_denoise_tile_classification";
pub(crate) const FILTER_ENTRY: &str = "traced_denoise_filter";
/// The slots the denoiser may filter (`TRACED_DENOISED_SLOTS`): Wicked's
/// first four, the slot table's layer 0.
pub(super) const DENOISED_SLOTS: u32 = 4;

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
/// AMD's callbacks, and the scratch layout's pack and unpack.
static COMMON: shading::Module = shading::Module {
    name: "traced_denoise_common",
    source: include_str!("traced_denoise_common.wgsl"),
    deps: &[&super::COMMON],
};
/// The lanes a program's invocation filters: slots 0 to 3, or slot 0.
static LANES_FOUR: shading::Module = shading::Module {
    name: "traced_denoise_lanes_four",
    source: include_str!("traced_denoise_lanes_four.wgsl"),
    deps: &[],
};
static LANES_ONE: shading::Module = shading::Module {
    name: "traced_denoise_lanes_one",
    source: include_str!("traced_denoise_lanes_one.wgsl"),
    deps: &[],
};
/// The tile classification and the filter, with four lanes and with one:
/// one source each, its lanes its program's.
pub(crate) static TILE_CLASSIFICATION_FOUR: shading::Module = shading::Module {
    name: "traced_denoise_tileclassification_four",
    source: include_str!("traced_denoise_tileclassification.wgsl"),
    deps: &[
        &FFX_TILE_CLASSIFICATION,
        &shading::SHADOW_MASK_SLOTS,
        &COMMON,
        &LANES_FOUR,
    ],
};
pub(crate) static TILE_CLASSIFICATION_ONE: shading::Module = shading::Module {
    name: "traced_denoise_tileclassification_one",
    source: include_str!("traced_denoise_tileclassification.wgsl"),
    deps: &[
        &FFX_TILE_CLASSIFICATION,
        &shading::SHADOW_MASK_SLOTS,
        &COMMON,
        &LANES_ONE,
    ],
};
pub(crate) static FILTER_FOUR: shading::Module = shading::Module {
    name: "traced_denoise_filter_four",
    source: include_str!("traced_denoise_filter.wgsl"),
    deps: &[&FFX_FILTER, &COMMON, &LANES_FOUR],
};
pub(crate) static FILTER_ONE: shading::Module = shading::Module {
    name: "traced_denoise_filter_one",
    source: include_str!("traced_denoise_filter.wgsl"),
    deps: &[&FFX_FILTER, &COMMON, &LANES_ONE],
};

/// What the denoiser runs in a frame: the slots it filters, from slot 0, a
/// lane each, and its filter passes, at steps 1, 2 and 4 in turn, the last
/// final. High filters slots 0 to 3 in three passes, as Wicked runs AMD's
/// denoiser over its first four lights, or slot 0 alone in them where
/// slots 1 to 3 hold no light, which leaves slot 0's result as four lanes
/// leave it, at less cost; Low filters slot 0 alone in two (the
/// architecture's Ray-traced shadows, RD-6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Shape {
    pub slots: u32,
    pub passes: u32,
}

impl Shape {
    /// The shape at `quality` (resolved: Low or High) of a frame whose slot
    /// table's layer 0, slots 0 to 3, holds `lights`.
    pub fn of(quality: RayTracedShadowQuality, lights: [u32; 4]) -> Self {
        let local = lights[1..]
            .iter()
            .any(|&key| key != shadow_mask::SHADOW_MASK_EMPTY);
        match quality {
            RayTracedShadowQuality::Low => Self { slots: 1, passes: 2 },
            _ => Self {
                slots: if local { DENOISED_SLOTS } else { 1 },
                passes: 3,
            },
        }
    }

    /// Whether a slot it filters holds a light, without which nothing
    /// reads its result.
    pub fn runs(self, lights: [u32; 4]) -> bool {
        lights[..self.slots as usize]
            .iter()
            .any(|&key| key != shadow_mask::SHADOW_MASK_EMPTY)
    }
}

/// The format of the tracing pixels' shading normals the trace writes for
/// the denoiser.
pub(super) const NORMAL_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// The denoiser's targets at one tracing size: each 8×4 tile's hit masks
/// and each tracing pixel's shading normal, which the trace writes; each
/// 8×8 group's metadata; the reprojected and filtered visibility and
/// variance, packed as two halves (A, then B, which keeps the first filter
/// pass's result for the next frame); the moments' pair, a layer a denoised
/// slot; and the denoised visibility, a word a tracing pixel, packed as the
/// trace packs its first word. The masks, metadata and halves hold the four
/// denoised slots in a texel's channels. Every shape shares them, so slot
/// 0's history continues from a frame of one shape to the next of another.
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
                1,
                wgpu::TextureFormat::Rgba32Uint,
            ),
            scratch: [0, 1].map(|_| {
                texture(
                    device,
                    "ray-traced shadow denoise scratch",
                    reduced,
                    1,
                    wgpu::TextureFormat::Rgba32Uint,
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
                    usage: wgpu::BufferUsages::STORAGE,
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

/// A shape's passes: the tile classification and the filter passes.
struct Chain {
    classification: Pass,
    filters: Vec<Pass>,
}

impl Chain {
    fn new(device: &wgpu::Device, shape: Shape) -> Self {
        let (classification, filter) = if shape.slots == 1 {
            (&TILE_CLASSIFICATION_ONE, &FILTER_ONE)
        } else {
            (&TILE_CLASSIFICATION_FOUR, &FILTER_FOUR)
        };
        let last = shape.passes - 1;
        let flag = |on: bool| f64::from(u8::from(on));
        Self {
            classification: Pass::new(device, classification, TILE_CLASSIFICATION_ENTRY, &[]),
            filters: (0..shape.passes)
                .map(|pass| {
                    // Upstream's second pass of three finds a cleared tile
                    // in its target as the classification wrote it; every
                    // other pass, the last among them, writes it.
                    let write_cleared = pass != 1 || pass == last;
                    Pass::new(
                        device,
                        filter,
                        FILTER_ENTRY,
                        &[
                            ("filter_step", f64::from(1u32 << pass)),
                            ("filter_final", flag(pass == last)),
                            ("filter_write_cleared", flag(write_cleared)),
                        ],
                    )
                })
                .collect(),
        }
    }
}

/// The denoiser's passes for each shape a frame took, made in the first
/// frame that takes it.
#[derive(Default)]
pub(super) struct Denoiser {
    chains: Vec<(Shape, Chain)>,
}

impl Denoiser {
    /// Denoises the traced visibility of the slots `shape` filters, whose
    /// tiles `targets` holds, into its denoised words.
    pub fn encode(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        timing: Option<&crate::timing::GpuTiming>,
        shape: Shape,
        targets: &Targets,
        inputs: Inputs<'_>,
    ) {
        let index = match self.chains.iter().position(|(made, _)| *made == shape) {
            Some(index) => index,
            None => {
                self.chains.push((shape, Chain::new(device, shape)));
                self.chains.len() - 1
            }
        };
        let chain = &mut self.chains[index].1;
        let resource = wgpu::BindingResource::TextureView;
        let (current, previous) = (inputs.current, 1 - inputs.current);
        let [width, height] = inputs.reduced;
        let groups = [width.div_ceil(8), height.div_ceil(8), 1];
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
        let classification = chain.classification.groups[current].get(
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
            &chain.classification.pipeline,
            classification,
            "ray-traced shadow tile classification",
        );
        // Pass 0 filters A into B, which the next frame's classification
        // reads as its history, and each pass after it the half the one
        // before wrote into the other, the last into the denoised words,
        // binding as its history the half it does not write.
        for (index, pass) in chain.filters.iter_mut().enumerate() {
            let (input, history) = if index % 2 == 0 {
                (scratch_a, scratch_b)
            } else {
                (scratch_b, scratch_a)
            };
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
