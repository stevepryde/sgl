//! Post: bloom, then tone mapping with the frame's exposure and colour
//! grading, dithered to the output; where SMAA runs (or stands in for TAA
//! that did not run), tone mapping at the scene size, SMAA, then the
//! antialiased scene resampled and dithered to the output. Opt-in spatial
//! upscaling reconstructs a smaller scene with AMD FSR1 EASU. Where
//! sharpening is in effect, the final pass, which writes the output transfer
//! and the dither, runs AMD's RCAS over the display-referred image at the
//! output size, whichever antialiasing completed the frame: over EASU's
//! output, SMAA's at the output size, or an output-size target the scene
//! is tone mapped or resampled into. It keeps no history.
//!
//! Reads: the completed scene at the scene size, motion-blurred when motion
//! blur ran (`stages::motion_blur`), within `shading::SCENE_COLOR`'s bound
//! (`inputs`), which antialiasing completed it (`Completed`), and the
//! frame's exposure (`stages::exposure`).
//! Writes: its own bloom chain, combined HDR, SMAA, tone-mapped scene,
//! antialiased and tone-mapped targets, the optional spatial reconstruction
//! target, the sharpening input, and the output. The diagnostic output-size
//! tone-mapped target exists only while capture is enabled, and holds the
//! final image, sharpened where sharpening runs, undithered. The tone-mapped
//! scene, antialiased and SMAA's edge and weight targets are made the first
//! frame SMAA runs at the scene size; SMAA's own are released when it stops.
//! The output-size sharpening input is made the first frame that sharpens
//! without SMAA's or EASU's output at the output size to read, and released
//! the first that does not.
//! Honours: bloom (and the frame's authored bloom parameters, and the
//! effective sizing's bloom targets), the frame's colour grading, the
//! antialiasing in effect and SMAA's quality, spatial upscaling, the
//! sharpness, the bloom and SMAA diagnostics layers and the diagnostics
//! tone-map capture.
//! Timing groups: `bloom`, `SMAA`, `tone map`, `spatial upscale`,
//! `sharpening`. While SMAA runs, `tone map` also times the resample after
//! it, so its passes are not consecutive; the group sums them. `spatial
//! upscale` times EASU; `sharpening` times the final pass where it runs
//! RCAS, and `tone map` the final pass otherwise.
pub(crate) mod bloom;
pub(crate) mod fsr1;
pub(crate) mod inputs;
mod output;
pub(crate) mod smaa;
pub(crate) mod tone_map;
pub(crate) use output::Output;

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
    spatial: bool,
    /// RCAS sharpens the final image at this sharpness.
    sharpness: Option<f32>,
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
    tone_mapping: crate::settings::ToneMapping,
}

/// Tone mapping's scene-size output for SMAA or spatial upscaling, and
/// SMAA's output only while it runs.
struct SmaaTargets {
    /// The tone-mapped scene.
    tone_mapped: wgpu::TextureView,
    antialiased: Option<wgpu::TextureView>,
}

impl SmaaTargets {
    fn new(device: &wgpu::Device, scene: [u32; 2]) -> Self {
        Self {
            tone_mapped: target(device, "tone-mapped scene", scene, HDR),
            antialiased: None,
        }
    }
}

pub(crate) struct Post {
    inputs: inputs::Inputs,
    bloom: bloom::Bloom,
    smaa: smaa::Smaa,
    scene_size: [u32; 2],
    output_size: [u32; 2],
    fsr1: fsr1::Fsr1,
    /// SMAA's targets, from the first frame SMAA runs at `scene_size`.
    smaa_targets: Option<SmaaTargets>,
    /// The display-referred image RCAS reads at the output size, where
    /// neither SMAA nor EASU writes it there.
    sharpening_input: Option<wgpu::TextureView>,
    tone_map: tone_map::ToneMap,
}

