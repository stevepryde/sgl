//! The surface shading library's modules: the lighting model's lobes and
//! tables, the one `Surface` and its shading, and the builders that evaluate
//! a rasterized fragment's or a ray hit's (specs/sgl3d-architecture.md,
//! Surface shading). `shading` re-exports each.
use super::*;

pub(crate) static ANISOTROPY: Module = Module {
    name: "anisotropy",
    source: include_str!("anisotropy.wgsl"),
    deps: &[],
};
pub(crate) static IRIDESCENCE: Module = Module {
    name: "iridescence",
    source: include_str!("iridescence.wgsl"),
    deps: &[],
};
pub(crate) static SHEEN: Module = Module {
    name: "sheen",
    source: include_str!("sheen.wgsl"),
    deps: &[],
};
/// Lit group 0's lookup tables: their layers and the DFG table's lookup.
pub(crate) static LOOKUP_TABLES: Module = Module {
    name: "lookup_tables",
    source: include_str!("lookup_tables.wgsl"),
    deps: &[],
};
/// Reads `lookup_tables` and `environment_sampler`.
pub(crate) static DFG: Module = Module {
    name: "dfg",
    source: include_str!("dfg.wgsl"),
    deps: &[&LOOKUP_TABLES],
};
/// A surface's specular lobes, the lobe a screen-space method traces and
/// the formula that composes its result: one owner for source completion,
/// composition and lit shading.
pub(crate) static SPECULAR_LOBES: Module = Module {
    name: "specular_lobes",
    source: include_str!("specular_lobes.wgsl"),
    deps: &[&PBR, &ANISOTROPY, &LOOKUP_TABLES],
};
/// Ambient occlusion of a receiver's ambient light and environment
/// specular, by the lesser of its material's occlusion and the frame's
/// ambient occlusion: one owner for source completion, composition and lit
/// shading.
pub(crate) static OCCLUSION: Module = Module {
    name: "occlusion",
    source: include_str!("occlusion.wgsl"),
    deps: &[&SPECULAR_LOBES],
};
/// One `Surface` and its shading for every view. Reads the lit bindings, and
/// the Extended tier's through the lit provider the program composes
/// (`lit_provider`).
pub(crate) static SURFACE: Module = Module {
    name: "surface",
    source: include_str!("surface.wgsl"),
    deps: &[
        &ANISOTROPY,
        &IRIDESCENCE,
        &SHEEN,
        &PBR,
        &DFG,
        &SPECULAR_LOBES,
        &OCCLUSION,
        &LIGHT_SAMPLE,
        &RECT_LIGHT,
        &ENVIRONMENT,
        &PROBE_GRID,
        &BAKED_LIGHTING,
        &IRRADIANCE_VOLUME,
        &DIRECTIONAL_SHADOW,
        &LIGHTS,
        &DECALS,
    ],
};
/// Light transmitted through a surface from the frame behind it, across its
/// volume and spread by dispersion (three.js r185's getIBLVolumeRefraction),
/// from the transmission provider the program composes
/// (`transmission_provider`). Reads `view`, the object records and the DFG
/// table.
pub(crate) static TRANSMISSION: Module = Module {
    name: "transmission",
    source: include_str!("transmission.wgsl"),
    deps: &[&VERTEX, &PBR, &DFG, &SPECULAR_LOBES],
};
/// A rasterized fragment's `Surface`. Reads `view`, `object` and the
/// material, and the Extended tier's maps through the material-map provider
/// the program composes (`material_provider`).
pub(crate) static SURFACE_RASTER: Module = Module {
    name: "surface_raster",
    source: include_str!("surface_raster.wgsl"),
    deps: &[
        &VERTEX,
        &PBR,
        &ANISOTROPY,
        &MATERIAL_RASTER,
        &SURFACE,
        &MATERIAL_SHADER,
    ],
};
/// A ray hit's `Surface` and its shading, a dynamic GI probe ray's hit's
/// light with its visibility ray among it, which goes through the ray
/// function set of the root its pipeline composes (`ray_trace_root`).
/// Reads the lit bindings and the scene's ray buffers.
pub(crate) static SURFACE_RAY: Module = Module {
    name: "surface_ray",
    source: include_str!("surface_ray.wgsl"),
    deps: &[
        &SCENE_RAYS,
        &SCENE_RAYS_PREDICATE,
        &ANISOTROPY,
        &BAKED_LIGHTING,
        &SURFACE,
        &LIGHT_SURFACE,
        &VERTEX,
    ],
};
