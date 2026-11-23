//! Material samplers (specs/sgl3d-architecture.md, Material samplers): a
//! map's sampler state as the GPU reads it, one byte of the record's
//! `map_samplers` per map binding, which `sampler_state.wgsl` decodes; the
//! state group 2's one sampler binds and the map bindings the shader samples
//! itself; and the GPU sampler of a state.
use super::Module;
use super::bind::group2::MAP_BINDINGS;
use crate::asset::{Sampler, TextureFilter, TextureWrap};

/// A state's wrap modes, U in bits 0–1 and V in bits 2–3.
pub(crate) const WRAP_REPEAT: u32 = 0;
pub(crate) const WRAP_MIRRORED_REPEAT: u32 = 1;
pub(crate) const WRAP_CLAMP_TO_EDGE: u32 = 2;
/// Its magnification and minification filters, linear where set.
pub(crate) const MAG_LINEAR: u32 = 16;
pub(crate) const MIN_LINEAR: u32 = 32;
/// Its mipmap filter, in bits 6–7: none (level 0 alone), nearest or linear.
pub(crate) const MIP_SHIFT: u32 = 6;
pub(crate) const MIP_NONE: u32 = 0;
pub(crate) const MIP_NEAREST: u32 = 1;
pub(crate) const MIP_LINEAR: u32 = 2;

/// The state's decoding and the one wrap formula the shader-sampled maps and
/// ray hits take.
pub(crate) static SAMPLER_STATE: Module = Module {
    name: "sampler_state",
    source: include_str!("sampler_state.wgsl"),
    deps: &[],
};
/// A rasterized fragment's sample of a material map, through group 2's
/// sampler or, for a map in another state, in the shader.
pub(crate) static MATERIAL_SAMPLING: Module = Module {
    name: "material_sampling",
    source: include_str!("material_sampling.wgsl"),
    deps: &[&super::BIND_MATERIAL, &SAMPLER_STATE],
};

/// `sampler`'s byte.
pub(crate) fn code(sampler: &Sampler) -> u32 {
    let wrap = |mode| match mode {
        TextureWrap::Repeat => WRAP_REPEAT,
        TextureWrap::MirroredRepeat => WRAP_MIRRORED_REPEAT,
        TextureWrap::ClampToEdge => WRAP_CLAMP_TO_EDGE,
    };
    let linear = |filter, bit| match filter {
        TextureFilter::Nearest => 0,
        TextureFilter::Linear => bit,
    };
    let mip = match sampler.mipmap_filter {
        None => MIP_NONE,
        Some(TextureFilter::Nearest) => MIP_NEAREST,
        Some(TextureFilter::Linear) => MIP_LINEAR,
    };
    wrap(sampler.wrap[0])
        | wrap(sampler.wrap[1]) << 2
        | linear(sampler.mag_filter, MAG_LINEAR)
        | linear(sampler.min_filter, MIN_LINEAR)
        | mip << MIP_SHIFT
}

/// Whether `sampler`'s magnification, minification and mipmap filters are
/// all linear: the one kind of state anisotropic filtering applies to.
fn trilinear(sampler: &Sampler) -> bool {
    sampler.mag_filter == TextureFilter::Linear
        && sampler.min_filter == TextureFilter::Linear
        && sampler.mipmap_filter == Some(TextureFilter::Linear)
}

/// The GPU sampler of `sampler` under the game's anisotropy clamp
/// `anisotropy`, which applies only where its three filters are linear, as
/// WebGPU requires and three.js r187 does (WebGPUTextureUtils.js 187–193).
/// A non-mipmapped minification filter selects level 0 by nearest mip
/// selection under a maximum level of detail of 0.25, the Vulkan
/// specification's emulation of OpenGL's `NEAREST` and `LINEAR`
/// minification (VkSamplerCreateInfo, Mapping of OpenGL to Vulkan Filter
/// Modes): 0 would always magnify.
pub(crate) fn descriptor(sampler: &Sampler, anisotropy: u16) -> wgpu::SamplerDescriptor<'static> {
    let filter = |filter| match filter {
        TextureFilter::Nearest => wgpu::FilterMode::Nearest,
        TextureFilter::Linear => wgpu::FilterMode::Linear,
    };
    let address = |mode| match mode {
        TextureWrap::Repeat => wgpu::AddressMode::Repeat,
        TextureWrap::MirroredRepeat => wgpu::AddressMode::MirrorRepeat,
        TextureWrap::ClampToEdge => wgpu::AddressMode::ClampToEdge,
    };
    wgpu::SamplerDescriptor {
        label: Some("material sampler"),
        address_mode_u: address(sampler.wrap[0]),
        address_mode_v: address(sampler.wrap[1]),
        mag_filter: filter(sampler.mag_filter),
        min_filter: filter(sampler.min_filter),
        mipmap_filter: match sampler.mipmap_filter {
            Some(TextureFilter::Linear) => wgpu::MipmapFilterMode::Linear,
            Some(TextureFilter::Nearest) | None => wgpu::MipmapFilterMode::Nearest,
        },
        lod_max_clamp: if sampler.mipmap_filter.is_none() {
            0.25
        } else {
            32.
        },
        anisotropy_clamp: if trilinear(sampler) { anisotropy } else { 1 },
        ..Default::default()
    }
}

/// How a material's maps are sampled: each map binding's state, in
/// `MAP_BINDINGS` order, the one group 2's sampler binds and the bindings in
/// another state, which raster samples in the shader.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MapSamplers {
    bound: Sampler,
    states: [Sampler; MAP_BINDINGS.len()],
    shader_sampled: u16,
}

