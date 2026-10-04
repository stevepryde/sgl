// Auto exposure, metered at the render size before antialiasing, since FSR2
// reads the result (stages/exposure.rs). Ports Bevy 9d12036
// crates/bevy_post_process/src/auto_exposure/auto_exposure.wesl
// (color_to_bin, metering_weight, compute_histogram, compute_average), MIT OR
// Apache-2.0 (src/LICENSE-bevy.txt). Changes: luminance is the shared Rec. 709
// `luminance` of the frame after the authored stops (Bevy's camera exposure);
// only the first 64 invocations add a workgroup's bins to the histogram (Bevy
// indexes its 64 bins with all 256); the compensation curve is linear between
// authored points instead of a 256-texel lookup; a history reset sets the
// correction to its target; the result is also written as the frame's
// exposure multiplier, which the tone map and FSR2 read.
struct AutoExposure {
 min_log_lum:f32,
 inv_log_lum_range:f32,
 log_lum_range:f32,
 low_percent:f32,
 high_percent:f32,
 speed_up:f32,
 speed_down:f32,
 exponential_transition_distance:f32,
 correction_min:f32,
 correction_max:f32,
 // Bevy's `globals.delta_time`: the frame's seconds.
 delta_time:f32,
 // `Exposure::stops`, applied before metering and added to the correction.
 stops:f32,
 // Nonzero when history restarts: the correction takes its target.
 reset:u32,
 compensation_points:u32,
 // The compensation curve's points, two per vector: log2 luminance, stops.
 compensation:array<vec4<f32>,4>,
}
@group(0) @binding(0) var<uniform> settings:AutoExposure;
@group(0) @binding(1) var tex_color:texture_2d<f32>;
@group(0) @binding(2) var tex_mask:texture_2d<f32>;
@group(0) @binding(3) var<storage,read_write> histogram:array<atomic<u32>,64>;
// Bevy's `exposure`: the adapted correction in stops.
@group(0) @binding(4) var<storage,read_write> correction:f32;
// The frame's exposure multiplier.
@group(0) @binding(5) var exposure:texture_storage_2d<r32float,write>;
var<workgroup> histogram_shared:array<atomic<u32>,64>;
// The histogram bin of a colour.
fn color_to_bin(hdr:vec3<f32>)->u32 {
 let lum=luminance(hdr)*exp2(settings.stops);
 if lum<exp2(settings.min_log_lum) {
  return 0u;
 }
 // Log2 luminance in [0, 1] over the histogram's range.
 let log_lum=saturate((log2(lum)-settings.min_log_lum)*settings.inv_log_lum_range);
 // Bins 1 to 63; the epsilon check above takes bin 0.
 return u32(log_lum*62.+1.);
}
// The metering mask's weight at `coords`, in 16 levels: at most
// 2^32 / 16 = 16384^2 weighted pixels sum without overflow.
fn metering_weight(coords:vec2<f32>)->u32 {
 let pos=vec2<i32>(coords*vec2<f32>(textureDimensions(tex_mask)));
 let mask=textureLoad(tex_mask,pos,0).r;
 return u32(mask*16.);
}
@compute @workgroup_size(16,16,1)
fn compute_histogram(@builtin(global_invocation_id) global_invocation_id:vec3<u32>,@builtin(local_invocation_index) local_invocation_index:u32) {
 if local_invocation_index<64u {
  atomicStore(&histogram_shared[local_invocation_index],0u);
 }
 workgroupBarrier();
 let dim=textureDimensions(tex_color);
 let uv=vec2<f32>(global_invocation_id.xy)/vec2<f32>(dim);
 if global_invocation_id.x<dim.x&&global_invocation_id.y<dim.y {
  let col=textureLoad(tex_color,vec2<i32>(global_invocation_id.xy),0).rgb;
  let index=color_to_bin(col);
  let weight=metering_weight(uv);
  atomicAdd(&histogram_shared[index],weight);
 }
 workgroupBarrier();
 // The global histogram is cleared by compute_average.
 if local_invocation_index<64u {
  let histogram_value=atomicLoad(&histogram_shared[local_invocation_index]);
  atomicAdd(&histogram[local_invocation_index],histogram_value);
 }
}
// Point `index` of the compensation curve.
fn compensation_point(index:u32)->vec2<f32> {
 let pair=settings.compensation[index/2u];
 return select(pair.xy,pair.zw,index%2u==1u);
}
// The compensation in stops for an average log2 luminance: linear between
// the points, constant beyond the ends, none without points.
fn compensation(log_lum:f32)->f32 {
 if settings.compensation_points==0u {
  return 0.;
 }
 var previous=compensation_point(0u);
 if log_lum<=previous.x {
  return previous.y;
 }
 for(var index=1u;index<settings.compensation_points;index++) {
  let point=compensation_point(index);
  if log_lum<=point.x {
   return mix(previous.y,point.y,(log_lum-previous.x)/(point.x-previous.x));
  }
  previous=point;
 }
 return previous.y;
}
@compute @workgroup_size(1,1,1)
fn compute_average() {
 // The cumulative histogram, clearing the bins for the next frame.
 var histogram_sum=0u;
 for(var i=0u;i<64u;i+=1u) {
  histogram_sum+=atomicLoad(&histogram[i]);
  atomicStore(&histogram_shared[i],histogram_sum);
  atomicStore(&histogram[i],0u);
 }
 let first_index=u32(f32(histogram_sum)*settings.low_percent);
 let last_index=u32(f32(histogram_sum)*settings.high_percent);
 var count=0u;
 var sum=0.;
 // The lowest bin counts at the minimum luminance.
 var previous=first_index;
 for(var i=0u;i<64u;i+=1u) {
  // The bin's samples between the filtered ends.
  let cumulative=clamp(atomicLoad(&histogram_shared[i]),first_index,last_index);
  let bin_count=cumulative-previous;
  previous=cumulative;
  sum+=f32(bin_count)*f32(i);
  count+=bin_count;
 }
 var avg_lum=settings.min_log_lum;
 if count>0u {
  avg_lum=sum/(f32(count)*63.)*settings.log_lum_range+settings.min_log_lum;
 }
 // The correction that brings the average to the compensation's stops.
 let target_exposure=compensation(avg_lum)-avg_lum;
 if settings.reset!=0u {
  correction=target_exposure;
 } else {
  let delta=target_exposure-correction;
  if target_exposure>correction {
   let speed_down=settings.speed_down*settings.delta_time;
   let exp_down=speed_down/settings.exponential_transition_distance;
   correction=correction+min(speed_down,delta*exp_down);
  } else {
   let speed_up=settings.speed_up*settings.delta_time;
   let exp_up=speed_up/settings.exponential_transition_distance;
   correction=correction+max(-speed_up,delta*exp_up);
  }
 }
 correction=clamp(correction,settings.correction_min,settings.correction_max);
 textureStore(exposure,vec2(0),vec4(exp2(settings.stops+correction),0.,0.,0.));
}
