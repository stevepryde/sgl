//! Bloom: Bevy 9d12036's energy-conserving bloom (`bloom.wgsl`; the
//! orchestration of `crates/bevy_post_process/src/bloom/mod.rs`). The
//! completed scene is downsampled into a mip chain whose first level is 512
//! texels high (Bevy's `max_mip_dimension`; `chain_size` narrows it where
//! that would be wider than the device allows), each level by a 13-tap filter,
//! the first with a Karis average; the chain is upsampled back by a 3×3
//! tent, each level blended into the next finer one by Bevy's blend factor
//! for it; and the last upsample is mixed into the completed scene at the
//! scene size.
use super::inputs::{Inputs, draw, pipeline};
use crate::BloomParameters;
use crate::shading::gbuffer::COLOR as HDR;
use crate::view::targets::target;

/// The downsamples, upsamples and composite.
pub(crate) static BLOOM: crate::shading::Module = crate::shading::Module {
    name: "bloom",
    source: include_str!("bloom.wgsl"),
    deps: &[&super::inputs::INPUTS, &crate::shading::LUMINANCE],
};
/// The entry points bloom's pipelines are created with, beside
/// `inputs::VS_ENTRY`.
pub(crate) const DOWNSAMPLE_FIRST_ENTRY: &str = "downsample_first";
pub(crate) const DOWNSAMPLE_ENTRY: &str = "downsample";
pub(crate) const UPSAMPLE_ENTRY: &str = "upsample";
pub(crate) const COMPOSITE_ENTRY: &str = "composite";

/// Bevy's default `max_mip_dimension`: mip 0's height.
const MAX_MIP_DIMENSION: u32 = 512;
/// Bevy's mip count for it, `ilog2(max_mip_dimension).max(2) - 1`.
const MIP_COUNT: u32 = MAX_MIP_DIMENSION.ilog2() - 1;
/// Bevy's `Bloom::NATURAL` shape of the halo
/// (`crates/bevy_post_process/src/bloom/settings.rs`): how much more the
/// widest scattering contributes, how far that boost reaches toward
/// narrower scattering, and the widest scattering angle, 1 being 90°.
const LOW_FREQUENCY_BOOST: f32 = 0.7;
const LOW_FREQUENCY_BOOST_CURVATURE: f32 = 0.95;
const HIGH_PASS_FREQUENCY: f32 = 1.;

/// `BloomSettings` in `inputs.wgsl`: what bloom's passes read.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct BloomSettings {
    /// Bevy's blend factor of the last upsample: the share of the completed
    /// scene the bloom replaces.
    composite_blend: f32,
    /// `exp2(Exposure::stops)`: Bevy's scene is exposed by its camera
    /// before bloom, so its Karis average and floor see exposed light.
    exposure: f32,
}

impl Default for BloomSettings {
    fn default() -> Self {
        settings(&BloomParameters::default(), 0.)
    }
}

/// The settings uniform for the frame's `bloom` with the frame's authored
/// exposure `stops`.
pub(crate) fn settings(bloom: &BloomParameters, stops: f32) -> BloomSettings {
    BloomSettings {
        composite_blend: blend_factor(bloom, 0, MIP_COUNT - 1),
        exposure: stops.exp2(),
    }
}

/// The layouts bloom mirrors.
#[cfg(test)]
pub(crate) fn mirrors() -> Vec<crate::shading::layout_tests::Mirror> {
    use crate::shading::layout_tests::mirror;
    vec![mirror!(
        "bloom",
        "BloomSettings",
        BloomSettings,
        [composite_blend, exposure]
    )]
}

