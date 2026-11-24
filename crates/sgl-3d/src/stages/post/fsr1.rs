//! AMD FSR1 EASU, after display mapping/antialiasing, writing gamma 2.
//! RCAS, whose kernel `fsr1.wgsl` also holds, runs in post's final pass
//! (`tone_map::Final`), which decodes EASU's output with or without it.
//! Same stage order on native and WebGPU, with no optional GPU features.
use super::inputs::{self, Inputs, draw, pipeline};
use crate::shading::gbuffer::COLOR as HDR;
use crate::view::compile::{Compiled, Compiler};
use crate::view::targets::target;

pub(crate) static FSR1: crate::shading::Module = crate::shading::Module {
    name: "fsr1",
    source: sgl_post_fx::embed_wgsl!("fsr1.wgsl"),
    deps: &[&inputs::INPUTS],
};

#[derive(Default)]
pub(super) struct Fsr1 {
    easu: Compiled<wgpu::RenderPipeline>,
    target: Option<wgpu::TextureView>,
}
impl Fsr1 {
    pub fn target_is_held(&self) -> bool {
        self.target.is_some()
    }
    pub fn release(&mut self) {
        self.target = None;
    }
    #[allow(clippy::too_many_arguments)]
    pub fn encode(
        &mut self,
        (device, compiler): (&wgpu::Device, &Compiler),
        encoder: &mut wgpu::CommandEncoder,
        inputs: &Inputs,
        scene: &wgpu::TextureView,
        bloom: &wgpu::TextureView,
        size: [u32; 2],
        timing: Option<&crate::timing::GpuTiming>,
    ) -> &wgpu::TextureView {
        let target = self
            .target
            .get_or_insert_with(|| target(device, "FSR1 EASU output", size, HDR));
        // A planning pass that lacks the pipeline leaves the pass out: the
        // target is handed on either way.
        let Some(easu) = self.easu.get(compiler, "FSR1 EASU", || {
            let layout = inputs.layout().clone();
            move |device: &wgpu::Device| {
                let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some("FSR1 EASU"),
                    source: wgpu::ShaderSource::Wgsl(crate::shading::compose(&[&FSR1]).into()),
                });
                pipeline(
                    device,
                    &[Some(&layout)],
                    &shader,
                    "fsr1_easu",
                    &[HDR],
                    (None, &[]),
                )
            }
        }) else {
            return target;
        };
        let group = inputs.group(device, scene, bloom);
        draw(
            encoder,
            easu,
            &group,
            target,
            "FSR1 EASU",
            timing.and_then(|t| t.render_pass("spatial upscale")),
        );
        target
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