impl MapSamplers {
    /// Of the map bindings' states `states`, in `MAP_BINDINGS` order, none
    /// where a binding holds no map in effect: the bound state is the one
    /// most of them take, so the fewest maps are sampled in the shader. Of
    /// states that tie, a trilinear one (`trilinear`) wins, which alone
    /// takes anisotropic filtering and costs the shader the most texel
    /// reads, then the earliest binding's. The default where none holds a
    /// map. A binding without a map takes the bound state.
    pub fn new(states: [Option<Sampler>; MAP_BINDINGS.len()]) -> Self {
        let count = |state: &Sampler| states.iter().filter(|s| s.as_ref() == Some(state)).count();
        let rank = |state: &Sampler| (count(state), trilinear(state));
        let bound = states
            .iter()
            .flatten()
            .fold(None, |best: Option<Sampler>, state| match best {
                Some(best) if rank(&best) >= rank(state) => Some(best),
                _ => Some(*state),
            })
            .unwrap_or_default();
        let states = states.map(|state| state.unwrap_or(bound));
        let shader_sampled = states
            .iter()
            .enumerate()
            .filter(|(_, state)| **state != bound)
            .fold(0, |bits, (index, _)| bits | 1 << index);
        Self {
            bound,
            states,
            shader_sampled,
        }
    }

    /// The state group 2's sampler binds.
    pub fn bound(&self) -> Sampler {
        self.bound
    }

    /// A bit, by `MAP_BINDINGS` index, for each map binding in a state
    /// other than the bound one, which raster samples in the shader
    /// (`Variant::shader_sampled`).
    pub fn shader_sampled(&self) -> u16 {
        self.shader_sampled
    }

    /// The record's `map_samplers`: each binding's byte, four to a word.
    pub fn codes(&self) -> [u32; 4] {
        let mut words = [0; 4];
        for (index, state) in self.states.iter().enumerate() {
            words[index / 4] |= code(state) << (8 * (index % 4));
        }
        words
    }
}

impl Default for MapSamplers {
    /// Every binding in glTF's default state.
    fn default() -> Self {
        Self::new([None; MAP_BINDINGS.len()])
    }
}

/// The WGSL twins of the state's encoding (sampler_state.wgsl) and of each
/// map binding's slot, its `MAP_BINDINGS` index (material.wgsl's
/// `MATERIAL_SLOT_*`).
#[cfg(test)]
pub(crate) fn constants() -> Vec<crate::shading::layout_tests::Constant> {
    use super::bind::group2::*;
    use crate::shading::layout_tests::Constant;
    let slot = |binding: u32| {
        MAP_BINDINGS
            .iter()
            .position(|map| map.binding == binding)
            .unwrap() as u32
    };
    [
        ("SAMPLER_WRAP_REPEAT", WRAP_REPEAT),
        ("SAMPLER_WRAP_MIRRORED_REPEAT", WRAP_MIRRORED_REPEAT),
        ("SAMPLER_WRAP_CLAMP_TO_EDGE", WRAP_CLAMP_TO_EDGE),
        ("SAMPLER_MAG_LINEAR", MAG_LINEAR),
        ("SAMPLER_MIN_LINEAR", MIN_LINEAR),
        ("SAMPLER_MIP_SHIFT", MIP_SHIFT),
        ("SAMPLER_MIP_NONE", MIP_NONE),
        ("SAMPLER_MIP_NEAREST", MIP_NEAREST),
        ("SAMPLER_MIP_LINEAR", MIP_LINEAR),
        ("MATERIAL_SLOT_BASE", slot(BASE_MAP)),
        ("MATERIAL_SLOT_METALLIC_ROUGHNESS", slot(MR_MAP)),
        ("MATERIAL_SLOT_EMISSION", slot(EMISSION_MAP)),
        ("MATERIAL_SLOT_RELIEF", slot(RELIEF_MAP)),
        ("MATERIAL_SLOT_ANISOTROPY", slot(ANISOTROPY_MAP)),
        ("MATERIAL_SLOT_CLEARCOAT", slot(CLEARCOAT_MAP)),
        ("MATERIAL_SLOT_COAT_ROUGHNESS", slot(COAT_ROUGHNESS_MAP)),
        ("MATERIAL_SLOT_COAT_NORMAL", slot(COAT_NORMAL_MAP)),
        ("MATERIAL_SLOT_IRIDESCENCE", slot(IRIDESCENCE_MAP)),
        (
            "MATERIAL_SLOT_IRIDESCENCE_THICKNESS",
            slot(IRIDESCENCE_THICKNESS_MAP),
        ),
        ("MATERIAL_SLOT_TRANSMISSION", slot(TRANSMISSION_MAP)),
        ("MATERIAL_SLOT_THICKNESS", slot(THICKNESS_MAP)),
        ("MATERIAL_SLOT_SHEEN_COLOR", slot(SHEEN_COLOR_MAP)),
        ("MATERIAL_SLOT_SHEEN_ROUGHNESS", slot(SHEEN_ROUGHNESS_MAP)),
        (
            "MATERIAL_SLOT_DIFFUSE_TRANSMISSION",
            slot(DIFFUSE_TRANSMISSION_MAP),
        ),
        (
            "MATERIAL_SLOT_DIFFUSE_TRANSMISSION_COLOR",
            slot(DIFFUSE_TRANSMISSION_COLOR_MAP),
        ),
    ]
    .map(|(name, value)| Constant::new("geometry", name, naga::Literal::U32(value)))
    .into()
}
