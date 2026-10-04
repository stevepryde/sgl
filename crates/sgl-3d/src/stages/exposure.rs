//! Exposure: the frame's one exposure, which FSR2 and the tone map read.
//! Fixed, it is `Exposure::stops`; automatic, it is Bevy's auto exposure
//! (`exposure.wgsl`): a 64-bin log-luminance histogram of the complete HDR
//! frame through the metering mask, then its filtered average, the
//! compensation curve and the adaptation at the authored speeds.
//!
//! Placement: Bevy meters after TAA and upscaling, at the output size,
//! before tone mapping. Here FSR2 needs the frame's exposure before it
//! runs, so metering reads the frame at the render size before
//! antialiasing, as Godot reduces luminance on its render-size internal
//! texture (b130438 `renderer_scene_render_rd.cpp` 570). The histogram sees
//! the jittered, un-antialiased frame and no bloom.
//!
//! Reads: the complete HDR frame at the render size (the composite), the
//! frame's exposure and frame time, and whether history continues.
//! Writes: its 1×1 R32Float exposure multiplier, and its adapted correction.
//! Honours: `FrameInput::exposure`.
//! Timing group: `exposure`.
//! History: the adapted correction, which takes its target when history
//! restarts or automatic exposure starts.
use crate::frame_input::{AutoExposure, Exposure as AuthoredExposure, MeteringMask};
use crate::view::frame::FrameContext;
use wgpu::util::DeviceExt;

/// The histogram and adaptation.
pub(crate) static EXPOSURE: crate::shading::Module = crate::shading::Module {
    name: "exposure",
    source: include_str!("exposure.wgsl"),
    deps: &[&crate::shading::LUMINANCE],
};

/// The exposure multiplier's format, which FSR2 reads as its exposure.
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R32Float;

// How auto exposure meters and adapts, Bevy's `AutoExposure` defaults
// (9d12036 `bevy_post_process/src/auto_exposure/settings.rs`). Metering
// follows `Exposure::stops`, so one fixed range serves every scene.
/// The log2 luminance the histogram spans (Bevy's `range`). Luminance below
/// it is metered at the least; above it counts in the highest bin.
const MIN_LOG_LUMINANCE: f32 = -8.;
const MAX_LOG_LUMINANCE: f32 = 8.;
/// The share of samples, from the darkest, metering ignores, and the share
/// it keeps (Bevy's `filter`): the darkest and brightest 10% are outliers.
const FILTER_LOW: f32 = 0.1;
const FILTER_HIGH: f32 = 0.9;
/// How far in stops from the target the adaptation turns from linear to
/// exponential, against jitter when the target keeps moving slightly.
const EXPONENTIAL_TRANSITION_DISTANCE: f32 = 1.5;

/// `AutoExposure` in `exposure.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct AutoExposureUniform {
    min_log_lum: f32,
    inv_log_lum_range: f32,
    log_lum_range: f32,
    low_percent: f32,
    high_percent: f32,
    speed_up: f32,
    speed_down: f32,
    exponential_transition_distance: f32,
    correction_min: f32,
    correction_max: f32,
    delta_time: f32,
    stops: f32,
    reset: u32,
    compensation_points: u32,
    _padding: [u32; 2],
    compensation: [[f32; 4]; 4],
}

