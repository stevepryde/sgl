// Denoises world-space reflection rays: Wicked Engine's (revision
// 2ff1d9e7b36091d6edf9f823af77e6bc9af20e3b, MIT, see LICENSE-wicked.txt)
// shaders/ssr_resolveCS.hlsl (BRDF-weighted spatial ray reuse),
// ssr_temporalCS.hlsl (its colour accumulation in temporal_reprojection.wgsl,
// its variance here) and ssr_upsampleCS.hlsl (bilateral upsample), with D_GGX
// and V_SmithGGXCorrelated from brdf.hlsli, as Wicked's RT reflections run
// them. Modified: translated to WGSL; alpha carries the share of rays that hit
// (premultiplied radiance), weighted like colour; a pixel that traced nothing
// contributes no weight; loads outside the grid are skipped or clamped where
// HLSL would read zero; a resolve neighbour floors its offset pixel where
// Wicked's int2 truncates, so one off the grid is skipped rather than
// read as the edge pixel; the temporal pass keeps its own reduced-grid depth
// history, and takes a traced surface under a blended receiver, where the
// classification traced no ray and the motion is the receiver's, as
// Wicked's temporal pass takes one that needs no reflection, storing no
// depth there (0, the sky's), against which the surface fails the
// disocclusion test once uncovered (#774); the upsample's blur skips a
// neighbour under a blended receiver as Wicked's skips one that needs no
// reflection, since it holds no rays and would fade the opaque pixels
// beside the receiver toward their fallback (#806); the upsample reads the
// reduced grid, at the pixel and at each blur tap, by the hardware's
// bilinear filter as Wicked's does only where all four texels the read
// takes hold rays, as the temporal pass records them (its variance
// target's second channel); where some do not, it weights the four by the
// filter's weights where they hold rays and 0 where they do not and
// renormalises, as Godot Engine's half-resolution SSR resolve
// (servers/rendering/renderer_rd/shaders/effects/
// screen_space_reflection_resolve.glsl, revision
// ed1daf0bf001b61586d9930840f2f1394092c079, 4.7.2-stable, MIT, see
// LICENSE-godot.txt) weights its four by its bilateral weights, the empty
// colour where none does, so a texel that holds no rays (an opaque surface
// the screen-space method does not trace, or one under a receiver) no
// longer takes up to 75% of a traced pixel's reflection beside it to its
// fallback (#811); the variance read beside it takes the same weights,
// since a texel without rays stores 0 there, which would stop the blur
// beside the edge; Wicked's upsample is unchanged at its current master
// (55fca37716f1079a27df4859147f71b8f215d453) and Godot's resolve at
// 203bf0a23fd7d90a860917e66de08e20d2596244, while Bevy (9d12036) and
// Filament (ef1a133) trace their SSR at full resolution and FidelityFX
// SSSR (c6efa6b) denoises at full resolution, so none has such a read;
// each workgroup reads its texels' records once into a mask and takes
// Wicked's reads unchanged where they all hold rays, and a read whose four
// do is the hardware's anywhere, so pixels away from a texel without rays
// are Wicked's to the bit; with no such texel in view that costs about
// 0.05 ms of the denoise passes' 2.15 ms at 1920x1080 on an Apple M5,
// where testing the G-buffer at every read doubled them; the resolve reads
// radiance above binary16's range (+infinity) as its largest finite value,
// as FSR's PrepareRgb clamps its input, so its tone map makes no inf / inf,
// and keeps its inverse within that range (#464).
@group(0) @binding(8) var linear_sampler:sampler;

