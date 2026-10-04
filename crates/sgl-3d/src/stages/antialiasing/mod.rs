//! Antialiasing: DiligentFX's TAA (a thin operation over the renderer's
//! lent post-effect context) or AMD's FSR2 (`fsr2.rs`), resolving the
//! complete linear HDR frame from the render size to the scene size before
//! bloom and tone mapping, as in Hydrogent, Bevy, Unreal and AMD's FSR2
//! placement.
//!
//! Reads: the complete HDR frame (the composite), depth, motion, FSR2's
//! masks, the camera's projection, the frame time, the frame's exposure
//! (`stages::exposure`), TAA's parameters (`FrameInput::taa`) and the lent
//! post-effect context.
//! Writes: the frame's jitter and mip bias; TAA's accumulation (in the
//! context) or its own FSR2 output; which view completes the scene
//! (`Completed`).
//! Honours: the antialiasing in effect, FSR2 and its quality (the effective
//! sizing), the TAA and FSR2 diagnostics layers.
//! Timing groups: `TAA`, `FSR2`.
//! History: FSR2's context, which restarts on SGL3D's history loss; TAA's
//! lives in the post-effect context.
pub(crate) mod fsr2;

use crate::settings::Fsr2Quality;
use crate::view::Jitter;
use crate::view::effective::Effective;
use crate::view::frame::{Completed, FrameContext};
use crate::view::history::HistoryFrame;
use crate::view::post_fx::{PostFx, TAA_MIP_BIAS};
use crate::view::targets::Sizes;

#[derive(Default)]
pub(crate) struct Antialiasing {
    /// Present while FSR2 is chosen and its context runs on this device.
    fsr2: Option<fsr2::Fsr2>,
    /// Why FSR2 could not run; it is not retried.
    fsr2_error: Option<String>,
    /// FSR2 failed this frame and stopped.
    failed: bool,
}

impl Antialiasing {
    pub fn fsr2_running(&self) -> bool {
        self.fsr2.is_some()
    }

    pub fn error(&self) -> Option<&str> {
        self.fsr2_error.as_deref()
    }

    /// The render size for `scene_size`: FSR2's for its `fsr2` quality while
    /// FSR2 is chosen and its context runs, else the scene size. As in AMD's
    /// sample, the context upscales to the scene size with that as its
    /// maximum render size, and is recreated when the scene size changes.
    /// Also returns whether the context was dropped or recreated.
    pub fn resize(
        &mut self,
        device: &wgpu::Device,
        scene_size: [u32; 2],
        fsr2: Option<Fsr2Quality>,
    ) -> ([u32; 2], bool) {
        let Some(quality) = fsr2.filter(|_| self.fsr2_error.is_none()) else {
            let dropped = self.fsr2.take().is_some();
            return (scene_size, dropped);
        };
        let mut restarted = false;
        if self
            .fsr2
            .as_ref()
            .is_none_or(|fsr2| fsr2.display_size() != scene_size)
        {
            self.fsr2 = None;
            restarted = true;
            match fsr2::Fsr2::new(device, scene_size) {
                Ok(fsr2) => self.fsr2 = Some(fsr2),
                Err(error) => {
                    self.fsr2_error = Some(error);
                    return (scene_size, restarted);
                }
            }
        }
        (fsr2::render_size(scene_size, quality), restarted)
    }

    /// Before the frame's geometry, after the lent post-effect context's
    /// `prepare` returned `context_jitter`: the jitter and mip bias of the
    /// temporal antialiasing that runs, if any. TAA's jitter is the
    /// context's, at Hydrogent's mip bias. FSR2 decides whether its history
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
        if effective.taa {
            Some(Jitter {
                ndc: context_jitter.expect("TAA runs in the post-effect context"),
                mip_bias: TAA_MIP_BIAS,
            })
        } else if effective.fsr2 {
            let upscaler = self.fsr2.as_mut().unwrap();
            let jitter = Jitter {
                ndc: upscaler.prepare(sizes.render, history.frames, history.valid),
                mip_bias: fsr2::mip_bias(sizes.render, sizes.scene),
            };
            if let Some(post_fx) = post_fx {
                post_fx.set_jitter(jitter.ndc);
            }
            Some(jitter)
        } else {
            None
        }
    }

    /// TAA or FSR2 of the composite, the complete linear HDR frame, which
    /// `exposure` exposes. Returns where the completed scene is: the
    /// composite when neither ran, or FSR2 failed.
    pub fn encode(
        &mut self,
        ctx: &mut FrameContext<'_>,
        post_fx: Option<&mut PostFx>,
        exposure: &wgpu::Texture,
    ) -> Completed {
        let color = &ctx.targets.composite;
        if ctx.effective.taa {
            let post_fx = post_fx.unwrap();
            post_fx.temporal_anti_aliasing(
                ctx.device,
                ctx.queue,
                ctx.encoder,
                &ctx.input.taa,
                color,
                ctx.timing,
            );
            Completed::Taa
        } else if ctx.effective.fsr2 {
            // FSR2 upscales the same linear HDR frame (AMD's FSR2 placement).
            let upscaler = self.fsr2.as_mut().unwrap();
            let inputs = fsr2::Inputs {
                color,
                depth: &ctx.targets.depth,
                motion: &ctx.targets.motion,
                masks: &ctx.targets.fsr2_masks,
                projection: glam::Mat4::from_cols_array_2d(&ctx.values.view.projection),
                frame_time_ms: ctx.input.frame_time_ms,
                exposure,
            };
            match upscaler.dispatch(ctx.device, ctx.encoder, &inputs, ctx.timing) {
                Ok(()) => Completed::Fsr2,
                // TAA, the antialiasing in effect without FSR2, takes over.
                Err(error) => {
                    self.fsr2 = None;
                    self.fsr2_error = Some(error);
                    self.failed = true;
                    Completed::Composite
                }
            }
        } else {
            Completed::Composite
        }
    }

    /// Whether FSR2 failed and stopped since the last call.
    pub fn take_failure(&mut self) -> bool {
        std::mem::take(&mut self.failed)
    }

    /// FSR2's output of the last successful frame.
    pub fn output(&self) -> &wgpu::TextureView {
        self.fsr2.as_ref().expect("FSR2 ran").output()
    }

    #[cfg(all(test, not(target_arch = "wasm32")))]
    pub fn fsr2(&self) -> Option<&fsr2::Fsr2> {
        self.fsr2.as_ref()
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