impl Post {
    /// Presentation to `format` for `sizes`; bloom's targets start full
    /// size with `bloom_targets`, and SMAA's pipelines are built for
    /// `smaa_quality`.
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        output: Output,
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
            scene_size: sizes.scene,
            output_size: sizes.output,
            fsr1: fsr1::Fsr1::new(device, &inputs),
            smaa_targets: None,
            sharpening_input: None,
            tone_map: tone_map::ToneMap::new(device, &inputs, output, sizes.output),
            inputs,
        })
    }

    /// New targets for `sizes`, bloom's full size with `bloom_targets`.
    pub fn resize(&mut self, device: &wgpu::Device, sizes: Sizes, bloom_targets: bool) {
        self.inputs.forget();
        self.bloom.resize(device, sizes.scene, bloom_targets);
        self.scene_size = sizes.scene;
        self.output_size = sizes.output;
        self.fsr1.release();
        self.smaa_targets = None;
        self.sharpening_input = None;
        self.smaa.forget_inputs();
        self.tone_map.resize(device, sizes.output);
        self.smaa.resize(sizes.scene[0], sizes.scene[1]);
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
            spatial: spatial_resolves(ctx.effective.upscaling, ctx.sizes, kind),
            sharpness: ctx.effective.sharpness,
        };
        let look = Look {
            exposure,
            stops: ctx.input.exposure.stops,
            bloom: &ctx.input.bloom,
            grading: &ctx.input.color_grading,
            tone_mapping: ctx.effective.tone_mapping,
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

    /// Bloom, then tone mapping of `completed` to `output`, as
    /// `presentation` asks, with `look`; with SMAA, tone mapping at the scene
    /// size, SMAA, then the result resampled to `output`.
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
        // completed scene goes to tone mapping.
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
        // SMAA after tone mapping and before the resample to the output, as
        // Godot (`_render_buffers_post_process_and_tonemap`) and Bevy order
        // them: its thresholds are for display colour.
        if presentation.smaa.is_none() {
            self.smaa.release();
        }
        if !presentation.spatial && self.fsr1.target_is_held() {
            self.inputs.forget();
            self.fsr1.release();
        }
        // RCAS reads SMAA's or EASU's output where either is at the output
        // size; else the scene tone mapped or resampled into its own input.
        let sharpening_input = presentation.sharpness.is_some()
            && !presentation.spatial
            && (presentation.smaa.is_none() || self.scene_size != self.output_size);
        if !sharpening_input && self.sharpening_input.take().is_some() {
            self.inputs.forget();
        }
        let output_size = self.output_size;
        // RCAS as the final pass, over EASU's gamma-2 output where `encoded`.
        let sharpen = |encoded| {
            presentation
                .sharpness
                .map(|sharpness| tone_map::Final::Sharpen { sharpness, encoded })
        };
        if presentation.smaa.is_none() && !presentation.spatial {
            if self.smaa_targets.take().is_some() {
                self.inputs.forget();
            }
            let Some(last) = sharpen(false) else {
                self.tone_map.encode(
                    device,
                    queue,
                    encoder,
                    &self.inputs,
                    combined,
                    self.bloom.halo(),
                    look.exposure,
                    look.grading,
                    look.tone_mapping,
                    tone_map::Destination::Output {
                        capture: presentation.capture,
                        output,
                    },
                    timing,
                );
                return;
            };
            let input = self
                .sharpening_input
                .get_or_insert_with(|| target(device, "sharpening input", output_size, HDR));
            self.tone_map.encode(
                device,
                queue,
                encoder,
                &self.inputs,
                combined,
                self.bloom.halo(),
                look.exposure,
                look.grading,
                look.tone_mapping,
                tone_map::Destination::Intermediate(input),
                timing,
            );
            self.tone_map.present_final(
                device,
                queue,
                encoder,
                &self.inputs,
                input,
                self.bloom.halo(),
                presentation.capture,
                last,
                output,
                timing,
            );
            return;
        }
        let scene_size = self.scene_size;
        let targets = self
            .smaa_targets
            .get_or_insert_with(|| SmaaTargets::new(device, scene_size));
        self.tone_map.encode(
            device,
            queue,
            encoder,
            &self.inputs,
            combined,
            self.bloom.halo(),
            look.exposure,
            look.grading,
            look.tone_mapping,
            tone_map::Destination::Intermediate(&targets.tone_mapped),
            timing,
        );
        if presentation.smaa.is_none() && targets.antialiased.take().is_some() {
            self.inputs.forget();
        }
        let scene = if let Some(quality) = presentation.smaa {
            let antialiased = targets
                .antialiased
                .get_or_insert_with(|| target(device, "antialiased scene", scene_size, HDR));
            self.smaa.set_quality(device, quality);
            self.smaa
                .encode(device, encoder, &targets.tone_mapped, antialiased, timing);
            &*antialiased
        } else {
            &targets.tone_mapped
        };
        let (scene, last) = if presentation.spatial {
            let upscaled = self.fsr1.encode(
                device,
                encoder,
                &self.inputs,
                scene,
                self.bloom.halo(),
                output_size,
                timing,
            );
            (upscaled, sharpen(true).unwrap_or(tone_map::Final::Decode))
        } else if sharpening_input {
            let input = self
                .sharpening_input
                .get_or_insert_with(|| target(device, "sharpening input", output_size, HDR));
            self.tone_map.resample_into(
                device,
                encoder,
                &self.inputs,
                scene,
                self.bloom.halo(),
                input,
                timing,
            );
            (&*input, sharpen(false).unwrap_or(tone_map::Final::Resample))
        } else {
            (scene, sharpen(false).unwrap_or(tone_map::Final::Resample))
        };
        self.tone_map.present_final(
            device,
            queue,
            encoder,
            &self.inputs,
            scene,
            self.bloom.halo(),
            presentation.capture,
            last,
            output,
            timing,
        );
    }
}

/// Spatial upscaling fills only the scene-to-output gap, never FSR's
/// render-to-scene gap (including a failed FSR dispatch).
fn spatial_resolves(
    choice: crate::settings::Upscaling,
    sizes: Sizes,
    completed: Completed,
) -> bool {
    choice == crate::settings::Upscaling::On
        && (sizes.render == sizes.scene || completed == Completed::Fsr)
        && sizes.scene[0] <= sizes.output[0]
        && sizes.scene[1] <= sizes.output[1]
        && sizes.scene != sizes.output
}

/// Whether SMAA antialiases the scene `completed` holds: SMAA is in effect,
/// or it stands in for TAA that did not run, which is TAA in effect that
/// cannot run with this camera or diagnostics layer, or TAA replacing FSR
/// that failed this frame. A frame FSR did not upscale is otherwise
/// presented as it is, as is any frame not at the scene size.
fn smaa_resolves(effective: &Effective, sizes: Sizes, completed: Completed) -> bool {
    let antialiasing = match effective.antialiasing {
        Antialiasing::Smaa => true,
        Antialiasing::Taa => completed != Completed::Taa,
        Antialiasing::Fsr2 | Antialiasing::Fsr3 => effective.fsr && completed != Completed::Fsr,
        _ => false,
    };
    antialiasing && effective.smaa && sizes.render == sizes.scene
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
