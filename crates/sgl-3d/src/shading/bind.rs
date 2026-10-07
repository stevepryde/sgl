//! Group-0 layouts, one per bind module (bind_*.wgsl), the lit one by
//! binding tier, group 1's, group 2's (`group2`, with its binding tiers),
//! the blended pipelines' group 3
//! and the GPU-built cascades' casters' group 3, and the binding numbers
//! they and the groups built for them use. The layout test checks every number
//! against naga's binding of the WGSL variable it is named after.

/// Group 0's bindings, as bind_lit.wgsl, bind_lit_extended.wgsl,
/// bind_unlit.wgsl and bind_shadow.wgsl declare them, and the binding tier
/// each takes.
pub(crate) mod group0 {
    use super::BindingTier;

    pub(crate) const VIEW: u32 = 0;
    pub(crate) const FRAME: u32 = 1;
    pub(crate) const SHADOW_SAMPLER: u32 = 2;
    pub(crate) const BACKDROP_MAP: u32 = 3;
    pub(crate) const ENVIRONMENT_MAP: u32 = 5;
    pub(crate) const ENVIRONMENT_SAMPLER: u32 = 7;
    pub(crate) const LIGHTS: u32 = 10;
    pub(crate) const CLUSTERS: u32 = 11;
    pub(crate) const BAKED: u32 = 12;
    pub(crate) const COLLECTION: u32 = 13;
    pub(crate) const LOCAL_SHADOW_ATLAS: u32 = 15;
    pub(crate) const LOCAL_SHADOWS: u32 = 16;
    pub(crate) const DIRECTIONAL_SHADOW_MAP: u32 = 17;
    pub(crate) const STATIC_LIGHTMAP: u32 = 18;
    pub(crate) const STATIC_LIGHTMAP_DIRECTION: u32 = 19;
    pub(crate) const BAKED_SAMPLER: u32 = 20;
    pub(crate) const STATIC_IRRADIANCE_ATLAS: u32 = 21;
    pub(crate) const LOOKUP_TABLES: u32 = 22;
    pub(crate) const DECAL_ATLAS: u32 = 23;
    pub(crate) const DECAL_SAMPLER: u32 = 24;
    pub(crate) const DECALS: u32 = 25;
    pub(crate) const STATIC_DIRECTION_ATLAS: u32 = 26;
    pub(crate) const FOG_VOLUME: u32 = 27;
    pub(crate) const FOG_SAMPLER: u32 = 28;
    pub(crate) const DYNAMIC_GI_PROBES: u32 = 29;
    pub(crate) const IRRADIANCE_VOLUME: u32 = 30;

    /// The least binding tier that binds `binding`, the one declaration of
    /// each one's: the lightmap's and the irradiance atlas's
    /// directionality and the dynamic GI volume's probes are `Extended`'s
    /// alone, which fall back to non-directional baked light and to no
    /// dynamic GI below it.
    pub(crate) fn tier(binding: u32) -> BindingTier {
        match binding {
            STATIC_LIGHTMAP_DIRECTION | STATIC_DIRECTION_ATLAS | DYNAMIC_GI_PROBES => {
                BindingTier::Extended
            }
            _ => BindingTier::Basic,
        }
    }
}

/// Group 1's bindings, as bind_scene.wgsl and scene_rays.wgsl declare them.
pub(crate) mod group1 {
    pub(crate) const OBJECTS: u32 = 0;
    pub(crate) const SCENE_SOURCE: u32 = 1;
    pub(crate) const SCENE_INSTANCES: u32 = 2;
}

/// Group 2's bindings and their tiers, the maps a material fills them with,
/// and its layout.
pub(crate) mod group2;
pub(crate) use group2::{BindingTier, material};

/// The scene's TLAS, which each tracing pass binds in its own group 3, as
/// scene_rays_hardware.wgsl declares it.
pub(crate) mod hardware {
    pub(crate) const SCENE_TLAS: u32 = 16;
}

/// The entry a tracing pass's group 3 binds the scene's TLAS at
/// (`hardware::SCENE_TLAS`), which its compute stage reads: every pass that
/// traces is a compute pass.
pub(crate) fn tlas_entry() -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding: hardware::SCENE_TLAS,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::AccelerationStructure {
            vertex_return: false,
        },
        count: None,
    }
}

/// The GPU-built directional cascades' casters' group 3, as
/// bind_caster_positions.wgsl declares it.
pub(crate) mod caster {
    pub(crate) const POSITIONS: u32 = 0;
}

/// The blended pipelines' group 3, as bind_blended.wgsl declares it.
pub(crate) mod blended {
    pub(crate) const REFLECTIONS: u32 = 0;
    pub(crate) const SURFACE_DEPTH: u32 = 1;
    pub(crate) const TRACE: u32 = 2;
}

/// The opaque stage's lighting pass's group 3 while ray-traced shadows run,
/// as bind_shadow_mask.wgsl declares it: numbers clear of the blended
/// group 3's, which the same program declares.
pub(crate) mod shadow_mask {
    pub(crate) const MASK: u32 = 8;
    pub(crate) const SLOTS: u32 = 9;
}

