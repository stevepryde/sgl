// What the dynamic GI stage's passes share: the frame's volume
// (stages/dynamic_gi.rs's VolumeUniform), a traced ray's packing and each
// irradiance texel's estimator, Wicked's MultiscaleMeanEstimator.
//
// Ports Wicked Engine df44c3db4c4927492bc9c791eac715d98d7ed091,
// WickedEngine/shaders/ShaderInterop_DDGI.h (DDGI_KEEP_DISTANCE,
// DDGI_RAY_BUCKET_COUNT, DDGIRayData and DDGIRayDataPacked,
// DDGIVarianceData and DDGIVarianceDataPacked, MultiscaleMeanEstimator
// 390–443) and wiRenderer.cpp (DDGI_BLEND_SPEED 144), MIT
// (src/LICENSE-wicked.txt). Changed: f32 in place of half, a ray packed as
// halves into four words and an estimator into six, values clamped to the
// half range they are packed into.
// How far probes keep from surfaces, in their rays' greatest distance.
const DDGI_KEEP_DISTANCE:f32=.1;
// Each frame's share in a depth map: Wicked's 0.02.
const DDGI_DEPTH_BLEND:f32=.02;
// Rays a probe traces in buckets of this many.
const DDGI_RAY_BUCKET_COUNT:u32=4u;
// Wicked's default blend speed: the estimator's short window.
const DDGI_BLEND_SPEED:f32=.1;
// A ray's weight below which a texel takes nothing from it.
const DDGI_WEIGHT_EPSILON:f32=.0001;
// Workgroups to a row of a two-dimensional dispatch over probes or rays.
const DDGI_GROUP_ROW:u32=32768u;
// The rays each workgroup of the trace traces.
const DDGI_TRACE_THREADS:u32=32u;
// Half's largest finite value: Wicked's MEDIUMP_FLT_MAX.
const DDGI_HALF_MAX:f32=65504.;
// The frame's volume.
struct DdgiVolume {
 origin:vec3<f32>,
 // The longest distance a ray's depth counts: Wicked's max_distance, 1.5
 // spacings along the volume's longest.
 max_distance:f32,
 spacing:vec3<f32>,
 // The most rays a probe traces this frame.
 max_rays:u32,
 probes:vec3<u32>,
 probe_count:u32,
 // Wicked's random_orientation: this frame's rotation of every probe's
 // spherical Fibonacci rays.
 rotation:mat3x3<f32>,
 // The camera's frustum, each plane normalised with the inside where
 // dot(xyz, p) + w >= 0, for Wicked's tenth of the rays outside it.
 frustum:array<vec4<f32>,6>,
 eye:vec3<f32>,
 // Frames since the volume restarted: the rays' random numbers.
 frame:u32,
 // The rays the frame traces, which the allocation counts and a copy
 // brings here before the trace.
 rays:u32,
 // The most probes not yet blended that start this frame.
 ramp_probes:u32,
 // The probes that trace rays this frame, which a copy brings here before
 // the blends.
 traced:u32,
 // Where the probes are stored (ddgi_probe_stored).
 scroll:vec3<u32>,
 // The whole spacings the volume has moved since the last frame that ran
 // it, at most its probes on each axis: the planes that enter, which the
 // scroll pass clears.
 scrolled:vec3<i32>,
}
// The probe a workgroup of a two-dimensional dispatch over probes serves.
fn ddgi_group_probe(group:vec3<u32>)->u32 {
 return group.x+group.y*DDGI_GROUP_ROW;
}
struct DdgiRay {
 direction:vec3<f32>,
 // To what it hit, or -1 where it missed.
 depth:f32,
 radiance:vec3<f32>,
 // It met a single-sided surface from behind, as RTXGI marks a back-face
 // hit by a negative distance.
 backface:bool,
}
fn ddgi_pack_ray(ray:DdgiRay)->vec4<u32> {
 let depth=min(ray.depth,DDGI_HALF_MAX);
 let radiance=clamp(ray.radiance,vec3(0.),vec3(DDGI_HALF_MAX));
 return vec4(pack2x16float(ray.direction.xy),pack2x16float(vec2(ray.direction.z,depth)),pack2x16float(radiance.rg),pack2x16float(vec2(radiance.b,select(0.,1.,ray.backface))));
}
fn ddgi_unpack_ray(data:vec4<u32>)->DdgiRay {
 let direction_depth=vec4(unpack2x16float(data.x),unpack2x16float(data.y));
 let radiance=vec4(unpack2x16float(data.z),unpack2x16float(data.w));
 return DdgiRay(direction_depth.xyz,direction_depth.w,radiance.rgb,radiance.w>0.);
}
// One irradiance texel's estimator: Wicked's DDGIVarianceData.
struct DdgiVariance {
 mean:vec3<f32>,
 short_mean:vec3<f32>,
 vbbr:f32,
 variance:vec3<f32>,
 inconsistency:f32,
}
// Its six words in the stage's estimator buffer.
const DDGI_VARIANCE_WORDS:u32=6u;
fn ddgi_pack_half2(a:f32,b:f32)->u32 {
 return pack2x16float(clamp(vec2(a,b),vec2(-DDGI_HALF_MAX),vec2(DDGI_HALF_MAX)));
}
fn ddgi_pack_variance(data:DdgiVariance)->array<u32,6> {
 return array<u32,6>(
  ddgi_pack_half2(data.mean.r,data.mean.g),
  ddgi_pack_half2(data.mean.b,data.short_mean.r),
  ddgi_pack_half2(data.short_mean.g,data.short_mean.b),
  ddgi_pack_half2(data.variance.r,data.variance.g),
  ddgi_pack_half2(data.variance.b,data.vbbr),
  ddgi_pack_half2(data.inconsistency,0.),
 );
}
fn ddgi_unpack_variance(words:array<u32,6>)->DdgiVariance {
 let a=unpack2x16float(words[0]);
 let b=unpack2x16float(words[1]);
 let c=unpack2x16float(words[2]);
 let d=unpack2x16float(words[3]);
 let e=unpack2x16float(words[4]);
 let f=unpack2x16float(words[5]);
 return DdgiVariance(vec3(a,b.x),vec3(b.y,c),e.y,vec3(d,e.x),f.x);
}
// A probe's state in the stage's probe buffer: its relocated offset in half
// spacings, whether it has been blended since the volume restarted, the
// share of its rays that meet single-sided surfaces from behind, which
// classifies it, and the back faces its fixed rays have met over the frames
// of the cycle it has traced so far.
struct DdgiProbe {
 offset:vec3<f32>,
 blended:bool,
 backfaces:f32,
 fixed_backfaces:u32,
 fixed_frames:u32,
}
fn ddgi_pack_probe(probe:DdgiProbe)->vec4<u32> {
 return vec4(ddgi_pack_half2(probe.offset.x,probe.offset.y),ddgi_pack_half2(probe.offset.z,select(0.,1.,probe.blended)),bitcast<u32>(probe.backfaces),probe.fixed_backfaces|(probe.fixed_frames<<16u));
}
fn ddgi_unpack_probe(words:vec4<u32>)->DdgiProbe {
 let offset=vec4(unpack2x16float(words.x),unpack2x16float(words.y));
 return DdgiProbe(offset.xyz,offset.w>0.,bitcast<f32>(words.z),words.w&0xffffu,words.w>>16u);
}
// A probe not yet blended, at rest, as a restart or a scroll starts one.
fn ddgi_fresh_probe()->DdgiProbe {
 return DdgiProbe(vec3(0.),false,0.,0u,0u);
}
// RTXGI's RTXGI_DDGI_NUM_FIXED_RAYS: the fixed directions that classify a
// probe, spread evenly over the sphere and never rotated, so a probe's class
// holds still while what it sees does. A probe traces
// DDGI_FIXED_RAYS_PER_FRAME of them a frame, all of them over a cycle of
// DDGI_FIXED_CYCLE frames.
const DDGI_FIXED_RAYS:u32=32u;
const DDGI_FIXED_CYCLE:u32=DDGI_FIXED_RAYS/DDGI_FIXED_RAYS_PER_FRAME;
// RTXGI's probeBackfaceThreshold: a probe more than this share of whose rays
// meet single-sided surfaces from behind is inside geometry or beyond a
// wall, and inactive.
const DDGI_BACKFACE_THRESHOLD:f32=.25;
// Whether a probe lights receivers: RTXGI's probe classification. An
// inactive probe weighs nothing in the sample and traces the fewest rays.
fn ddgi_probe_active(probe:DdgiProbe)->bool {
 return probe.backfaces<=DDGI_BACKFACE_THRESHOLD;
}
fn ddgi_luminance_weights()->vec3<f32> {
 return vec3(.299,.587,.114);
}
// Wicked's MultiscaleMeanEstimator: the texel's mean follows its samples
// `y` quickly where they are inconsistent with it and slowly where they
// agree, with fireflies suppressed.
fn multiscale_mean_estimator(y_in:vec3<f32>,data:ptr<function,DdgiVariance>,short_window_blend:f32) {
 var y=y_in;
 var mean=(*data).mean;
 var short_mean=(*data).short_mean;
 var vbbr=(*data).vbbr;
 var variance=(*data).variance;
 var inconsistency=(*data).inconsistency;
 // Suppress fireflies.
 {
  let dev=sqrt(max(vec3(1e-5),variance));
  let high_threshold=.1+short_mean+dev*8.;
  let overflow=max(vec3(0.),y-high_threshold);
  y-=overflow;
 }
 let delta=y-short_mean;
 short_mean=mix(short_mean,y,short_window_blend);
 let delta2=y-short_mean;
 // A longer window than short_window_blend, to avoid bias from the
 // variance getting smaller when the short-term mean does.
 let variance_blend=short_window_blend*.5;
 variance=mix(variance,delta*delta2,variance_blend);
 let dev=sqrt(max(vec3(1e-5),variance));
 let short_diff=mean-short_mean;
 let relative_diff=dot(ddgi_luminance_weights(),abs(short_diff)/max(vec3(1e-5),dev));
 inconsistency=mix(inconsistency,relative_diff,.08);
 let variance_based_blend_reduction=clamp(dot(ddgi_luminance_weights(),.5*short_mean/max(vec3(1e-5),dev)),1./32.,1.);
 var catch_up_blend=clamp(vec3(smoothstep(0.,1.,relative_diff*max(.02,inconsistency-.2))),vec3(1./256.),vec3(1.));
 catch_up_blend*=vbbr;
 vbbr=mix(vbbr,variance_based_blend_reduction,.1);
 mean=mix(mean,y,saturate(catch_up_blend));
 (*data).mean=mean;
 (*data).short_mean=short_mean;
 (*data).vbbr=vbbr;
 (*data).variance=variance;
 (*data).inconsistency=inconsistency;
}
