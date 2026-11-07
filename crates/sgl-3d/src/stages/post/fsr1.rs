//! AMD FSR1 EASU, after display mapping/antialiasing. RCAS is fused into
//! tone mapping's existing final output/dither pass (or diagnostic capture).
//! Same stage order on native and WebGPU, with no optional GPU features.
use super::inputs::{self, Inputs, draw, pipeline};
use crate::shading::gbuffer::COLOR as HDR;
use crate::view::targets::target;

pub(crate) static FSR1: crate::shading::Module = crate::shading::Module {
    name: "fsr1",
    source: include_str!("fsr1.wgsl"),
    deps: &[&inputs::INPUTS],
};

pub(super) struct Fsr1 {
    easu: wgpu::RenderPipeline,
    target: Option<wgpu::TextureView>,
}
impl Fsr1 {
    pub fn new(device: &wgpu::Device, inputs: &Inputs) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("FSR1 EASU"),
            source: wgpu::ShaderSource::Wgsl(crate::shading::compose(&[&FSR1]).into()),
        });
        Self {
            easu: pipeline(
                device,
                &[Some(inputs.layout())],
                &shader,
                "fsr1_easu",
                &[HDR],
                (None, &[]),
            ),
            target: None,
        }
    }
    pub fn target_is_held(&self) -> bool {
        self.target.is_some()
    }
    pub fn release(&mut self) {
        self.target = None;
    }
    #[allow(clippy::too_many_arguments)]
    pub fn encode(
        &mut self,
        device: &wgpu::Device,
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
        let group = inputs.group(device, scene, bloom);
        draw(
            encoder,
            &self.easu,
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
