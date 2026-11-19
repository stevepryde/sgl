// Screen-space contact shadows for the directional light with the frame's
// cascades (the architecture's Contact shadows): one compute pass over the
// opaque depth at the render size, in waves of WAVE_SIZE pixels along the
// light's direction on screen. Each pixel reads the depth twice a read
// step into its wave's shared array, then tests its own depth against the
// SAMPLE_COUNT pixels toward the light the wave read, and writes the
// light's visibility into the contact mask's red channel. READ_COUNT comes
// from the level's module (contact_reads_*.wgsl).
//
// Ports Godot b130438 servers/rendering/renderer_rd/shaders/effects/
// screen_space_contact_shadows.glsl, MIT (src/LICENSE-godot.txt), keeping
// its names and order. That shader derives from Bend Studio's screen-space
// shadows (bend_sss_gpu.h, with its dispatch in bend_sss_cpu.h), Copyright
// 2023 Sony Interactive Entertainment, licensed under the Apache License,
// Version 2.0 (src/LICENSE-bend-screen-space-shadows.txt); this file is a
// changed version of both. Changes: WGSL; the depth by textureLoad, a read
// outside the target being the far depth, 0, which Godot's border sampler
// returns; the light's shadow opacity is not taken here (Godot's `opacity`):
// lighting blends the mask with the cascades at the light's one shadow
// opacity; `blur` is 1 only while temporal antialiasing resolves the
// dither, which SGL3D's interleaved gradient noise turns by the frame
// (noise.wgsl); the surface thickness is Godot's default, a constant; a
// texel outside the target is not written; the debug edge flag is dropped;
// and the averaged and fading tests are one loop of four tests a step, a
// bucket each, with the same results: Godot's per-test loops, whose bucket
// is a dynamic index into a vector, cost twice as much through Metal (5.3
// against 2.4 ms at Medium, 1920x1080, Bistro's street on an Apple M5).
//
// Every loop's bound is a named compile-time constant no buffer's contents
// can raise (AR-12): READ_COUNT read steps, and HARD_SHADOW_SAMPLES, then
// the rest of SAMPLE_COUNT tests four a step, the level's reach, an
// invocation in all.

struct ContactParams {
 screen_size:vec2<i32>,
 // The first wave's offset from the light's screen position, in pixels.
 light_offset:vec2<i32>,
 // The light's pixel position, depth and w's sign, or its direction on
 // screen with w 0 where its rays are parallel there.
 light_coordinate:vec4<f32>,
 // 1 while temporal antialiasing resolves the dither, else 0.
 blur:f32,
 frame:u32,
}

@group(0) @binding(0) var depth_buffer:texture_depth_2d;
@group(0) @binding(1) var output_shadow:texture_storage_2d<rgba8unorm,write>;
@group(0) @binding(2) var<uniform> params:ContactParams;

const WAVE_SIZE:i32=64;
// The tests a pixel makes: the level's reach in pixels.
const SAMPLE_COUNT:i32=(READ_COUNT-1)*WAVE_SIZE-1;
const HARD_SHADOW_SAMPLES:i32=4;
const FADE_OUT_SAMPLES:i32=8;

// Reverse-Z
const DEPTH_NEAR:f32=1.0;
const DEPTH_FAR:f32=0.0;
const Z_SIGN:f32=-1.0;

const BILINEAR_THRESHOLD:f32=0.02;
// Godot's rendering/lights_and_shadows/contact_shadow/surface_thickness
// default.
const SURFACE_THICKNESS:f32=0.01;

var<workgroup> DepthData:array<f32,READ_COUNT*WAVE_SIZE>;

// The opaque depth at `pixel`; the far depth outside the target, as
// Godot's border sampler returns.
fn contact_depth(pixel:vec2<i32>)->f32 {
 if any(pixel<vec2(0)) || any(pixel>=params.screen_size) {
  return DEPTH_FAR;
 }
 return textureLoad(depth_buffer,pixel,0);
}

