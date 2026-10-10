//! Tone mapping and presentation (`tone_map.wgsl`): the frame's exposure,
//! Bevy's colour grading and Filament's AgX with its look, from the HDR
//! scene to the output, with Bevy's deband dither. Directly, or, when
//! diagnostics capture it, into the tone-mapped target at the output size,
//! undithered, which is then copied to the output texel for texel and
//! dithered alike. Where SMAA runs, into a target at the scene size
//! (`encode_scene`), which SMAA antialiases and `resample` then presents the
//! same two ways.
use super::inputs::{self, Inputs, draw, pipeline, sampled, uniform_entry};
use crate::frame_input::{AgxLook, ColorGrading};
use crate::shading::gbuffer::COLOR as HDR;
use crate::view::targets::target;
use glam::{Mat3, Vec2, Vec3, vec2, vec3};

/// Exposure, grading, AgX and the tone-mapped target's copy.
pub(crate) static TONE_MAP: crate::shading::Module = crate::shading::Module {
    name: "tone_map",
    source: include_str!("tone_map.wgsl"),
    deps: &[&inputs::INPUTS, &crate::shading::LUMINANCE],
};
/// The entry points tone mapping's pipelines are created with, beside
/// `inputs::VS_ENTRY`.
pub(crate) const PRESENT_ENTRY: &str = "present";
pub(crate) const PRESENT_DIRECT_ENTRY: &str = "present_direct";
pub(crate) const COPY_PIXEL_ENTRY: &str = "copy_pixel";
pub(crate) const RESAMPLE_ENTRY: &str = "resample";
pub(crate) const RESAMPLE_DIRECT_ENTRY: &str = "resample_direct";

/// `ColorGrading` in `tone_map.wgsl`: Bevy's `ColorGradingUniform` without
/// its exposure.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct ColorGradingUniform {
    balance: [[f32; 4]; 3],
    saturation: [f32; 3],
    _padding0: f32,
    contrast: [f32; 3],
    _padding1: f32,
    gamma: [f32; 3],
    _padding2: f32,
    gain: [f32; 3],
    _padding3: f32,
    lift: [f32; 3],
    _padding4: f32,
    midtone_range: [f32; 2],
    hue: f32,
    post_saturation: f32,
    /// Filament's `AgxLook` value.
    agx_look: u32,
    _padding5: [u32; 3],
}

/// Bevy's `RGB_TO_LMS`, `LMS_TO_RGB`, `D65_XY` and `D65_LMS`
/// (`crates/bevy_render/src/view/mod.rs`).
const RGB_TO_LMS: Mat3 = Mat3::from_cols(
    vec3(0.311692, 0.0905138, 0.00764433),
    vec3(0.652085, 0.901341, 0.0486554),
    vec3(0.0362225, 0.00814478, 0.943700),
);
const LMS_TO_RGB: Mat3 = Mat3::from_cols(
    vec3(4.06305, -0.40791, -0.0118812),
    vec3(-2.93241, 1.40437, -0.0486532),
    vec3(-0.130646, 0.00353630, 1.0605344),
);
const D65_XY: Vec2 = vec2(0.31272, 0.32903);
const D65_LMS: Vec3 = vec3(0.975538, 1.01648, 1.08475);

impl ColorGradingUniform {
    /// Bevy's `From<ColorGrading> for ColorGradingUniform`: the white
    /// balance as one matrix (sRGB to LMS, the white point scaled to D65's,
    /// back to sRGB) and the sections' values by section; and the AgX look.
    pub fn new(grading: &ColorGrading) -> Self {
        let global = &grading.global;
        let white_point_xy = D65_XY + vec2(-global.temperature, global.tint);
        // The white point from CIE 1931 xy (Y = 1) to LMS by CAM16.
        let white_point_lms = vec3(0.701634, 1.15856, -0.904175)
            + (vec3(-0.051461, 0.045854, 0.953127)
                + vec3(0.452749, -0.296122, -0.955206) * white_point_xy.x)
                / white_point_xy.y;
        let white_point_adjustment = Mat3::from_diagonal(D65_LMS / white_point_lms);
        let balance = LMS_TO_RGB * white_point_adjustment * RGB_TO_LMS;
        let sections = [grading.shadows, grading.midtones, grading.highlights];
        let by_section =
            |value: fn(&crate::ColorGradingSection) -> f32| sections.each_ref().map(value);
        Self {
            balance: balance.to_cols_array_2d().map(|[x, y, z]| [x, y, z, 0.]),
            saturation: by_section(|section| section.saturation),
            contrast: by_section(|section| section.contrast),
            gamma: by_section(|section| section.gamma),
            gain: by_section(|section| section.gain),
            lift: by_section(|section| section.lift),
            midtone_range: [global.midtones_start, global.midtones_end],
            hue: global.hue,
            post_saturation: global.post_saturation,
            // Filament's `AgxLook` values.
            agx_look: match grading.agx_look {
                AgxLook::None => 0,
                AgxLook::Punchy => 1,
                AgxLook::Golden => 2,
            },
            ..Default::default()
        }
    }
}

