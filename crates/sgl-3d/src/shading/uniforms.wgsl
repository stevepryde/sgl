// View and frame data (group 0 bindings 0 and 1) and the per-instance object
// record (group 1 binding 0). Rust mirrors: shading/uniforms.rs; the layout test
// compares the two. Colours are linear RGB, lengths metres and angles radians
// about +Y.
// One rendered view: the camera, a probe-capture face or a shadow face.
struct View {
 view:mat4x4<f32>,
 // Unjittered.
 projection:mat4x4<f32>,
 // What raster uses: the jittered projection times the view.
 view_projection:mat4x4<f32>,
 inverse_view_projection:mat4x4<f32>,
 // Unjittered, for motion.
 stable_view_projection:mat4x4<f32>,
 previous_view_projection:mat4x4<f32>,
 eye:vec3<f32>,
 // Material texture mip bias of the antialiasing in effect.
 mip_bias:f32,
 // Half the NDC jitter: raster offsets clip xy by 2 jitter w.
 jitter:vec2<f32>,
 viewport:vec2<f32>,
 flags:u32,
}
const VIEW_PROBE_CAPTURE:u32=1u;
// A light at infinity (FrameInput::directional_lights).
struct DirectionalLight {
 // From a receiver toward the light; any nonzero length.
 direction_to_light:vec3<f32>,
 flags:u32,
 color:vec3<f32>,
 // Zero for no light.
 illuminance:f32,
 // Godot's volumetric_fog_energy: the scale of its light in the fog.
 fog_energy:f32,
 // Godot's shadow_opacity: how dark its shadow is, 0 to 1
 // (shadow_sampling.wgsl's shadow_opacity_visibility).
 shadow_opacity:f32,
 // The tangent of half its angular diameter (DirectionalLight's
 // angular_diameter): the radius of its disc at unit distance, which sizes
 // its highlights (pbr_sized_light) and its rays.
 disc_radius:f32,
}
// DirectionalLight.flags: the light has the frame's shadow cascades.
const DIRECTIONAL_LIGHT_SHADOW:u32=1u;
// One cascade of the directional shadow (Bevy's DirectionalCascade), drawn
// into its layer of directional_shadow_map.
struct ShadowCascade {
 // Reversed-Z orthographic view-projection.
 clip_from_world:mat4x4<f32>,
 // World metres per shadow-map texel.
 texel_size:f32,
 // The camera's view depth where the cascade ends; a probe capture's cascade
 // ends this far from its centre along each axis.
 far_bound:f32,
}
// The most shadow cascades a frame holds (view::cascades::MAX_SHADOW_CASCADES).
const FRAME_SHADOW_CASCADES:u32=4u;
// What every view of one frame shares.
struct Frame {
 directional_lights:array<DirectionalLight,2>,
 // The shadowed light's cascades, nearest first: the first
 // shadow_cascade_count are in use.
 shadow_cascades:array<ShadowCascade,FRAME_SHADOW_CASCADES>,
 // Three.js's HemisphereLight: irradiance facing up and facing down.
 hemisphere_sky_color:vec3<f32>,
 hemisphere_intensity:f32,
 hemisphere_ground_color:vec3<f32>,
 // The environment map's diffuse lighting: its yaw and radiance scale.
 // Specular takes reflection_yaw and reflection_intensity.
 diffuse_environment_yaw:f32,
 // The backdrop with FRAME_BACKDROP_COLOR.
 backdrop_color:vec3<f32>,
 diffuse_environment_intensity:f32,
 // The mist's colour where its noise is thin and dense, and the densest
 // noise's opacity.
 mist_thin_color:vec3<f32>,
 mist_opacity:f32,
 mist_dense_color:vec3<f32>,
 // The panorama backdrop's yaw and radiance scale.
 backdrop_yaw:f32,
 // Each mist billboard's width and height, and how far its noise moves
 // across it each second (Mist::drift).
 mist_size:vec2<f32>,
 mist_drift:vec2<f32>,
 backdrop_brightness:f32,
 // With FRAME_FOG: one over the fog volume's length and over its detail
 // spread (fog.wgsl).
 fog_inverse_length:f32,
 fog_inverse_detail_spread:f32,
 reflection_yaw:f32,
 reflection_intensity:f32,
 elapsed_seconds:f32,
 // Multiplies the irradiance atlas (scene::static_lighting).
 fixed_irradiance_scale:f32,
 visibility_mask:u32,
 flags:u32,
 shadow_cascade_count:u32,
 // Frames since history restarted: the temporal shadow filter's noise
 // turns with it.
 frame_count:u32,
 // The frame's time within the period over which material animation
 // repeats, as a fraction of it (shading::material::animation_phase): a
 // normal layer moves its whole repeats per period times it (material.wgsl).
 animation_phase:f32,
 // The lightmap's chart transform: chart UV = material UV * xy + zw.
 lightmap_chart:vec4<f32>,
 // With FRAME_DYNAMIC_GI, the scene's dynamic GI volume (dynamic_gi.wgsl):
 // its first probe's position, the spacing of its probes, their count
 // along each axis, and its scroll, where they are stored
 // (ddgi_probe_stored).
 dynamic_gi_origin:vec3<f32>,
 dynamic_gi_spacing:vec3<f32>,
 dynamic_gi_probes:vec3<u32>,
 dynamic_gi_scroll:vec3<u32>,
 // With FRAME_IRRADIANCE_VOLUME, the scene's irradiance volume
 // (irradiance_volume.wgsl): its first cell's least corner, the size of its
 // cells and their count along each axis.
 irradiance_volume_origin:vec3<f32>,
 irradiance_volume_cell_size:vec3<f32>,
 irradiance_volume_cells:vec3<u32>,
}
// The frame's volumetric fog ran: draws fog themselves from its volume.
const FRAME_FOG:u32=1u;
const FRAME_BAKED_LIGHTING:u32=2u;
// An irradiance atlas is installed (Scene::set_static_irradiance_atlas).
const FRAME_IRRADIANCE_ATLAS:u32=4u;
const FRAME_BACKDROP_COLOR:u32=8u;
// The camera's shadows take the temporal filter, for TAA or FSR2 to resolve.
const FRAME_TEMPORAL_SHADOW_FILTER:u32=16u;
// The camera's shadows take one hardware 2x2 tap (the Low shadow quality).
const FRAME_HARDWARE_SHADOW_FILTER:u32=32u;
// The dynamic GI volume lights the frame: the scene holds one and
// Settings::dynamic_gi runs it, and group 0 binds its probes.
const FRAME_DYNAMIC_GI:u32=64u;
// The irradiance volume lights the frame: the scene holds one and baked
// lighting is on, and group 0 binds its cells.
const FRAME_IRRADIANCE_VOLUME:u32=128u;
// One instance's record, at its index in the scene's object buffer. That
// index plus one is the source identity the G-buffer stores.
struct Object {
 model:mat4x4<f32>,
 previous_model:mat4x4<f32>,
 baked_irradiance:array<vec4<f32>,6>,
 flags:u32,
 // A deforming instance's vertices in the scene source (deformation.wgsl):
 // its positions this frame and in the last submitted frame, and its
 // normals and tangents. Zero for an instance that does not deform.
 deformed_positions:u32,
 previous_positions:u32,
 deformed_normals:u32,
}
// Object.flags: a static instance (a moving one has the bit clear); the
// main camera draws it (InstanceState::visible); the other views show it
// (InstanceState::capture_visible); it deforms.
const OBJECT_STATIC:u32=1u;
const OBJECT_VISIBLE:u32=2u;
const OBJECT_CAPTURE_VISIBLE:u32=4u;
const OBJECT_DEFORMING:u32=8u;
