//! Antialiasing: DiligentFX's TAA (a thin operation over the renderer's
//! lent post-effect context) or AMD's FSR (`fsr.rs`), resolving the
//! complete linear HDR frame from the render size to the scene size before
//! bloom and tone mapping, as in Hydrogent, Bevy, Unreal and AMD's FSR
//! placement.
//!
//! Reads: the complete HDR frame (the composite), the surface's depth and
//! motion (the Surface contract), FSR's masks, the camera's projection, the
//! frame time, the frame's exposure (`stages::exposure`) and the lent
//! post-effect context, whose inputs hold the surface.
//! Writes: the frame's jitter and mip bias; TAA's accumulation (in the
//! context) or its own FSR output; which view completes the scene
//! (`Completed`).
//! Honours: the antialiasing in effect, FSR and its quality (the effective
//! sizing), FSR's sharpening, the TAA and FSR diagnostics layers.
//! Timing groups: `TAA`, `FSR2`, `FSR3`.
//! History: FSR's context, which restarts on SGL3D's history loss; TAA's
//! lives in the post-effect context.
pub(crate) mod fsr;

use crate::settings::{FsrQuality, FsrVersion};
use crate::view::Jitter;
use crate::view::effective::Effective;
use crate::view::frame::{Completed, FrameContext};
use crate::view::history::HistoryFrame;
use crate::view::post_fx::{PostFx, TAA_MIP_BIAS};
use crate::view::targets::Sizes;

#[derive(Default)]
pub(crate) struct Antialiasing {
    /// Present while FSR is chosen and its context runs on this device.
    fsr: Option<fsr::Fsr>,
    /// Why FSR could not run; it is not retried.
    errors: [Option<String>; 2],
    selected: Option<FsrVersion>,
    /// FSR failed this frame and stopped.
    failed: bool,
}

impl Antialiasing {
    pub fn fsr_running(&self) -> Option<FsrVersion> {
        self.fsr.as_ref().map(|fsr| fsr.version())
    }

    pub fn error(&self) -> Option<&str> {
        self.selected
            .and_then(|version| self.errors[version.index()].as_deref())
    }

    /// Recreate the one context on version or output-size changes. A failed
    /// version stays unavailable for this renderer; the other can still run.
    pub fn resize(
        &mut self,
        device: &wgpu::Device,
        scene_size: [u32; 2],
        selection: Option<(FsrVersion, FsrQuality)>,
    ) -> ([u32; 2], bool) {
        self.selected = selection.map(|(version, _)| version);
        let Some((version, quality)) =
            selection.filter(|(version, _)| self.errors[version.index()].is_none())
        else {
            let dropped = self.fsr.take().is_some();
            return (scene_size, dropped);
        };
        let mut restarted = false;
        if self
            .fsr
            .as_ref()
            .is_none_or(|fsr| fsr.display_size() != scene_size || fsr.version() != version)
        {
            self.fsr = None;
            restarted = true;
            match fsr::Fsr::new(device, scene_size, version) {
                Ok(fsr) => self.fsr = Some(fsr),
                Err(error) => {
                    self.errors[version.index()] = Some(error);
                    return (scene_size, restarted);
                }
            }
        }
        (fsr::render_size(scene_size, quality, version), restarted)
    }

    /// Before the frame's geometry, after the lent post-effect context's
    /// `prepare` returned `context_jitter`: the jitter and mip bias of the
    /// temporal antialiasing that runs, if any. TAA's jitter is the
    /// context's, at Hydrogent's mip bias. FSR decides whether its history
    /// continues and jitters at AMD's mip bias; the context's camera records
    /// that jitter.
    pub fn prepare(
        &mut self,
        effective: &Effective,
        sizes: Sizes,
        history: HistoryFrame,
        post_fx: Option<&mut PostFx>,
        context_jitter: Option<[f32; 2]>,
    ) -> Option<Jitter> {
        // A reset ends FSR's history even in a frame it does not run.
        if !history.valid
            && let Some(upscaler) = self.fsr.as_mut()
        {
            upscaler.forget_history();
        }
        if effective.taa {
            Some(Jitter {
                ndc: context_jitter.expect("TAA runs in the post-effect context"),
                mip_bias: TAA_MIP_BIAS,
            })
        } else if effective.fsr {
            let upscaler = self.fsr.as_mut().unwrap();
            let jitter = Jitter {
                ndc: upscaler.prepare(sizes.render, history.frames, history.valid),
                mip_bias: fsr::mip_bias(sizes.render, sizes.scene),
            };
            if let Some(post_fx) = post_fx {
                post_fx.set_jitter(jitter.ndc);
            }
            Some(jitter)
        } else {
            None
        }
    }

    /// TAA or FSR of the composite, the complete linear HDR frame, which
    /// `exposure` exposes. Returns where the completed scene is: the
    /// composite when neither ran, or FSR failed.
    pub fn encode(
        &mut self,
        ctx: &mut FrameContext<'_>,
        post_fx: Option<&mut PostFx>,
        exposure: &wgpu::Texture,
    ) -> Completed {
        let color = &ctx.targets.composite;
        if ctx.effective.taa {
            let post_fx = post_fx.unwrap();
            post_fx.temporal_anti_aliasing(ctx.device, ctx.queue, ctx.encoder, color, ctx.timing);
            Completed::Taa
        } else if ctx.effective.fsr {
            // FSR upscales the same linear HDR frame (AMD's FSR placement).
            let upscaler = self.fsr.as_mut().unwrap();
            let inputs = fsr::Inputs {
                color,
                depth: ctx.surface.depth,
                motion: &ctx.targets.motion,
                masks: ctx.targets.fsr_masks.as_ref().expect("FSR frame masks"),
                projection: glam::Mat4::from_cols_array_2d(&ctx.values.view.projection),
                frame_time_ms: ctx.input.frame_time_ms,
                exposure,
                sharpness: ctx.effective.fsr_sharpness,
            };
            match upscaler.dispatch(ctx.device, ctx.encoder, &inputs, ctx.timing) {
                Ok(()) => Completed::Fsr,
                // TAA, the antialiasing in effect without FSR, takes over.
                Err(error) => {
                    self.errors[upscaler.version().index()] = Some(error);
                    self.fsr = None;
                    self.failed = true;
                    Completed::Composite
                }
            }
        } else {
            Completed::Composite
        }
    }

    /// Whether FSR failed and stopped since the last call.
    pub fn take_failure(&mut self) -> bool {
        std::mem::take(&mut self.failed)
    }

    /// FSR's output of the last successful frame.
    pub fn output(&self) -> &wgpu::TextureView {
        self.fsr.as_ref().expect("FSR ran").output()
    }

    #[cfg(all(test, not(target_arch = "wasm32")))]
    pub fn fsr(&self) -> Option<&fsr::Fsr> {
        self.fsr.as_ref()
    }
}
