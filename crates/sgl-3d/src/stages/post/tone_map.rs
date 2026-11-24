//! Tone mapping and presentation (`tone_map.wgsl`): the frame's exposure,
//! Bevy's colour grading and the selected tone mapper, from the HDR
//! scene to the output, with Bevy's deband dither. Directly, or, when
//! diagnostics capture it, into the tone-mapped target at the output size,
//! undithered, which is then copied to the output texel for texel and
//! dithered alike. Where SMAA or spatial upscaling runs, into a target at
//! the scene size, and where sharpening runs without them, into one at the
//! output size (`encode` to `Destination::Intermediate`), which the final
//! pass (`present_final`) then resamples, decodes or sharpens and presents
//! the same two ways.
use super::inputs::{self, Inputs, draw, sampled, uniform_entry};
use crate::frame_input::{AgxLook, ColorGrading};
use crate::settings::ToneMapping;
use crate::shading::gbuffer::COLOR as HDR;
use crate::view::compile::{Compiler, SharedModule};
use crate::view::targets::target;
use glam::{Mat3, Vec2, Vec3, vec2, vec3};

/// Exposure, grading, tone mapping and the tone-mapped target's copy.
pub(crate) static TONE_MAP: crate::shading::Module = crate::shading::Module {
    name: "tone_map",
    source: sgl_post_fx::embed_wgsl!("tone_map.wgsl"),
    deps: &[
        &inputs::INPUTS,
        &crate::shading::LUMINANCE,
        &crate::shading::SRGB,
        &super::fsr1::FSR1,
        &super::local_exposure::LOCAL_EXPOSURE,
    ],
};
/// The entry points tone mapping's pipelines are created with, beside
/// `inputs::VS_ENTRY`.
pub(crate) const PRESENT_ENTRY: &str = "present";
pub(crate) const PRESENT_DIRECT_ENTRY: &str = "present_direct";
pub(crate) const COPY_PIXEL_ENTRY: &str = "copy_pixel";
pub(crate) const RESAMPLE_ENTRY: &str = "resample";
pub(crate) const RESAMPLE_DIRECT_ENTRY: &str = "resample_direct";
pub(crate) const SHARPEN_ENTRY: &str = "sharpen";
pub(crate) const SHARPEN_DIRECT_ENTRY: &str = "sharpen_direct";
pub(crate) const SHARPEN_ENCODED_ENTRY: &str = "sharpen_encoded";
pub(crate) const SHARPEN_ENCODED_DIRECT_ENTRY: &str = "sharpen_encoded_direct";
pub(crate) const DECODE_ENTRY: &str = "decode";
pub(crate) const DECODE_DIRECT_ENTRY: &str = "decode_direct";

/// `Sharpening` in `tone_map.wgsl`: RCAS's strength.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct SharpeningUniform {
    strength: f32,
}

impl SharpeningUniform {
    /// AMD's `sharpness` in 0..=1 as its SDK passes it to RCAS: 2 - 2
    /// `sharpness` stops of attenuation (`ffx_fsr2.cpp` 1262,
    /// `sharpenessRemapped`), as `FsrRcasCon` turns stops into a factor,
    /// `exp2(-stops)`.
    pub fn new(sharpness: f32) -> Self {
        Self {
            strength: (-(2. - 2. * sharpness)).exp2(),
        }
    }
}

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
    /// Khronos Neutral, ACES Filmic, or an AgX look.
    tone_mapping: u32,
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
    /// back to sRGB) and the sections' values by section; and the tone mapper.
    pub fn new(grading: &ColorGrading, tone_mapping: ToneMapping) -> Self {
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
            // Values shared with tone_map.wgsl.
            tone_mapping: match tone_mapping {
                ToneMapping::KhronosPbrNeutral => 0,
                ToneMapping::AcesFilmic => 1,
                ToneMapping::Agx => match grading.agx_look {
                    AgxLook::None => 2,
                    AgxLook::Punchy => 3,
                    AgxLook::Golden => 4,
                },
            },
            ..Default::default()
        }
    }
}

/// The layouts tone mapping mirrors.
#[cfg(test)]
pub(crate) fn mirrors() -> Vec<crate::shading::layout_tests::Mirror> {
    use crate::shading::layout_tests::mirror;
    vec![
        mirror!(
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
                tone_mapping,
            ]
        ),
        mirror!("tone_map", "Sharpening", SharpeningUniform, [strength]),
    ]
}

/// Where tone mapping writes.
pub(super) enum Destination<'a> {
    /// The output, through the tone-mapped target with `capture`.
    Output {
        capture: bool,
        output: &'a wgpu::TextureView,
    },
    /// An RGBA16F target, undithered, for the passes before the final one.
    Intermediate(&'a wgpu::TextureView),
}

