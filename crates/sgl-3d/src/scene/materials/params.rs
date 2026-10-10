//! A material's shader parameter blocks (`Scene::set_shader_parameters`):
//! this frame's and the last submitted frame's, which its group 2 binds for
//! its shader's programs (shader_inputs_bound.wgsl), and the CPU copies that
//! keep the second a submitted frame behind the first (scene history,
//! S3D-4). A material without a shader binds one shared zero block twice,
//! which its programs never read.

/// The bytes a block of `size` bytes takes: a uniform binding's whole
/// 16-byte rows, at least one.
fn buffer_size(size: u32) -> u64 {
    u64::from(size.max(16).next_multiple_of(16))
}

/// A zero block that group 2 binds for a material without a shader.
pub(super) fn zero_block(device: &wgpu::Device) -> wgpu::Buffer {
    block(device, "no shader parameters", 16)
}

fn block(device: &wgpu::Device, label: &str, size: u32) -> wgpu::Buffer {
    crate::counters::buffer(
        device,
        &wgpu::BufferDescriptor {
            label: Some(label),
            size: buffer_size(size),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        },
    )
}

pub(super) struct ParamBlock {
    pub current: wgpu::Buffer,
    pub previous: wgpu::Buffer,
    /// This frame's block.
    bytes: Vec<u8>,
    /// The last submitted frame's block, which `previous` holds once
    /// `stale` is clear.
    committed: Vec<u8>,
    stale: bool,
}

impl ParamBlock {
    /// Both blocks zero, `size` bytes each: a material that gains a shader
    /// has no motion from its parameters in its first frame.
    pub fn new(device: &wgpu::Device, size: u32) -> Self {
        Self {
            current: block(device, "shader parameters", size),
            previous: block(device, "previous shader parameters", size),
            bytes: vec![0; size as usize],
            committed: vec![0; size as usize],
            stale: false,
        }
    }

    /// This frame's block becomes `bytes`, of its size; returns whether it
    /// changed.
    pub fn set(&mut self, queue: &wgpu::Queue, bytes: &[u8]) -> bool {
        let changed = self.bytes != bytes;
        if changed {
            self.bytes.copy_from_slice(bytes);
            crate::counters::write_buffer(queue, &self.current, 0, bytes);
        }
        changed
    }

    /// Commits a submitted frame: its block becomes the last submitted one.
    pub fn finish_frame(&mut self) {
        if self.committed != self.bytes {
            self.committed.clone_from(&self.bytes);
            self.stale = true;
        }
    }

    /// Before a frame: `previous` holds the last submitted frame's block.
    pub fn prepare_frame(&mut self, queue: &wgpu::Queue) {
        if self.stale {
            crate::counters::write_buffer(queue, &self.previous, 0, &self.committed);
            self.stale = false;
        }
    }
}
