//! A bind group kept across frames while it binds the same resources, which
//! stages lay out for their own passes. Their resources change only at a
//! resize, a history swap or a new input, so making their groups every frame
//! only costs encode time.

/// What one entry binds, by identity.
enum Bound {
    View(wgpu::TextureView),
    Sampler(wgpu::Sampler),
    Buffer(wgpu::Buffer, wgpu::BufferAddress, Option<wgpu::BufferSize>),
    /// An array or other binding, which is never reused.
    Other,
}

impl Bound {
    fn new(resource: &wgpu::BindingResource<'_>) -> Self {
        match resource {
            wgpu::BindingResource::TextureView(view) => Self::View((*view).clone()),
            wgpu::BindingResource::Sampler(sampler) => Self::Sampler((*sampler).clone()),
            wgpu::BindingResource::Buffer(binding) => {
                Self::Buffer(binding.buffer.clone(), binding.offset, binding.size)
            }
            _ => Self::Other,
        }
    }

    fn binds(&self, resource: &wgpu::BindingResource<'_>) -> bool {
        match (self, resource) {
            (Self::View(bound), wgpu::BindingResource::TextureView(view)) => bound == *view,
            (Self::Sampler(bound), wgpu::BindingResource::Sampler(sampler)) => bound == *sampler,
            (Self::Buffer(buffer, offset, size), wgpu::BindingResource::Buffer(binding)) => {
                buffer == binding.buffer && *offset == binding.offset && *size == binding.size
            }
            _ => false,
        }
    }
}

/// One bind group of `layout`, remade only when an entry binds a resource
/// it does not: a reallocated target, the other half of a history pair or
/// another input.
pub(crate) struct CachedGroup {
    layout: wgpu::BindGroupLayout,
    bound: Vec<(u32, Bound)>,
    group: Option<wgpu::BindGroup>,
}

impl CachedGroup {
    pub fn new(layout: wgpu::BindGroupLayout) -> Self {
        Self {
            layout,
            bound: Vec::new(),
            group: None,
        }
    }

    /// The group binding `entries` (binding, resource).
    pub fn get(
        &mut self,
        device: &wgpu::Device,
        label: &str,
        entries: &[(u32, wgpu::BindingResource<'_>)],
    ) -> &wgpu::BindGroup {
        let current = self.group.is_some()
            && self.bound.len() == entries.len()
            && self.bound.iter().zip(entries).all(
                |((bound_binding, bound), (binding, resource))| {
                    bound_binding == binding && bound.binds(resource)
                },
            );
        if !current {
            self.bound = entries
                .iter()
                .map(|(binding, resource)| (*binding, Bound::new(resource)))
                .collect();
            let entries: Vec<_> = entries
                .iter()
                .map(|(binding, resource)| wgpu::BindGroupEntry {
                    binding: *binding,
                    resource: resource.clone(),
                })
                .collect();
            self.group = Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(label),
                layout: &self.layout,
                entries: &entries,
            }));
        }
        self.group.as_ref().unwrap()
    }
}
