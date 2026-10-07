// Group 0 of lit scene geometry: view and frame data, lights, decals,
// shadows, environment, probes, baked diffuse, the irradiance volume's
// cells, lookup tables and the fog volume, on every binding tier; the
// Extended tier's own bindings are bind_lit_extended.wgsl's. Rust layout:
// shading::bind::lit.
@group(0) @binding(0) var<uniform> view:View;
@group(0) @binding(1) var<uniform> frame:Frame;
@group(0) @binding(2) var shadow_sampler:sampler_comparison;
// The frame's environment PMREM atlas.
@group(0) @binding(5) var environment_map:texture_2d_array<f32>;
@group(0) @binding(7) var environment_sampler:sampler;
// The rectangle lights' GGX fit and the DFG table (lookup_tables.wgsl).
@group(0) @binding(22) var lookup_tables:texture_2d_array<f32>;
// The scene's lights, at their identities' indices (Scene::add_light).
@group(0) @binding(10) var<storage,read> lights:array<Light>;
// The view's clusters: which lights and decals reach each part of it.
@group(0) @binding(11) var<storage,read> clusters:Clusters;
// The scene's decals, at their identities' indices (Scene::add_decal), the
// atlas their images are packed in, and the sampler that filters it.
@group(0) @binding(25) var<storage,read> decals:array<Decal>;
@group(0) @binding(23) var decal_atlas:texture_2d<f32>;
@group(0) @binding(24) var decal_sampler:sampler;
// Installed baked specular probes, with their world grid. Probe captures,
// world-space ray hits and the camera's blended surfaces sample them for the
// environment specular of the surfaces they shade (surface.wgsl); the
// camera's opaque surfaces take theirs from source completion, which adds
// it from the G-buffer.
@group(0) @binding(12) var baked:texture_cube_array<f32>;
@group(0) @binding(13) var<storage,read> collection:ProbeCollection;
// The local-light shadow atlas and where each light's shadow is in it.
// The camera binds the frame's atlas; probe captures and ray hits its static
// layers. One layer, so the shared filters take it.
@group(0) @binding(15) var local_shadow_atlas:texture_depth_2d_array;
@group(0) @binding(16) var<storage,read> local_shadows:array<LocalShadow>;
@group(0) @binding(17) var directional_shadow_map:texture_depth_2d_array;
// Baked diffuse (baked_lighting.wgsl): the lightmap and the irradiance
// atlas's front and back layers, each irradiance / PI, and the sampler
// that filters both.
@group(0) @binding(18) var static_lightmap:texture_2d_array<f32>;
@group(0) @binding(20) var baked_sampler:sampler;
@group(0) @binding(21) var static_irradiance_atlas:texture_2d_array<f32>;
// The irradiance volume's cells (irradiance_volume.wgsl), filtered through
// `baked_sampler`; a stand-in in frames FRAME_IRRADIANCE_VOLUME leaves clear.
@group(0) @binding(30) var irradiance_volume:texture_3d<f32>;
// The frame's fog volume (frame_fog.wgsl) and its sampler.
@group(0) @binding(27) var fog_volume:texture_3d<f32>;
@group(0) @binding(28) var fog_sampler:sampler;
