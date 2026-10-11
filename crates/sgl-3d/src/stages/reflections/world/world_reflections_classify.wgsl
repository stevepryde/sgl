// Classifies the tracing grid for world-space reflection rays: AMD
// FidelityFX SDK 1.1.4 (revision c6efa6bf7f2027b3ec94f28578bb5965eabb9e55,
// MIT, see LICENSE-amd-fidelityfx.txt), SSSR's ClassifyTiles
// (sdk/include/FidelityFX/gpu/sssr/ffx_sssr_classify_tiles.h) and
// PrepareIndirectArgs (ffx_sssr_prepare_indirect_args.h), dispatched as
// sdk/src/components/sssr/ffx_sssr.cpp 684–687 dispatches them: a workgroup a
// tile of tracing pixels, each deciding whether its receiver needs a ray,
// the rays compacted into the ray list with one count increment a
// workgroup, and the trace's indirect arguments from the count. Modified:
// translated to WGSL; workgroup atomics in place of wave intrinsics; one
// ray a tracing pixel (the trace already runs at a reduced resolution), so
// no samples per quad, no variance-guided tracing and no environment
// fallback, which composition supplies; a pixel needs a ray where
// world_trace_ray would trace one (its jittered receiver traced, its
// fallback taking a share of it, as composition gives the fallback what
// the screen-space method's confidence times the roughness fade leaves,
// not under a blended receiver); every
// pixel's trace targets start as a miss, which the trace overwrites where
// it traces, as ClassifyTiles stores every pixel's result; the ray list is
// a texture and its count reaches the trace through WorldParams (world.rs),
// since the trace's compute stage holds S3D-1's storage buffers already;
// the count is cleared each frame before the classification rather than
// reset here; no denoiser tile list, since the denoise passes run over the
// whole grid (the architecture's Reflections says why); and the indirect
// dispatch runs in rows of WORLD_GROUP_ROW workgroups.
// The screen-space method's result and the surface depth (the Surface
// contract, specs/sgl3d-architecture.md): a receiver whose lobe composition
// takes whole from the method, or an opaque one under a blended receiver,
// which composes its own, traces nothing.
@group(3) @binding(5) var world_screen_space:texture_2d<f32>;
@group(3) @binding(7) var world_surface_depth:texture_depth_2d;
@group(0) @binding(10) var classify_indirect:texture_storage_2d<rgba16float,write>;
@group(0) @binding(11) var classify_direction_pdf:texture_storage_2d<rgba16float,write>;
@group(0) @binding(12) var classify_length:texture_storage_2d<r32float,write>;
@group(0) @binding(13) var classify_rays:texture_storage_2d<r32uint,write>;
// The rays listed, cleared each frame before the classification (world.rs),
// and the trace's indirect arguments.
struct WorldRayCount {
 rays:atomic<u32>,
 groups:array<u32,3>,
}
@group(0) @binding(14) var<storage,read_write> world_ray_count:WorldRayCount;

// Whether tracing pixel `tracing` traces a ray this frame.
fn world_needs_ray(tracing:vec2<u32>)->bool {
 let pixel=world_traced_pixel(tracing);
 if world_fallback_share(pixel,textureLoad(world_screen_space,pixel,0).a)<=0.001 {
  return false;
 }
 return !gbuffer_under_receiver(textureLoad(world_surface_depth,pixel,0),textureLoad(world_depth,pixel,0));
}
var<workgroup> tile_rays:atomic<u32>;
var<workgroup> tile_base:u32;
@compute @workgroup_size(WORLD_TILE,WORLD_TILE)
fn world_classify(@builtin(global_invocation_id) id:vec3<u32>,@builtin(local_invocation_index) lane:u32) {
 let tracing=id.xy;
 var needs_ray=false;
 if all(tracing<vec2<u32>(world.reduced.xy)) {
  textureStore(classify_indirect,tracing,vec4(0.));
  textureStore(classify_direction_pdf,tracing,vec4(0.));
  textureStore(classify_length,tracing,vec4(0.));
  needs_ray=world_needs_ray(tracing);
 }
 var slot=0u;
 if needs_ray {
  slot=atomicAdd(&tile_rays,1u);
 }
 workgroupBarrier();
 if lane==0u {
  tile_base=atomicAdd(&world_ray_count.rays,atomicLoad(&tile_rays));
 }
 let base=workgroupUniformLoad(&tile_base);
 if needs_ray {
  textureStore(classify_rays,world_ray_texel(base+slot),vec4(world_pack_ray(tracing),0u,0u,0u));
 }
}

// The trace's indirect dispatch: a workgroup each WORLD_TRACE_THREADS
// listed rays, in rows of WORLD_GROUP_ROW.
@compute @workgroup_size(1)
fn world_prepare_rays() {
 let rays=min(atomicLoad(&world_ray_count.rays),u32(world.reduced.x)*u32(world.reduced.y));
 let groups=(rays+WORLD_TRACE_THREADS-1u)/WORLD_TRACE_THREADS;
 world_ray_count.groups[0]=min(groups,WORLD_GROUP_ROW);
 world_ray_count.groups[1]=(groups+WORLD_GROUP_ROW-1u)/WORLD_GROUP_ROW;
 world_ray_count.groups[2]=1u;
}
