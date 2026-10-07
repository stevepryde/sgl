//! A material's raster group 2: its values and lightmap eligibility
//! buffers, the views of its maps in effect (white for the rest of the map
//! bindings the device's binding tier binds) and a sampler of their
//! wrapping at the scene's anisotropic filtering.
use super::maps::InEffect;
use crate::scene::textures::{self, Textures};
use crate::shading::bind::{BindingTier, group2};
use gltf::texture::WrappingMode;

/// What group 2 binds: the material's maps in effect, each a texture index,
/// and their wrapping, which its sampler takes.
pub(super) struct Bound {
    pub maps: InEffect,
    pub wrap: [WrappingMode; 2],
}

/// What every material's group 2 shares: the device's binding tier and its
/// layout, the white fallback and the samplers' anisotropy.
pub(super) struct Groups {
    pub tier: BindingTier,
    layout: wgpu::BindGroupLayout,
    /// White, for maps a material does not have.
    fallback: wgpu::TextureView,
    /// The samplers' `anisotropy_clamp`.
    pub anisotropy: u16,
}

impl Groups {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let white = image::RgbaImage::from_pixel(1, 1, image::Rgba([255; 4]));
        let tier = BindingTier::of(&device.limits());
        Self {
            tier,
            layout: crate::shading::bind::material(device, tier),
            fallback: textures::upload(device, queue, &white, false),
            anisotropy: crate::settings::AnisotropicFiltering::default().clamp(),
        }
    }

    /// Group 2 of a material binding `bound`, of `textures`, its values
    /// `buffer` and lightmap eligibility `baked`, sampled with the current
    /// anisotropy.
    pub fn group(
        &self,
        device: &wgpu::Device,
        textures: &Textures,
        bound: &Bound,
        buffer: &wgpu::Buffer,
        baked: &wgpu::Buffer,
    ) -> wgpu::BindGroup {
        let view = |texture: Option<usize>, colour: bool| -> &wgpu::TextureView {
            match texture {
                Some(texture) => {
                    let texture = textures.get(texture);
                    if colour {
                        texture.color.as_ref()
                    } else {
                        texture.data.as_ref()
                    }
                    .expect("a texture is uploaded as each material samples it")
                }
                None => &self.fallback,
            }
        };
        let address = |mode| match mode {
            WrappingMode::Repeat => wgpu::AddressMode::Repeat,
            WrappingMode::MirroredRepeat => wgpu::AddressMode::MirrorRepeat,
            WrappingMode::ClampToEdge => wgpu::AddressMode::ClampToEdge,
        };
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("authored material sampler"),
            address_mode_u: address(bound.wrap[0]),
            address_mode_v: address(bound.wrap[1]),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            anisotropy_clamp: self.anisotropy,
            ..Default::default()
        });
        let mut entries = vec![
            wgpu::BindGroupEntry {
                binding: group2::MATERIAL,
                resource: buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: group2::TEX_SAMPLER,
                resource: wgpu::BindingResource::Sampler(&sampler),
            },
            wgpu::BindGroupEntry {
                binding: group2::BAKED_MATERIAL,
                resource: baked.as_entire_binding(),
            },
        ];
        entries.extend(
            group2::map_bindings(self.tier).map(|map| wgpu::BindGroupEntry {
                binding: map.binding,
                resource: wgpu::BindingResource::TextureView(view(
                    bound.maps.bound(map.binding),
                    map.colour,
                )),
            }),
        );
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("retained material"),
            layout: &self.layout,
            entries: &entries,
        })
    }
}
