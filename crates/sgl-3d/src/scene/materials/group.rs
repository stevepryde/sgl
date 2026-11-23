//! A material's raster group 2: its values and lightmap eligibility
//! buffers, the views of its maps in effect (white for the rest of the map
//! bindings the device's binding tier binds; the render texture it names
//! in its emission map's) and a sampler of their wrapping at the scene's
//! anisotropic filtering.
use super::maps::InEffect;
use super::params::ParamBlock;
use crate::scene::render_textures::RenderTextures;
use crate::scene::textures::{self, Textures};
use crate::shading::bind::{BindingTier, group2};
use gltf::texture::WrappingMode;

/// What group 2 binds: the material's maps in effect, each a texture index,
/// and their wrapping, which its sampler takes.
pub(super) struct Bound {
    pub maps: InEffect,
    pub wrap: [WrappingMode; 2],
}

/// What every material's group 2 shares: the device, which a material's
/// edit that changes its shader makes its new group on, its binding tier
/// and its layout, the white fallback, the zero parameter block of a
/// material without a shader and the samplers' anisotropy.
pub(super) struct Groups {
    pub device: wgpu::Device,
    pub tier: BindingTier,
    layout: wgpu::BindGroupLayout,
    /// White, for maps a material does not have.
    fallback: wgpu::TextureView,
    /// Bound twice for a material without a shader.
    zero_params: wgpu::Buffer,
    /// The samplers' `anisotropy_clamp`.
    pub anisotropy: u16,
}

impl Groups {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let white = image::RgbaImage::from_pixel(1, 1, image::Rgba([255; 4]));
        let tier = BindingTier::of(&device.limits());
        Self {
            device: device.clone(),
            tier,
            layout: crate::shading::bind::material(device, tier),
            fallback: textures::upload(device, queue, &white, false),
            zero_params: super::params::zero_block(device),
            anisotropy: crate::settings::AnisotropicFiltering::default().clamp(),
        }
    }

    /// Group 2 of `material` as it stands: its maps in effect, its buffers,
    /// its shader's parameter blocks and the render texture it names, of
    /// `render_textures`.
    pub fn of(
        &self,
        device: &wgpu::Device,
        textures: &Textures,
        render_textures: &RenderTextures,
        material: &super::Material,
    ) -> wgpu::BindGroup {
        let emission = material.values.emission_render_texture.map(|id| {
            &render_textures
                .get(id)
                .expect("a named render texture lives")
                .view
        });
        self.group(
            device,
            textures,
            &material.bound,
            [&material.buffer, &material.baked],
            material.params.as_ref(),
            emission,
        )
    }

    /// Group 2 of a material binding `bound`, of `textures`, its values
    /// `buffer`, lightmap eligibility `baked` and its shader's parameter
    /// blocks `params`, the zero block twice without, sampled with the
    /// current anisotropy. `emission`, the view of the render texture the
    /// material names, fills the emission map's binding in place of its
    /// image.
    pub fn group(
        &self,
        device: &wgpu::Device,
        textures: &Textures,
        bound: &Bound,
        [buffer, baked]: [&wgpu::Buffer; 2],
        params: Option<&ParamBlock>,
        emission: Option<&wgpu::TextureView>,
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
            wgpu::BindGroupEntry {
                binding: group2::SHADER_PARAMS,
                resource: params
                    .map_or(&self.zero_params, |params| &params.current)
                    .as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: group2::PREVIOUS_SHADER_PARAMS,
                resource: params
                    .map_or(&self.zero_params, |params| &params.previous)
                    .as_entire_binding(),
            },
        ];
        entries.extend(
            group2::map_bindings(self.tier).map(|map| wgpu::BindGroupEntry {
                binding: map.binding,
                resource: wgpu::BindingResource::TextureView(
                    match emission.filter(|_| map.binding == group2::EMISSION_MAP) {
                        Some(picture) => picture,
                        None => view(bound.maps.bound(map.binding), map.colour),
                    },
                ),
            }),
        );
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("retained material"),
            layout: &self.layout,
            entries: &entries,
        })
    }
}
