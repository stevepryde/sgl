//! Presentation pipelines retained by a view, each built for the first pass
//! that draws with it. Intermediate and capture targets both hold display-linear colour.
use super::{
    COPY_PIXEL_ENTRY, DECODE_DIRECT_ENTRY, DECODE_ENTRY, PRESENT_DIRECT_ENTRY, PRESENT_ENTRY,
    RESAMPLE_DIRECT_ENTRY, RESAMPLE_ENTRY, SHARPEN_DIRECT_ENTRY, SHARPEN_ENCODED_DIRECT_ENTRY,
    SHARPEN_ENCODED_ENTRY, SHARPEN_ENTRY,
};
use crate::shading::gbuffer::COLOR as HDR;
use crate::stages::post::{Output, inputs::pipeline};
use crate::view::compile::{Compiled, Compiler, SharedModule};
use std::collections::HashMap;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum Target {
    Linear,
    Output,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum Pass {
    ToneMap(Target),
    Copy,
    Resample(Target),
    Decode(Target),
    Sharpen { target: Target, encoded: bool },
}

pub(super) struct Pipelines {
    pub look_layout: wgpu::BindGroupLayout,
    pub sharpening_layout: wgpu::BindGroupLayout,
    pub inputs_layout: wgpu::BindGroupLayout,
    pub shader: SharedModule,
    pub output: Output,
    /// Each pass's pipeline, a compile job a pass.
    pub cached: HashMap<Pass, Compiled<wgpu::RenderPipeline>>,
}

impl Pipelines {
    /// `pass`'s pipeline, made where a frame first draws with it or by its
    /// compile job; none in a planning pass while it is missing, where
    /// the caller leaves the pass out.
    pub fn get(&mut self, compiler: &Compiler, pass: Pass) -> Option<wgpu::RenderPipeline> {
        use Target::{Linear, Output};
        let (entry, layout, target) = match pass {
            Pass::ToneMap(Linear) => (PRESENT_ENTRY, Some(&self.look_layout), Linear),
            Pass::ToneMap(Output) => (PRESENT_DIRECT_ENTRY, Some(&self.look_layout), Output),
            Pass::Copy => (COPY_PIXEL_ENTRY, None, Output),
            Pass::Resample(Linear) => (RESAMPLE_ENTRY, None, Linear),
            Pass::Resample(Output) => (RESAMPLE_DIRECT_ENTRY, None, Output),
            Pass::Decode(Linear) => (DECODE_ENTRY, None, Linear),
            Pass::Decode(Output) => (DECODE_DIRECT_ENTRY, None, Output),
            Pass::Sharpen { target, encoded } => {
                let entry = match (target, encoded) {
                    (Linear, false) => SHARPEN_ENTRY,
                    (Output, false) => SHARPEN_DIRECT_ENTRY,
                    (Linear, true) => SHARPEN_ENCODED_ENTRY,
                    (Output, true) => SHARPEN_ENCODED_DIRECT_ENTRY,
                };
                (entry, Some(&self.sharpening_layout), target)
            }
        };
        let (inputs, shader, output) = (&self.inputs_layout, &self.shader, self.output);
        self.cached
            .entry(pass)
            .or_default()
            .get(compiler, "presentation", || {
                let (inputs, extra, shader) = (inputs.clone(), layout.cloned(), shader.clone());
                move |device: &wgpu::Device| {
                    let layouts: &[_] = match &extra {
                        Some(extra) => &[Some(&inputs), Some(extra)],
                        None => &[Some(&inputs)],
                    };
                    let encodes = [("OUTPUT_ENCODES_SRGB", f64::from(output.encodes_srgb))];
                    let (format, constants): (_, &[_]) = match target {
                        Linear => (HDR, &[]),
                        Output => (output.format, &encodes),
                    };
                    pipeline(
                        device,
                        layouts,
                        shader.get(device),
                        entry,
                        &[format],
                        (None, constants),
                    )
                }
            })
            .map(|pipeline| pipeline.clone())
    }
}
