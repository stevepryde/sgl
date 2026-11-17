// Motion blur: Wicked Engine's tile-max reconstruction filter, after Jimenez
// 2014 ("Next Generation Post Processing in Call of Duty: Advanced Warfare")
// and McGuire et al. 2012 ("A Reconstruction Filter for Plausible Motion
// Blur"), over the antialiased frame (stages/motion_blur.rs).
//
// Ports Wicked Engine 4323a33 WickedEngine/shaders/motionblurCS.hlsl
// (DepthCmp, SpreadCmp, SampleWeight, main and its MOTIONBLUR_CHEAP and
// MOTIONBLUR_EARLYEXIT variants), with globals.hlsli BayerMatrix8 and dither,
// MIT (src/LICENSE-wicked.txt), over the tiles of its tile passes, which
// the shared motion_tiles module ports (shading/motion_tiles.wgsl). Changes:
// - Velocities are blur vectors in pixels of the frame being blurred: the
//   G-buffer's motion times the frame's size and the shutter
//   (`MotionTiles.velocity_scale`), so a pixel blurs over the shutter's share
//   of its motion at any resolution and frame rate. Wicked scales UV velocity
//   by its strength (100 at 60 Hz) and the reciprocal resolution.
// - Samples are eight pairs mirrored about the pixel (Jimenez's mirror
//   filter), each pair's offset jittered within its stratum by interleaved
//   gradient noise, as Jimenez and McGuire jitter sample positions; they span
//   the blur vector. Wicked takes adjacent samples along a span that its blue
//   noise scales.
// - SpreadCmp compares in sample units, as its pixelToSampleUnitsScale
//   parameter names: each pair's offset is its stratum plus its jitter, and
//   each velocity's blur radius (half its length) in pixels is scaled by the
//   samples per pixel of the span, so a spread fades over one sample step.
//   Wicked passes the neighbourhood's speed as every offset and scales UV
//   speeds by 1000. (Jimenez's slides, a 400 MB deck, were not read; the
//   units follow the parameter's name.)
// - Each blur is at most two tiles long, its radius at most a tile, as
//   McGuire et al. 2012 clamp each velocity's radius to the tile size: a 3x3
//   tile neighbourhood carries a velocity one tile. Wicked does not clamp.
// - Depth is linear view depth in metres (shading/depth.wgsl); Wicked's is
//   linear depth over its far plane, which its depth scale (the far plane)
//   returns to metres.
// - One dispatch runs every tile, and each workgroup (inside one tile) takes
//   its tile's variant from the tile's neighbourhood: under a pixel of blur
//   copies (early exit); blur lengths within a pixel of each other average
//   along the pixel's own velocity (cheap); else the weighted filter. Wicked
//   lists tiles for three indirect dispatches, with UV thresholds.
// - Samples beyond the frame's edge are its edge pixels.
// - The frame's colour is read through finite_scene_color
//   (shading/scene_color.wgsl): a texel of +infinity, which the frame holds
//   unclamped where neither TAA nor FSR ran, would make a sample's weight of
//   0 NaN and carry infinity into every pixel gathering it. main's sampling
//   loop is motionblur_gather, run again with bounded samples only for a
//   pixel whose result is not finite: bounding every sample of every pixel
//   cost 0.3-0.45 ms at 1920x1080 on an Apple M5, where the test costs
//   nothing measurable.

// The tile passes' bindings 0, 1, 3, 4 and 5 are the motion_tiles module's:
// the filter reads the tile neighbourhoods at 4 and writes the blurred frame
// at 5.
struct MotionBlur {
 // The perspective projection's near plane.
 near:f32,
 // Frames since history restarted, which turn the noise.
 frame:u32,
}
@group(0) @binding(2) var texture_depth:texture_depth_2d;
@group(0) @binding(6) var<uniform> settings:MotionBlur;