/// What the final pass does before the output transfer and dither.
#[derive(Clone, Copy, Debug)]
pub(super) enum Final {
    /// Resamples the display-referred scene to the output, linearly.
    Resample,
    /// Decodes EASU's gamma-2 output, texel for texel.
    Decode,
    /// AMD's RCAS at `sharpness`, texel for texel, over linear display
    /// colour at the output size, or EASU's gamma-2 output where `encoded`.
    Sharpen { sharpness: f32, encoded: bool },
}

mod pipelines;
use pipelines::{Pass, Pipelines, Target};

pub(super) struct ToneMap {
    pipelines: Pipelines,
    /// `SharpeningUniform`, and the group binding it.
    sharpening: wgpu::Buffer,
    sharpening_group: wgpu::BindGroup,
    /// `ColorGradingUniform`.
    grading: wgpu::Buffer,
    /// `local_exposure::LocalExposureUniform`.
    local_exposure: wgpu::Buffer,
    /// Local exposure's grid and base while it runs, else 1-texel
    /// placeholders the lookup never samples.
    local_views: [wgpu::TextureView; 2],
    local_placeholders: [wgpu::TextureView; 2],
    /// The look group for the exposure view and local exposure's views it
    /// binds, made for the first of each.
    look: Option<([wgpu::TextureView; 3], wgpu::BindGroup)>,
    tone_mapped: Option<wgpu::TextureView>,
    output_size: [u32; 2],
    /// The last frame wrote `tone_mapped`.
    #[cfg(any(test, feature = "diagnostics"))]
    captured: bool,
}

