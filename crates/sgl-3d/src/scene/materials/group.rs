//! A material's raster group 2: its values buffer and its bindings buffer
//! (lightmap eligibility and each map binding's transform), the views of its maps in effect (white for the rest of the map
//! bindings the device's binding tier binds; the render texture it names
//! in its emission map's) and the sampler of their bound state at the
//! scene's anisotropic filtering, shared by every material of that state
//! (specs/sgl3d-architecture.md, Material samplers).
use super::maps::InEffect;
use super::params::ParamBlock;
use crate::asset::Sampler;
use crate::scene::render_textures::RenderTextures;
use crate::scene::textures::{self, Textures};
use crate::shading::bind::{BindingTier, group2};
use crate::shading::map_uvs::MapUvs;
use crate::shading::samplers::{self, MapSamplers};
use std::collections::HashMap;

/// What group 2 binds: the material's maps in effect, each a texture index,
/// how they are sampled, whose bound state its sampler takes, and where
/// (each binding's UV set and transform). A render texture in the emission
/// map's binding is sampled in that binding's state at that binding's
/// coordinates.
pub(super) struct Bound {
    pub maps: InEffect,
    pub samplers: MapSamplers,
    pub uvs: MapUvs,
}

/// What every material's group 2 shares: the device, which a material's
/// edit that changes its shader makes its new group on, its binding tier
/// and its layout, the white fallback, the zero parameter block of a
/// material without a shader, the samplers' anisotropy and a GPU sampler
/// of each state at it, as three.js r187 keeps one per state
/// (`WebGPUTextureUtils.js` 162–207).
pub(super) struct Groups {
    pub device: wgpu::Device,
    pub tier: BindingTier,
    layout: wgpu::BindGroupLayout,
    /// White, for maps a material does not have.
    fallback: wgpu::TextureView,
    /// Bound twice for a material without a shader.
    zero_params: wgpu::Buffer,
    /// The samplers' `anisotropy_clamp`.
    anisotropy: u16,
    /// The GPU sampler of each state a group has bound at `anisotropy`.
    samplers: HashMap<Sampler, wgpu::Sampler>,
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
            samplers: HashMap::new(),
        }
    }

    /// Samples with at most `anisotropy` anisotropic samples from now on;
    /// whether that changes it, so every group must be made again.
    pub fn set_anisotropy(&mut self, anisotropy: u16) -> bool {
        if self.anisotropy == anisotropy {
            return false;
        }
        self.anisotropy = anisotropy;
        self.samplers.clear();
        true
    }

    /// Group 2 of `material` as it stands: its maps in effect, its buffers,
    /// its shader's parameter blocks and the render texture it names, of
    /// `render_textures`.
    pub fn of(
        &mut self,
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
        &mut self,
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
        let state = bound.samplers.bound();
        let anisotropy = self.anisotropy;
        let sampler: &wgpu::Sampler = self
            .samplers
            .entry(state)
            .or_insert_with(|| device.create_sampler(&samplers::descriptor(&state, anisotropy)));
        let mut entries = vec![
            wgpu::BindGroupEntry {
                binding: group2::MATERIAL,
                resource: buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: group2::TEX_SAMPLER,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
            wgpu::BindGroupEntry {
                binding: group2::MATERIAL_BINDINGS,
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
