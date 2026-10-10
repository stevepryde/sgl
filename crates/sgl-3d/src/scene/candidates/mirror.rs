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
    /// The records changed since the last upload, each once, in no order,
    /// and whether each record is among them, so many edits of one record
    /// keep one entry and a copy of the mirror costs its records.
    changed: Vec<u32>,
    marked: Vec<bool>,
    /// None until the first upload.
    buffer: Option<wgpu::Buffer>,
}

impl<T: Pod> Mirror<T> {
    pub fn new(label: &'static str) -> Self {
        Self {
            label,
            records: Vec::new(),
            changed: Vec::new(),
            marked: Vec::new(),
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
            self.marked.resize(at + 1, false);
        }
        self.records[at] = record;
        if !self.marked[at] {
            self.marked[at] = true;
            self.changed.push(index);
        }
    }

    /// The records changed since the last upload.
    #[cfg(test)]
    pub fn changed(&self) -> usize {
        self.changed.len()
    }

    /// Forgets which records changed: the upload took them.
    fn uploaded(&mut self) {
        for &index in &self.changed {
            self.marked[index as usize] = false;
        }
        self.changed.clear();
    }

    /// Uploads the records changed since the last upload: into a new buffer
    /// of twice the old one, holding every record, when they outgrew it.
    /// Doubling stops at what the device binds whole, as the scene's other
    /// growable buffers' does: the owner keeps the records within it
    /// (`Candidates::fits`), and the cull binds the buffer entire.
    pub fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        let stride = std::mem::size_of::<T>() as u64;
        // A binding is never empty.
        let needed = (self.records.len() as u64).max(1) * stride;
        if self
            .buffer
            .as_ref()
            .is_none_or(|buffer| buffer.size() < needed)
        {
            let limits = device.limits();
            let binding = limits
                .max_storage_buffer_binding_size
                .min(limits.max_buffer_size);
            let size = self.buffer.as_ref().map_or(needed, |buffer| {
                (buffer.size() * 2).min(binding).max(needed)
            });
            self.buffer = Some(crate::counters::buffer(
                device,
                &wgpu::BufferDescriptor {
                    label: Some(self.label),
                    size,
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                },
            ));
            self.uploaded();
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
        self.uploaded();
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

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::Mirror;

    // Plausible defect: records that outgrow more than half of what the
    // device binds doubling the buffer past it, so binding it whole, as the
    // cull binds the candidates, fails validation though the records fit.
    // The oracle is the device's own validation at a small binding limit.
    #[test]
    fn growth_stops_at_what_the_device_binds() {
        let Some(adapter) = crate::test_support::adapter() else {
            return;
        };
        // A hundred 48-byte records, as `DrawCandidate`s are.
        let binding = 100 * 48;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            required_limits: wgpu::Limits {
                max_storage_buffer_binding_size: binding,
                ..crate::graphics_device::limits(&adapter)
            },
            ..Default::default()
        }))
        .unwrap();
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let mut mirror = Mirror::<[u32; 12]>::new("records");
        let validation = device.push_error_scope(wgpu::ErrorFilter::Validation);
        // 60 records, then 90: more than half the limit, so doubling the
        // first buffer would pass it.
        for count in [60u32, 90] {
            for index in 0..count {
                mirror.set(index, [index; 12], [0; 12]);
            }
            mirror.upload(&device, &queue);
            let _ = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: mirror.buffer().as_entire_binding(),
                }],
            });
        }
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        if let Some(error) = pollster::block_on(validation.pop()) {
            panic!("{error}");
        }
        assert!((90 * 48..=binding).contains(&mirror.bytes()));
    }
}
