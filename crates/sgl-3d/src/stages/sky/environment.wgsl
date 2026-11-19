// The sky's environment atlas (the architecture's Sky): Three r185's PMREM
// atlas (CubeUV) for a cube of 128 texels, which the environment resolver
// binds wherever a map's atlas is. The capture fills its sharp level with the
// sky-view table toward each texel's direction, as Bevy
// 9d120361303727a66b62f31f0d053793af62417a
// crates/bevy_pbr/src/atmosphere/environment.wesl (MIT OR Apache-2.0,
// src/LICENSE-bevy.txt) fills its cube: no light's disc, whose reflection is
// each light's own highlight. The filter writes one step of the rest, Three's
// GGX filter from the level before (shading/pmrem_filter.wgsl), into the
// scratch atlas, which the stage copies back. Departures from Bevy: the
// target is the atlas, not a cube, and directions go from the atlas's to the
// world's through pmrem_direction.

// One step of the atlas's cycle: the level it writes (its face size and the
// texel at its face 0's corner), its first face, and for the filter the
// roughness and the mip it reads the level before at.
struct SkyEnvironmentStep {
 origin:vec2<u32>,
 size:u32,
 first_face:u32,
 roughness:f32,
 mip_in:f32,
}
@group(0) @binding(5) var<uniform> step:SkyEnvironmentStep;
@group(0) @binding(6) var sky_view_lut_in:texture_2d<f32>;
@group(0) @binding(7) var atlas_in:texture_2d_array<f32>;
@group(0) @binding(8) var atlas_out:texture_storage_2d<rgba16float,write>;

// The texel `id` writes, and its atlas direction: x and y across a face, z
// the face from the step's first.
struct StepTexel {
 texel:vec2<u32>,
 direction:vec3<f32>,
}
fn step_texel(id:vec3<u32>)->StepTexel {
 let face=step.first_face+id.z;
 let corner=step.origin+vec2(face%3u,face/3u)*step.size;
 return StepTexel(corner+id.xy,pmrem_texel_direction(id.xy,face,step.size));
}

@compute @workgroup_size(8,8,1)
fn sky_environment_capture(@builtin(global_invocation_id) id:vec3<u32>) {
 if any(id.xy>=vec2(step.size)) {
  return;
 }
 let t=step_texel(id);
 let direction=pmrem_direction(t.direction,0.);
 let inscattering=sample_sky_view_lut(sky_view_lut_in,sky_lut_sampler,sky.camera_radius,direction);
 textureStore(atlas_out,t.texel,vec4(inscattering,1.));
}

@compute @workgroup_size(8,8,1)
fn sky_environment_filter(@builtin(global_invocation_id) id:vec3<u32>) {
 if any(id.xy>=vec2(step.size)) {
  return;
 }
 let t=step_texel(id);
 let filtered=pmrem_ggx_filter(atlas_in,sky_lut_sampler,t.direction,step.roughness,step.mip_in);
 textureStore(atlas_out,t.texel,vec4(filtered,1.));
}
