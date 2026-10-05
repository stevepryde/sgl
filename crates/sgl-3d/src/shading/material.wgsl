// One normal layer (MATERIAL_NORMAL_LAYERS; content::material::NormalLayer):
// the material's normal map drawn `scale` times per unit of its UVs, moving
// `cycles` whole repeats of the map along U and V each animation period, its
// slopes taken at `strength` with the material's normal_scale. Rust mirror:
// shading::material::NormalLayerUniform.
struct NormalLayer {
 cycles:vec2<f32>,
 scale:f32,
 strength:f32,
}
// A material's values as the GPU reads them: group 2's uniform
// (bind_material.wgsl) and the first words of its record in the scene's ray
// source (scene_rays.wgsl). Rust mirror: shading::material::MaterialUniform,
// packed from the typed SurfaceMaterial. Flags are MATERIAL_* bits.
struct Material {
 base:vec4<f32>,
 emission:vec3<f32>,
 environment_scale:f32,
 metallic:f32,
 roughness:f32,
 coat:f32,
 coat_roughness:f32,
 normal_scale:f32,
 bump_scale:f32,
 anisotropy_strength:f32,
 anisotropy_rotation:f32,
 alpha_cutoff:f32,
 visibility_group:u32,
 flags:u32,
 normal_layers:array<NormalLayer,2>,
}
const MATERIAL_UNLIT:u32=1u;
const MATERIAL_DOUBLE_SIDED:u32=2u;
// The maps it was added with: a normal map, a bump map (used only without a
// normal map) and an anisotropy direction and strength map.
const MATERIAL_NORMAL_MAP:u32=4u;
const MATERIAL_BUMP_MAP:u32=8u;
const MATERIAL_ANISOTROPY_MAP:u32=16u;
// Its alpha mode: masked or blended; neither is opaque. A blended one may
// receive the frame's screen-space reflections (AlphaMode::Blend).
const MATERIAL_ALPHA_MASK:u32=32u;
const MATERIAL_ALPHA_BLEND:u32=64u;
const MATERIAL_RECEIVES_SCREEN_SPACE_REFLECTIONS:u32=128u;
// It draws its normal map as two scrolling layers (normal_layers).
const MATERIAL_NORMAL_LAYERS:u32=256u;
// Global illumination gathers the light it gives off itself, its emission
// and an unlit material's whole colour (SurfaceMaterial::emits_into_gi).
const MATERIAL_EMITS_INTO_GI:u32=512u;
// The tangent-space normal of material `m`'s normal map texel `texel`: its X
// and Y scaled by normal_scale, as glTF's normalTexture.scale scales them.
fn material_mapped_normal(m:Material,texel:vec4<f32>)->vec3<f32> {
 let mapped=texel.xyz*2.-vec3(1.);
 return vec3(mapped.xy*m.normal_scale,mapped.z);
}
// Where normal layer `layer` samples its material's normal map at material
// UV `uv` at the frame's animation phase `phase` (Frame.animation_phase):
// the map at the layer's scale, moved back by the whole repeats it travels
// each period times the phase, so its pattern moves along its velocity.
// Wicked Engine 4323a33 offsets its water's two normal map samples by the
// material's texture animation (shaders/objectHF.hlsli, SHADERTYPE_WATER),
// and Bevy 9d12036's water example each octave's UV by its velocity times
// the time (assets/shaders/water_material.wesl); MIT, see LICENSE-wicked.txt
// and LICENSE-bevy.txt.
fn material_normal_layer_uv(layer:NormalLayer,uv:vec2<f32>,phase:f32)->vec2<f32> {
 return uv*layer.scale-fract(layer.cycles*phase);
}
// The tangent-space normal of material `m`'s two normal layers, from their
// map texels `first` and `second`: the height fields they draw added, so
// their slopes add, each scaled by its layer's strength and normal_scale.
// Barré-Brisebois and Hill's partial derivative blend ("Blending in Detail",
// 2012), unnormalised: Wicked Engine's water adds its two samples' X and Y
// over a unit Z, which agrees for shallow slopes.
fn material_layered_normal(m:Material,first:vec4<f32>,second:vec4<f32>)->vec3<f32> {
 let a=first.xyz*2.-vec3(1.);
 let b=second.xyz*2.-vec3(1.);
 let a_slope=a.xy*m.normal_scale*m.normal_layers[0].strength;
 let b_slope=b.xy*m.normal_scale*m.normal_layers[1].strength;
 return vec3(a_slope*b.z+b_slope*a.z,a.z*b.z);
}
// Whether material `m` cuts out a texel of base alpha `alpha`: a masked
// material below its cutoff. Bevy 9d12036's alpha_discard
// (crates/bevy_pbr/src/render/pbr_functions.wesl), MIT OR Apache-2.0
// (src/LICENSE-bevy.txt); its shadow casters and prepass cut out the same
// texels (prepass_alpha_discard in pbr_prepass_functions.wesl).
fn material_cut_out(m:Material,alpha:f32)->bool {
 return (m.flags&MATERIAL_ALPHA_MASK)!=0u && alpha<m.alpha_cutoff;
}
