// Shared by the world-space reflection trace and its denoiser: the receiver
// G-buffer, the reduced tracing grid and camera reconstruction, as Wicked
// Engine's RT reflection passes read them (world_reflections.wgsl header).
struct WorldParams {
 inverse_view_projection:mat4x4<f32>,
 previous_view_projection:mat4x4<f32>,
 // xyz eye; w the reversed-Z infinite projection's near plane.
 eye:vec4<f32>,
 // Full and reduced (tracing) resolutions: width, height, 1/width, 1/height.
 full:vec4<f32>,
 reduced:vec4<f32>,
 // Frames since the last reset; 0 resets history.
 frame:u32,
 // Full-resolution pixels per tracing pixel on each axis.
 downscale:u32,
 // Alpha roughness below which the screen-space method traces a lobe.
 traced:f32,
 // Ray range in metres.
 range:f32,
}
@group(3) @binding(0) var world_depth:texture_depth_2d;
@group(3) @binding(1) var world_normal:texture_2d<f32>;
@group(3) @binding(2) var world_material:texture_2d<f32>;
@group(3) @binding(3) var world_f0:texture_2d<f32>;
@group(3) @binding(4) var<uniform> world:WorldParams;

const WORLD_PI:f32=3.14159265359;
const WORLD_FLT_MAX:f32=3.402823466e+38;
// Wicked surfaceHF.hlsli min_roughness.
const WORLD_MIN_ROUGHNESS:f32=0.045;

// The lobe SSR traces (the coat of a coated receiver, else the base): its
// normal, perceptual roughness, and whether the screen-space method traces it.
struct WorldReceiver {
 depth:f32,
 normal:vec3<f32>,
 roughness:f32,
 traced:bool,
}
fn world_receiver(p:vec2<i32>)->WorldReceiver {
 let full=vec2<i32>(world.full.xy)-vec2(1);
 let q=clamp(p,vec2(0),full);
 let depth=textureLoad(world_depth,q,0);
 let normals=textureLoad(world_normal,q,0);
 let material=gbuffer_material(textureLoad(world_material,q,0));
 let lit=gbuffer_lit(textureLoad(world_f0,q,0));
 let roughness=gbuffer_traced_roughness(material,lit);
 let traced=depth>0. && lit && specular_traces(roughness,world.traced);
 return WorldReceiver(depth,gbuffer_reflection_normal(normals,material.coat),roughness,traced);
}
// Wicked globals.hlsli reconstruct_position; linear depth for SGL3D's infinite
// reversed-Z projection.
fn world_position(uv:vec2<f32>,z:f32)->vec3<f32> {
 let h=world.inverse_view_projection*vec4(uv.x*2.-1.,(1.-uv.y)*2.-1.,z,1.);
 return h.xyz/h.w;
}
fn world_linear_depth(z:f32)->f32 {
 return linear_depth(world.eye.w,z);
}
fn world_inverse_linear_depth(linear:f32)->f32 {
 return linear_depth(world.eye.w,linear);
}
// Wicked ssr_resolveCS.hlsl baseHash and hash33.
fn world_base_hash(p0:vec3<u32>)->u32 {
 let p=1103515245u*((p0>>vec3(1u))^p0.yzx);
 let h32=1103515245u*((p.x^p.z)^(p.y>>3u));
 return h32^(h32>>16u);
}
fn world_hash33(x:vec3<u32>)->vec3<u32> {
 let n=world_base_hash(x);
 return vec3(n,n*16807u,n*48271u);
}