/// The screen-space method's cutoff and fade the blended draw composes its
/// result with; matches `BlendedTrace` in bind_blended.wgsl.
#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct BlendedTrace {
    /// Perceptual roughness at which the method traces no lobe; 0 composes
    /// nothing.
    pub cutoff: f32,
    /// The width of its fade below the cutoff.
    pub fade: f32,
    pub padding: [f32; 2],
}

use group0::*;

fn uniform(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn texture(
    binding: u32,
    sample_type: wgpu::TextureSampleType,
    view_dimension: wgpu::TextureViewDimension,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type,
            view_dimension,
            multisampled: false,
        },
        count: None,
    }
}

fn storage(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only: true },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

/// The frame's environment: its panorama, PMREM atlas and sampler.
fn environment() -> [wgpu::BindGroupLayoutEntry; 3] {
    let filterable = wgpu::TextureSampleType::Float { filterable: true };
    [
        texture(BACKDROP_MAP, filterable, wgpu::TextureViewDimension::D2),
        texture(
            ENVIRONMENT_MAP,
            filterable,
            wgpu::TextureViewDimension::D2Array,
        ),
        wgpu::BindGroupLayoutEntry {
            binding: ENVIRONMENT_SAMPLER,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            count: None,
        },
    ]
}

/// Every group-0 binding of lit and unlit views; each layout takes a subset.
fn entries() -> Vec<wgpu::BindGroupLayoutEntry> {
    let filterable = wgpu::TextureSampleType::Float { filterable: true };
    uniform_entries()
        .into_iter()
        .chain([wgpu::BindGroupLayoutEntry {
            binding: SHADOW_SAMPLER,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
            count: None,
        }])
        .chain(environment())
        .chain([
            texture(
                LOOKUP_TABLES,
                filterable,
                wgpu::TextureViewDimension::D2Array,
            ),
            storage(LIGHTS),
            storage(CLUSTERS),
            storage(DECALS),
            texture(DECAL_ATLAS, filterable, wgpu::TextureViewDimension::D2),
            wgpu::BindGroupLayoutEntry {
                binding: DECAL_SAMPLER,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
            texture(BAKED, filterable, wgpu::TextureViewDimension::CubeArray),
            storage(COLLECTION),
            texture(
                LOCAL_SHADOW_ATLAS,
                wgpu::TextureSampleType::Depth,
                wgpu::TextureViewDimension::D2Array,
            ),
            storage(LOCAL_SHADOWS),
            texture(
                DIRECTIONAL_SHADOW_MAP,
                wgpu::TextureSampleType::Depth,
                wgpu::TextureViewDimension::D2Array,
            ),
            texture(
                STATIC_LIGHTMAP,
                filterable,
                wgpu::TextureViewDimension::D2Array,
            ),
            texture(
                STATIC_LIGHTMAP_DIRECTION,
                filterable,
                wgpu::TextureViewDimension::D2Array,
            ),
            wgpu::BindGroupLayoutEntry {
                binding: BAKED_SAMPLER,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
            texture(
                STATIC_IRRADIANCE_ATLAS,
                filterable,
                wgpu::TextureViewDimension::D2Array,
            ),
            texture(
                STATIC_DIRECTION_ATLAS,
                filterable,
                wgpu::TextureViewDimension::D2Array,
            ),
            texture(
                DYNAMIC_GI_PROBES,
                filterable,
                wgpu::TextureViewDimension::D2,
            ),
            texture(
                IRRADIANCE_VOLUME,
                filterable,
                wgpu::TextureViewDimension::D3,
            ),
        ])
        .map(|mut entry| {
            entry.visibility |= wgpu::ShaderStages::COMPUTE;
            entry
        })
        .chain(fog())
        .collect()
}

/// The frame's fog volume and its sampler, which draws sample. Fragment
/// stages alone see them, so a compute pipeline over group 0, the fog's own
/// among them, counts neither.
fn fog() -> [wgpu::BindGroupLayoutEntry; 2] {
    [
        texture(
            FOG_VOLUME,
            wgpu::TextureSampleType::Float { filterable: true },
            wgpu::TextureViewDimension::D3,
        ),
        wgpu::BindGroupLayoutEntry {
            binding: FOG_SAMPLER,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            count: None,
        },
    ]
}

/// bind_lit.wgsl's entries, and bind_lit_extended.wgsl's on a device of
/// the `Extended` binding tier (`group0::tier`).
pub(crate) fn lit_entries(tier: BindingTier) -> Vec<wgpu::BindGroupLayoutEntry> {
    entries()
        .into_iter()
        .filter(|entry| entry.binding != BACKDROP_MAP && group0::tier(entry.binding) <= tier)
        .collect()
}

/// bind_unlit.wgsl's entries.
pub(crate) fn unlit_entries() -> Vec<wgpu::BindGroupLayoutEntry> {
    entries()
        .into_iter()
        .filter(|entry| {
            matches!(
                entry.binding,
                VIEW | FRAME
                    | BACKDROP_MAP
                    | ENVIRONMENT_MAP
                    | ENVIRONMENT_SAMPLER
                    | FOG_VOLUME
                    | FOG_SAMPLER
            )
        })
        .collect()
}

fn layout(
    device: &wgpu::Device,
    label: &str,
    entries: &[wgpu::BindGroupLayoutEntry],
) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some(label),
        entries,
    })
}

