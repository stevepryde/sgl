//! The WGSL modules of group 2's Extended binding tier: its bind module and
//! the two material-map providers, of which a program that composes
//! `SURFACE_RASTER` composes the one of the device's tier.
use super::{BIND_MATERIAL, Module, bind};

/// Group 2's maps of the Extended binding tier alone.
pub(crate) static BIND_MATERIAL_EXTENDED: Module = Module {
    name: "bind_material_extended",
    source: include_str!("bind_material_extended.wgsl"),
    deps: &[&BIND_MATERIAL],
};
/// The material-map provider of the Extended binding tier: samples its
/// maps. A program that composes `SURFACE_RASTER` composes it or
/// `MATERIAL_MAPS_BASIC`, exactly one (`material_maps`).
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
/// The material-map provider a program composes on a device of `tier`.
pub(crate) fn material_maps(tier: bind::BindingTier) -> &'static Module {
    match tier {
        bind::BindingTier::Basic => &MATERIAL_MAPS_BASIC,
        bind::BindingTier::Extended => &MATERIAL_MAPS_EXTENDED,
    }
}
