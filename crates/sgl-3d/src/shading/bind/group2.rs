//! Group 2, a material: its bindings as bind_material.wgsl and
//! bind_material_extended.wgsl declare them, the binding tier each map
//! binding takes, the maps a material is authored with and the binding each
//! fills, and the layout of each tier (specs/sgl3d-architecture.md, Binding
//! tiers).
use super::{layout, texture};

pub(crate) const MATERIAL: u32 = 0;
pub(crate) const BASE_MAP: u32 = 1;
pub(crate) const MR_MAP: u32 = 2;
pub(crate) const TEX_SAMPLER: u32 = 3;
pub(crate) const EMISSION_MAP: u32 = 4;
/// The normal map, else the bump map: shading takes a bump map only where
/// the material has no normal map.
pub(crate) const RELIEF_MAP: u32 = 5;
pub(crate) const BAKED_MATERIAL: u32 = 7;
pub(crate) const ANISOTROPY_MAP: u32 = 8;
pub(crate) const TRANSMISSION_MAP: u32 = 9;
pub(crate) const THICKNESS_MAP: u32 = 10;
pub(crate) const CLEARCOAT_MAP: u32 = 11;
pub(crate) const COAT_ROUGHNESS_MAP: u32 = 12;
pub(crate) const COAT_NORMAL_MAP: u32 = 13;
pub(crate) const IRIDESCENCE_MAP: u32 = 14;
pub(crate) const IRIDESCENCE_THICKNESS_MAP: u32 = 15;
pub(crate) const SHEEN_COLOR_MAP: u32 = 16;
pub(crate) const SHEEN_ROUGHNESS_MAP: u32 = 17;
pub(crate) const DIFFUSE_TRANSMISSION_MAP: u32 = 18;
pub(crate) const DIFFUSE_TRANSMISSION_COLOR_MAP: u32 = 19;
/// A material's shader's parameter blocks, this frame's and the last
/// submitted frame's (shader_inputs_bound.wgsl), on every tier, which its
/// vertex and fragment stages read.
pub(crate) const SHADER_PARAMS: u32 = 20;
pub(crate) const PREVIOUS_SHADER_PARAMS: u32 = 21;

/// Which bindings SGL3D binds on a device, by its sampled textures per
/// shader stage: `Basic` from S3D-1's floor up to 47, where the bindings
/// of `Extended` alone (lit group 0's directionality and dynamic GI probes,
/// the anisotropy, clearcoat, iridescence, transmission, thickness, sheen
/// and diffuse transmission maps, and the transparent stage's copy of the
/// frame) give way to their
/// fallbacks, and `Extended` from 48, with every binding. A device's tier
/// is fixed;
/// `Renderer::binding_tier` reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BindingTier {
    Basic,
    Extended,
}

/// The sampled textures per shader stage from which a device takes
/// `Extended`: Dawn's upper tier, so the browser keeps every binding where
/// its adapter offers it.
const EXTENDED_SAMPLED_TEXTURES: u32 = 48;

impl BindingTier {
    /// The tier of a device with `limits`, the device's own.
    pub(crate) fn of(limits: &wgpu::Limits) -> Self {
        if limits.max_sampled_textures_per_shader_stage >= EXTENDED_SAMPLED_TEXTURES {
            Self::Extended
        } else {
            Self::Basic
        }
    }
}

/// A group-2 binding that holds a material's map.
pub(crate) struct MapBinding {
    pub binding: u32,
    /// Whether it is sampled as sRGB colour rather than linear data.
    pub colour: bool,
    /// The least tier that binds it.
    pub tier: BindingTier,
}

/// Group 2's map bindings: the one declaration of each one's tier.
pub(crate) const MAP_BINDINGS: [MapBinding; 16] = [
    MapBinding {
        binding: BASE_MAP,
        colour: true,
        tier: BindingTier::Basic,
    },
    MapBinding {
        binding: MR_MAP,
        colour: false,
        tier: BindingTier::Basic,
    },
    MapBinding {
        binding: EMISSION_MAP,
        colour: true,
        tier: BindingTier::Basic,
    },
    MapBinding {
        binding: RELIEF_MAP,
        colour: false,
        tier: BindingTier::Basic,
    },
    MapBinding {
        binding: ANISOTROPY_MAP,
        colour: false,
        tier: BindingTier::Extended,
    },
    MapBinding {
        binding: CLEARCOAT_MAP,
        colour: false,
        tier: BindingTier::Extended,
    },
    MapBinding {
        binding: COAT_ROUGHNESS_MAP,
        colour: false,
        tier: BindingTier::Extended,
    },
    MapBinding {
        binding: COAT_NORMAL_MAP,
        colour: false,
        tier: BindingTier::Extended,
    },
    MapBinding {
        binding: IRIDESCENCE_MAP,
        colour: false,
        tier: BindingTier::Extended,
    },
    MapBinding {
        binding: IRIDESCENCE_THICKNESS_MAP,
        colour: false,
        tier: BindingTier::Extended,
    },
    MapBinding {
        binding: TRANSMISSION_MAP,
        colour: false,
        tier: BindingTier::Extended,
    },
    MapBinding {
        binding: THICKNESS_MAP,
        colour: false,
        tier: BindingTier::Extended,
    },
    MapBinding {
        binding: SHEEN_COLOR_MAP,
        colour: true,
        tier: BindingTier::Extended,
    },
    MapBinding {
        binding: SHEEN_ROUGHNESS_MAP,
        colour: false,
        tier: BindingTier::Extended,
    },
    MapBinding {
        binding: DIFFUSE_TRANSMISSION_MAP,
        colour: false,
        tier: BindingTier::Extended,
    },
    MapBinding {
        binding: DIFFUSE_TRANSMISSION_COLOR_MAP,
        colour: true,
        tier: BindingTier::Extended,
    },
];