/// Bevy's `compute_blend_factor` for its energy-conserving composite: the
/// blend of mip `mip` into the next finer level (the scene for mip 0), of
/// `max_mip`. A nonfinite intensity blends nothing.
fn blend_factor(bloom: &BloomParameters, mip: u32, max_mip: u32) -> f32 {
    let mip = mip as f32 / max_mip as f32;
    let mut lf_boost =
        (1. - (1. - mip).powf(1. / (1. - LOW_FREQUENCY_BOOST_CURVATURE))) * LOW_FREQUENCY_BOOST;
    let high_pass_lq = 1. - ((mip - HIGH_PASS_FREQUENCY) / HIGH_PASS_FREQUENCY).clamp(0., 1.);
    lf_boost *= 1. - bloom.intensity;
    let blend = (bloom.intensity + lf_boost) * high_pass_lq;
    if blend.is_finite() {
        blend.clamp(0., 1.)
    } else {
        0.
    }
}

/// Mip 0's size for a `scene_size` scene on a device whose largest 2D
/// texture side is `largest`: the scene scaled to `MAX_MIP_DIMENSION` texels
/// high (Bevy's `prepare_bloom_textures`), or, where that is wider than the
/// device allows, scaled to the device's largest width instead. Bevy's
/// sizing alone fails for a scene more than `largest / MAX_MIP_DIMENSION`
/// times as wide as it is high (32 at 16384; Bevy's issue 16182); the
/// fallback, which Filament c0d63e8 also takes (`PostProcessManager::bloom`,
/// its #9784), keeps the scene's aspect and every size Bevy's sizing fits.
/// Either way the longer side is at least 512 texels (WebGPU guarantees
/// 8192), so `MIP_COUNT` levels fit.
fn chain_size(scene_size: [u32; 2], largest: u32) -> [u32; 2] {
    let [width, height] = scene_size.map(|x| x.max(1) as f32);
    let ratio = (MAX_MIP_DIMENSION as f32 / height).min(largest as f32 / width);
    scene_size.map(|x| ((x as f32 * ratio).round() as u32).clamp(1, largest))
}

/// Bloom's targets, or 1×1 stand-ins while it does not run.
struct Targets {
    /// One view per mip of the chain.
    mips: Vec<wgpu::TextureView>,
    /// The completed scene with bloom, at the scene size.
    combined: wgpu::TextureView,
    /// Made for bloom that runs.
    enabled: bool,
}