/// Lit group 0's layout and the binding tier it is of, whose lit provider
/// every program that binds it and composes `SURFACE` composes
/// (`shading::lit_provider`).
pub(crate) struct LitLayout {
    pub layout: wgpu::BindGroupLayout,
    pub tier: BindingTier,
}

/// bind_lit.wgsl, with bind_lit_extended.wgsl on a device of the
/// `Extended` binding tier.
pub(crate) fn lit(device: &wgpu::Device, tier: BindingTier) -> LitLayout {
    LitLayout {
        layout: layout(device, "lit frame", &lit_entries(tier)),
        tier,
    }
}

/// bind_unlit.wgsl.
pub(crate) fn unlit(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    layout(device, "sky and transient frame", &unlit_entries())
}

/// The view and frame uniforms, bind_shadow.wgsl's entries.
pub(crate) fn uniform_entries() -> [wgpu::BindGroupLayoutEntry; 2] {
    [uniform(VIEW), uniform(FRAME)]
}

/// bind_shadow.wgsl.
pub(crate) fn shadow(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    layout(device, "shadow frame", &uniform_entries())
}

/// The view and frame buffers.
pub(crate) fn uniforms<'a>(
    view: &'a wgpu::Buffer,
    frame: &'a wgpu::Buffer,
) -> [wgpu::BindGroupEntry<'a>; 2] {
    [
        wgpu::BindGroupEntry {
            binding: VIEW,
            resource: view.as_entire_binding(),
        },
        wgpu::BindGroupEntry {
            binding: FRAME,
            resource: frame.as_entire_binding(),
        },
    ]
}

/// Group 1, the scene: bind_scene.wgsl's object records (`scene::objects`),
/// then scene_rays.wgsl's source and instance buffers.
pub(crate) fn scene(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    layout(device, "scene objects and rays", &scene_entries())
}

/// Group 1's entries: geometry passes read the object records; ray queries
/// and ray hits, which also run in compute, read the ray buffers and, for a
/// hit's pose, flags and ambient cube, the object records.
pub(crate) fn scene_entries() -> [wgpu::BindGroupLayoutEntry; 3] {
    let storage = |binding, visibility| wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only: true },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    };
    let rays = wgpu::ShaderStages::VERTEX_FRAGMENT | wgpu::ShaderStages::COMPUTE;
    [
        storage(group1::OBJECTS, rays),
        storage(group1::SCENE_SOURCE, rays),
        storage(group1::SCENE_INSTANCES, rays),
    ]
}

/// The GPU-built directional cascades' casters' group 3:
/// bind_caster_positions.wgsl's positions slab (`scene::geometry`), which
/// their vertex stage reads.
pub(crate) fn caster_positions(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    layout(device, "caster positions", &caster_positions_entries())
}

/// Group 3's entries for the GPU-built cascades' casters.
pub(crate) fn caster_positions_entries() -> [wgpu::BindGroupLayoutEntry; 1] {
    [wgpu::BindGroupLayoutEntry {
        binding: caster::POSITIONS,
        visibility: wgpu::ShaderStages::VERTEX,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only: true },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }]
}

/// The blended pipelines' group 3: bind_blended.wgsl's screen-space
/// method's result, surface depth and trace.
pub(crate) fn blended(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    layout(device, "blended reflections", &blended_entries())
}

/// Group 3's entries for the blended pipelines.
pub(crate) fn blended_entries() -> [wgpu::BindGroupLayoutEntry; 3] {
    let texture = |binding, sample_type| wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type,
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    };
    [
        texture(
            blended::REFLECTIONS,
            wgpu::TextureSampleType::Float { filterable: false },
        ),
        texture(blended::SURFACE_DEPTH, wgpu::TextureSampleType::Depth),
        wgpu::BindGroupLayoutEntry {
            binding: blended::TRACE,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        },
    ]
}

/// The lighting pass's group 3 while ray-traced shadows run:
/// bind_shadow_mask.wgsl's mask and slot table.
pub(crate) fn shadow_mask(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    layout(device, "ray-traced shadow mask", &shadow_mask_entries())
}

/// Group 3's entries for the lighting pass while ray-traced shadows run.
pub(crate) fn shadow_mask_entries() -> [wgpu::BindGroupLayoutEntry; 2] {
    [
        wgpu::BindGroupLayoutEntry {
            binding: shadow_mask::MASK,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D2Array,
                multisampled: false,
            },
            count: None,
        },
        wgpu::BindGroupLayoutEntry {
            binding: shadow_mask::SLOTS,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        },
    ]
}

#[cfg(test)]
pub(crate) fn mirrors() -> [crate::shading::layout_tests::Mirror; 1] {
    [crate::shading::layout_tests::mirror!(
        "geometry",
        "BlendedTrace",
        BlendedTrace,
        [cutoff, fade, padding]
    )]
}