var<private> BayerMatrix8:array<array<f32,8>,8>=array<array<f32,8>,8>(
 array<f32,8>(1.0/65.0,49.0/65.0,13.0/65.0,61.0/65.0,4.0/65.0,52.0/65.0,16.0/65.0,64.0/65.0),
 array<f32,8>(33.0/65.0,17.0/65.0,45.0/65.0,29.0/65.0,36.0/65.0,20.0/65.0,48.0/65.0,32.0/65.0),
 array<f32,8>(9.0/65.0,57.0/65.0,5.0/65.0,53.0/65.0,12.0/65.0,60.0/65.0,8.0/65.0,56.0/65.0),
 array<f32,8>(41.0/65.0,25.0/65.0,37.0/65.0,21.0/65.0,44.0/65.0,28.0/65.0,40.0/65.0,24.0/65.0),
 array<f32,8>(3.0/65.0,51.0/65.0,15.0/65.0,63.0/65.0,2.0/65.0,50.0/65.0,14.0/65.0,62.0/65.0),
 array<f32,8>(35.0/65.0,19.0/65.0,47.0/65.0,31.0/65.0,34.0/65.0,18.0/65.0,46.0/65.0,30.0/65.0),
 array<f32,8>(11.0/65.0,59.0/65.0,7.0/65.0,55.0/65.0,10.0/65.0,58.0/65.0,6.0/65.0,54.0/65.0),
 array<f32,8>(43.0/65.0,27.0/65.0,39.0/65.0,23.0/65.0,42.0/65.0,26.0/65.0,38.0/65.0,22.0/65.0),
);

fn dither(pixel:vec2<u32>)->f32 {
 return BayerMatrix8[pixel.x%8u][pixel.y%8u];
}

// Linear view depth in metres at `pixel` of the frame; very far where
// nothing was drawn.
fn motionblur_lineardepth(pixel:vec2<i32>)->f32 {
 let frame_pixel=clamp(pixel,vec2(0),vec2<i32>(textureDimensions(input))-1);
 let texel=min(vec2<u32>((vec2<f32>(frame_pixel)+0.5)*motion_tiles.render_scale),textureDimensions(texture_depth)-1u);
 return linear_depth(settings.near,textureLoad(texture_depth,texel,0));
}

fn DepthCmp(centerDepth:f32,sampleDepth:f32,depthScale:f32)->vec2<f32> {
 return saturate(0.5+vec2(depthScale,-depthScale)*(sampleDepth-centerDepth));
}

fn SpreadCmp(offsetLen:f32,spreadLen:vec2<f32>,pixelToSampleUnitsScale:f32)->vec2<f32> {
 return saturate(pixelToSampleUnitsScale*spreadLen-offsetLen+1.0);
}

fn SampleWeight(centerDepth:f32,sampleDepth:f32,offsetLen:f32,centerSpreadLen:f32,sampleSpreadLen:f32,pixelToSampleUnitsScale:f32,depthScale:f32)->f32 {
 let depthCmp=DepthCmp(centerDepth,sampleDepth,depthScale);
 let spreadCmp=SpreadCmp(offsetLen,vec2(centerSpreadLen,sampleSpreadLen),pixelToSampleUnitsScale);
 return dot(depthCmp,spreadCmp);
}

// The pairs of samples each pixel takes.
const MOTIONBLUR_PAIRS=8;

// The frame's colour at `pixel`, edge pixels beyond it; read through
// finite_scene_color where `bounded`.
fn motionblur_color(pixel:vec2<i32>,bounded:bool)->vec3<f32> {
 let color=textureLoad(input,clamp(pixel,vec2(0),vec2<i32>(textureDimensions(input))-1),0);
 if bounded {
  return finite_scene_color(color).rgb;
 }
 return color.rgb;
}