/// The layouts tone mapping mirrors.
#[cfg(test)]
pub(crate) fn mirrors() -> Vec<crate::shading::layout_tests::Mirror> {
    use crate::shading::layout_tests::mirror;
    vec![mirror!(
        "tone_map",
        "ColorGrading",
        ColorGradingUniform,
        [
            balance,
            saturation,
            contrast,
            gamma,
            gain,
            lift,
            midtone_range,
            hue,
            post_saturation,
            agx_look,
        ]
    )]
}

/// Where tone mapping writes.
pub(super) enum Destination<'a> {
    /// The output, through the tone-mapped target with `capture`.
    Output {
        capture: bool,
        output: &'a wgpu::TextureView,
    },
    /// An RGBA16F target of the scene's size, undithered, for SMAA.
    Scene(&'a wgpu::TextureView),
}

pub(super) struct ToneMap {
    /// Into the tone-mapped target.
    present: wgpu::RenderPipeline,
    /// Into the output, rounded as the tone-mapped target would be.
    present_direct: wgpu::RenderPipeline,
    /// The tone-mapped target into the output.
    present_copy: wgpu::RenderPipeline,
    /// The antialiased tone-mapped scene into the tone-mapped target.
    resample: wgpu::RenderPipeline,
    /// The antialiased tone-mapped scene into the output, as the copy
    /// writes it.
    resample_direct: wgpu::RenderPipeline,
    look_layout: wgpu::BindGroupLayout,
    /// `ColorGradingUniform`.
    grading: wgpu::Buffer,
    /// The exposure view and grading, made for the first exposure view.
    look: Option<(wgpu::TextureView, wgpu::BindGroup)>,
    tone_mapped: wgpu::TextureView,
    /// The last frame wrote `tone_mapped`.
    #[cfg(any(test, feature = "diagnostics"))]
    captured: bool,
}

