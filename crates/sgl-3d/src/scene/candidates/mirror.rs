//! A storage buffer of records the scene keeps on the CPU and mirrors to the
//! GPU: an edit changes records and marks them, and the frame's prepare
//! (`Scene::prepare_frame`) uploads the marked ones through the queue, or
//! every record into a new buffer when they outgrew it. Nothing goes through
//! a frame's encoder, so an abandoned frame loses no edit.
use bytemuck::Pod;

#[derive(Clone)]
pub(super) struct Mirror<T: Pod> {
    label: &'static str,
    records: Vec<T>,
    /// The records changed since the last upload, in no order.
    changed: Vec<u32>,
    /// None until the first upload.
    buffer: Option<wgpu::Buffer>,
}

impl<T: Pod> Mirror<T> {
    pub fn new(label: &'static str) -> Self {
        Self {
            label,
            records: Vec::new(),
            changed: Vec::new(),
            buffer: None,
        }
    }

    /// How many records it holds.
    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn get(&self, index: u32) -> &T {
        &self.records[index as usize]
    }

    /// Sets record `index`, growing the records with `fill` to reach it.
    pub fn set(&mut self, index: u32, record: T, fill: T) {
        let at = index as usize;
        if self.records.len() <= at {
            self.records.resize(at + 1, fill);
        }
        self.records[at] = record;
        self.changed.push(index);
    }

    /// Uploads the records changed since the last upload: into a new buffer
    /// of at least twice the records, holding every one, when they outgrew
    /// the buffer.
    pub fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        let stride = std::mem::size_of::<T>() as u64;
        // A binding is never empty.
        let needed = (self.records.len() as u64).max(1) * stride;
        if self
            .buffer
            .as_ref()
            .is_none_or(|buffer| buffer.size() < needed)
        {
            let size = self
                .buffer
                .as_ref()
                .map_or(needed, |buffer| needed.max(buffer.size() * 2));
            self.buffer = Some(crate::counters::buffer(
                device,
                &wgpu::BufferDescriptor {
                    label: Some(self.label),
                    size,
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                },
            ));
            self.changed.clear();
            if !self.records.is_empty() {
                let buffer = self.buffer.as_ref().unwrap();
                crate::counters::write_buffer(
                    queue,
                    buffer,
                    0,
                    bytemuck::cast_slice(&self.records),
                );
            }
            return;
        }
        let buffer = self.buffer.as_ref().unwrap();
        self.changed.sort_unstable();
        self.changed.dedup();
        let mut at = 0;
        while at < self.changed.len() {
            let start = self.changed[at];
            let mut end = start + 1;
            at += 1;
            while at < self.changed.len() && self.changed[at] == end {
                end += 1;
                at += 1;
            }
            let records = &self.records[start as usize..end as usize];
            crate::counters::write_buffer(
                queue,
                buffer,
                u64::from(start) * stride,
                bytemuck::cast_slice(records),
            );
        }
        self.changed.clear();
    }

    /// The buffer, once uploaded.
    pub fn buffer(&self) -> &wgpu::Buffer {
        self.buffer
            .as_ref()
            .expect("the scene uploads its candidates before a frame binds them")
    }

    /// The buffer's bytes, zero before the first upload.
    #[cfg(any(test, feature = "diagnostics"))]
    pub fn bytes(&self) -> u64 {
        self.buffer.as_ref().map_or(0, wgpu::Buffer::size)
    }
}
