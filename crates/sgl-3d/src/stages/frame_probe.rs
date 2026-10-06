//! Opt-in asynchronous numerical observations of the actual frame pipeline
//! (`Diagnostics::frame_probe`), returned to the game as one JSON report per
//! frame. Counts are diagnostics, not a rendering quality verdict. No SDK
//! shader changes.
//!
//! Reads: the views the renderer observes after opaque (colour before ambient
//! occlusion), composition, antialiasing and tone mapping, and the primary
//! raster's depth and identity. Writes: its own counters and readbacks. Honours: the frame
//! probe diagnostics switch. Timing groups: none.
use crate::FrameInput;
use crate::view::history::HistoryFrame;
use crate::view::targets::Sizes;
use std::{
    collections::VecDeque,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
};
/// The counters both probe programs write.
static STATS: crate::shading::Module = crate::shading::Module {
    name: "frame_probe_stats",
    source: include_str!("frame_probe_stats.wgsl"),
    deps: &[],
};
/// Each observed stage's IEEE-754 classification.
pub(crate) static PROBE: crate::shading::Module = crate::shading::Module {
    name: "frame_probe",
    source: include_str!("frame_probe.wgsl"),
    deps: &[&STATS],
};
/// The primary raster's coverage.
pub(crate) static COVERAGE: crate::shading::Module = crate::shading::Module {
    name: "frame_probe_coverage",
    source: include_str!("frame_probe_coverage.wgsl"),
    deps: &[&STATS],
};
/// The entry point both programs' pipelines are created with.
pub(crate) const MAIN_ENTRY: &str = "main";
const STAGES: [&str; 5] = [
    "lit_scene",
    "sssr_filtered",
    "sssr_composed",
    "atmosphere_effects",
    "tone_mapped",
];
const BYTES: u64 = (STAGES.len() * 32 * 4) as u64;
struct Pending {
    buffer: wgpu::Buffer,
    ready: Arc<AtomicU8>,
    metadata: serde_json::Value,
}
/// The report metadata of a frame at `sizes` seen as `input` with
/// `history`.
pub(crate) fn metadata(
    sizes: Sizes,
    input: &FrameInput,
    history: HistoryFrame,
) -> serde_json::Value {
    use crate::content::identity::Identity;
    let environment_index = input.environment.map(Identity::index);
    serde_json::json!({"size":sizes.render,"output_size":sizes.output,"environment_index":environment_index,"temporal_valid":history.valid,"temporal_frames":history.frames})
}
pub(crate) struct FrameProbe {
    pipeline: wgpu::ComputePipeline,
    coverage_pipeline: wgpu::ComputePipeline,
    stats: wgpu::Buffer,
    pending: VecDeque<Pending>,
    next: Option<Pending>,
    /// Reports read back and not yet taken.
    reports: Vec<serde_json::Value>,
    frame: u64,
}
impl FrameProbe {
    /// The probe in `slot` when `observe` is set, created on first use.
    /// Every frame starts with nothing observed: a probed frame abandoned
    /// before `finish_frame` leaves a readback whose copy never ran, which no
    /// later frame may map.
    pub fn for_frame<'a>(
        slot: &'a mut Option<Self>,
        device: &wgpu::Device,
        observe: bool,
    ) -> Option<&'a mut Self> {
        if let Some(probe) = slot.as_mut() {
            probe.next = None;
        }
        if !observe {
            return None;
        }
        Some(slot.get_or_insert_with(|| Self::new(device)))
    }
    fn new(device: &wgpu::Device) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("numerical frame probe"),
            source: wgpu::ShaderSource::Wgsl(crate::shading::compose(&[&PROBE]).into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("numerical frame probe"),
            layout: None,
            module: &shader,
            entry_point: Some(MAIN_ENTRY),
            compilation_options: Default::default(),
            cache: None,
        });
        let coverage_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("primary raster coverage probe"),
            source: wgpu::ShaderSource::Wgsl(crate::shading::compose(&[&COVERAGE]).into()),
        });
        let coverage_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("primary raster coverage probe"),
            layout: None,
            module: &coverage_shader,
            entry_point: Some(MAIN_ENTRY),
            compilation_options: Default::default(),
            cache: None,
        });
        let stats = crate::counters::buffer(
            device,
            &wgpu::BufferDescriptor {
                label: Some("frame probe counters"),
                size: BYTES,
                usage: wgpu::BufferUsages::STORAGE
                    | wgpu::BufferUsages::COPY_SRC
                    | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            },
        );
        Self {
            pipeline,
            coverage_pipeline,
            stats,
            pending: VecDeque::new(),
            next: None,
            reports: Vec::new(),
            frame: 0,
        }
    }
    pub fn begin(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        mut metadata: serde_json::Value,
    ) {
        self.poll(device);
        metadata["frame"] = self.frame.into();
        self.frame += 1;
        // Keep diagnostics bounded without waiting for the GPU or delaying play.
        if self.pending.len() >= 64 {
            self.reports
                .push(serde_json::json!({"skipped":metadata,"reason":"readback backlog"}));
            return;
        }
        encoder.clear_buffer(&self.stats, 0, None);
        self.next = Some(Pending {
            buffer: crate::counters::buffer(
                device,
                &wgpu::BufferDescriptor {
                    label: Some("frame probe readback"),
                    size: BYTES,
                    usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false,
                },
            ),
            ready: Arc::new(AtomicU8::new(0)),
            metadata,
        });
    }
    pub fn observe(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        stage: u32,
        current: &wgpu::TextureView,
        source: &wgpu::TextureView,
        compare: bool,
    ) {
        if self.next.is_none() {
            return;
        }
        let params = crate::counters::buffer_init(
            device,
            &wgpu::util::BufferInitDescriptor {
                label: Some("frame probe stage"),
                contents: bytemuck::cast_slice(&[stage, u32::from(compare), 0, 0]),
                usage: wgpu::BufferUsages::UNIFORM,
            },
        );
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("frame probe textures"),
            layout: &self.pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(current),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(source),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.stats.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: params.as_entire_binding(),
                },
            ],
        });
        let mut pass = encoder.begin_compute_pass(&Default::default());
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &group, &[]);
        pass.dispatch_workgroups(
            current.texture().width().div_ceil(8),
            current.texture().height().div_ceil(8),
            1,
        );
    }
    pub fn finish(&self, encoder: &mut wgpu::CommandEncoder) {
        if let Some(next) = &self.next {
            encoder.copy_buffer_to_buffer(&self.stats, 0, &next.buffer, 0, BYTES);
        }
    }
    pub fn coverage(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        depth: &wgpu::TextureView,
        identity: &wgpu::TextureView,
    ) {
        if self.next.is_none() {
            return;
        }
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("primary raster coverage inputs"),
            layout: &self.coverage_pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(depth),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(identity),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.stats.as_entire_binding(),
                },
            ],
        });
        let mut pass = encoder.begin_compute_pass(&Default::default());
        pass.set_pipeline(&self.coverage_pipeline);
        pass.set_bind_group(0, &group, &[]);
        pass.dispatch_workgroups(
            depth.texture().width().div_ceil(8),
            depth.texture().height().div_ceil(8),
            1,
        );
    }
    /// After the caller submitted the frame: queues its readback.
    pub fn submitted(&mut self) {
        if let Some(next) = self.next.take() {
            let ready = next.ready.clone();
            next.buffer
                .slice(..)
                .map_async(wgpu::MapMode::Read, move |r| {
                    ready.store(if r.is_ok() { 1 } else { 2 }, Ordering::Release)
                });
            self.pending.push_back(next);
        }
    }
    /// The reports read back since the last call, oldest first.
    pub fn take_reports(&mut self, device: &wgpu::Device) -> Vec<serde_json::Value> {
        self.poll(device);
        std::mem::take(&mut self.reports)
    }
    fn poll(&mut self, device: &wgpu::Device) {
        device.poll(wgpu::PollType::Poll).unwrap();
        while self
            .pending
            .front()
            .is_some_and(|p| p.ready.load(Ordering::Acquire) != 0)
        {
            let p = self.pending.pop_front().unwrap();
            if p.ready.load(Ordering::Acquire) == 2 {
                panic!("frame probe readback failed");
            }
            let mapped = p.buffer.slice(..).get_mapped_range();
            let words: &[u32] = bytemuck::cast_slice(&mapped);
            let mut result = p.metadata;
            for (stage, name) in STAGES.iter().enumerate() {
                let v = &words[stage * 32..(stage + 1) * 32];
                result[*name] = serde_json::json!({"observed":v[31]!=0,"black_pixels":v[0],"nonfinite_pixels":v[1],"negative_pixels":v[2],"nan_pixels":v[5],"positive_inf_pixels":v[6],"negative_inf_pixels":v[7],"positive_source_to_black":v[3],"maximum_finite_component":f32::from_bits(v[4]),"first_nonfinite_xy_current_source_bits":if v[1]>0{Some(&v[8..16])}else{None},"first_lost_xy_current_source_bits":if v[3]>0{Some(&v[16..24])}else{None}});
            }
            result["raster_coverage"] = serde_json::json!({"geometry_pixels":words[24],"unshaded_geometry_pixels":words[25],"first_unshaded_xy_depth_bits":if words[25]>0{Some(&words[26..29])}else{None}});
            self.reports.push(result);
            drop(mapped);
            p.buffer.unmap();
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    // The detector itself could miss NaN/sign bits or retain another frame's
    // counters. Known IEEE-754 binary16 inputs exercise actual GPU classification.
    #[test]
    fn frame_probe_detects_invalid_values_and_pixel_loss() {
        let Some((device, queue)) = crate::test_support::device() else {
            return;
        };
        pollster::block_on(async {
            let mut probe = FrameProbe::new(&device);
            let make = || {
                device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("IEEE-754 probe calibration"),
                    size: wgpu::Extent3d {
                        width: 8,
                        height: 1,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba16Float,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                })
            };
            let current = make();
            let source = make();
            let write = |t: &wgpu::Texture, values: &[[u16; 4]]| {
                queue.write_texture(
                    t.as_image_copy(),
                    bytemuck::cast_slice(values),
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(64),
                        rows_per_image: Some(1),
                    },
                    t.size(),
                )
            };
            write(&source, &[[0x3c00; 4]; 8]);
            write(
                &current,
                &[
                    [0x3c00, 0x4000, 0x4200, 0x3c00],
                    [0, 0, 0, 0x3c00],
                    [0x7c00, 0, 0, 0x3c00],
                    [0xfc00, 0, 0, 0x3c00],
                    [0x7e00, 0, 0, 0x3c00],
                    [0xbc00, 0, 0, 0x3c00],
                    [0x8000, 0x8000, 0, 0x3c00],
                    [0x7bff, 0, 0, 0x3c00],
                ],
            );
            for frame in 0..2 {
                if frame == 1 {
                    write(&current, &[[0x3c00; 4]; 8]);
                }
                let mut encoder = device.create_command_encoder(&Default::default());
                probe.begin(&device, &mut encoder, serde_json::json!({}));
                probe.observe(
                    &device,
                    &mut encoder,
                    2,
                    &current.create_view(&Default::default()),
                    &source.create_view(&Default::default()),
                    true,
                );
                probe.finish(&mut encoder);
                queue.submit([encoder.finish()]);
                probe.submitted();
                device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
            }
            let rows = probe.take_reports(&device);
            let first = &rows[0]["sssr_composed"];
            for (key, expected) in [
                ("black_pixels", 2),
                ("nonfinite_pixels", 3),
                ("negative_pixels", 1),
                ("nan_pixels", 1),
                ("positive_inf_pixels", 1),
                ("negative_inf_pixels", 1),
                ("positive_source_to_black", 2),
            ] {
                assert_eq!(first[key], expected, "{key}");
            }
            assert_eq!(first["maximum_finite_component"], 65504.0);
            let next = &rows[1]["sssr_composed"];
            assert_eq!(next["nonfinite_pixels"], 0);
            assert_eq!(next["positive_source_to_black"], 0);
            assert_eq!(next["maximum_finite_component"], 1.0);
        });
    }
}