fn organise_groups(light:vec4<f32>,converging:f32,group_id:i32,group_offset:vec2<i32>,thread_id:i32,x_major_axis:ptr<function,bool>,group_start:ptr<function,vec2<f32>>,group_end:ptr<function,vec2<f32>>,group_delta:ptr<function,vec2<f32>>,pixel_distance:ptr<function,f32>,pixel_pos:ptr<function,vec2<f32>>) {
 let light_xy=floor(light.xy)+0.5;
 let light_xy_fraction=light.xy-light_xy;

 var xy=group_offset*WAVE_SIZE+params.light_offset;
 let sign_xy=vec2<i32>(sign(vec2<f32>(xy)));

 // Determine group orientation and direction
 let horizontal=abs(xy.y)>abs(xy.x) || xy.x==xy.y;
 let axis=select(vec2(0,sign_xy.x),vec2(-sign_xy.y,0),horizontal);

 xy+=axis*group_id;
 let xy_f=vec2<f32>(xy);

 *x_major_axis=abs(xy_f.x)>abs(xy_f.y);
 let main_axis=select(xy_f.y,xy_f.x,*x_major_axis);

 var ma_light_frac=select(light_xy_fraction.y,light_xy_fraction.x,*x_major_axis);
 ma_light_frac=select(ma_light_frac,-ma_light_frac,main_axis>0.0);

 let main_axis_start=abs(main_axis)+ma_light_frac;
 let main_axis_end=max(main_axis_start-f32(WAVE_SIZE),0.0);

 *group_start=xy_f+light_xy;
 *group_end=mix(light.xy,*group_start,main_axis_end/main_axis_start);

 // Bake direction into delta so caller can use: pixel_pos += out_delta (same as custom version)
 let thread_step=f32(thread_id^select(0,WAVE_SIZE-1,converging>0.0));

 *pixel_pos=mix(*group_start,*group_end,thread_step/f32(WAVE_SIZE));
 *pixel_distance=main_axis_start-thread_step;
 *group_delta=converging*(*group_start-*group_end);
}

fn organise_groups_orthogonal(light:vec4<f32>,group_id:i32,group_offset:vec2<i32>,thread_id:i32,x_major_axis:ptr<function,bool>,group_start:ptr<function,vec2<f32>>,group_end:ptr<function,vec2<f32>>,group_delta:ptr<function,vec2<f32>>,pixel_distance:ptr<function,f32>,pixel_pos:ptr<function,vec2<f32>>) {
 var xy=group_offset*WAVE_SIZE+params.light_offset;

 // Determine group orientation and direction
 let horizontal=abs(light.x)>abs(light.y);
 let axis=select(vec2(1,0),vec2(0,1),horizontal);
 *x_major_axis=horizontal;

 // Offset groups
 xy+=axis*group_id;
 let xy_f=vec2<f32>(xy);

 *group_start=xy_f;
 *group_end=*group_start+f32(WAVE_SIZE)*light.xy;

 // Bake direction into delta so caller can use: pixel_pos += out_delta (same as custom version)
 *pixel_pos=mix(*group_start,*group_end,f32(thread_id)/f32(WAVE_SIZE));
 *pixel_distance=f32(thread_id);
 *group_delta=*group_end-*group_start;
}

