//! Which of the camera's opaque and masked instances show
//! (`InstanceVisibility`, feature `diagnostics`), an oracle for occlusion
//! culling. In an observed frame, after the opaque stage, a pass marks each
//! object record with at least one pixel in the source identity target, a
//! bit each, and copies the marks for a readback whose map is requested once
//! the frame is submitted and never waited on. A readback that has arrived
//! reports the instances the frame's camera list drew and those of them
//! without a pixel, with their triangles, and becomes the set a skipping
//! frame's camera list leaves out.
//!
//! Reads: the source identity target and the camera's draw list. Writes:
//! its own marks and readbacks. Honours: the instance visibility
//! diagnostics switch. Timing groups: `instance visibility`.
use crate::content::identity::{Identity, InstanceId};
use crate::diagnostics::InstanceVisibilityReport;
use crate::timing::GpuTiming;
use crate::view::draw_list::DrawList;
use crate::view::hidden::HiddenInstances;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

pub(crate) static VISIBLE_INSTANCES: crate::shading::Module = crate::shading::Module {
    name: "visible_instances",
    source: include_str!("visible_instances.wgsl"),
    deps: &[],
};

/// Readbacks in flight at most; an observed frame past them is not
/// observed, so a game that never polls keeps bounded memory.
const MOST_PENDING: usize = 8;
const PENDING: u8 = 0;
const READY: u8 = 1;
const FAILED: u8 = 2;

/// One observed frame's readback and what its camera list drew.
struct Pending {
    readback: wgpu::Buffer,
    ready: Arc<AtomicU8>,
    /// Each instance the camera list drew and the triangles it submitted
    /// for it.
    drawn: Vec<(InstanceId, u64)>,
}

pub(crate) struct VisibleInstances {
    pipeline: wgpu::ComputePipeline,
    /// One bit an object record, grown to the frame's.
    marks: Option<wgpu::Buffer>,
    /// The frame observed, until it is submitted; a frame abandoned instead
    /// is replaced by the next.
    next: Option<Pending>,
    pending: VecDeque<Pending>,
    /// Readback buffers to reuse.
    spare: Vec<wgpu::Buffer>,
    reports: Vec<InstanceVisibilityReport>,
    /// The newest report's hidden instances.
    hidden: HiddenInstances,
}