impl ToneMap {
    /// Tone mapping to `format` at `output_size`.
    pub fn new(
        device: &wgpu::Device,
        inputs: &Inputs,
        format: wgpu::TextureFormat,
        output_size: [u32; 2],
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("HDR presentation"),
            source: wgpu::ShaderSource::Wgsl(crate::shading::compose(&[&TONE_MAP]).into()),
        });
        let look_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("tone map exposure and grading"),
            entries: &[sampled(0, false), uniform_entry(1)],
        });
        let layouts = [Some(inputs.layout()), Some(&look_layout)];
        let copy = [Some(inputs.layout())];
        Self {
            present: pipeline(device, &layouts, &shader, PRESENT_ENTRY, &[HDR], None),
            present_direct: pipeline(
                device,
                &layouts,
                &shader,
                PRESENT_DIRECT_ENTRY,
                &[format],
                None,
            ),
            present_copy: pipeline(device, &copy, &shader, COPY_PIXEL_ENTRY, &[format], None),
            resample: pipeline(device, &copy, &shader, RESAMPLE_ENTRY, &[HDR], None),
            resample_direct: pipeline(
                device,
                &copy,
                &shader,
                RESAMPLE_DIRECT_ENTRY,
                &[format],
                None,
            ),
            grading: crate::counters::buffer(
                device,
                &wgpu::BufferDescriptor {
                    label: Some("colour grading"),
                    size: size_of::<ColorGradingUniform>() as u64,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                },
            ),
            look_layout,
            look: None,
            tone_mapped: target(device, "tone-mapped linear scene", output_size, HDR),
            #[cfg(any(test, feature = "diagnostics"))]
            captured: false,
        }
    }

    pub fn resize(&mut self, device: &wgpu::Device, output_size: [u32; 2]) {
        self.tone_mapped = target(device, "tone-mapped linear scene", output_size, HDR);
        #[cfg(any(test, feature = "diagnostics"))]
        {
            self.captured = false;
        }
    }

    /// The tone-mapped scene of the last frame that captured it.
    #[cfg(any(test, feature = "diagnostics"))]
    pub fn tone_mapped(&self) -> &wgpu::TextureView {
        &self.tone_mapped
    }

    /// The tone-mapped scene, when the last frame captured it.
    #[cfg(any(test, feature = "diagnostics"))]
    pub fn captured(&self) -> Option<&wgpu::TextureView> {
        self.captured.then_some(&self.tone_mapped)
    }

    /// The group binding `exposure` and the grading, made once per view.
    fn look(&mut self, device: &wgpu::Device, exposure: &wgpu::TextureView) -> wgpu::BindGroup {
        if let Some((view, group)) = &self.look
            && view == exposure
        {
            return group.clone();
        }
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("tone map exposure and grading"),
            layout: &self.look_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(exposure),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: self.grading.as_entire_binding(),
                },
            ],
        });
        self.look = Some((exposure.clone(), group.clone()));
        group
    }

    /// `hdr` exposed by `exposure`, graded as `grading` and tone mapped, to
    /// `destination`. `bloom` is the bloom view the inputs' groups bind.
    #[allow(clippy::too_many_arguments)]
    pub fn encode(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        inputs: &Inputs,
        hdr: &wgpu::TextureView,
        bloom: &wgpu::TextureView,
        exposure: &wgpu::TextureView,
        grading: &ColorGrading,
        destination: Destination<'_>,
        timing: Option<&crate::timing::GpuTiming>,
    ) {
        crate::counters::write_buffer(
            queue,
            &self.grading,
            0,
            bytemuck::bytes_of(&ColorGradingUniform::new(grading)),
        );
        let look = self.look(device, exposure);
        let group = inputs.group(device, hdr, bloom);
        let (pipeline, target, label, presented) = match destination {
            Destination::Scene(scene) => {
                (&self.present, scene, "tone map the scene for SMAA", None)
            }
            Destination::Output { capture, output } => {
                #[cfg(any(test, feature = "diagnostics"))]
                {
                    self.captured = capture;
                }
                if capture {
                    (
                        &self.present,
                        &self.tone_mapped,
                        "tone map to the tone-mapped target",
                        Some(output),
                    )
                } else {
                    (&self.present_direct, output, "tone map to the output", None)
                }
            }
        };
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some(label),
            timestamp_writes: timing.and_then(|t| t.render_pass("tone map")),
            color_attachments: &[crate::view::targets::attachment(target)],
            ..Default::default()
        });
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &group, &[]);
        pass.set_bind_group(1, &look, &[]);
        pass.draw(0..3, 0..1);
        drop(pass);
        if let Some(output) = presented {
            self.present_output(device, encoder, inputs, bloom, output, timing);
        }
    }

    /// `scene`, the antialiased tone-mapped scene at the scene size,
    /// resampled to `output`, through the tone-mapped target with `capture`.
    #[allow(clippy::too_many_arguments)]
    pub fn resample(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        inputs: &Inputs,
        scene: &wgpu::TextureView,
        bloom: &wgpu::TextureView,
        capture: bool,
        output: &wgpu::TextureView,
        timing: Option<&crate::timing::GpuTiming>,
    ) {
        #[cfg(any(test, feature = "diagnostics"))]
        {
            self.captured = capture;
        }
        let group = inputs.group(device, scene, bloom);
        let (pipeline, target) = if capture {
            (&self.resample, &self.tone_mapped)
        } else {
            (&self.resample_direct, output)
        };
        draw(
            encoder,
            pipeline,
            &group,
            target,
            "resample the antialiased scene",
            timing.and_then(|t| t.render_pass("tone map")),
        );
        if capture {
            self.present_output(device, encoder, inputs, bloom, output, timing);
        }
    }

    /// The tone-mapped target to `output`, texel for texel, dithered.
    pub(super) fn present_output(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        inputs: &Inputs,
        bloom: &wgpu::TextureView,
        output: &wgpu::TextureView,
        timing: Option<&crate::timing::GpuTiming>,
    ) {
        let group = inputs.group(device, &self.tone_mapped, bloom);
        draw(
            encoder,
            &self.present_copy,
            &group,
            output,
            "present tone-mapped texels",
            timing.and_then(|t| t.render_pass("tone map")),
        );
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