@compute @workgroup_size(64,1,1)
fn contact_shadows(@builtin(workgroup_id) workgroup_id:vec3<u32>,@builtin(local_invocation_id) local_invocation_id:vec3<u32>) {
 let group_id=i32(workgroup_id.x);
 let thread_id=i32(local_invocation_id.x);
 let group_offset=vec2<i32>(workgroup_id.yz);
 let light=params.light_coordinate;

 let converging=-(light.w); // 1 converging rays, -1 diverging rays, 0 parrella rays (orthogonal)
 var group_start:vec2<f32>;
 var group_end:vec2<f32>;
 var pixel_pos:vec2<f32>;
 var group_delta:vec2<f32>;
 var pixel_distance:f32;
 var x_major_axis:bool;
 if converging!=0.0 {
  organise_groups(light,converging,group_id,group_offset,thread_id,&x_major_axis,&group_start,&group_end,&group_delta,&pixel_distance,&pixel_pos);
 } else {
  organise_groups_orthogonal(light,group_id,group_offset,thread_id,&x_major_axis,&group_start,&group_end,&group_delta,&pixel_distance,&pixel_pos);
 }

 let group_below_zero=max(group_start,group_end)<vec2(0.0);
 let group_above_screen=min(group_start,group_end)>vec2<f32>(params.screen_size);

 // Groups entirely offscreen can exit early. The wave's extents depend on
 // the workgroup and the parameters alone, never the invocation, so the
 // barrier below stays in uniform control flow, as WGSL requires.
 if any(group_below_zero) || any(group_above_screen) {
  return;
 }

 var sampling_depth:array<f32,READ_COUNT>;
 var shadowing_depth:array<f32,READ_COUNT>;
 var sample_distance:array<f32,READ_COUNT>;
 var depth_thickness_scale:array<f32,READ_COUNT>;

 let write_xy=vec2<i32>(pixel_pos);
 // READ_COUNT read steps: the level's reach.
 for(var i:i32=0;i<READ_COUNT;i++) {
  let read_xy=floor(pixel_pos);
  let minor_axis=select(pixel_pos.x,pixel_pos.y,x_major_axis);
  let bilinear=fract(minor_axis)-0.5;
  let neighbor_bias=select(-1,1,bilinear>0.0);
  let neighbor_offset=select(vec2(neighbor_bias,0),vec2(0,neighbor_bias),x_major_axis);

  var depths:vec2<f32>;
  depths.x=contact_depth(vec2<i32>(read_xy));
  depths.y=contact_depth(vec2<i32>(read_xy)+neighbor_offset);

  depth_thickness_scale[i]=abs(DEPTH_FAR-depths.x);
  let use_point_filter=abs(depths.x-depths.y)>depth_thickness_scale[i]*BILINEAR_THRESHOLD;

  // The pixel starts sampling at this depth
  sampling_depth[i]=depths.x;

  let edge_depth=depths.x;
  // Any sample in this wavefront is possibly interpolated towards the bilinear sample
  // So use should use a shadowing depth that is further away, based on the difference between the two samples
  let shadow_depth=depths.x+abs(depths.x-depths.y)*Z_SIGN;

  // Shadows cast from this depth
  shadowing_depth[i]=select(shadow_depth,edge_depth,use_point_filter);

  var stored_depth:f32;
  if converging!=0.0 {
   sample_distance[i]=pixel_distance+f32(WAVE_SIZE*i)*converging;
   stored_depth=(shadowing_depth[i]-light.z)/sample_distance[i];
  } else {
   sample_distance[i]=pixel_distance+f32(WAVE_SIZE*i);
   stored_depth=shadowing_depth[i]-light.z*sample_distance[i];
  }

  if i!=0 {
   stored_depth=select(1e10,stored_depth,sample_distance[i]>0.0);
  }

  let idx=(i*WAVE_SIZE)+thread_id;
  DepthData[idx]=stored_depth;
  pixel_pos+=group_delta;
 }

 workgroupBarrier();

 // Check if a pixel is beyond the far plane (can't be shaded)
 if sampling_depth[0]<=DEPTH_FAR {
  return;
 }

 var depth_scale:f32;
 var start_depth=sampling_depth[0];
 if converging!=0.0 {
  depth_scale=(1.0/SURFACE_THICKNESS)*sample_distance[0]/depth_thickness_scale[0];
  start_depth=(start_depth-light.z)/sample_distance[0];
  start_depth=start_depth*depth_scale-Z_SIGN;
 } else {
  // In orthogonal case it no longer makes sense to use sample distance in depth_scaling
  // 10x is used because it looked best visually
  depth_scale=(10.0/SURFACE_THICKNESS)/depth_thickness_scale[0];
  start_depth=(start_depth-light.z*sample_distance[0])*depth_scale-Z_SIGN;
 }

 start_depth+=params.blur*interleaved_gradient_noise(vec2<f32>(write_xy),params.frame);

 let sample_index=thread_id+1;
 var shadow_value=vec4(1.0);
 var hard_shadow=1.0;

 // HARD_SHADOW_SAMPLES tests, the first of the level's SAMPLE_COUNT.
 for(var i:i32=0;i<HARD_SHADOW_SAMPLES;i++) {
  let depth_delta=abs(start_depth-DepthData[sample_index+i]*depth_scale);
  hard_shadow=min(hard_shadow,depth_delta);
 }

 // Main averaged samples — accumulated into 4 buckets to soften aliasing —
 // then fade out samples — push distant shadow edge toward lit. Test i
 // falls in bucket i&3 and fades from SAMPLE_COUNT-FADE_OUT_SAMPLES on, as
 // in Godot's two loops; here a step tests four, one a bucket, since
 // HARD_SHADOW_SAMPLES is a multiple of 4. The last step's fourth test
 // (SAMPLE_COUNT itself, whose read is still the wave's) is left out.
 // (SAMPLE_COUNT-HARD_SHADOW_SAMPLES)/4 steps, rounded up: the rest of the
 // level's reach.
 for(var i:i32=HARD_SHADOW_SAMPLES;i<SAMPLE_COUNT;i+=4) {
  let index=vec4(i)+vec4(0,1,2,3);
  let at=sample_index+i;
  let stored=vec4(DepthData[at],DepthData[at+1],DepthData[at+2],DepthData[at+3]);
  let fade_out=select(vec4(0.0),vec4<f32>(index+1-(SAMPLE_COUNT-FADE_OUT_SAMPLES))/f32(FADE_OUT_SAMPLES+1)*0.75,index>=vec4(SAMPLE_COUNT-FADE_OUT_SAMPLES));
  let depth_delta=abs(start_depth-stored*depth_scale)+fade_out;
  shadow_value=min(shadow_value,select(vec4(1.0),depth_delta,index<vec4(SAMPLE_COUNT)));
 }

 // Increase base shadow contrast
 shadow_value=clamp(shadow_value*2.0-1.0,vec4(0.0),vec4(1.0));
 hard_shadow=clamp(hard_shadow*2.0-1.0,0.0,1.0);

 // Average the 4 buckets, then take the harder of hard_shadow and averaged result
 let shadow=min(hard_shadow,dot(shadow_value,vec4(0.25)));
 if all(write_xy>=vec2(0)) && all(write_xy<params.screen_size) {
  textureStore(output_shadow,write_xy,vec4(shadow,0.0,0.0,1.0));
 }
}