/// The map bindings a device of `tier` binds.
pub(crate) fn map_bindings(tier: BindingTier) -> impl Iterator<Item = &'static MapBinding> {
    MAP_BINDINGS.iter().filter(move |map| map.tier <= tier)
}

/// A map a material is authored with. Each fills one map binding, whose tier
/// is its own, and owns one bit of the record's `maps` word
/// (`MATERIAL_MAP_*` in material.wgsl).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MaterialMap {
    Base,
    MetallicRoughness,
    /// An occlusion map in the red channel of the metallic-roughness map's
    /// image (ORM packing), the one place SGL3D samples occlusion from.
    Occlusion,
    Emission,
    Normal,
    Bump,
    Anisotropy,
    Clearcoat,
    CoatRoughness,
    CoatNormal,
    Iridescence,
    IridescenceThickness,
    /// KHR_materials_transmission's map, in its red channel.
    Transmission,
    /// KHR_materials_volume's thickness map, in its green channel.
    Thickness,
    SheenColor,
    SheenRoughness,
    DiffuseTransmission,
    DiffuseTransmissionColor,
}

impl MaterialMap {
    pub(crate) const ALL: [Self; 18] = [
        Self::Base,
        Self::MetallicRoughness,
        Self::Occlusion,
        Self::Emission,
        Self::Normal,
        Self::Bump,
        Self::Anisotropy,
        Self::Clearcoat,
        Self::CoatRoughness,
        Self::CoatNormal,
        Self::Iridescence,
        Self::IridescenceThickness,
        Self::Transmission,
        Self::Thickness,
        Self::SheenColor,
        Self::SheenRoughness,
        Self::DiffuseTransmission,
        Self::DiffuseTransmissionColor,
    ];

    /// The map binding it fills.
    pub(crate) fn binding(self) -> &'static MapBinding {
        let number = match self {
            Self::Base => BASE_MAP,
            Self::MetallicRoughness | Self::Occlusion => MR_MAP,
            Self::Emission => EMISSION_MAP,
            Self::Normal | Self::Bump => RELIEF_MAP,
            Self::Anisotropy => ANISOTROPY_MAP,
            Self::Clearcoat => CLEARCOAT_MAP,
            Self::CoatRoughness => COAT_ROUGHNESS_MAP,
            Self::CoatNormal => COAT_NORMAL_MAP,
            Self::Iridescence => IRIDESCENCE_MAP,
            Self::IridescenceThickness => IRIDESCENCE_THICKNESS_MAP,
            Self::Transmission => TRANSMISSION_MAP,
            Self::Thickness => THICKNESS_MAP,
            Self::SheenColor => SHEEN_COLOR_MAP,
            Self::SheenRoughness => SHEEN_ROUGHNESS_MAP,
            Self::DiffuseTransmission => DIFFUSE_TRANSMISSION_MAP,
            Self::DiffuseTransmissionColor => DIFFUSE_TRANSMISSION_COLOR_MAP,
        };
        MAP_BINDINGS
            .iter()
            .find(|map| map.binding == number)
            .expect("every map fills a map binding")
    }

    /// Its bit in the record's `maps` word.
    pub(crate) fn bit(self) -> u32 {
        1 << self as u32
    }
}

/// Group 2, a material, on a device of `tier`: bind_material.wgsl's values,
/// its maps and sampler and its baked-lighting eligibility, its shader's
/// parameter blocks (shader_inputs_bound.wgsl), and on `Extended`
/// bind_material_extended.wgsl's maps.
pub(crate) fn material(device: &wgpu::Device, tier: BindingTier) -> wgpu::BindGroupLayout {
    layout(device, "material", &material_entries(tier))
}

/// Group 2's entries on a device of `tier`.
pub(crate) fn material_entries(tier: BindingTier) -> Vec<wgpu::BindGroupLayoutEntry> {
    let uniform = |binding| wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    };
    let mut entries = vec![uniform(MATERIAL)];
    entries.extend(map_bindings(tier).map(|map| {
        texture(
            map.binding,
            wgpu::TextureSampleType::Float { filterable: true },
            wgpu::TextureViewDimension::D2,
        )
    }));
    entries.push(wgpu::BindGroupLayoutEntry {
        binding: TEX_SAMPLER,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    });
    entries.push(uniform(BAKED_MATERIAL));
    entries.extend([SHADER_PARAMS, PREVIOUS_SHADER_PARAMS].map(|binding| {
        wgpu::BindGroupLayoutEntry {
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ..uniform(binding)
        }
    }));
    entries
}