// Walter et al. 2007; Heitz 2014 (Wicked brdf.hlsli, mediump-saturated).
fn world_d_ggx(roughness:f32,nh:f32)->f32 {
 let one=1.-nh*nh;
 let a=nh*roughness;
 let k=roughness/(one+a*a);
 return min(k*k*(1./WORLD_PI),65504.);
}
fn world_v_smith(roughness:f32,nv:f32,nl:f32)->f32 {
 let a2=roughness*roughness;
 let lambda_v=nl*sqrt((nv-a2*nv)*nv+a2);
 let lambda_l=nv*sqrt((nl-a2*nl)*nl+a2);
 return min(.5/(lambda_v+lambda_l),65504.);
}
fn world_in_grid(p:vec2<i32>,size:vec4<f32>)->bool {
 return all(p>=vec2(0)) && all(p<vec2<i32>(size.xy));
}

// Resolve.
@group(0) @binding(10) var ray_indirect:texture_2d<f32>;
@group(0) @binding(11) var ray_direction_pdf:texture_2d<f32>;
@group(0) @binding(12) var ray_length:texture_2d<f32>;
@group(0) @binding(20) var resolve_output:texture_storage_2d<rgba16float,write>;
@group(0) @binding(21) var resolve_variance_output:texture_storage_2d<rgba16float,write>;
@group(0) @binding(22) var reprojection_output:texture_storage_2d<r32float,write>;
// A minimum size of the downscale factor.
const RESOLVE_SPATIAL_SIZE_MIN_MAX:vec2<f32>=vec2(2.,8.);
const RESOLVE_SPATIAL_RECONSTRUCTION_COUNT:u32=4u;
fn world_resolve_weight(neighbor:vec2<i32>,v:vec3<f32>,n:vec3<f32>,roughness:f32,nv:f32)->f32 {
 let direction_pdf=textureLoad(ray_direction_pdf,neighbor,0);
 if direction_pdf.w<=0. {
  return 0.;
 }
 let l=normalize(direction_pdf.xyz);
 let h=normalize(l+v);
 let nh=clamp(dot(n,h),0.,1.);
 let nl=clamp(dot(n,l),0.,1.);
 let alpha=clamp(roughness,WORLD_MIN_ROUGHNESS,1.)*clamp(roughness,WORLD_MIN_ROUGHNESS,1.);
 let brdf=world_v_smith(alpha,nv,nl)*world_d_ggx(alpha,nh)*nl;
 return brdf/max(direction_pdf.w,.00001);
}
// Hammersley with a random shift (Wicked ssr_resolveCS.hlsl).
fn world_hammersley(index:u32,count:u32,random:vec2<u32>)->vec2<f32> {
 var bits=index;
 bits=(bits<<16u)|(bits>>16u);
 bits=((bits&0x55555555u)<<1u)|((bits&0xAAAAAAAAu)>>1u);
 bits=((bits&0x33333333u)<<2u)|((bits&0xCCCCCCCCu)>>2u);
 bits=((bits&0x0F0F0F0Fu)<<4u)|((bits&0xF0F0F0F0u)>>4u);
 bits=((bits&0x00FF00FFu)<<8u)|((bits&0xFF00FF00u)>>8u);
 let radical=f32(bits^random.y)*2.3283064365386963e-10;
 return vec2(fract(f32(index)/f32(count)+f32(random.x&0xffffu)/65536.),radical);
}
@compute @workgroup_size(8,8) fn world_resolve(@builtin(global_invocation_id) id:vec3<u32>) {
 if !world_in_grid(vec2<i32>(id.xy),world.reduced) {
  return;
 }
 let p=vec2<i32>(id.xy);
 let downscale=i32(WORLD_DOWNSCALE);
 let receiver=world_receiver(p*downscale);
 if !receiver.traced {
  textureStore(resolve_output,p,textureLoad(ray_indirect,p,0));
  textureStore(resolve_variance_output,p,vec4(0.));
  textureStore(reprojection_output,p,vec4(0.));
  return;
 }
 let uv=(vec2<f32>(id.xy)+.5)*world.reduced.zw;
 let position=world_position(uv,receiver.depth);
 let v=normalize(world.eye.xyz-position);
 let n=receiver.normal;
 let nv=clamp(dot(n,v),0.,1.);
 // Roughness 0.2 is the destination.
 let spatial=mix(RESOLVE_SPATIAL_SIZE_MIN_MAX.x,RESOLVE_SPATIAL_SIZE_MIN_MAX.y,clamp(receiver.roughness*5.,0.,1.));
 var result=vec4(0.);
 var weight_sum=0.;
 var mean=0.;
 var s=0.;
 var closest_length=0.;
 let random=hash33(vec3(id.xy,world.frame)).xy;
 for(var i=0u;i<RESOLVE_SPATIAL_RECONSTRUCTION_COUNT;i++) {
  let offset=(world_hammersley(i,RESOLVE_SPATIAL_RECONSTRUCTION_COUNT,random)-vec2(.5))*spatial;
  let neighbor=vec2<i32>(floor(vec2<f32>(p)+offset));
  if !world_in_grid(neighbor,world.reduced) {
   continue;
  }
  if textureLoad(world_depth,neighbor*downscale,0)<=0. {
   continue;
  }
  let weight=world_resolve_weight(neighbor,v,n,receiver.roughness,nv);
  var color=min(textureLoad(ray_indirect,neighbor,0),vec4(65504.));
  color=vec4(color.rgb/(1.+luminance(color.rgb)),color.a);
  result+=color*weight;
  weight_sum+=weight;
  // Weighted incremental variance.
  if weight_sum>0. {
   let sample_luminance=luminance(color.rgb);
   let old=mean;
   mean+=weight/weight_sum*(sample_luminance-old);
   s+=weight*(sample_luminance-old)*(sample_luminance-mean);
  }
  if weight>.001 {
   closest_length=max(closest_length,textureLoad(ray_length,neighbor,0).x);
  }
 }
 var variance=0.;
 if weight_sum>0. {
  result/=weight_sum;
  result=vec4(result.rgb/(1.-luminance(result.rgb)),result.a);
  variance=s/weight_sum;
 }
 // Post-projection depth of the reflected point, for hit reprojection.
 let reprojection=reflection_virtual_depth(receiver.depth,distance(world.eye.xyz,position),closest_length);
 textureStore(resolve_output,p,clamp(result,vec4(.00001),vec4(65504.)));
 textureStore(resolve_variance_output,p,vec4(variance));
 textureStore(reprojection_output,p,vec4(reprojection));
}

