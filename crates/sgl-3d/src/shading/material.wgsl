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
}
const MATERIAL_UNLIT:u32=1u;
const MATERIAL_DOUBLE_SIDED:u32=2u;
// The maps it was added with: a normal map, a bump map (used only without a
// normal map) and an anisotropy direction and strength map.
const MATERIAL_NORMAL_MAP:u32=4u;
const MATERIAL_BUMP_MAP:u32=8u;
const MATERIAL_ANISOTROPY_MAP:u32=16u;
// Its alpha mode: masked or blended; neither is opaque.
const MATERIAL_ALPHA_MASK:u32=32u;
const MATERIAL_ALPHA_BLEND:u32=64u;
// Whether material `m` cuts out a texel of base alpha `alpha`: a masked
// material below its cutoff. Bevy 9d12036's alpha_discard
// (crates/bevy_pbr/src/render/pbr_functions.wesl), MIT OR Apache-2.0
// (src/LICENSE-bevy.txt); its shadow casters and prepass cut out the same
// texels (prepass_alpha_discard in pbr_prepass_functions.wesl).
fn material_cut_out(m:Material,alpha:f32)->bool {
 return (m.flags&MATERIAL_ALPHA_MASK)!=0u && alpha<m.alpha_cutoff;
}