impl AutoExposureUniform {
    /// `automatic` after `stops` for a frame of `delta_time` seconds,
    /// restarting at the target when `reset`.
    fn new(automatic: &AutoExposure, stops: f32, delta_time: f32, reset: bool) -> Self {
        let log_lum_range = MAX_LOG_LUMINANCE - MIN_LOG_LUMINANCE;
        // Bevy's `correction_bounds`: an invalid range does not limit.
        let (correction_min, correction_max) = if automatic.correction_min.is_finite()
            && automatic.correction_max.is_finite()
            && automatic.correction_min <= automatic.correction_max
        {
            (automatic.correction_min, automatic.correction_max)
        } else {
            (f32::MIN, f32::MAX)
        };
        let points = automatic.compensation.points();
        let mut compensation = [[0.; 4]; 4];
        for (index, point) in points.iter().enumerate() {
            compensation[index / 2][index % 2 * 2..index % 2 * 2 + 2].copy_from_slice(point);
        }
        Self {
            min_log_lum: MIN_LOG_LUMINANCE,
            inv_log_lum_range: log_lum_range.recip(),
            log_lum_range,
            low_percent: FILTER_LOW,
            high_percent: FILTER_HIGH,
            speed_up: automatic.speed_brighten,
            speed_down: automatic.speed_darken,
            exponential_transition_distance: EXPONENTIAL_TRANSITION_DISTANCE,
            correction_min,
            correction_max,
            delta_time,
            stops,
            reset: u32::from(reset),
            compensation_points: points.len() as u32,
            _padding: [0; 2],
            compensation,
        }
    }
}

/// The layouts this stage mirrors.
#[cfg(test)]
pub(crate) fn mirrors() -> Vec<crate::shading::layout_tests::Mirror> {
    use crate::shading::layout_tests::mirror;
    vec![mirror!(
        "exposure",
        "AutoExposure",
        AutoExposureUniform,
        [
            min_log_lum,
            inv_log_lum_range,
            log_lum_range,
            low_percent,
            high_percent,
            speed_up,
            speed_down,
            exponential_transition_distance,
            correction_min,
            correction_max,
            delta_time,
            stops,
            reset,
            compensation_points,
            compensation,
        ]
    )]
}

/// One frame's auto exposure.
#[derive(Clone, Copy)]
struct Metering<'a> {
    automatic: &'a AutoExposure,
    /// `Exposure::stops`.
    stops: f32,
    /// The frame's seconds.
    delta_time: f32,
    /// The correction takes its target.
    reset: bool,
}

pub(crate) struct Exposure {
    histogram_pipeline: wgpu::ComputePipeline,
    average_pipeline: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
    settings: wgpu::Buffer,
    histogram: wgpu::Buffer,
    correction: wgpu::Buffer,
    mask: wgpu::Texture,
    /// The mask `mask` holds.
    mask_weights: Option<MeteringMask>,
    exposure: wgpu::Texture,
    exposure_view: wgpu::TextureView,
    /// The fixed exposure last written, while exposure is fixed.
    fixed: Option<f32>,
    /// One bind group per frame view metered, until the views are replaced.
    groups: Vec<(wgpu::TextureView, wgpu::BindGroup)>,
}