// Temporal.
@group(0) @binding(30) var temporal_current:texture_2d<f32>;
@group(0) @binding(31) var temporal_history:texture_2d<f32>;
@group(0) @binding(32) var temporal_variance_current:texture_2d<f32>;
@group(0) @binding(33) var temporal_variance_history:texture_2d<f32>;
@group(0) @binding(34) var temporal_reprojection:texture_2d<f32>;
@group(0) @binding(35) var world_motion:texture_2d<f32>;
@group(0) @binding(36) var temporal_depth_history:texture_2d<f32>;
@group(0) @binding(40) var temporal_output:texture_storage_2d<rgba16float,write>;
// x the variance; y 1 where the texel holds rays (its opaque surface traces
// and lies under no blended receiver, where the pass keeps history), else
// 0, which the upsample reads.
@group(0) @binding(41) var temporal_variance_output:texture_storage_2d<rgba16float,write>;
@group(0) @binding(42) var temporal_depth_output:texture_storage_2d<r32float,write>;
const VARIANCE_TEMPORAL_RESPONSE:f32=.9;
@compute @workgroup_size(8,8) fn world_temporal(@builtin(global_invocation_id) id:vec3<u32>) {
 if !world_in_grid(vec2<i32>(id.xy),world.reduced) {
  return;
 }
 let p=vec2<i32>(id.xy);
 let downscale=i32(WORLD_DOWNSCALE);
 let receiver=world_receiver(p*downscale);
 let covered=receiver.traced && world_under_receiver(p*downscale,receiver.depth);
 textureStore(temporal_depth_output,p,vec4(select(receiver.depth,0.,covered)));
 let current=textureLoad(temporal_current,p,0);
 let holds_rays=receiver.traced && !covered;
 if world.frame==0u {
  textureStore(temporal_output,p,current);
  textureStore(temporal_variance_output,p,vec4(textureLoad(temporal_variance_current,p,0).x,select(0.,1.,holds_rays),0.,0.));
  return;
 }
 if !holds_rays {
  textureStore(temporal_output,p,current);
  textureStore(temporal_variance_output,p,vec4(0.));
  return;
 }
 let view=TemporalView(world.inverse_view_projection,world.previous_view_projection,world.reduced,world.previous_near);
 // SGL3D motion (current minus previous), negated into Wicked's.
 let velocity=-textureLoad(world_motion,p*downscale,0).xy;
 let accumulated=temporal_accumulate(view,p,current,velocity,textureLoad(temporal_reprojection,p,0).x,receiver.depth);
 var current_variance=textureLoad(temporal_variance_current,p,0).x;
 var response=VARIANCE_TEMPORAL_RESPONSE;
 if accumulated.disocclusion<DISOCCLUSION_THRESHOLD || !temporal_saturated(accumulated.uv) {
  // White variance on disocclusion hides temporal artifacts.
  response=0.;
  current_variance=1.;
 }
 let previous_variance=textureSampleLevel(temporal_variance_history,linear_sampler,accumulated.uv,0.).x;
 textureStore(temporal_output,p,max(vec4(0.),accumulated.color));
 textureStore(temporal_variance_output,p,vec4(max(0.,mix(current_variance,previous_variance,response)),1.,0.,0.));
}

