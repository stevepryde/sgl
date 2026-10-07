//! The shared WGSL library: named modules with declared dependencies, composed
//! into programs by one function, and the Rust layouts that mirror them.
//!
//! A library module references the bindings it reads by name (`view`,
//! `frame`, `environment_map`, ...); the program chooses the bind module that
//! declares them (`BIND_LIT`, `BIND_UNLIT`, `BIND_SHADOW`).
//! Every struct, binding and function is declared by exactly one module.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod anisotropy_tests;
pub(crate) mod bind;
pub(crate) mod clusters;
pub(crate) mod culling;
pub(crate) mod decals;
pub(crate) mod deformation;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod diffuse_transmission_tests;
pub(crate) mod dynamic_gi;
pub(crate) mod fog;
pub(crate) mod gbuffer;
#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) mod iridescence_tests;
#[cfg(test)]
pub(crate) mod layout_tests;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod lighting_model_tests;
pub(crate) mod lights;
pub(crate) mod lod;
pub(crate) mod material;
pub(crate) mod tiers;
pub(crate) use tiers::{lit_provider, material_provider, transmission_provider};
// Wired into the ray source and its readers with #135's prepared models.
pub(crate) mod packed_vertex;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod packed_vertex_tests;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod rect_light_tests;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod scene_ray_valid_tests;
mod scene_rays;
pub(crate) use scene_rays::{RayQueryForm, SCENE_RAYS, SCENE_RAYS_PREDICATE, ray_trace_root};
#[cfg(test)]
pub(crate) use scene_rays::{SCENE_RAYS_PORTABLE, SCENE_RAYS_QUERY_OPAQUE};
pub(crate) mod shadow_mask;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod shadow_normal_tests;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod sheen_tests;
pub(crate) mod srgb;
pub(crate) mod uniforms;
pub(crate) mod vertex;

/// One WGSL source file and the modules it uses.
pub(crate) struct Module {
    pub name: &'static str,
    pub source: &'static str,
    pub deps: &'static [&'static Module],
}

