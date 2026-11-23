//! The local shadow stage's depth textures: the frame's atlas and the static
//! layers, which the stage creates together once a shadow is placed, and
//! the one-texel stand-in group 0 binds for both until then, of the same
//! format and dimension.

/// The frame's atlas, the static layers, and the copy's group, which binds
/// the layers.
pub(super) struct Atlases {
    pub frame: Atlas,
    pub layers: Atlas,
    pub copy_group: wgpu::BindGroup,
}

impl Atlases {
    /// Both atlases at `size` texels a side.
    pub fn new(device: &wgpu::Device, copy_layout: &wgpu::BindGroupLayout, size: u32) -> Self {
        let layers = Atlas::new(device, "local light shadow static layers", size);
        Self {
            frame: Atlas::new(device, "local light shadow atlas", size),
            copy_group: copy_group(device, copy_layout, &layers),
            layers,
        }
    }
}

/// One of the stage's two atlases of one layout: the view its passes draw
/// into and the one-layer array group 0 samples, as the shared shadow
/// filters take.
pub(super) struct Atlas {
    pub target: wgpu::TextureView,
    pub sampled: wgpu::TextureView,
}

impl Atlas {
    /// An atlas of `size` texels a side.
    fn new(device: &wgpu::Device, label: &str, size: u32) -> Self {
        let texture = depth_texture(device, label, size);
        Self {
            target: texture.create_view(&wgpu::TextureViewDescriptor {
                dimension: Some(wgpu::TextureViewDimension::D2),
                ..Default::default()
            }),
            sampled: sampled(&texture),
        }
    }
}

/// The one-layer array view group 0 samples of `texture`.
fn sampled(texture: &wgpu::Texture) -> wgpu::TextureView {
    texture.create_view(&wgpu::TextureViewDescriptor {
        dimension: Some(wgpu::TextureViewDimension::D2Array),
        ..Default::default()
    })
}

/// A depth texture of the atlases' format, `size` texels a side.
fn depth_texture(device: &wgpu::Device, label: &str, size: u32) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: crate::shading::gbuffer::DEPTH,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    })
}

/// What group 0 binds for both atlases while there are none.
pub(super) fn stand_in(device: &wgpu::Device) -> wgpu::TextureView {
    sampled(&depth_texture(device, "no local light shadows", 1))
}

/// The copy's group: the static layers it copies faces from.
fn copy_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    layers: &Atlas,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("local light shadow static layers"),
        layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: wgpu::BindingResource::TextureView(&layers.target),
        }],
    })
}
