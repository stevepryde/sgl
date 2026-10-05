// Denoises world-space reflection rays: Wicked Engine's (revision
// 2ff1d9e7b36091d6edf9f823af77e6bc9af20e3b, MIT, see LICENSE-wicked.txt)
// shaders/ssr_resolveCS.hlsl (BRDF-weighted spatial ray reuse),
// ssr_temporalCS.hlsl (its colour accumulation in temporal_reprojection.wgsl,
// its variance here) and ssr_upsampleCS.hlsl (bilateral upsample), with D_GGX
// and V_SmithGGXCorrelated from brdf.hlsli, as Wicked's RT reflections run
// them. Modified: translated to WGSL; alpha carries the share of rays that hit
// (premultiplied radiance), weighted like colour; a pixel that traced nothing
// contributes no weight; loads outside the grid are skipped or clamped where
// HLSL would read zero; the temporal pass keeps its own reduced-grid depth
// history.
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
 let downscale=i32(world.downscale);
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
 let random=world_hash33(vec3(id.xy,world.frame)).xy;
 for(var i=0u;i<RESOLVE_SPATIAL_RECONSTRUCTION_COUNT;i++) {
  let offset=(world_hammersley(i,RESOLVE_SPATIAL_RECONSTRUCTION_COUNT,random)-vec2(.5))*spatial;
  let neighbor=vec2<i32>(vec2<f32>(p)+offset);
  if !world_in_grid(neighbor,world.reduced) {
   continue;
  }
  if textureLoad(world_depth,neighbor*downscale,0)<=0. {
   continue;
  }
  let weight=world_resolve_weight(neighbor,v,n,receiver.roughness,nv);
  var color=textureLoad(ray_indirect,neighbor,0);
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
 let reprojection=world_inverse_linear_depth(world_linear_depth(receiver.depth)+closest_length);
 textureStore(resolve_output,p,max(result,vec4(.00001)));
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
@group(0) @binding(41) var temporal_variance_output:texture_storage_2d<rgba16float,write>;
@group(0) @binding(42) var temporal_depth_output:texture_storage_2d<r32float,write>;
const VARIANCE_TEMPORAL_RESPONSE:f32=.9;
@compute @workgroup_size(8,8) fn world_temporal(@builtin(global_invocation_id) id:vec3<u32>) {
 if !world_in_grid(vec2<i32>(id.xy),world.reduced) {
  return;
 }
 let p=vec2<i32>(id.xy);
 let downscale=i32(world.downscale);
 let receiver=world_receiver(p*downscale);
 textureStore(temporal_depth_output,p,vec4(receiver.depth));
 let current=textureLoad(temporal_current,p,0);
 if world.frame==0u {
  textureStore(temporal_output,p,current);
  textureStore(temporal_variance_output,p,textureLoad(temporal_variance_current,p,0));
  return;
 }
 if !receiver.traced {
  textureStore(temporal_output,p,current);
  textureStore(temporal_variance_output,p,vec4(0.));
  return;
 }
 let view=TemporalView(world.inverse_view_projection,world.previous_view_projection,world.reduced,world.eye.w);
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
 textureStore(temporal_variance_output,p,vec4(max(0.,mix(current_variance,previous_variance,response))));
}

// Upsample.
@group(0) @binding(50) var upsample_temporal:texture_2d<f32>;
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
@compute @workgroup_size(8,8) fn world_upsample(@builtin(global_invocation_id) id:vec3<u32>) {
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
 var output=textureSampleLevel(upsample_temporal,linear_sampler,uv,0.);
 let variance=textureSampleLevel(upsample_variance,linear_sampler,uv,0.).x;
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
    // Don't let invalid roughness samples interfere.
    if !sample.traced {
     continue;
    }
    let sample_uv=(vec2<f32>(q)+.5)*world.full.zw;
    let color=textureSampleLevel(upsample_temporal,linear_sampler,sample_uv,0.);
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