impl ToneMap {
    /// Tone mapping to `format` at `output_size`.
    pub fn new(
        device: &wgpu::Device,
        inputs: &Inputs,
        destination: super::Output,
        output_size: [u32; 2],
    ) -> Self {
        let shader = SharedModule::composed("HDR presentation", &TONE_MAP);
        let look_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("tone map exposure, grading and local exposure"),
            entries: &[
                sampled(0, false),
                uniform_entry(1),
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D3,
                        multisampled: false,
                    },
                    count: None,
                },
                sampled(4, true),
                uniform_entry(5),
            ],
        });
        let placeholder = |label, depth, dimension| {
            device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d {
                        width: 1,
                        height: 1,
                        depth_or_array_layers: depth,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension,
                    format: HDR,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                })
                .create_view(&Default::default())
        };
        let local_placeholders = [
            placeholder("no local exposure grid", 1, wgpu::TextureDimension::D3),
            placeholder("no local exposure base", 1, wgpu::TextureDimension::D2),
        ];
        let sharpening_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("sharpening"),
            entries: &[uniform_entry(2)],
        });
        let sharpening = crate::counters::buffer_init(
            device,
            &wgpu::util::BufferInitDescriptor {
                label: Some("sharpening"),
                contents: bytemuck::bytes_of(&SharpeningUniform::default()),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            },
        );
        let sharpening_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("sharpening"),
            layout: &sharpening_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 2,
                resource: sharpening.as_entire_binding(),
            }],
        });
        Self {
            pipelines: Pipelines {
                shader,
                output: destination,
                inputs_layout: inputs.layout().clone(),
                look_layout,
                sharpening_layout,
                cached: Default::default(),
            },
            sharpening,
            sharpening_group,
            grading: crate::counters::buffer(
                device,
                &wgpu::BufferDescriptor {
                    label: Some("colour grading"),
                    size: size_of::<ColorGradingUniform>() as u64,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                },
            ),
            local_exposure: crate::counters::buffer_init(
                device,
                &wgpu::util::BufferInitDescriptor {
                    label: Some("local exposure"),
                    contents: bytemuck::bytes_of(
                        &super::local_exposure::LocalExposureUniform::new(None),
                    ),
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                },
            ),
            local_views: local_placeholders.clone(),
            local_placeholders,
            look: None,
            tone_mapped: None,
            output_size,
            #[cfg(any(test, feature = "diagnostics"))]
            captured: false,
        }
    }

    pub fn resize(&mut self, _device: &wgpu::Device, output_size: [u32; 2]) {
        self.tone_mapped = None;
        // Local exposure's targets are released at resize too: drop the
        // views and the group that hold them until the next frame binds
        // its own.
        self.local_views = self.local_placeholders.clone();
        self.look = None;
        self.output_size = output_size;
        #[cfg(any(test, feature = "diagnostics"))]
        {
            self.captured = false;
        }
    }

    /// The tone-mapped scene of the last frame that captured it.
    #[cfg(any(test, feature = "diagnostics"))]
    pub fn tone_mapped(&self) -> &wgpu::TextureView {
        self.tone_mapped
            .as_ref()
            .expect("tone-mapped capture allocated")
    }

    /// The tone-mapped scene, when the last frame captured it.
    #[cfg(any(test, feature = "diagnostics"))]
    pub fn captured(&self) -> Option<&wgpu::TextureView> {
        self.tone_mapped.as_ref().filter(|_| self.captured)
    }

    #[cfg(all(test, not(target_arch = "wasm32")))]
    pub(super) fn holds_capture(&self) -> bool {
        self.tone_mapped.is_some()
    }

    /// Diagnostics alone need a full output-size floating-point target.
    pub(super) fn hold_capture(&mut self, device: &wgpu::Device) {
        self.tone_mapped.get_or_insert_with(|| {
            target(device, "tone-mapped linear scene", self.output_size, HDR)
        });
    }

    /// Release both the image and input groups retaining its view.
    pub(super) fn release_capture(&mut self, inputs: &Inputs) {
        if self.tone_mapped.take().is_some() {
            inputs.forget();
        }
        #[cfg(any(test, feature = "diagnostics"))]
        {
            self.captured = false;
        }
    }

    /// Whether the lookup binds local exposure's views (not placeholders).
    #[cfg(all(test, not(target_arch = "wasm32")))]
    pub(super) fn binds_local_exposure(&self) -> bool {
        self.local_views != self.local_placeholders
    }

    /// Local exposure's lookup for the next tone mapping: its blurred grid
    /// and base at `contrasts` (highlight, shadow), or off.
    pub fn set_local_exposure(
        &mut self,
        queue: &wgpu::Queue,
        lookup: Option<(wgpu::TextureView, wgpu::TextureView, [f32; 2])>,
    ) {
        let (views, contrasts) = match lookup {
            Some((grid, base, contrasts)) => ([grid, base], Some(contrasts)),
            None => (self.local_placeholders.clone(), None),
        };
        self.local_views = views;
        crate::counters::write_buffer(
            queue,
            &self.local_exposure,
            0,
            bytemuck::bytes_of(&super::local_exposure::LocalExposureUniform::new(contrasts)),
        );
    }

    /// The group binding `exposure`, the grading and local exposure's
    /// views, made once per set of views.
    fn look(&mut self, device: &wgpu::Device, exposure: &wgpu::TextureView) -> wgpu::BindGroup {
        let [grid, base] = &self.local_views;
        let views = [exposure.clone(), grid.clone(), base.clone()];
        if let Some((held, group)) = &self.look
            && *held == views
        {
            return group.clone();
        }
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("tone map exposure, grading and local exposure"),
            layout: &self.pipelines.look_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(exposure),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: self.grading.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(grid),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(base),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: self.local_exposure.as_entire_binding(),
                },
            ],
        });
        self.look = Some((views, group.clone()));
        group
    }

    /// `hdr` exposed by `exposure`, graded as `grading` and tone mapped, to
    /// `destination`. `bloom` is the bloom view the inputs' groups bind.
    #[allow(clippy::too_many_arguments)]
    pub fn encode(
        &mut self,
        (device, compiler): (&wgpu::Device, &Compiler),
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        inputs: &Inputs,
        hdr: &wgpu::TextureView,
        bloom: &wgpu::TextureView,
        exposure: &wgpu::TextureView,
        grading: &ColorGrading,
        tone_mapping: ToneMapping,
        destination: Destination<'_>,
        timing: Option<&crate::timing::GpuTiming>,
    ) {
        crate::counters::write_buffer(
            queue,
            &self.grading,
            0,
            bytemuck::bytes_of(&ColorGradingUniform::new(grading, tone_mapping)),
        );
        let look = self.look(device, exposure);
        let group = inputs.group(device, hdr, bloom);
        if matches!(destination, Destination::Output { capture: false, .. }) {
            self.release_capture(inputs);
        }
        if matches!(destination, Destination::Output { capture: true, .. }) {
            self.hold_capture(device);
        }
        let (pipeline, target, label, presented) = match destination {
            Destination::Intermediate(target) => (
                Pass::ToneMap(Target::Linear),
                target,
                "tone map to an intermediate target",
                None,
            ),
            Destination::Output { capture, output } => {
                #[cfg(any(test, feature = "diagnostics"))]
                {
                    self.captured = capture;
                }
                if capture {
                    (
                        Pass::ToneMap(Target::Linear),
                        self.tone_mapped.as_ref().unwrap(),
                        "tone map to the tone-mapped target",
                        Some(output),
                    )
                } else {
                    (
                        Pass::ToneMap(Target::Output),
                        output,
                        "tone map to the output",
                        None,
                    )
                }
            }
        };
        // Not in a planning pass whose pipeline is still compiling.
        if let Some(pipeline) = self.pipelines.get(compiler, pipeline) {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some(label),
                timestamp_writes: timing.and_then(|t| t.render_pass("tone map")),
                color_attachments: &[crate::view::targets::attachment(target)],
                ..Default::default()
            });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &group, &[]);
            pass.set_bind_group(1, &look, &[]);
            pass.draw(0..3, 0..1);
        }
        if let Some(output) = presented {
            self.present_output((device, compiler), encoder, inputs, bloom, output, timing);
        }
    }

    /// `scene`, the display-referred scene, linearly resampled into
    /// `target`, an RGBA16F target of the output size, undithered.
    #[allow(clippy::too_many_arguments)]
    pub fn resample_into(
        &mut self,
        (device, compiler): (&wgpu::Device, &Compiler),
        encoder: &mut wgpu::CommandEncoder,
        inputs: &Inputs,
        scene: &wgpu::TextureView,
        bloom: &wgpu::TextureView,
        target: &wgpu::TextureView,
        timing: Option<&crate::timing::GpuTiming>,
    ) {
        if let Some(pipeline) = self.pipelines.get(compiler, Pass::Resample(Target::Linear)) {
            draw(
                encoder,
                &pipeline,
                &inputs.group(device, scene, bloom),
                target,
                "resample the scene for sharpening",
                timing.and_then(|t| t.render_pass("tone map")),
            );
        }
    }

    /// The final pass: `scene`, display-referred, through `last` to
    /// `output`, through the tone-mapped target with `capture`.
    #[allow(clippy::too_many_arguments)]
    pub fn present_final(
        &mut self,
        (device, compiler): (&wgpu::Device, &Compiler),
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        inputs: &Inputs,
        scene: &wgpu::TextureView,
        bloom: &wgpu::TextureView,
        capture: bool,
        last: Final,
        output: &wgpu::TextureView,
        timing: Option<&crate::timing::GpuTiming>,
    ) {
        #[cfg(any(test, feature = "diagnostics"))]
        {
            self.captured = capture;
        }
        if capture {
            self.hold_capture(device);
        } else {
            self.release_capture(inputs);
        }
        let target = if capture {
            Target::Linear
        } else {
            Target::Output
        };
        let (pipeline, label, timing_group) = match last {
            Final::Resample => (Pass::Resample(target), "resample the scene", "tone map"),
            Final::Decode => (
                Pass::Decode(target),
                "decode the upscaled scene",
                "tone map",
            ),
            Final::Sharpen { sharpness, encoded } => {
                crate::counters::write_buffer(
                    queue,
                    &self.sharpening,
                    0,
                    bytemuck::bytes_of(&SharpeningUniform::new(sharpness)),
                );
                (
                    Pass::Sharpen { target, encoded },
                    "sharpen (RCAS)",
                    "sharpening",
                )
            }
        };
        let target = if capture {
            self.tone_mapped.as_ref().unwrap()
        } else {
            output
        };
        let group = inputs.group(device, scene, bloom);
        // Not in a planning pass whose pipeline is still compiling.
        if let Some(pipeline) = self.pipelines.get(compiler, pipeline) {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some(label),
                timestamp_writes: timing.and_then(|t| t.render_pass(timing_group)),
                color_attachments: &[crate::view::targets::attachment(target)],
                ..Default::default()
            });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &group, &[]);
            if matches!(last, Final::Sharpen { .. }) {
                pass.set_bind_group(1, &self.sharpening_group, &[]);
            }
            pass.draw(0..3, 0..1);
        }
        if capture {
            self.present_output((device, compiler), encoder, inputs, bloom, output, timing);
        }
    }

    /// The tone-mapped target to `output`, texel for texel, dithered.
    pub(super) fn present_output(
        &mut self,
        (device, compiler): (&wgpu::Device, &Compiler),
        encoder: &mut wgpu::CommandEncoder,
        inputs: &Inputs,
        bloom: &wgpu::TextureView,
        output: &wgpu::TextureView,
        timing: Option<&crate::timing::GpuTiming>,
    ) {
        let group = inputs.group(device, self.tone_mapped.as_ref().unwrap(), bloom);
        if let Some(pipeline) = self.pipelines.get(compiler, Pass::Copy) {
            draw(
                encoder,
                &pipeline,
                &group,
                output,
                "present tone-mapped texels",
                timing.and_then(|t| t.render_pass("tone map")),
            );
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
pub(super) mod tests;
