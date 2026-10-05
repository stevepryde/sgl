//! The scene's object records: one storage buffer holding each instance's
//! `ObjectUniform` at its index. Group 1 binds it whole, with the scene's ray
//! buffers, and each instance of a draw reads its own record at the index its
//! draw instance names (`shading::vertex::DrawInstance`), as Bevy's batched
//! draws read each instance's `MeshUniform` from one buffer. The buffer grows
//! as instances are added; growing rewrites every record.
use super::SceneError;
use crate::shading::uniforms::ObjectUniform;
use crate::static_lighting::AmbientCube;

/// The bytes between consecutive records, `array<Object>`'s stride.
const STRIDE: u64 = std::mem::size_of::<ObjectUniform>() as u64;

pub(crate) struct Objects {
    buffer: wgpu::Buffer,
}

impl Objects {
    /// Room for one record.
    pub fn new(device: &wgpu::Device) -> Self {
        Self {
            buffer: object_buffer(device, 1),
        }
    }

    pub fn buffer(&self) -> &wgpu::Buffer {
        &self.buffer
    }

    /// Room for `count` records. When the buffer is replaced, its records
    /// are lost and the caller writes them again; returns whether it was.
    pub fn reserve(&mut self, device: &wgpu::Device, count: usize) -> Result<bool, SceneError> {
        let capacity = self.buffer.size() / STRIDE;
        if count as u64 <= capacity {
            return Ok(false);
        }
        let limits = device.limits();
        let limit = limits
            .max_buffer_size
            .min(limits.max_storage_buffer_binding_size)
            / STRIDE;
        if count as u64 > limit {
            return Err(SceneError::DeviceLimit);
        }
        self.buffer = object_buffer(device, (count as u64).max(capacity * 2).min(limit));
        Ok(true)
    }

    pub fn write(&self, queue: &wgpu::Queue, index: usize, record: &ObjectUniform) {
        crate::counters::write_buffer(
            queue,
            &self.buffer,
            offset(index),
            bytemuck::bytes_of(record),
        );
    }

    /// Writes `records`, record `i` at index `i`, in one write.
    pub fn write_all(&self, queue: &wgpu::Queue, records: &[ObjectUniform]) {
        if !records.is_empty() {
            crate::counters::write_buffer(queue, &self.buffer, 0, bytemuck::cast_slice(records));
        }
    }

    pub fn write_baked_irradiance(&self, queue: &wgpu::Queue, index: usize, cube: AmbientCube) {
        crate::counters::write_buffer(
            queue,
            &self.buffer,
            offset(index) + std::mem::offset_of!(ObjectUniform, baked_irradiance) as u64,
            bytemuck::cast_slice(&cube.packed()),
        );
    }
}

/// Record `index`'s byte offset.
fn offset(index: usize) -> u64 {
    index as u64 * STRIDE
}

fn object_buffer(device: &wgpu::Device, records: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("scene objects"),
        size: records * STRIDE,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}