impl VisibleInstances {
    /// The observer in `slot` when `used`, created on first use. Every
    /// frame starts with nothing observed: an observed frame abandoned
    /// before `finish_frame` leaves a readback whose copy never ran, which no
    /// later frame may map.
    pub fn for_frame<'a>(
        slot: &'a mut Option<Self>,
        device: &wgpu::Device,
        used: bool,
    ) -> Option<&'a mut Self> {
        if let Some(observer) = slot.as_mut() {
            observer.next = None;
        }
        if !used {
            return None;
        }
        Some(slot.get_or_insert_with(|| Self::new(device)))
    }

    fn new(device: &wgpu::Device) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("instance visibility"),
            source: wgpu::ShaderSource::Wgsl(crate::shading::compose(&[&VISIBLE_INSTANCES]).into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("instance visibility"),
            layout: None,
            module: &shader,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        Self {
            pipeline,
            marks: None,
            next: None,
            pending: VecDeque::new(),
            spare: Vec::new(),
            reports: Vec::new(),
            hidden: HiddenInstances::default(),
        }
    }

    /// The hidden instances of the newest observed frame read back, without
    /// blocking.
    pub fn hidden(&mut self, device: &wgpu::Device) -> &HiddenInstances {
        self.poll(device);
        &self.hidden
    }

    /// Marks the object records with a pixel in `source_identity`, the frame
    /// `list` drew of `scene` as its camera's opaque and masked surfaces,
    /// and copies the marks for readback.
    pub fn observe(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        timing: Option<&GpuTiming>,
        source_identity: &wgpu::TextureView,
        (scene, list): (&crate::Scene, &DrawList),
    ) {
        self.next = None;
        if self.pending.len() >= MOST_PENDING {
            return;
        }
        let instances = &scene.instances.slots;
        let drawn: Vec<(InstanceId, u64)> = list
            .triangles_by_object()
            .into_iter()
            .filter_map(|(object, triangles)| Some((instances.id_at(object as usize)?, triangles)))
            .collect();
        let objects = drawn
            .iter()
            .map(|(id, _)| id.index() + 1)
            .max()
            .unwrap_or(1);
        let size = (objects.div_ceil(32) * 4) as u64;
        if self.marks.as_ref().is_none_or(|marks| marks.size() < size) {
            self.marks = Some(crate::counters::buffer(
                device,
                &wgpu::BufferDescriptor {
                    label: Some("instance visibility marks"),
                    size: size.next_power_of_two(),
                    usage: wgpu::BufferUsages::STORAGE
                        | wgpu::BufferUsages::COPY_SRC
                        | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                },
            ));
        }
        let marks = self.marks.as_ref().unwrap();
        let readback = match self.spare.iter().position(|spare| spare.size() >= size) {
            Some(at) => self.spare.swap_remove(at),
            None => crate::counters::buffer(
                device,
                &wgpu::BufferDescriptor {
                    label: Some("instance visibility readback"),
                    size: size.next_power_of_two(),
                    usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false,
                },
            ),
        };
        encoder.clear_buffer(marks, 0, Some(size));
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("instance visibility"),
            layout: &self.pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(source_identity),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: marks.as_entire_binding(),
                },
            ],
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("instance visibility"),
                timestamp_writes: timing.and_then(|t| t.compute_pass("instance visibility")),
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &group, &[]);
            let target = source_identity.texture();
            pass.dispatch_workgroups(target.width().div_ceil(8), target.height().div_ceil(8), 1);
        }
        encoder.copy_buffer_to_buffer(marks, 0, &readback, 0, size);
        self.next = Some(Pending {
            readback,
            ready: Arc::new(AtomicU8::new(PENDING)),
            drawn,
        });
    }

    /// After the caller submitted the frame: requests its readback's map.
    pub fn submitted(&mut self) {
        if let Some(next) = self.next.take() {
            let ready = next.ready.clone();
            next.readback
                .slice(..)
                .map_async(wgpu::MapMode::Read, move |result| {
                    ready.store(
                        if result.is_ok() { READY } else { FAILED },
                        Ordering::Release,
                    );
                });
            self.pending.push_back(next);
        }
    }

    /// The reports read back since the last call, oldest first.
    pub fn take_reports(&mut self, device: &wgpu::Device) -> Vec<InstanceVisibilityReport> {
        self.poll(device);
        std::mem::take(&mut self.reports)
    }

    /// Reads back, without waiting, the observed frames the device has
    /// completed, in order.
    fn poll(&mut self, device: &wgpu::Device) {
        let _ = device.poll(wgpu::PollType::Poll);
        while let Some(front) = self.pending.front() {
            match front.ready.load(Ordering::Acquire) {
                PENDING => break,
                READY => {}
                _ => panic!("instance visibility readback failed"),
            }
            let pending = self.pending.pop_front().unwrap();
            let mut report = InstanceVisibilityReport::default();
            let mut hidden = HiddenInstances::default();
            {
                let mapped = pending.readback.slice(..).get_mapped_range();
                let marks: &[u32] = bytemuck::cast_slice(&mapped);
                for &(id, triangles) in &pending.drawn {
                    let index = id.index();
                    report.drawn_instances += 1;
                    report.drawn_triangles += triangles;
                    if marks[index / 32] & (1 << (index % 32)) == 0 {
                        report.hidden_instances += 1;
                        report.hidden_triangles += triangles;
                        hidden.insert(id);
                    }
                }
            }
            pending.readback.unmap();
            self.spare.push(pending.readback);
            self.reports.push(report);
            self.hidden = hidden;
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
