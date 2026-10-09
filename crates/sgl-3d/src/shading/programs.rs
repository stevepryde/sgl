//! The programs that rasterise scene geometry: the geometry program, whose
//! entry points every camera and probe-capture pass is created with, and the
//! caster program of every shadow view, with the entry points their
//! pipelines name. `view::pipelines` creates its pipelines from them; they
//! are the shading layer's so that the scene can compose and validate the
//! same programs (specs/sgl3d-architecture.md, Shader contract).
use super::bind::BindingTier;
use super::{Module, compose};

/// Scene geometry's camera and probe-capture passes. A program composes it
/// with one shadow-mask provider, one lit provider, one material-map
/// provider and one transmission provider (`geometry_program`).
pub(crate) static GEOMETRY: Module = Module {
    name: "geometry",
    source: include_str!("geometry.wgsl"),
    deps: &[
        &super::BIND_LIT,
        &super::BIND_SCENE,
        &super::BIND_MATERIAL,
        &super::BIND_BLENDED,
        &super::GBUFFER,
        &super::VERTEX,
        &super::VERTEX_PULL,
        &super::SURFACE,
        &super::SURFACE_RASTER,
        &super::FRAME_FOG,
        &super::TRANSMISSION,
    ],
};
/// The entry points the geometry passes' pipelines are created with, from
/// either geometry program: the vertex all share and each pass's fragment.
pub(crate) const SOURCE_VS_ENTRY: &str = "source_vs";
pub(crate) const FS_ENTRY: &str = "fs";
pub(crate) const STABLE_FS_ENTRY: &str = "stable_fs";
pub(crate) const STABLE_LEGACY_FS_ENTRY: &str = "stable_legacy_fs";
pub(crate) const ANISOTROPY_FS_ENTRY: &str = "anisotropy_fs";
pub(crate) const SOURCE_FS_ENTRY: &str = "source_fs";
pub(crate) const FUSED_OPAQUE_FS_ENTRY: &str = "fused_opaque_fs";
pub(crate) const BLENDED_FS_ENTRY: &str = "blended_fs";
pub(crate) const BLENDED_FSR2_MASKED_FS_ENTRY: &str = "blended_fsr2_masked_fs";
pub(crate) const RECEIVER_FS_ENTRY: &str = "receiver_fs";
pub(crate) const FSR2_COMPOSITION_FS_ENTRY: &str = "fsr2_composition_fs";

/// The geometry program on a device of `tier`: `GEOMETRY` with the shadow
/// mask's provider where `shadow_mask`, else with the provider that holds
/// no slot, and the tier's lit, material-map and transmission providers.
pub(crate) fn geometry_program(shadow_mask: bool, tier: BindingTier) -> String {
    let provider = if shadow_mask {
        &super::SHADOW_MASK
    } else {
        &super::SHADOW_MASK_NONE
    };
    compose(&[
        &GEOMETRY,
        provider,
        super::lit_provider(tier),
        super::material_provider(tier),
        super::transmission_provider(tier),
    ])
}

/// The directional and local-light shadow casters.
pub(crate) static CASTER: Module = Module {
    name: "caster",
    source: include_str!("caster.wgsl"),
    deps: &[
        &super::BIND_SHADOW,
        &super::BIND_SCENE,
        &super::SCENE_RAYS,
        &super::VERTEX_PULL,
        &super::MATERIAL_RASTER,
        &super::BIND_CASTER_POSITIONS,
    ],
};
/// The entry points the casters' pipelines are created with: a vertex for
/// each way a caster's vertices arrive and its depth is clipped, and the
/// fragments that clamp depth or cut out masked texels.
pub(crate) const SHADOW_VS_ENTRY: &str = "shadow_vs";
pub(crate) const SHADOW_UNCLIPPED_VS_ENTRY: &str = "shadow_unclipped_vs";
pub(crate) const SHADOW_MASKED_VS_ENTRY: &str = "shadow_masked_vs";
pub(crate) const SHADOW_MASKED_UNCLIPPED_VS_ENTRY: &str = "shadow_masked_unclipped_vs";
pub(crate) const SHADOW_PULLED_VS_ENTRY: &str = "shadow_pulled_vs";
pub(crate) const SHADOW_PULLED_UNCLIPPED_VS_ENTRY: &str = "shadow_pulled_unclipped_vs";
pub(crate) const SHADOW_PULLED_MASKED_VS_ENTRY: &str = "shadow_pulled_masked_vs";
pub(crate) const SHADOW_PULLED_MASKED_UNCLIPPED_VS_ENTRY: &str =
    "shadow_pulled_masked_unclipped_vs";
pub(crate) const SHADOW_PAIRED_VS_ENTRY: &str = "shadow_paired_vs";
pub(crate) const SHADOW_PAIRED_UNCLIPPED_VS_ENTRY: &str = "shadow_paired_unclipped_vs";
pub(crate) const SHADOW_UNCLIPPED_FS_ENTRY: &str = "shadow_unclipped_fs";
pub(crate) const SHADOW_MASKED_FS_ENTRY: &str = "shadow_masked_fs";
pub(crate) const SHADOW_MASKED_UNCLIPPED_FS_ENTRY: &str = "shadow_masked_unclipped_fs";

/// The caster program.
pub(crate) fn caster_program() -> String {
    compose(&[&CASTER])
}
