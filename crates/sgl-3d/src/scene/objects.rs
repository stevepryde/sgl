//! The scene's object records: one storage buffer holding each instance's
//! `ObjectUniform` at its index. Group 1 binds it whole, with the scene's ray
//! buffers, and each instance of a draw reads its own record at the index its
//! draw instance names (`shading::vertex::DrawInstance`), as Bevy's batched
//! draws read each instance's `MeshUniform` from one buffer. Beside it, a
//! second storage buffer holds each instance's shader data
//! (`Scene::set_instance_shader_data`), this frame's and the last submitted
//! frame's, at its index, which only a game's shader's programs read
//! (shader_inputs_bound.wgsl), as Godot b130438 keeps instance uniforms in
//! its global shader uniform buffer apart from its instance records, which
//! name only where they start (`instance_uniforms_ofs`,
//! servers/rendering/renderer_rd/forward_clustered/render_forward_clustered.h
//! 353, read through `global_shader_uniforms.data` in
//! scene_shader_forward_clustered.cpp 922–923): a program without a shader
//! reads records of the size it always did. Both grow as instances are
//! added; growing rewrites every record.
use super::SceneError;
use crate::shading::uniforms::ObjectUniform;
use crate::static_lighting::AmbientCube;

/// The bytes between consecutive records, `array<Object>`'s stride.
const STRIDE: u64 = std::mem::size_of::<ObjectUniform>() as u64;
/// The bytes of an instance's shader data: this frame's and the last
/// submitted frame's `vec4<f32>`.
const SHADER_DATA_STRIDE: u64 = std::mem::size_of::<ShaderData>() as u64;

/// An instance's shader data, this frame's and the last submitted frame's.
pub(crate) type ShaderData = [[f32; 4]; 2];

pub(crate) struct Objects {
    buffer: wgpu::Buffer,
    shader_data: wgpu::Buffer,
}

impl Objects {
    /// Room for one record.
    pub fn new(device: &wgpu::Device) -> Self {
        Self {
            buffer: object_buffer(device, 1),
            shader_data: shader_data_buffer(device, 1),
        }
    }

    pub fn buffer(&self) -> &wgpu::Buffer {
        &self.buffer
    }

    pub fn shader_data(&self) -> &wgpu::Buffer {
        &self.shader_data
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
        let records = (count as u64).max(capacity * 2).min(limit);
        self.buffer = object_buffer(device, records);
        self.shader_data = shader_data_buffer(device, records);
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

    /// Writes the shader data of the instance at `index`.
    pub fn write_shader_data(&self, queue: &wgpu::Queue, index: usize, data: &ShaderData) {
        crate::counters::write_buffer(
            queue,
            &self.shader_data,
            index as u64 * SHADER_DATA_STRIDE,
            bytemuck::cast_slice(data),
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

fn shader_data_buffer(device: &wgpu::Device, records: u64) -> wgpu::Buffer {
    crate::counters::buffer(
        device,
        &wgpu::BufferDescriptor {
            label: Some("scene instance shader data"),
            size: records * SHADER_DATA_STRIDE,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        },
    )
}

fn object_buffer(device: &wgpu::Device, records: u64) -> wgpu::Buffer {
    crate::counters::buffer(
        device,
        &wgpu::BufferDescriptor {
            label: Some("scene objects"),
            size: records * STRIDE,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        },
    )
}