impl Exposure {
    pub fn new(device: &wgpu::Device) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("auto exposure"),
            source: wgpu::ShaderSource::Wgsl(crate::shading::compose(&[&EXPOSURE]).into()),
        });
        let texture = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let buffer = |binding, ty| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let storage = wgpu::BufferBindingType::Storage { read_only: false };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("auto exposure"),
            entries: &[
                buffer(0, wgpu::BufferBindingType::Uniform),
                texture(1),
                texture(2),
                buffer(3, storage),
                buffer(4, storage),
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: FORMAT,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("auto exposure"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = |entry| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry),
                layout: Some(&pipeline_layout),
                module: &shader,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        let texture = |label, size: u32, format, usage| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width: size,
                    height: size,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST | usage,
                view_formats: &[],
            })
        };
        let exposure = texture(
            "frame exposure",
            1,
            FORMAT,
            wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
        );
        Self {
            histogram_pipeline: pipeline("compute_histogram"),
            average_pipeline: pipeline("compute_average"),
            settings: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("auto exposure settings"),
                size: size_of::<AutoExposureUniform>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            histogram: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("auto exposure histogram"),
                contents: &[0; 64 * 4],
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            }),
            correction: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("auto exposure correction"),
                contents: &[0; 4],
                usage: wgpu::BufferUsages::STORAGE,
            }),
            mask: texture(
                "metering mask",
                16,
                wgpu::TextureFormat::R8Unorm,
                wgpu::TextureUsages::empty(),
            ),
            mask_weights: None,
            exposure_view: exposure.create_view(&Default::default()),
            exposure,
            fixed: None,
            layout,
            groups: Vec::new(),
        }
    }

    /// The frame's exposure multiplier, 1×1 R32Float, which FSR2 reads.
    pub fn texture(&self) -> &wgpu::Texture {
        &self.exposure
    }

    /// The frame's exposure multiplier, which the tone map reads.
    pub fn view(&self) -> &wgpu::TextureView {
        &self.exposure_view
    }

    /// Drops the bind groups of the frame views, which a resize replaces.
    pub fn forget_inputs(&mut self) {
        self.groups.clear();
    }

    /// The exposure of `frame`, the complete HDR frame, as the frame's
    /// `exposure` asks.
    pub fn encode(&mut self, ctx: &mut FrameContext<'_>, frame: &wgpu::TextureView) {
        let AuthoredExposure { stops, automatic } = ctx.input.exposure;
        let Some(automatic) = automatic else {
            if self.fixed != Some(stops) {
                ctx.queue.write_texture(
                    self.exposure.as_image_copy(),
                    bytemuck::bytes_of(&stops.exp2()),
                    wgpu::TexelCopyBufferLayout::default(),
                    self.exposure.size(),
                );
                self.fixed = Some(stops);
            }
            return;
        };
        // Automatic exposure restarts at its target with history, and when
        // it follows a fixed exposure.
        let reset = !ctx.history.valid || self.fixed.take().is_some();
        self.meter(
            ctx.device,
            ctx.queue,
            ctx.encoder,
            frame,
            Metering {
                automatic: &automatic,
                stops,
                delta_time: ctx.input.frame_time_ms.max(0.) / 1000.,
                reset,
            },
            ctx.timing,
        );
    }

    /// Auto exposure of `frame` as `metering` asks.
    fn meter(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        frame: &wgpu::TextureView,
        metering: Metering<'_>,
        timing: Option<&crate::timing::GpuTiming>,
    ) {
        self.upload(queue, metering);
        let group = self.group(device, frame);
        let size = frame.texture().size();
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("auto exposure"),
            timestamp_writes: timing.and_then(|t| t.compute_pass("exposure")),
        });
        pass.set_bind_group(0, &group, &[]);
        pass.set_pipeline(&self.histogram_pipeline);
        pass.dispatch_workgroups(size.width.div_ceil(16), size.height.div_ceil(16), 1);
        pass.set_pipeline(&self.average_pipeline);
        pass.dispatch_workgroups(1, 1, 1);
    }

    /// Writes `metering`'s settings and, when it changed, its mask.
    fn upload(&mut self, queue: &wgpu::Queue, metering: Metering<'_>) {
        let automatic = metering.automatic;
        let uniform = AutoExposureUniform::new(
            automatic,
            metering.stops,
            metering.delta_time,
            metering.reset,
        );
        queue.write_buffer(&self.settings, 0, bytemuck::bytes_of(&uniform));
        if self.mask_weights != Some(automatic.metering_mask) {
            queue.write_texture(
                self.mask.as_image_copy(),
                automatic.metering_mask.weights.as_flattened(),
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(16),
                    rows_per_image: None,
                },
                self.mask.size(),
            );
            self.mask_weights = Some(automatic.metering_mask);
        }
    }

    /// The bind group metering `frame`, made once per view.
    fn group(&mut self, device: &wgpu::Device, frame: &wgpu::TextureView) -> wgpu::BindGroup {
        if let Some((_, group)) = self.groups.iter().find(|(view, _)| view == frame) {
            return group.clone();
        }
        let mask = self.mask.create_view(&Default::default());
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("auto exposure"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.settings.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(frame),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&mask),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: self.histogram.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: self.correction.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::TextureView(&self.exposure_view),
                },
            ],
        });
        self.groups.push((frame.clone(), group.clone()));
        group
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