impl Targets {
    fn new(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        scene_size: [u32; 2],
        enabled: bool,
    ) -> Self {
        let (size, mip_count, combined) = if enabled {
            let largest = device.limits().max_texture_dimension_2d;
            (chain_size(scene_size, largest), MIP_COUNT, scene_size)
        } else {
            ([1, 1], 1, [1, 1])
        };
        let chain = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("bloom mip chain"),
            size: wgpu::Extent3d {
                width: size[0],
                height: size[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: mip_count,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        Self {
            mips: (0..mip_count)
                .map(|mip| {
                    chain.create_view(&wgpu::TextureViewDescriptor {
                        base_mip_level: mip,
                        mip_level_count: Some(1),
                        ..Default::default()
                    })
                })
                .collect(),
            combined: target(device, "HDR scene with bloom", combined, HDR),
            enabled,
        }
    }
}

pub(super) struct Bloom {
    downsample_first: wgpu::RenderPipeline,
    downsample: wgpu::RenderPipeline,
    upsample: wgpu::RenderPipeline,
    composite: wgpu::RenderPipeline,
    /// The mip chain's format: Bevy's `Rg11b10Ufloat` where the device
    /// renders to it, else RGBA16F.
    format: wgpu::TextureFormat,
    targets: Targets,
    scene_size: [u32; 2],
}

impl Bloom {
    /// Bloom of a `scene_size` scene; its targets are full size when
    /// `enabled` (the High preset) and 1×1 otherwise.
    pub fn new(
        device: &wgpu::Device,
        inputs: &Inputs,
        scene_size: [u32; 2],
        enabled: bool,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("HDR bloom"),
            source: wgpu::ShaderSource::Wgsl(crate::shading::compose(&[&BLOOM]).into()),
        });
        let format = if device
            .features()
            .contains(wgpu::Features::RG11B10UFLOAT_RENDERABLE)
        {
            wgpu::TextureFormat::Rg11b10Ufloat
        } else {
            HDR
        };
        let input = [Some(inputs.layout())];
        // Bevy's energy-conserving upsample blend: the constant's share of
        // the coarser level over the finer one.
        let blend = wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::Constant,
                dst_factor: wgpu::BlendFactor::OneMinusConstant,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::Zero,
                dst_factor: wgpu::BlendFactor::One,
                operation: wgpu::BlendOperation::Add,
            },
        };
        Self {
            downsample_first: pipeline(
                device,
                &input,
                &shader,
                DOWNSAMPLE_FIRST_ENTRY,
                &[format],
                (None, &[]),
            ),
            downsample: pipeline(
                device,
                &input,
                &shader,
                DOWNSAMPLE_ENTRY,
                &[format],
                (None, &[]),
            ),
            upsample: pipeline(
                device,
                &input,
                &shader,
                UPSAMPLE_ENTRY,
                &[format],
                (Some(blend), &[]),
            ),
            composite: pipeline(
                device,
                &input,
                &shader,
                COMPOSITE_ENTRY,
                &[HDR],
                (None, &[]),
            ),
            format,
            targets: Targets::new(device, format, scene_size, enabled),
            scene_size,
        }
    }

    /// New targets for `scene_size`, full size when `enabled`.
    pub fn resize(&mut self, device: &wgpu::Device, scene_size: [u32; 2], enabled: bool) {
        self.scene_size = scene_size;
        self.targets = Targets::new(device, self.format, scene_size, enabled);
    }

    /// Mip 0, which every post group binds as its bloom view.
    pub fn halo(&self) -> &wgpu::TextureView {
        &self.targets.mips[0]
    }

    /// Sizes the targets for whether bloom runs this frame. True when the
    /// targets were replaced: groups binding the old ones are stale.
    pub fn prepare(&mut self, device: &wgpu::Device, enabled: bool) -> bool {
        let replaced = self.targets.enabled != enabled;
        if replaced {
            self.targets = Targets::new(device, self.format, self.scene_size, enabled);
        }
        replaced
    }

    /// After `prepare` with bloom on: `completed` down the mip chain and
    /// back up as `bloom` blends it, then mixed into the combined target,
    /// which it returns. The composite's blend is `settings(bloom)` in the
    /// inputs' uniform.
    pub fn encode(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        inputs: &Inputs,
        completed: &wgpu::TextureView,
        bloom: &BloomParameters,
        timing: Option<&crate::timing::GpuTiming>,
    ) -> &wgpu::TextureView {
        let t = &self.targets;
        // Mip passes bind the combined target, which none of them writes, as
        // the group's unread bloom view.
        let group = inputs.group(device, completed, &t.combined);
        draw(
            encoder,
            &self.downsample_first,
            &group,
            &t.mips[0],
            "bloom first downsample",
            timing.and_then(|t| t.render_pass("bloom")),
        );
        for mip in 1..t.mips.len() {
            let group = inputs.group(device, &t.mips[mip - 1], &t.combined);
            draw(
                encoder,
                &self.downsample,
                &group,
                &t.mips[mip],
                "bloom downsample",
                None,
            );
        }
        let max_mip = t.mips.len() as u32 - 1;
        for mip in (1..t.mips.len()).rev() {
            let group = inputs.group(device, &t.mips[mip], &t.combined);
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("bloom upsample"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &t.mips[mip - 1],
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_pipeline(&self.upsample);
            pass.set_bind_group(0, &group, &[]);
            let blend = f64::from(blend_factor(bloom, mip as u32, max_mip));
            pass.set_blend_constant(wgpu::Color {
                r: blend,
                g: blend,
                b: blend,
                a: blend,
            });
            pass.draw(0..3, 0..1);
        }
        let group = inputs.group(device, completed, &t.mips[0]);
        draw(
            encoder,
            &self.composite,
            &group,
            &t.combined,
            "bloom composite",
            timing.and_then(|t| t.render_pass("bloom")),
        );
        &t.combined
    }
}

#[cfg(test)]
mod sizing_tests;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
