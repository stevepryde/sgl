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
 // The previous frame's near plane, which wrote the depth history.
 previous_near:f32,
 // Width of the screen-space method's fade below its cutoff, in
 // perceptual roughness.
 fade:f32,
}
@group(3) @binding(0) var world_depth:texture_depth_2d;
@group(3) @binding(1) var world_normal:texture_2d<f32>;
@group(3) @binding(2) var world_material:texture_2d<f32>;
@group(3) @binding(3) var world_f0:texture_2d<f32>;
@group(3) @binding(4) var<uniform> world:WorldParams;
// The surface depth (the Surface contract, specs/sgl3d-architecture.md),
// which the classification, the temporal pass and the upsample read.
@group(3) @binding(7) var world_surface_depth:texture_depth_2d;

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
// What the denoiser compares of two traced lobes: device depth, normal and
// perceptual roughness.
struct WorldSurface {
 depth:f32,
 normal:vec3<f32>,
 roughness:f32,
}
fn world_surface(receiver:WorldReceiver)->WorldSurface {
 return WorldSurface(receiver.depth,receiver.normal,receiver.roughness);
}
// How far reduced texel `texel`'s surface, as its block's first pixel
// judges it (the resolve and the temporal pass judge it there), is the
// surface `surface` that reads it: 1 where depth, normal and roughness
// agree, falling towards 0 across a silhouette or a crease. Godot Engine's
// half-resolution SSR resolve weight (get_sample in
// servers/rendering/renderer_rd/shaders/effects/
// screen_space_reflection_resolve.glsl 23-55, revision
// ed1daf0bf001b61586d9930840f2f1394092c079, 4.7.2-stable, unchanged at
// master 203bf0a23fd7d90a860917e66de08e20d2596244, MIT, see
// LICENSE-godot.txt), with its factors: its depths are device depths, as
// Godot's are (the depth buffer and its half-resolution copy), and its
// roughness perceptual, as Godot's normal-roughness buffer holds it.
// Modified: translated to WGSL; it takes decoded surfaces; each term is its
// exponent, which world_same_depth and world_surface_edge share (#815).
const WORLD_SURFACE_DEPTH_FACTOR:f32=2048.;
const WORLD_SURFACE_NORMAL_FACTOR:f32=32.;
const WORLD_SURFACE_ROUGHNESS_FACTOR:f32=16.;
fn world_depth_exponent(difference:f32)->f32 {
 return abs(difference)*WORLD_SURFACE_DEPTH_FACTOR;
}
fn world_normal_exponent(a:vec3<f32>,b:vec3<f32>)->f32 {
 return clamp(1.-dot(a,b),0.,1.)*WORLD_SURFACE_NORMAL_FACTOR;
}
fn world_roughness_exponent(a:f32,b:f32)->f32 {
 return abs(a-b)*WORLD_SURFACE_ROUGHNESS_FACTOR;
}
fn world_same_surface(texel:WorldSurface,surface:WorldSurface)->f32 {
 let weight_depth=world_same_depth(texel.depth,surface.depth);
 let weight_normal=exp(-world_normal_exponent(surface.normal,texel.normal));
 let weight_roughness=exp(-world_roughness_exponent(surface.roughness,texel.roughness));
 return weight_depth*weight_normal*weight_roughness;
}
// world_same_surface's depth term alone, which the resolve's spatial reuse
// weighs a neighbour's rays by: how far a texel whose surface lies at
// device depth `texel` is the surface at `depth`.
fn world_same_depth(texel:f32,depth:f32)->f32 {
 return exp(-world_depth_exponent(depth-texel));
}
// Whether texel surface `texel` meets its neighbour `neighbor` at an edge,
// where the texel departs from the line through its two neighbours on that
// neighbour's axis by `departure` in device depth (z[i-1] - 2 z[i] + z[i+1]):
// world_same_surface below one half, with its depth term taken on that
// departure rather than on the step between the two. Device depth is
// affine in screen space over a plane and the texels are evenly spaced, so
// a plane's departure is 0 at any slope and only a silhouette or a crease
// breaks it. SVGF compensates its depth edge-stopping for slope the same
// way, dividing the step by the depth gradient (Schied et al. 2017,
// "Spatiotemporal Variance-Guided Filtering"; NVIDIA Falcor
// 759aad033ff610fb0d82c74f7e0a508d0096d5f2,
// Source/RenderPasses/SVGFPass/SVGFAtrous.ps.slang 97 and 122-124,
// SVGFCommon.slang 35-54); this takes the idea, not that code. The same
// depth term on another argument; the normal and roughness terms are
// Godot's between the two. Below one half: where the exponents' sum
// exceeds ln 2.
const WORLD_LN_2:f32=.69314718;
fn world_surface_edge(departure:f32,texel:WorldSurface,neighbor:WorldSurface)->bool {
 return world_depth_exponent(departure)+world_normal_exponent(texel.normal,neighbor.normal)+world_roughness_exponent(texel.roughness,neighbor.roughness)>WORLD_LN_2;
}
// Whether a blended receiver is the surface at `p`, whose opaque depth is
// `depth`: its opaque surface, which composes its fallback alone there,
// traces no world ray, and the G-buffer's motion there is the receiver's.
fn world_under_receiver(p:vec2<i32>,depth:f32)->bool {
 return gbuffer_under_receiver(textureLoad(world_surface_depth,world_clamped(p),0),depth);
}
fn world_clamped(p:vec2<i32>)->vec2<i32> {
 return clamp(p,vec2(0),vec2<i32>(world.full.xy)-vec2(1));
}
// Whether a receiver of `depth`, `lit` and traced `roughness` has a lobe
// the screen-space method traces, which world-space rays fill.
fn world_traces(depth:f32,lit:bool,roughness:f32)->bool {
 return depth>0. && lit && specular_traces(roughness,world.traced);
}
// The share of a receiver's traced lobe that composition leaves to its
// fallback, world-space rays included, where the screen-space method's
// result holds `confidence` (specular_fallback_share, with the fade
// compose_screen_space takes), 0 where the receiver at `p` has no traced
// lobe. world_receiver's test alone, without the normal it needs no load
// of.
fn world_fallback_share(p:vec2<i32>,confidence:f32)->f32 {
 let q=world_clamped(p);
 let material=gbuffer_material(textureLoad(world_material,q,0));
 let lit=gbuffer_lit(textureLoad(world_f0,q,0));
 let roughness=gbuffer_traced_roughness(material,lit);
 if !world_traces(textureLoad(world_depth,q,0),lit,roughness) {
  return 0.;
 }
 return specular_fallback_share(confidence,specular_trace_fade(roughness,sqrt(world.traced),world.fade));
}
fn world_random(p:vec2<u32>,frame:u32)->vec2<f32> {
 return hash33_unit(vec3(p,frame)).xy;
}
// The full-resolution pixel tracing pixel `tracing` traces this frame: one
// jitter a frame chooses which pixel of each block traces, so that
// upscaling does not reuse the same pixels. world_random reaches 1, so the
// jitter is capped at the block's last pixel; the pixel is clamped to the
// frame, which a block of a 1-pixel side overhangs.
fn world_traced_pixel(tracing:vec2<u32>)->vec2<i32> {
 let jitter=min(vec2<u32>(floor(world_random(vec2(0u),world.frame)*f32(WORLD_DOWNSCALE))),vec2(WORLD_DOWNSCALE-1u));
 return world_clamped(vec2<i32>(jitter+tracing*WORLD_DOWNSCALE));
}
// A listed ray: its tracing pixel's coordinates in 16 bits each, as
// FidelityFX SSSR packs its denoiser tiles' (ffx_sssr_callbacks_hlsl.h
// 418–421, StoreDenoiserTile), where its ray list's PackRayCoords gives x 15
// bits and y 14 beside copy flags SGL3D has no use for.
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
