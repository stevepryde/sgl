//! The draw instances of one frame or probe capture: every draw list's, in
//! one vertex buffer written once, as Bevy 9d12036 writes its one
//! `BatchedInstanceBuffer` for every view of a frame
//! (crates/bevy_render/src/batching/no_gpu_preprocessing.rs:
//! `clear_batched_cpu_instance_buffers`, `write_batched_instance_buffer`).
use crate::shading::vertex::DrawInstance;

#[derive(Default)]
pub(crate) struct DrawInstances {
    entries: Vec<DrawInstance>,
    buffer: Option<wgpu::Buffer>,
}

impl DrawInstances {
    /// Starts a frame or capture: no list's instances.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Appends one list's instances and returns where the first is.
    pub(super) fn append(&mut self, instances: &[DrawInstance]) -> u32 {
        let first = self.entries.len() as u32;
        self.entries.extend_from_slice(instances);
        first
    }

    /// Writes every list's instances, growing the buffer as they need.
    /// Called once the lists are built and before any draws them, since a
    /// grown buffer replaces the one earlier draws bound.
    pub fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        if self.entries.is_empty() {
            return;
        }
        let bytes: &[u8] = bytemuck::cast_slice(&self.entries);
        let size = bytes.len() as u64;
        if self
            .buffer
            .as_ref()
            .is_none_or(|buffer| buffer.size() < size)
        {
            self.buffer = Some(crate::counters::buffer(
                device,
                &wgpu::BufferDescriptor {
                    label: Some("draw instances"),
                    size: size.next_power_of_two(),
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                },
            ));
        }
        crate::counters::write_buffer(queue, self.buffer.as_ref().unwrap(), 0, bytes);
    }

    /// The buffer the draws step through.
    pub(super) fn buffer(&self) -> &wgpu::Buffer {
        self.buffer
            .as_ref()
            .expect("draw instances are uploaded before they are drawn")
    }
}
