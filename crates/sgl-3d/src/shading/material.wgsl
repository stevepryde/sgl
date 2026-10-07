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
// packed from the typed SurfaceMaterial. Flags are MATERIAL_* bits, and
// its maps in effect MATERIAL_MAP_* bits in a word of their own.
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
 // glTF's occlusionTexture.strength, with MATERIAL_MAP_OCCLUSION.
 occlusion_strength:f32,
 // KHR_materials_ior's F0 times KHR_materials_specular's specular colour,
 // and its specular strength (material_dielectric_f0).
 specular_f0:vec3<f32>,
 specular:f32,
 normal_layers:array<NormalLayer,2>,
 maps:u32,
 // glTF's clearcoatNormalTexture.scale, with MATERIAL_MAP_COAT_NORMAL.
 coat_normal_scale:f32,
 // KHR_materials_iridescence's film: its strength (0 none), IOR and
 // thinnest and thickest thickness in nanometres.
 iridescence:f32,
 iridescence_ior:f32,
 iridescence_thickness:vec2<f32>,
}
const MATERIAL_UNLIT:u32=1u;
const MATERIAL_DOUBLE_SIDED:u32=2u;
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
// Its maps in effect (shading::bind::group2::MaterialMap::bit): those it
// was added with whose binding the device binds, a bump map only without a
// normal map, and an occlusion map in the red channel of its
// metallic-roughness map (ORM packing). A map whose white texel is not its
// neutral (the normal, bump and clearcoat normal maps, the anisotropy
// direction) is read only with its bit, on the raster and ray paths alike;
// every other map's white texel leaves its factor alone.
const MATERIAL_MAP_BASE:u32=1u;
const MATERIAL_MAP_METALLIC_ROUGHNESS:u32=2u;
const MATERIAL_MAP_OCCLUSION:u32=4u;
const MATERIAL_MAP_EMISSION:u32=8u;
const MATERIAL_MAP_NORMAL:u32=16u;
const MATERIAL_MAP_BUMP:u32=32u;
const MATERIAL_MAP_ANISOTROPY:u32=64u;
const MATERIAL_MAP_CLEARCOAT:u32=128u;
const MATERIAL_MAP_COAT_ROUGHNESS:u32=256u;
const MATERIAL_MAP_COAT_NORMAL:u32=512u;
const MATERIAL_MAP_IRIDESCENCE:u32=1024u;
const MATERIAL_MAP_IRIDESCENCE_THICKNESS:u32=2048u;
// The share of ambient light that reaches a texel of material `m` whose
// metallic-roughness map reads `mr`: with MATERIAL_MAP_OCCLUSION its red
// channel at occlusion_strength, as glTF 2.0 applies occlusionTexture
// (lerp(1, occlusion, strength)); else all of it.
fn material_occlusion(m:Material,mr:vec4<f32>)->f32 {
 if (m.maps&MATERIAL_MAP_OCCLUSION)==0u {
  return 1.;
 }
 return mix(1.,mr.r,m.occlusion_strength);
}
// Material `m`'s dielectric reflectance at normal incidence: the IOR's F0
// tinted by its specular colour, at most 1, times its specular strength, as
// KHR_materials_specular defines it and three.js 0.185.1 takes it
// (MeshPhysicalNodeMaterial.setupSpecular's specularColor).
fn material_dielectric_f0(m:Material)->vec3<f32> {
 return min(m.specular_f0,vec3(1.))*m.specular;
}
// The tangent-space normal of material `m`'s normal map texel `texel`: its X
// and Y scaled by normal_scale, as glTF's normalTexture.scale scales them.
fn material_mapped_normal(m:Material,texel:vec4<f32>)->vec3<f32> {
 let mapped=texel.xyz*2.-vec3(1.);
 return vec3(mapped.xy*m.normal_scale,mapped.z);
}
// Material `m`'s clearcoat at a texel whose clearcoat map reads `texel`
// (white without one): its factor times the red channel, as
// KHR_materials_clearcoat defines clearcoatTexture and the Khronos glTF
// Sample Renderer 0686eb2 reads it (source/Renderer/shaders/
// material_info.glsl 379–382).
fn material_coat(m:Material,texel:vec4<f32>)->f32 {
 return m.coat*texel.r;
}
// Material `m`'s clearcoat perceptual roughness at a texel whose clearcoat
// roughness map reads `texel`: its factor times the green channel
// (clearcoatRoughnessTexture; material_info.glsl 384–387, and three.js
// 2431a09's lights_physical_fragment, which reads .y).
fn material_coat_roughness(m:Material,texel:vec4<f32>)->f32 {
 return m.coat_roughness*texel.g;
}
// The tangent-space normal of material `m`'s clearcoat normal map texel
// `texel`: its X and Y scaled by coat_normal_scale, normalised, as the
// Khronos sample renderer builds it (material_info.glsl 200–202) before its
// TBN, the base normal map's tangent frame about the geometry normal.
fn material_coat_normal(m:Material,texel:vec4<f32>)->vec3<f32> {
 let mapped=texel.xyz*2.-vec3(1.);
 return normalize(vec3(mapped.xy*m.coat_normal_scale,mapped.z));
}
// Material `m`'s film strength at a texel whose iridescence map reads
// `texel`: its factor times the red channel (iridescenceTexture;
// material_info.glsl 323–325).
fn material_iridescence(m:Material,texel:vec4<f32>)->f32 {
 return m.iridescence*texel.r;
}
// Material `m`'s film thickness in nanometres at a texel whose iridescence
// thickness map reads `texel`: the green channel mixed from the thinnest to
// the thickest, the thickest without a map, whose white texel reads 1
// (iridescenceThicknessTexture; material_info.glsl 321, 327–330).
fn material_iridescence_thickness(m:Material,texel:vec4<f32>)->f32 {
 return mix(m.iridescence_thickness.x,m.iridescence_thickness.y,texel.g);
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