// Upsample.
@group(0) @binding(50) var upsample_temporal:texture_2d<f32>;
// The temporal pass's variance target: x the variance, y whether the
// texel holds rays.
@group(0) @binding(51) var upsample_variance:texture_2d<f32>;
@group(0) @binding(52) var upsample_output:texture_storage_2d<rgba16float,write>;
const UPSAMPLE_DEPTH_THRESHOLD:f32=10000.;
const UPSAMPLE_NORMAL_THRESHOLD:f32=1.;
// Larger variance values use stronger blur; variance must exceed the exit
// threshold to accept blur.
const UPSAMPLE_VARIANCE_ESTIMATE_THRESHOLD:f32=.015;
const UPSAMPLE_VARIANCE_EXIT_THRESHOLD:f32=.005;
const UPSAMPLE_RADIUS_MAX:f32=2.;
const UPSAMPLE_BILATERAL_SIGMA:f32=.9;
// The upsample's workgroups: squares of UPSAMPLE_GROUP full-resolution
// pixels, which read whether each reduced texel their reads take holds
// rays once, a texel a thread, into a bit of upsample_empty where it does
// not. Their pixels and blur taps span the group and UPSAMPLE_RADIUS_MAX
// pixels each side, 12 in all, whose bilinear reads take 8 texels on each
// axis where the reduced grid has at most half the pixels, so the window
// is the group's size; a read outside it, which no grid of the stage's
// sizes makes, loads its texels' flags itself. world.rs dispatches the
// denoise passes in groups of 8 by 8.
const UPSAMPLE_GROUP:u32=8u;
var<workgroup> upsample_empty:array<atomic<u32>,2>;
// The workgroup's window of reduced texels: its first, and a bit for each
// that holds no rays, row by row.
struct WorldWindow {
 first:vec2<i32>,
 empty:vec2<u32>,
}
// The reduced grid's texel coordinates at full-resolution `uv`, as the
// bilinear filter places its texel centres.
fn world_reduced_coord(uv:vec2<f32>)->vec2<f32> {
 return uv*world.reduced.xy-.5;
}
// The first texel of workgroup `group`'s window: the first that the read at
// its first blur tap takes.
fn world_window_first(group:vec2<u32>)->vec2<i32> {
 let first=vec2<i32>(group*UPSAMPLE_GROUP)-vec2(i32(UPSAMPLE_RADIUS_MAX));
 return vec2<i32>(floor(world_reduced_coord((vec2<f32>(first)+.5)*world.full.zw)));
}
// 1 where reduced texel `t`, clamped to the grid as the sampler clamps it,
// holds rays, as the temporal pass records it, else 0.
fn world_texel_holds_rays(t:vec2<i32>)->f32 {
 let clamped=clamp(t,vec2(0),vec2<i32>(world.reduced.xy)-vec2(1));
 return select(0.,1.,textureLoad(upsample_variance,clamped,0).y>0.);
}
fn world_window_holds_rays(t:vec2<i32>,window:WorldWindow)->f32 {
 let i=t-window.first;
 if all(i>=vec2(0)) && all(i<vec2(i32(UPSAMPLE_GROUP))) {
  let bit=u32(i.y)*UPSAMPLE_GROUP+u32(i.x);
  return select(1.,0.,((window.empty[bit/32u]>>(bit%32u))&1u)!=0u);
 }
 return world_texel_holds_rays(t);
}
// The four reduced texels a bilinear read at full-resolution `uv` takes
// (the first; the rest follow at +x, +y and +xy, clamped to the grid as
// the sampler clamps them), the filter's weight of each in that order
// where it holds rays and 0 where it does not, and whether all hold rays.
struct WorldFootprint {
 texel:vec2<i32>,
 weights:vec4<f32>,
 whole:bool,
}
fn world_footprint(uv:vec2<f32>,window:WorldWindow)->WorldFootprint {
 let coord=world_reduced_coord(uv);
 let first=floor(coord);
 let f=coord-first;
 let texel=vec2<i32>(first);
 let held=vec4(
  world_window_holds_rays(texel,window),
  world_window_holds_rays(texel+vec2(1,0),window),
  world_window_holds_rays(texel+vec2(0,1),window),
  world_window_holds_rays(texel+vec2(1,1),window),
 );
 let bilinear=vec4((1.-f.x)*(1.-f.y),f.x*(1.-f.y),(1.-f.x)*f.y,f.x*f.y);
 return WorldFootprint(texel,bilinear*held,all(held>vec4(0.)));
}
// The reduced grid `reduced` at `uv` from the texels of `footprint` that
// hold rays: the hardware's bilinear read where all do, else their
// weighted sum renormalised, as Godot's half-resolution resolve weights its
// four texels, and the empty colour where none does.
fn world_upsample_read(reduced:texture_2d<f32>,uv:vec2<f32>,footprint:WorldFootprint)->vec4<f32> {
 if footprint.whole {
  return textureSampleLevel(reduced,linear_sampler,uv,0.);
 }
 let total=dot(footprint.weights,vec4(1.));
 if total<=0. {
  return vec4(0.);
 }
 let last=vec2<i32>(world.reduced.xy)-vec2(1);
 let t=footprint.texel;
 let w=footprint.weights;
 return (textureLoad(reduced,clamp(t,vec2(0),last),0)*w.x
  +textureLoad(reduced,clamp(t+vec2(1,0),vec2(0),last),0)*w.y
  +textureLoad(reduced,clamp(t+vec2(0,1),vec2(0),last),0)*w.z
  +textureLoad(reduced,clamp(t+vec2(1,1),vec2(0),last),0)*w.w)/total;
}
@compute @workgroup_size(UPSAMPLE_GROUP,UPSAMPLE_GROUP)
fn world_upsample(@builtin(global_invocation_id) id:vec3<u32>,@builtin(workgroup_id) group:vec3<u32>,@builtin(local_invocation_id) local:vec3<u32>) {
 let first=world_window_first(group.xy);
 let bit=local.y*UPSAMPLE_GROUP+local.x;
 if world_texel_holds_rays(first+vec2<i32>(local.xy))<=0. {
  atomicOr(&upsample_empty[bit/32u],1u<<(bit%32u));
 }
 workgroupBarrier();
 let window=WorldWindow(first,vec2(atomicLoad(&upsample_empty[0]),atomicLoad(&upsample_empty[1])));
 // Every texel of the window holds rays: every read is the hardware's.
 let whole=all(window.empty==vec2(0u));
 if !world_in_grid(vec2<i32>(id.xy),world.full) {
  return;
 }
 let p=vec2<i32>(id.xy);
 let receiver=world_receiver(p);
 if !receiver.traced {
  textureStore(upsample_output,p,vec4(0.));
  return;
 }
 let linear_depth=world_linear_depth(receiver.depth);
 let n=receiver.normal;
 let uv=(vec2<f32>(id.xy)+.5)*world.full.zw;
 var output=vec4(0.);
 var variance=0.;
 if whole {
  output=textureSampleLevel(upsample_temporal,linear_sampler,uv,0.);
  variance=textureSampleLevel(upsample_variance,linear_sampler,uv,0.).x;
 } else {
  let footprint=world_footprint(uv,window);
  output=world_upsample_read(upsample_temporal,uv,footprint);
  variance=world_upsample_read(upsample_variance,uv,footprint).x;
 }
 // Roughness 0.125 is the destination.
 let radius=mix(0.,select(0.,UPSAMPLE_RADIUS_MAX,variance>UPSAMPLE_VARIANCE_ESTIMATE_THRESHOLD),clamp(receiver.roughness*8.,0.,1.));
 let sigma=radius*UPSAMPLE_BILATERAL_SIGMA;
 // At most the largest radius, so the loop ends whatever the inputs hold.
 let effective=clamp(i32(min(sigma*2.,radius)),0,i32(UPSAMPLE_RADIUS_MAX));
 if variance>UPSAMPLE_VARIANCE_EXIT_THRESHOLD && effective>0 {
  let position=world_position(uv,receiver.depth);
  var result=vec4(0.);
  var weight_sum=0.;
  for(var d=0;d<2;d++) {
   let direction=select(vec2(0,1),vec2(1,0),d<1);
   for(var r=-effective;r<=effective;r++) {
    let q=p+direction*r;
    if !world_in_grid(q,world.full) {
     continue;
    }
    let sample=world_receiver(q);
    // Don't let invalid roughness samples interfere, nor a neighbour under
    // a receiver, which holds no rays; the centre, traced, always counts.
    if !sample.traced || (r!=0 && world_under_receiver(q,sample.depth)) {
     continue;
    }
    let sample_uv=(vec2<f32>(q)+.5)*world.full.zw;
    var color=vec4(0.);
    if whole {
     color=textureSampleLevel(upsample_temporal,linear_sampler,sample_uv,0.);
    } else {
     color=world_upsample_read(upsample_temporal,sample_uv,world_footprint(sample_uv,window));
    }
    let dq=position-world_position(sample_uv,sample.depth);
    let plane=max(abs(dot(dq,sample.normal)),abs(dot(dq,n)));
    let depth_weight=exp(-(plane/linear_depth)*(plane/linear_depth)*UPSAMPLE_DEPTH_THRESHOLD);
    let normal_error=pow(clamp(dot(sample.normal,n),0.,1.),4.);
    let normal_weight=clamp(1.-(1.-normal_error)*UPSAMPLE_NORMAL_THRESHOLD,0.,1.);
    let gaussian=exp(-(f32(r)/sigma)*(f32(r)/sigma));
    // Skip the centre Gaussian peak.
    let weight=select(gaussian*depth_weight*normal_weight,1.,r==0);
    result+=color*weight;
    weight_sum+=weight;
   }
  }
  output=result/weight_sum;
 }
 textureStore(upsample_output,p,output);
}
