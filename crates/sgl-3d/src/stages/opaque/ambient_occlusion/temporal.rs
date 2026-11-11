//! DiligentFX SSAO temporal accumulation; private to the opaque stage.
use crate::{
    settings::AmbientOcclusionQuality,
    view::{cached_group::CachedGroup, history::HistoryFrame},
};

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    size: [u32; 2],
    jitter_delta: [f32; 2],
    depth_unpack: [f32; 2],
    reset: u32,
    padding: u32,
}
struct Targets {
    size: [u32; 2],
    history: [wgpu::TextureView; 2],
    output: wgpu::TextureView,
}
pub(super) struct Temporal {
    pipeline: wgpu::ComputePipeline,
    groups: [CachedGroup; 2],
    params: wgpu::Buffer,
    targets: Option<Targets>,
    last: Option<(u32, AmbientOcclusionQuality, f32)>,
    current: usize,
}
impl Temporal {
    pub fn new(device: &wgpu::Device) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("AO temporal accumulation"),
            source: wgpu::ShaderSource::Wgsl(include_str!("temporal.wgsl").into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("AO temporal accumulation"),
            layout: None,
            module: &module,
            entry_point: Some("resolve"),
            compilation_options: Default::default(),
            cache: None,
        });
        let groups = std::array::from_fn(|_| CachedGroup::new(pipeline.get_bind_group_layout(0)));
        Self {
            pipeline,
            groups,
            params: crate::counters::buffer(
                device,
                &wgpu::BufferDescriptor {
                    label: Some("AO temporal parameters"),
                    size: std::mem::size_of::<Params>() as u64,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                },
            ),
            targets: None,
            last: None,
            current: 0,
        }
    }
    pub fn output(&self) -> Option<&wgpu::TextureView> {
        self.targets.as_ref().map(|t| &t.output)
    }
    #[allow(clippy::too_many_arguments)]
    pub fn encode(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        ao: &wgpu::TextureView,
        depth: &wgpu::TextureView,
        motion: &wgpu::TextureView,
        history: HistoryFrame,
        quality: AmbientOcclusionQuality,
        radius: f32,
        timing: Option<&crate::timing::GpuTiming>,
    ) {
        let size = [depth.texture().width(), depth.texture().height()];
        let resized = self.targets.as_ref().is_none_or(|t| t.size != size);
        if resized {
            let make = |name, format| {
                crate::view::targets::target_with_usage(
                    device,
                    name,
                    size,
                    format,
                    wgpu::TextureUsages::STORAGE_BINDING,
                )
            };
            self.targets = Some(Targets {
                size,
                history: std::array::from_fn(|_| {
                    make("AO temporal history", wgpu::TextureFormat::Rgba32Float)
                }),
                output: make("AO accumulated visibility", wgpu::TextureFormat::R32Uint),
            });
        }
        let reset = resized
            || !history.valid
            || self.last.is_none_or(|(frame, q, r)| {
                frame.wrapping_add(1) != history.frames || q != quality || r != radius
            });
        let previous = history.previous_camera.unwrap_or(history.camera);
        let params = Params {
            size,
            jitter_delta: [
                (previous.jitter[0] - history.camera.jitter[0]) * 0.5,
                (history.camera.jitter[1] - previous.jitter[1]) * 0.5,
            ],
            depth_unpack: [
                history.camera.projection.w_axis.z,
                history.camera.projection.z_axis.z,
            ],
            reset: u32::from(reset),
            padding: 0,
        };
        crate::counters::write_buffer(queue, &self.params, 0, bytemuck::bytes_of(&params));
        let t = self.targets.as_ref().unwrap();
        let group = self.groups[self.current].get(
            device,
            "AO temporal accumulation",
            &[
                (0, self.params.as_entire_binding()),
                super::texture_entry(1, ao),
                super::texture_entry(2, depth),
                super::texture_entry(3, motion),
                super::texture_entry(4, &t.history[1 - self.current]),
                super::texture_entry(5, &t.history[self.current]),
                super::texture_entry(6, &t.output),
            ],
        );
        super::dispatch(
            encoder,
            &self.pipeline,
            group,
            size.map(|s| s.div_ceil(8)),
            timing,
        );
        self.last = Some((history.frames, quality, radius));
        self.current = 1 - self.current;
    }
}
