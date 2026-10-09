//! The WGSL modules of the binding tiers: lit group 0's, group 2's and the
//! blended group 3's bindings of the Extended tier, and each group's two
//! providers, of which a program composes the one of the device's tier
//! (`lit_provider`, `material_provider`, `transmission_provider`).
use super::{BIND_BLENDED, BIND_LIT, BIND_MATERIAL, DYNAMIC_GI_SAMPLE, Module, bind::BindingTier};

/// Lit group 0's bindings of the Extended binding tier alone.
pub(crate) static BIND_LIT_EXTENDED: Module = Module {
    name: "bind_lit_extended",
    source: include_str!("bind_lit_extended.wgsl"),
    deps: &[&BIND_LIT],
};
/// The lit provider of the Extended binding tier: the baked maps'
/// directional lobes and the dynamic GI volume's irradiance.
pub(crate) static LIT_EXTENDED: Module = Module {
    name: "lit_extended",
    source: include_str!("lit_extended.wgsl"),
    deps: &[&BIND_LIT_EXTENDED, &DYNAMIC_GI_SAMPLE],
};
/// The lit provider of the Basic binding tier: no lobe and no dynamic GI.
pub(crate) static LIT_BASIC: Module = Module {
    name: "lit_basic",
    source: include_str!("lit_basic.wgsl"),
    deps: &[],
};
/// The lit provider a program that composes `SURFACE` composes on a device
/// of `tier`.
pub(crate) fn lit_provider(tier: BindingTier) -> &'static Module {
    match tier {
        BindingTier::Basic => &LIT_BASIC,
        BindingTier::Extended => &LIT_EXTENDED,
    }
}

/// Group 2's maps of the Extended binding tier alone.
pub(crate) static BIND_MATERIAL_EXTENDED: Module = Module {
    name: "bind_material_extended",
    source: include_str!("bind_material_extended.wgsl"),
    deps: &[&BIND_MATERIAL],
};
/// The material-map provider of the Extended binding tier: samples its
/// maps.
pub(crate) static MATERIAL_MAPS_EXTENDED: Module = Module {
    name: "material_maps_extended",
    source: include_str!("material_maps_extended.wgsl"),
    deps: &[&BIND_MATERIAL_EXTENDED],
};
/// The material-map provider of the Basic binding tier: each Extended map
/// reads white.
pub(crate) static MATERIAL_MAPS_BASIC: Module = Module {
    name: "material_maps_basic",
    source: include_str!("material_maps_basic.wgsl"),
    deps: &[],
};
/// The material-map provider a program that composes `SURFACE_RASTER`
/// composes on a device of `tier`.
pub(crate) fn material_provider(tier: BindingTier) -> &'static Module {
    match tier {
        BindingTier::Basic => &MATERIAL_MAPS_BASIC,
        BindingTier::Extended => &MATERIAL_MAPS_EXTENDED,
    }
}

/// The blended pipelines' group 3 bindings of the Extended binding tier
/// alone: the transparent stage's copy of the composed frame, the opaque
/// depth and the volume layers.
pub(crate) static BIND_BLENDED_EXTENDED: Module = Module {
    name: "bind_blended_extended",
    source: include_str!("bind_blended_extended.wgsl"),
    deps: &[&BIND_BLENDED],
};
/// The transmission provider of the Extended binding tier: the frame behind
/// a transmissive surface from the copy, bicubically filtered, and the exit
/// volume layer the second exit layer's pass reads.
pub(crate) static TRANSMISSION_EXTENDED: Module = Module {
    name: "transmission_extended",
    source: include_str!("transmission_extended.wgsl"),
    deps: &[&BIND_BLENDED_EXTENDED, &BIND_LIT],
};
/// The transmission provider of the Basic binding tier: no copy, so the
/// blend passes the light behind a transmissive surface.
pub(crate) static TRANSMISSION_BASIC: Module = Module {
    name: "transmission_basic",
    source: include_str!("transmission_basic.wgsl"),
    deps: &[],
};
/// The transmission provider a program that composes `TRANSMISSION`
/// composes on a device of `tier`.
pub(crate) fn transmission_provider(tier: BindingTier) -> &'static Module {
    match tier {
        BindingTier::Basic => &TRANSMISSION_BASIC,
        BindingTier::Extended => &TRANSMISSION_EXTENDED,
    }
}