// main's sampling of the blur and its mix with the centre, with each sample
// read through finite_scene_color where `bounded`.
fn motionblur_gather(pixel:vec2<u32>,center_color:vec4<f32>,cheap:bool,sampling_direction:vec2<f32>,center_velocity_magnitude:f32,center_depth:f32,random_offset:f32,bounded:bool)->vec4<f32> {
 let center=vec2<f32>(pixel)+0.5;
 var sum=vec4(0.0);
 for (var i=0;i<MOTIONBLUR_PAIRS;i+=1) {
  // The pair's share of the blur vector either side of the pixel: one
  // jittered sample in each of the half span's strata.
  let offset=sampling_direction*(f32(i)+random_offset)/f32(2*MOTIONBLUR_PAIRS);
  let pixel1=vec2<i32>(floor(center+offset));
  let pixel2=vec2<i32>(floor(center-offset));
  let color1=motionblur_color(pixel1,bounded);
  let color2=motionblur_color(pixel2,bounded);
  if cheap {
   sum+=vec4(color1,1.0);
   sum+=vec4(color2,1.0);
   continue;
  }
  let depth1=motionblur_lineardepth(pixel1);
  let velocity_magnitude1=length(motionblur_velocity(pixel1));
  let depth2=motionblur_lineardepth(pixel2);
  let velocity_magnitude2=length(motionblur_velocity(pixel2));
  // The pair's offset in sample steps, and sample steps per pixel.
  let offset_length=f32(i)+random_offset;
  let pixel_to_sample_units_scale=f32(2*MOTIONBLUR_PAIRS)/max(length(sampling_direction),1e-30);
  // Blur radii: each velocity spreads half its length either side.
  var weight1=SampleWeight(center_depth,depth1,offset_length,0.5*center_velocity_magnitude,0.5*velocity_magnitude1,pixel_to_sample_units_scale,1.0);
  var weight2=SampleWeight(center_depth,depth2,offset_length,0.5*center_velocity_magnitude,0.5*velocity_magnitude2,pixel_to_sample_units_scale,1.0);
  let mirror=vec2<bool>(depth1>depth2,velocity_magnitude2>velocity_magnitude1);
  weight1=select(weight1,weight2,all(mirror));
  weight2=select(weight1,weight2,any(mirror));
  sum+=weight1*vec4(color1,1.0);
  sum+=weight2*vec4(color2,1.0);
 }
 sum/=f32(2*MOTIONBLUR_PAIRS);
 return vec4(sum.rgb+(1.0-sum.w)*center_color.rgb,center_color.a);
}

@compute @workgroup_size(8,8,1)
fn motionblur(@builtin(global_invocation_id) DTid:vec3<u32>) {
 let pixel=DTid.xy;
 if any(pixel>=textureDimensions(input)) {
  return;
 }
 let center_color=finite_scene_color(textureLoad(input,pixel,0));
 // The workgroup's tile, which selects the variant for all of it.
 let tile=textureLoad(tiles,pixel/MOTIONBLUR_TILESIZE,0);
 let tile_max_magnitude=length(tile.xy);
 let tile_min_magnitude=length(tile.zw);
 // MOTIONBLUR_EARLYEXIT: no blur reaches a pixel beyond the centre's.
 if tile_max_magnitude<1.0 {
  textureStore(output,pixel,center_color);
  return;
 }
 let cheap=tile_max_magnitude-tile_min_magnitude<1.0;
 // Dither to reduce tile artifact.
 let neighborhood_tile=vec2<i32>(floor((vec2<f32>(pixel)+(dither(pixel)-0.5)*16.0)/f32(MOTIONBLUR_TILESIZE)));
 let tile_dim=vec2<i32>(textureDimensions(tiles));
 let neighborhood_velocity=textureLoad(tiles,clamp(neighborhood_tile,vec2(0),tile_dim-1),0).xy;
 let center_velocity=motionblur_velocity(vec2<i32>(pixel));
 let center_velocity_magnitude=length(center_velocity);
 let center_depth=motionblur_lineardepth(vec2<i32>(pixel));
 let random_offset=interleaved_gradient_noise(vec2<f32>(pixel),settings.frame);
 // MOTIONBLUR_CHEAP samples along the pixel's own velocity.
 let sampling_direction=select(neighborhood_velocity,center_velocity,cheap);
 // An infinite sample makes the result infinite, or NaN where its weight is
 // 0, and a NaN sample makes it NaN; only then is the gather repeated with
 // every sample bounded, so a finite frame costs one test a pixel. A NaN
 // sample, outside the scene colour contract, may stay NaN. An invocation
 // runs at most these two gathers of MOTIONBLUR_PAIRS pairs (AR-12): two
 // calls rather than a loop of gathers, whose bounding became a branch in
 // every sample and cost 0.4-0.7 ms at 1920x1080 on an Apple M5.
 var blurred=motionblur_gather(pixel,center_color,cheap,sampling_direction,center_velocity_magnitude,center_depth,random_offset,false);
 if !all_finite(blurred) {
  blurred=motionblur_gather(pixel,center_color,cheap,sampling_direction,center_velocity_magnitude,center_depth,random_offset,true);
 }
 textureStore(output,pixel,blurred);
}