/// The dependency-first concatenation of `roots` and everything they use,
/// each module once, with every `enable` directive a module declares
/// hoisted to the program's head, once, where naga alone accepts it.
pub(crate) fn compose(roots: &[&'static Module]) -> String {
    fn visit(module: &'static Module, ordered: &mut Vec<&'static Module>) {
        if let Some(seen) = ordered.iter().find(|seen| seen.name == module.name) {
            debug_assert!(
                std::ptr::eq(*seen, module),
                "two WGSL modules are named {}",
                module.name
            );
            return;
        }
        for dependency in module.deps {
            visit(dependency, ordered);
        }
        ordered.push(module);
    }
    let mut ordered = Vec::new();
    for root in roots {
        visit(root, &mut ordered);
    }
    let is_directive = |line: &&str| line.trim_start().starts_with("enable ");
    let mut program = String::new();
    for module in &ordered {
        for directive in module.source.lines().filter(is_directive) {
            if !program.lines().any(|line| line == directive) {
                program.push_str(directive);
                program.push('\n');
            }
        }
    }
    for module in ordered {
        program.push_str("\n// ---- module ");
        program.push_str(module.name);
        program.push('\n');
        for line in module.source.lines().filter(|line| !is_directive(line)) {
            program.push_str(line);
            program.push('\n');
        }
    }
    program
}

pub(crate) static UNIFORMS: Module = Module {
    name: "uniforms",
    source: include_str!("uniforms.wgsl"),
    deps: &[],
};
pub(crate) static BIND_LIT: Module = Module {
    name: "bind_lit",
    source: include_str!("bind_lit.wgsl"),
    deps: &[
        &UNIFORMS,
        &PROBE_SAMPLING,
        &LIGHT_RECORDS,
        &CLUSTERS,
        &DECAL_RECORDS,
    ],
};
pub(crate) static BIND_UNLIT: Module = Module {
    name: "bind_unlit",
    source: include_str!("bind_unlit.wgsl"),
    deps: &[&UNIFORMS],
};
pub(crate) static BIND_SHADOW: Module = Module {
    name: "bind_shadow",
    source: include_str!("bind_shadow.wgsl"),
    deps: &[&UNIFORMS],
};
pub(crate) static BIND_SCENE: Module = Module {
    name: "bind_scene",
    source: include_str!("bind_scene.wgsl"),
    deps: &[&UNIFORMS, &DRAW_INSTANCE],
};
/// One instance of a scene geometry draw, which geometry passes step
/// through and the cull stage writes.
pub(crate) static DRAW_INSTANCE: Module = Module {
    name: "draw_instance",
    source: include_str!("draw_instance.wgsl"),
    deps: &[],
};
/// The projected error bound of a mesh's alternative.
pub(crate) static LOD: Module = Module {
    name: "lod",
    source: include_str!("lod.wgsl"),
    deps: &[],
};
/// The GPU draw lists' layouts, caps and clip volume test. Reads
/// `cull_view`.
pub(crate) static CULLING: Module = Module {
    name: "culling",
    source: include_str!("culling.wgsl"),
    deps: &[&LOD],
};
pub(crate) static BIND_MATERIAL: Module = Module {
    name: "bind_material",
    source: include_str!("bind_material.wgsl"),
    deps: &[&MATERIAL],
};
/// The GPU-built directional cascades' casters' group 3: a positions slab.
pub(crate) static BIND_CASTER_POSITIONS: Module = Module {
    name: "bind_caster_positions",
    source: include_str!("bind_caster_positions.wgsl"),
    deps: &[],
};
/// The blended pipelines' group 3: the screen-space method's result, the
/// surface depth and the method's cutoff and fade.
pub(crate) static BIND_BLENDED: Module = Module {
    name: "bind_blended",
    source: include_str!("bind_blended.wgsl"),
    deps: &[],
};
/// A material's values and flags, and which texels it cuts out.
pub(crate) static MATERIAL: Module = Module {
    name: "material",
    source: include_str!("material.wgsl"),
    deps: &[],
};
/// A rasterized texel of the bound material. Reads `view` and the material.
pub(crate) static MATERIAL_RASTER: Module = Module {
    name: "material_raster",
    source: include_str!("material_raster.wgsl"),
    deps: &[&BIND_MATERIAL],
};
/// Rec. 709 luminance of linear RGB, for SGL3D's own shaders; ports keep
/// their upstream helpers.
pub(crate) static LUMINANCE: Module = Module {
    name: "luminance",
    source: include_str!("luminance.wgsl"),
    deps: &[],
};
pub(crate) static PBR: Module = Module {
    name: "pbr",
    source: include_str!("pbr.wgsl"),
    deps: &[],
};
/// sRGB decoding (`shading::srgb`).
pub(crate) static SRGB: Module = Module {
    name: "srgb",
    source: include_str!("srgb.wgsl"),
    deps: &[],
};
/// The scene's packed vertex: its words and their decoding
/// (`shading::packed_vertex`).
pub(crate) static PACKED_VERTEX: Module = Module {
    name: "packed_vertex",
    source: include_str!("packed_vertex.wgsl"),
    deps: &[&SRGB],
};
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
pub(crate) static PROBE_SAMPLING: Module = Module {
    name: "probe_sampling",
    source: include_str!("probe_sampling.wgsl"),
    deps: &[],
};
/// A baked specular probe's mips and the roughness each holds.
pub(crate) static SPECULAR_PROBE_LEVELS: Module = Module {
    name: "specular_probe_levels",
    source: include_str!("specular_probe_levels.wgsl"),
    deps: &[],
};
/// Reads `collection` and `baked`.
pub(crate) static PROBE_COLLECTION: Module = Module {
    name: "probe_collection",
    source: include_str!("probe_collection.wgsl"),
    deps: &[&PROBE_SAMPLING, &SPECULAR_PROBE_LEVELS],
};
/// Reads `collection` and `baked`.
pub(crate) static PROBE_GRID: Module = Module {
    name: "probe_grid",
    source: include_str!("probe_grid.wgsl"),
    deps: &[&PROBE_COLLECTION],
};
/// Reads `frame`, `environment_map` and `environment_sampler`.
pub(crate) static ENVIRONMENT: Module = Module {
    name: "environment",
    source: include_str!("environment.wgsl"),
    deps: &[&PROBE_SAMPLING],
};
/// Reads `frame`, the lightmap, the irradiance atlas and `baked_sampler`.
pub(crate) static BAKED_LIGHTING: Module = Module {
    name: "baked_lighting",
    source: include_str!("baked_lighting.wgsl"),
    deps: &[],
};
/// The dynamic GI volume's probes: where each lies and where its maps and
/// data are in the probe texture.
pub(crate) static DYNAMIC_GI: Module = Module {
    name: "dynamic_gi",
    source: include_str!("dynamic_gi.wgsl"),
    deps: &[],
};
/// A volume's share of a receiver by where it lies on its lattice: the
/// dynamic GI volume's and the irradiance volume's.
pub(crate) static VOLUME_SHARE: Module = Module {
    name: "volume_share",
    source: include_str!("volume_share.wgsl"),
    deps: &[],
};
/// The dynamic GI volume's irradiance at a receiver. Reads `frame`,
/// `dynamic_gi_probes` and `baked_sampler`.
pub(crate) static DYNAMIC_GI_SAMPLE: Module = Module {
    name: "dynamic_gi_sample",
    source: include_str!("dynamic_gi_sample.wgsl"),
    deps: &[&DYNAMIC_GI, &VOLUME_SHARE],
};
/// The irradiance volume's light at a receiver. Reads `frame`,
/// `irradiance_volume` and `baked_sampler`.
pub(crate) static IRRADIANCE_VOLUME: Module = Module {
    name: "irradiance_volume",
    source: include_str!("irradiance_volume.wgsl"),
    deps: &[&BAKED_LIGHTING, &VOLUME_SHARE],
};
/// The volumetric fog's froxel volume: its slices, where a point samples it
/// and how it fogs a colour.
pub(crate) static FOG: Module = Module {
    name: "fog",
    source: include_str!("fog.wgsl"),
    deps: &[],
};
/// The frame's fog over a draw's colour. Reads `view`, `frame`,
/// `fog_volume` and `fog_sampler`.
pub(crate) static FRAME_FOG: Module = Module {
    name: "frame_fog",
    source: include_str!("frame_fog.wgsl"),
    deps: &[&UNIFORMS, &FOG],
};
/// Linear view depth under `perspective`'s projection.
pub(crate) static DEPTH: Module = Module {
    name: "depth",
    source: include_str!("depth.wgsl"),
    deps: &[],
};
/// Pseudo-random numbers from a pixel and a frame, where Wicked reads blue
/// noise: its hash33.
pub(crate) static HASH: Module = Module {
    name: "hash",
    source: include_str!("hash.wgsl"),
    deps: &[],
};
/// Where a ray toward a light ends on it: a point of a point or spot light's
/// sphere, of a rectangle's face, or a direction within a directional
/// light's disc. Reads nothing.
pub(crate) static LIGHT_SURFACE: Module = Module {
    name: "light_surface",
    source: include_str!("light_surface.wgsl"),
    deps: &[&LIGHT_RECORDS],
};
/// Interleaved gradient noise.
pub(crate) static NOISE: Module = Module {
    name: "noise",
    source: include_str!("noise.wgsl"),
    deps: &[],
};
/// The shadow-map filters and receiver bias every 2D shadow kind shares.
/// Reads `frame`.
pub(crate) static SHADOW_SAMPLING: Module = Module {
    name: "shadow_sampling",
    source: include_str!("shadow_sampling.wgsl"),
    deps: &[&NOISE],
};
/// The directional shadow's cascades. Reads `view`, `frame`,
/// `directional_shadow_map` and `shadow_sampler`, and calls the program's
/// mask provider (`camera_shadow_mask`).
pub(crate) static DIRECTIONAL_SHADOW: Module = Module {
    name: "directional_shadow",
    source: include_str!("directional_shadow.wgsl"),
    deps: &[&SHADOW_SAMPLING, &SHADOW_MASK_SLOTS],
};
/// The ray-traced shadow stage's slot table and its mask's layout
/// (`shading::shadow_mask`).
pub(crate) static SHADOW_MASK_SLOTS: Module = Module {
    name: "shadow_mask_slots",
    source: include_str!("shadow_mask_slots.wgsl"),
    deps: &[],
};
/// The key a slot of the slot table holds. Reads `shadow_mask_slots`.
pub(crate) static SHADOW_MASK_SLOT_KEY: Module = Module {
    name: "shadow_mask_slot_key",
    source: include_str!("shadow_mask_slot_key.wgsl"),
    deps: &[&SHADOW_MASK_SLOTS],
};
/// The opaque stage's lighting pass's group 3 while ray-traced shadows run:
/// the shadow mask and the slot table.
pub(crate) static BIND_SHADOW_MASK: Module = Module {
    name: "bind_shadow_mask",
    source: include_str!("bind_shadow_mask.wgsl"),
    deps: &[&SHADOW_MASK_SLOTS],
};
/// The mask provider of the opaque stage's lighting pass while ray-traced
/// shadows run: `camera_shadow_mask` reads the mask. It depends on what
/// calls it (`LIGHTS`, `DIRECTIONAL_SHADOW`), never they on it, and a
/// program composes it or `SHADOW_MASK_NONE`, exactly one.
pub(crate) static SHADOW_MASK: Module = Module {
    name: "shadow_mask",
    source: include_str!("shadow_mask.wgsl"),
    deps: &[
        &LIGHTS,
        &DIRECTIONAL_SHADOW,
        &BIND_SHADOW_MASK,
        &SHADOW_MASK_SLOT_KEY,
    ],
};
/// The mask provider of every other lit program: `camera_shadow_mask`
/// holds no slot, so every light takes the maps.
pub(crate) static SHADOW_MASK_NONE: Module = Module {
    name: "shadow_mask_none",
    source: include_str!("shadow_mask_none.wgsl"),
    deps: &[&LIGHTS, &DIRECTIONAL_SHADOW],
};
/// A light as it reaches a receiver point: what each light builds and
/// `surface_direct_light` shades.
pub(crate) static LIGHT_SAMPLE: Module = Module {
    name: "light_sample",
    source: include_str!("light_sample.wgsl"),
    deps: &[],
};
/// A rectangle light's integral over its face. Reads `lookup_tables` and
/// `environment_sampler`.
pub(crate) static RECT_LIGHT: Module = Module {
    name: "rect_light",
    source: include_str!("rect_light.wgsl"),
    deps: &[&LOOKUP_TABLES],
};
/// The scene-light record and a light's shadow record.
pub(crate) static LIGHT_RECORDS: Module = Module {
    name: "light_records",
    source: include_str!("light_records.wgsl"),
    deps: &[],
};
/// A view's clusters and the lookup of the one that holds a point. Reads
/// `view` and `clusters`.
pub(crate) static CLUSTERS: Module = Module {
    name: "clusters",
    source: include_str!("clusters.wgsl"),
    deps: &[],
};
/// The decal record.
pub(crate) static DECAL_RECORDS: Module = Module {
    name: "decal_records",
    source: include_str!("decal_records.wgsl"),
    deps: &[],
};
/// The decals that change a surface before it is lit. Reads `clusters`,
/// `decals`, `decal_atlas` and `decal_sampler`.
pub(crate) static DECALS: Module = Module {
    name: "decals",
    source: include_str!("decals.wgsl"),
    deps: &[&CLUSTERS, &DECAL_RECORDS],
};
/// A scene light's shadow in the local-light shadow atlas. Reads `frame`,
/// `local_shadows`, `local_shadow_atlas` and `shadow_sampler`.
pub(crate) static LOCAL_SHADOW: Module = Module {
    name: "local_shadow",
    source: include_str!("local_shadow.wgsl"),
    deps: &[&LIGHT_RECORDS, &SHADOW_SAMPLING],
};
/// How a scene light reaches a receiver point: its range, distance and
/// cone, and a rectangle's half-space.
pub(crate) static LIGHT_REACH: Module = Module {
    name: "light_reach",
    source: include_str!("light_reach.wgsl"),
    deps: &[&LIGHT_RECORDS],
};
/// The scene lights that reach a point, each as a `LightSample`. Reads
/// `view`, `lights`, `clusters` and the local-light shadows, and calls the
/// program's mask provider (`camera_shadow_mask`).
pub(crate) static LIGHTS: Module = Module {
    name: "lights",
    source: include_str!("lights.wgsl"),
    deps: &[
        &LIGHT_RECORDS,
        &LIGHT_REACH,
        &CLUSTERS,
        &LIGHT_SAMPLE,
        &LOCAL_SHADOW,
        &SHADOW_MASK_SLOTS,
    ],
};
/// Reads `view` and the drawn instances' object records.
pub(crate) static VERTEX: Module = Module {
    name: "vertex",
    source: include_str!("vertex.wgsl"),
    deps: &[&BIND_SCENE],
};
/// The G-buffer's targets: their encode and decode functions. Its traced
/// lobe is the one `SPECULAR_LOBES` selects.
pub(crate) static GBUFFER: Module = Module {
    name: "gbuffer",
    source: include_str!("gbuffer.wgsl"),
    deps: &[&SPECULAR_LOBES, &LUMINANCE],
};
/// The full-screen triangle: each vertex's corner and clip position, for a
/// vertex entry point of a pass's own.
pub(crate) static FULLSCREEN: Module = Module {
    name: "fullscreen",
    source: include_str!("fullscreen.wgsl"),
    deps: &[],
};
/// The full-screen triangle's vertex entry point, which a pipeline is
/// created with as `FULLSCREEN_VS_ENTRY`.
pub(crate) static FULLSCREEN_VS: Module = Module {
    name: "fullscreen_vs",
    source: include_str!("fullscreen_vs.wgsl"),
    deps: &[&FULLSCREEN],
};
pub(crate) const FULLSCREEN_VS_ENTRY: &str = "fullscreen_vs";
/// The scene source's layout: its records' words.
pub(crate) static SCENE_SOURCE: Module = Module {
    name: "scene_source",
    source: include_str!("scene_source.wgsl"),
    deps: &[],
};
/// Deformation's records in the scene source and the deform stage's
/// dispatch.
pub(crate) static DEFORMATION: Module = Module {
    name: "deformation",
    source: include_str!("deformation.wgsl"),
    deps: &[],
};
/// One texel of a BC7 block, as the GPU decodes it.
pub(crate) static BC7: Module = Module {
    name: "bc7",
    source: include_str!("bc7.wgsl"),
    deps: &[],
};
/// A scene vertex pulled from the scene source, as an instance shows it:
/// deformed when it deforms. Reads `object`.
pub(crate) static VERTEX_PULL: Module = Module {
    name: "vertex_pull",
    source: include_str!("vertex_pull.wgsl"),
    deps: &[&BIND_SCENE, &SCENE_RAYS, &DEFORMATION],
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
    deps: &[&VERTEX, &PBR, &ANISOTROPY, &MATERIAL_RASTER, &SURFACE],
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
    ],
};

/// The lit shading library with scene ray queries, for compute fixtures on
/// a device of the Extended binding tier: group 0 is the lit layout, group
/// 1 the scene's object records and ray buffers. A fixture appends its entry
/// point and its own bindings at group 3.
#[cfg(test)]
pub(crate) fn lit_compute_library() -> String {
    compose(&[
        &BIND_LIT,
        &SURFACE_RAY,
        &SHADOW_MASK_NONE,
        &SCENE_RAYS_PORTABLE,
        &tiers::LIT_EXTENDED,
    ])
}
