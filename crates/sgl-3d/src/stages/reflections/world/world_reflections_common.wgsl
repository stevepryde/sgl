// Shared by the world-space reflection classification, trace and denoiser:
// the receiver G-buffer, the reduced tracing grid and camera
// reconstruction, as Wicked Engine's RT reflection passes read them
// (world_reflections.wgsl header), and the one owner of the tracing grid's
// jitter and the ray list's packing.
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
 // The rays the classification listed this frame, copied in after it
 // (world.rs), so the trace's compute stage reads no storage buffer
 // (the architecture's Bind groups).
 rays:u32,
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

// Full-resolution pixels per tracing pixel on each axis (world.rs
// DOWNSCALE).
const WORLD_DOWNSCALE:u32=2u;
// The classification's workgroups: tiles of WORLD_TILE squared tracing
// pixels (world.rs classify::TILE).
const WORLD_TILE:u32=8u;
// The trace's threads in a workgroup, and its workgroups in a row of its
// indirect dispatch, small so that ordinary frames span several rows
// (world.rs WORLD_TRACE_THREADS, WORLD_GROUP_ROW).
const WORLD_TRACE_THREADS:u32=64u;
const WORLD_GROUP_ROW:u32=64u;
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
 let q=world_clamped(p);
 let depth=textureLoad(world_depth,q,0);
 let normals=textureLoad(world_normal,q,0);
 let material=gbuffer_material(textureLoad(world_material,q,0));
 let lit=gbuffer_lit(textureLoad(world_f0,q,0));
 let roughness=gbuffer_traced_roughness(material,lit);
 return WorldReceiver(depth,gbuffer_reflection_normal(normals,material.coat),roughness,world_traces(depth,lit,roughness));
}
fn world_clamped(p:vec2<i32>)->vec2<i32> {
 return clamp(p,vec2(0),vec2<i32>(world.full.xy)-vec2(1));
}
// Whether a receiver of `depth`, `lit` and traced `roughness` has a lobe
// the screen-space method traces, which world-space rays fill.
fn world_traces(depth:f32,lit:bool,roughness:f32)->bool {
 return depth>0. && lit && specular_traces(roughness,world.traced);
}
// world_receiver's `traced` alone, without the normal it needs no load of.
fn world_receives(p:vec2<i32>)->bool {
 let q=world_clamped(p);
 let material=gbuffer_material(textureLoad(world_material,q,0));
 let lit=gbuffer_lit(textureLoad(world_f0,q,0));
 return world_traces(textureLoad(world_depth,q,0),lit,gbuffer_traced_roughness(material,lit));
}
fn world_random(p:vec2<u32>,frame:u32)->vec2<f32> {
 return hash33_unit(vec3(p,frame)).xy;
}
// The full-resolution pixel tracing pixel `tracing` traces this frame: one
// jitter a frame chooses which pixel of each block traces, so that
// upscaling does not reuse the same pixels.
fn world_traced_pixel(tracing:vec2<u32>)->vec2<i32> {
 let jitter=vec2<u32>(floor(world_random(vec2(0u),world.frame)*f32(WORLD_DOWNSCALE)));
 return vec2<i32>(jitter+tracing*WORLD_DOWNSCALE);
}
// A listed ray: its tracing pixel's coordinates in 16 bits each, as
// FidelityFX SSSR packs its ray list's (PackRayCoords).
fn world_pack_ray(tracing:vec2<u32>)->u32 {
 return tracing.x|(tracing.y<<16u);
}
fn world_unpack_ray(word:u32)->vec2<u32> {
 return vec2(word&0xffffu,word>>16u);
}
// Ray `index`'s texel of the ray list, which holds a ray a tracing pixel
// at most.
fn world_ray_texel(index:u32)->vec2<u32> {
 let width=u32(world.reduced.x);
 return vec2(index%width,index/width);
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
