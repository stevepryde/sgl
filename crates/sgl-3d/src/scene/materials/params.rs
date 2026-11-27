//! A material's shader parameter blocks (`Scene::set_shader_parameters`):
//! this frame's and the last submitted frame's, which its group 2 binds for
//! its shader's programs (shader_inputs_bound.wgsl), and the CPU copies that
//! keep the second a submitted frame behind the first (scene history,
//! S3D-4): the block each frame drew, committed when it is finished. A block
//! that was in no finished frame has this frame's as its previous. A material without a shader binds one shared zero block twice,
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
    /// The block the frame being rendered draws (`prepare_frame`), none
    /// before a frame began with this block.
    rendered: Option<Vec<u8>>,
    /// The last submitted frame's block, none until the block was in a
    /// finished frame; `previous` holds it once `stale` is clear, and
    /// `bytes` before it.
    committed: Option<Vec<u8>>,
    stale: bool,
}

impl ParamBlock {
    /// Both blocks zero, `size` bytes each. Until it was in a finished
    /// frame, `previous` follows `current`, so a material that gains a
    /// shader or changes it, which gives it a new block, writes no motion
    /// from its parameters in its first frame, as an instance's shader
    /// data does before its first.
    pub fn new(device: &wgpu::Device, size: u32) -> Self {
        Self {
            current: block(device, "shader parameters", size),
            previous: block(device, "previous shader parameters", size),
            bytes: vec![0; size as usize],
            rendered: None,
            committed: None,
            stale: false,
        }
    }

    /// This frame's block becomes `bytes`, of its size, and so the previous
    /// one too until the block was in a finished frame; returns whether it
    /// changed.
    pub fn set(&mut self, queue: &wgpu::Queue, bytes: &[u8]) -> bool {
        let changed = self.bytes != bytes;
        if changed {
            self.bytes.copy_from_slice(bytes);
            crate::counters::write_buffer(queue, &self.current, 0, bytes);
            if self.committed.is_none() {
                crate::counters::write_buffer(queue, &self.previous, 0, bytes);
            }
        }
        changed
    }

    /// This frame's parameter block bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Whether this frame's block differs from the last submitted frame's;
    /// false before the block was in one, which has no last submitted
    /// block.
    pub fn changed(&self) -> bool {
        self.committed
            .as_ref()
            .is_some_and(|committed| *committed != self.bytes)
    }

    /// Commits a submitted frame: the block it drew becomes the last
    /// submitted one. A block the frame did not draw commits nothing.
    pub fn finish_frame(&mut self) {
        let Some(rendered) = &self.rendered else {
            return;
        };
        // What `previous` holds, which a later write keeps current: a
        // rewrite is due where it changes.
        let on_gpu = self.committed.as_ref().unwrap_or(&self.bytes);
        self.stale |= on_gpu != rendered;
        if self.committed.as_ref() != Some(rendered) {
            self.committed = Some(rendered.clone());
        }
    }

    /// Before a frame: `previous` holds the last submitted frame's block,
    /// and the block the frame draws is recorded.
    pub fn prepare_frame(&mut self, queue: &wgpu::Queue) {
        if self.stale {
            let committed = self
                .committed
                .as_ref()
                .expect("a stale block was committed");
            crate::counters::write_buffer(queue, &self.previous, 0, committed);
            self.stale = false;
        }
        match &mut self.rendered {
            Some(rendered) if *rendered == self.bytes => {}
            Some(rendered) => rendered.clone_from(&self.bytes),
            None => self.rendered = Some(self.bytes.clone()),
        }
    }
}
