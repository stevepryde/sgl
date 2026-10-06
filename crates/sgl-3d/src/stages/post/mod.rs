//! Post: bloom, SMAA (or its stand-in for TAA that did not run), then tone
//! mapping with the frame's exposure and colour grading, dithered to the
//! output.
//!
//! Reads: the completed scene at the scene size, motion-blurred when motion
//! blur ran (`stages::motion_blur`), which antialiasing completed it
//! (`Completed`), and the frame's exposure (`stages::exposure`).
//! Writes: its own bloom chain, combined HDR, SMAA and tone-mapped targets,
//! and the output.
//! Honours: bloom (and the frame's authored bloom parameters, and the
//! effective sizing's bloom targets), the frame's colour grading, the
//! antialiasing in effect and SMAA's quality, the bloom and SMAA diagnostics
//! layers and the diagnostics tone-map capture.
//! Timing groups: `bloom`, `SMAA`, `tone map`.
pub(crate) mod bloom;
pub(crate) mod inputs;
pub(crate) mod smaa;
pub(crate) mod tone_map;

use crate::settings::{Antialiasing, SmaaQuality};
use crate::shading::gbuffer::COLOR as HDR;
use crate::view::effective::Effective;
use crate::view::frame::{Completed, FrameContext};
use crate::view::targets::{Sizes, target};

/// What post does with one completed scene.
#[derive(Clone, Copy, Debug)]
struct Presentation {
    bloom: bool,
    /// SMAA antialiases the scene, at this preset.
    smaa: Option<SmaaQuality>,
    /// Tone mapping writes the tone-mapped target, which is then presented.
    capture: bool,
}

/// The frame's authored look that post applies.
#[derive(Clone, Copy)]
struct Look<'a> {
    /// The frame's exposure multiplier.
    exposure: &'a wgpu::TextureView,
    /// `Exposure::stops`, which bloom's first downsample takes.
    stops: f32,
    bloom: &'a crate::BloomParameters,
    grading: &'a crate::ColorGrading,
}

pub(crate) struct Post {
    inputs: inputs::Inputs,
    bloom: bloom::Bloom,
    smaa: smaa::Smaa,
    /// SMAA's output at the scene size.
    antialiased: wgpu::TextureView,
    tone_map: tone_map::ToneMap,
}

impl Post {
    /// Presentation to `format` for `sizes`; bloom's targets start full
    /// size with `bloom_targets`, and SMAA's pipelines are built for
    /// `smaa_quality`.
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        sizes: Sizes,
        bloom_targets: bool,
        smaa_quality: SmaaQuality,
    ) -> Result<Self, image::ImageError> {
        let inputs = inputs::Inputs::new(device);
        Ok(Self {
            bloom: bloom::Bloom::new(device, &inputs, sizes.scene, bloom_targets),
            smaa: smaa::Smaa::new(
                device,
                queue,
                sizes.scene[0],
                sizes.scene[1],
                HDR,
                smaa_quality,
            )?,
            antialiased: target(device, "antialiased HDR scene", sizes.scene, HDR),
            tone_map: tone_map::ToneMap::new(device, &inputs, format, sizes.output),
            inputs,
        })
    }

    /// New targets for `sizes`, bloom's full size with `bloom_targets`.
    pub fn resize(&mut self, device: &wgpu::Device, sizes: Sizes, bloom_targets: bool) {
        self.inputs.forget();
        self.bloom.resize(device, sizes.scene, bloom_targets);
        self.antialiased = target(device, "antialiased HDR scene", sizes.scene, HDR);
        self.tone_map.resize(device, sizes.output);
        self.smaa.resize(device, sizes.scene[0], sizes.scene[1]);
    }

    /// Drops cached input groups, which may hold a replaced input's views.
    pub fn forget_inputs(&mut self) {
        self.inputs.forget();
    }

    /// The tone-mapped scene, when diagnostics capture it before presenting.
    #[cfg(any(test, feature = "diagnostics"))]
    pub fn tone_mapped(&self) -> &wgpu::TextureView {
        self.tone_map.tone_mapped()
    }

    /// The tone-mapped scene, when the last frame captured it.
    #[cfg(any(test, feature = "diagnostics"))]
    pub fn captured_tone_mapped(&self) -> Option<&wgpu::TextureView> {
        self.tone_map.captured()
    }

    /// Bloom, antialiasing and tone mapping of `completed`, the view holding
    /// the scene as `kind`, exposed by `exposure`, into `output`.
    pub fn encode(
        &mut self,
        ctx: &mut FrameContext<'_>,
        completed: &wgpu::TextureView,
        kind: Completed,
        exposure: &wgpu::TextureView,
        output: &wgpu::TextureView,
    ) {
        let presentation = Presentation {
            bloom: ctx.effective.bloom,
            smaa: smaa_resolves(ctx.effective, ctx.sizes, kind)
                .then_some(ctx.effective.smaa_quality),
            capture: ctx.effective.capture_tone_target,
        };
        let look = Look {
            exposure,
            stops: ctx.input.exposure.stops,
            bloom: &ctx.input.bloom,
            grading: &ctx.input.color_grading,
        };
        self.present(
            ctx.device,
            ctx.queue,
            ctx.encoder,
            completed,
            presentation,
            look,
            output,
            ctx.timing,
        );
    }

    /// Bloom, SMAA, then tone mapping of `completed` to `output`, as
    /// `presentation` asks, with `look`.
    #[allow(clippy::too_many_arguments)]
    fn present(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        completed: &wgpu::TextureView,
        presentation: Presentation,
        look: Look<'_>,
        output: &wgpu::TextureView,
        timing: Option<&crate::timing::GpuTiming>,
    ) {
        if self.bloom.prepare(device, presentation.bloom) {
            self.inputs.forget();
        }
        // Without bloom, or at intensity 0 where Bevy skips its node, the
        // completed scene goes to SMAA and tone mapping.
        let combined = if presentation.bloom && look.bloom.intensity != 0. {
            crate::counters::write_buffer(
                queue,
                &self.inputs.settings,
                0,
                bytemuck::bytes_of(&bloom::settings(look.bloom, look.stops)),
            );
            self.bloom
                .encode(device, encoder, &self.inputs, completed, look.bloom, timing)
        } else {
            completed
        };
        let hdr = if let Some(quality) = presentation.smaa {
            self.smaa.set_quality(device, quality);
            self.smaa
                .encode(device, encoder, combined, &self.antialiased, timing);
            &self.antialiased
        } else {
            combined
        };
        self.tone_map.encode(
            device,
            queue,
            encoder,
            &self.inputs,
            hdr,
            self.bloom.halo(),
            look.exposure,
            look.grading,
            presentation.capture,
            output,
            timing,
        );
    }
}

/// Whether SMAA antialiases the scene `completed` holds: SMAA is in effect,
/// or it stands in for TAA that did not run, which is TAA in effect that
/// cannot run with this camera or diagnostics layer, or TAA replacing FSR2
/// that failed this frame. A frame FSR2 did not upscale is otherwise
/// presented as it is, as is any frame not at the scene size.
fn smaa_resolves(effective: &Effective, sizes: Sizes, completed: Completed) -> bool {
    let antialiasing = match effective.antialiasing {
        Antialiasing::Smaa => true,
        Antialiasing::Taa => completed != Completed::Taa,
        Antialiasing::Fsr2 => effective.fsr2 && completed != Completed::Fsr2,
        _ => false,
    };
    antialiasing && effective.smaa && sizes.render == sizes.scene
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
