// Depth of field: Wicked Engine's tiled scatter-as-gather, after Jimenez 2014
// ("Next Generation Post Processing in Call of Duty: Advanced Warfare"), over
// the antialiased frame (stages/depth_of_field.rs), with Bevy's thin-lens
// circle of confusion.
//
// Ports Wicked Engine 4323a33 WickedEngine/shaders/depthOfFieldHF.hlsli,
// depthoffield_tileMaxCOC_horizontalCS.hlsl,
// depthoffield_tileMaxCOC_verticalCS.hlsl,
// depthoffield_neighborhoodMaxCOCCS.hlsl, depthoffield_prepassCS.hlsl (and
// its _earlyexit variant), depthoffield_mainCS.hlsl (and its _cheap and
// _earlyexit variants), depthoffield_postfilterCS.hlsl and
// depthoffield_upsampleCS.hlsl, with wiRenderer.cpp's
// Postprocess_DepthOfField, MIT (src/LICENSE-wicked.txt). The circle of
// confusion is Bevy 706fdd9 (v0.17.3) crates/bevy_post_process/src/dof/
// dof.wgsl calculate_circle_of_confusion, with mod.rs's focal length,
// MIT OR Apache-2.0 (src/LICENSE-bevy.txt). Changes:
// - The circle of confusion is the thin lens's (Bevy), as a radius in
//   pixels of the half-size gather: coc_scale |z - focus| / z, z the view
//   depth in metres, at most DOF_MAX_COC. Wicked squares |1 - focus / z| and
//   scales it by a strength, capped at 18.
// - DOF_MAX_COC is half a tile (one tile of the scene's pixels), the reach of
//   the 3x3 tile neighbourhood, as McGuire et al. 2012 and the ported motion
//   blur clamp to the tile size.
// - Depth is linear view depth in metres (shading/depth.wgsl), at most
//   DOF_MAX_DEPTH so the sky's stays finite in a half float. Wicked's is
//   linear depth over its far plane: its depth scales (z_far * 1.5 in
//   DepthCmp2, z_far * 2 in the prepass, z_far in the upsample) become 1.5,
//   2 and 1 per metre.
// - The gather's rings: the level's (DepthOfField.rings: 2, 3 or 4 of
//   Wicked's 4-ring disc), fewer in a tile whose largest radius is under
//   that many half-size pixels (floor), so its rings stay at least a pixel
//   apart. Wicked counts ceil(sqrt(maxcoc) * 3) clamped to 3, which is
//   always 3 where the gather runs (maxcoc >= 1), and its kernel places
//   ring k at k/4 of the radius while its spread test takes ring k at k/3:
//   here ring k of n lies at k/n of the tile's largest radius, where
//   SpreadCmp takes it. Measured against Wicked's (three rings at k/4):
//   an edge 18.7 px of thin-lens radius away from focus spreads by 0.88 of
//   that radius with Wicked's and 1.16 with these (a uniform disc's is 1;
//   scenario_depth_of_field), and on the water example's dof-medium run at
//   1920x1080 on an Apple M5 the stage took 0.95 ms with Wicked's and
//   0.91-0.92 ms with these.
// - The prepass's ring taps read depth at the render-size texel under each
//   tap; Wicked reads its linear depth's half-size mip, which SGL3D does
//   not keep.
// - SampleAlpha takes the radius at least DOF_SINGLE_PIXEL_RADIUS before
//   dividing, the same value as Wicked's min of the two reciprocals
//   without dividing by zero at the plane of focus.
// - The frame's colour is read through finite_scene_color
//   (shading/scene_color.wgsl), as motion blur's is: one texel of +infinity
//   would make every gather that reaches it infinite or NaN.
// - One dispatch runs every tile, and each workgroup (inside one tile) takes
//   its tile's form from the tile neighbourhood: under a pixel of blur
//   copies (early exit), circles of confusion within a pixel of each other
//   average (cheap), else the weighted gather. Wicked lists tiles for three
//   indirect dispatches, as the ported motion blur also does not.
// - The main pass's alpha rides in its target's alpha, and the postfilter's
//   in its own, in rgba16float storage targets (WebGPU has no storage
//   R11G11B10 or R8); tiles hold (smallest depth, largest and smallest
//   circle of confusion) of their pixels, or neighbourhood, in one target.
// - The postfilter's median is of the 3x3 half-size pixels its circle of
//   offsets point-samples, which Smith 1996's network gives in any order.
// - Every loop runs a fixed count (a tile's DOF_TILESIZE pixels, 3x3 tiles,
//   DOF_PREFILTER_TAPS) or at most DOF_MAX_SAMPLES, the 4-ring disc, whatever
//   the uniform says (AR-12).

struct DepthOfField {
 // The perspective projection's near plane.
 near:f32,
 // Circle-of-confusion radius, in half-size pixels, per unit of
 // |z - focus| / z.
 coc_scale:f32,
 // Metres along the view to the plane in focus.
 focus:f32,
 // The level's rings.
 rings:u32,
 // The depth target's size over the frame's: depth texels per frame pixel.
 render_scale:vec2<f32>,
}
@group(0) @binding(0) var<uniform> settings:DepthOfField;
// The surface's depth at the render size.
@group(0) @binding(1) var texture_depth:texture_depth_2d;
// The antialiased frame, at the scene size.
@group(0) @binding(2) var input:texture_2d<f32>;
// The previous tile pass's tiles, or the tile neighbourhoods: smallest
// depth, largest and smallest circle of confusion.
@group(0) @binding(3) var tiles:texture_2d<f32>;
@group(0) @binding(4) var output:texture_storage_2d<rgba16float,write>;
@group(0) @binding(5) var output_b:texture_storage_2d<rgba16float,write>;
// The prepass's: circle of confusion, background and foreground weights.
@group(0) @binding(6) var texture_presort:texture_2d<f32>;
// The prepass's prefiltered colour.
@group(0) @binding(7) var texture_prefilter:texture_2d<f32>;
// The main pass's colour and alpha.
@group(0) @binding(8) var texture_main:texture_2d<f32>;
// The postfilter's colour and alpha.
@group(0) @binding(9) var texture_postfilter:texture_2d<f32>;
@group(0) @binding(10) var sampler_linear_clamp:sampler;

// Frame pixels per tile side (stages/depth_of_field.rs TILE_SIZE).
override DOF_TILESIZE:u32;

// The largest circle of confusion, in half-size pixels: one tile of the
// frame.
fn dof_max_coc()->f32 {
 return 0.5*f32(DOF_TILESIZE);
}
// The farthest depth, in metres, the passes keep.
const DOF_MAX_DEPTH:f32=60000.0;
// The prepass's ring: the disc's first.
const DOF_PREFILTER_TAPS:u32=8u;
const DOF_MAX_RINGS:u32=4u;
// The disc's samples, all four rings.
const DOF_MAX_SAMPLES:u32=80u;

// Linear view depth in metres at frame position `position` (frame pixels),
// edge texels beyond it.
fn dof_depth(position:vec2<f32>)->f32 {
 let dimensions=textureDimensions(texture_depth);
 let texel=min(vec2<u32>(max(position*settings.render_scale,vec2(0.0))),dimensions-1u);
 return min(linear_depth(settings.near,textureLoad(texture_depth,texel,0)),DOF_MAX_DEPTH);
}

// Bevy's thin-lens circle of confusion, as a radius in half-size pixels.
fn get_coc(linear_depth:f32)->f32 {
 return min(dof_max_coc(),settings.coc_scale*abs(linear_depth-settings.focus)/linear_depth);
}

// The frame's colour at frame position `position`, bilinear, read through
// finite_scene_color.
fn dof_input(position:vec2<f32>)->vec3<f32> {
 let uv=position/vec2<f32>(textureDimensions(input));
 return max(vec3(0.0),finite_scene_color(textureSampleLevel(input,sampler_linear_clamp,uv,0.0)).rgb);
}

// The frame's texel at `pixel`, edge texels beyond it, read through
// finite_scene_color.
fn dof_input_texel(pixel:vec2<u32>)->vec3<f32> {
 let texel=min(pixel,textureDimensions(input)-1u);
 return max(vec3(0.0),finite_scene_color(textureLoad(input,texel,0)).rgb);
}

fn DepthCmp2(depth:f32,closestTileDepth:f32)->vec2<f32> {
 let d=1.5*(depth-closestTileDepth);
 let background=smoothstep(0.0,1.0,d);
 return vec2(background,1.0-background);
}

const DOF_SINGLE_PIXEL_RADIUS:f32=0.7071;
const DOF_PI:f32=3.14159265358979;
fn SampleAlpha(sampleCoc:f32)->f32 {
 let radius=max(sampleCoc,DOF_SINGLE_PIXEL_RADIUS);
 return 1.0/(DOF_PI*radius*radius);
}

fn SpreadToe(offsetCoc:f32,spreadCmp:f32)->f32 {
 return select(spreadCmp,spreadCmp*spreadCmp,offsetCoc<=1.0);
}
fn SpreadCmp(offsetCoc:f32,sampleCoc:f32,pixelToSampleUnitsScale:f32)->f32 {
 return SpreadToe(offsetCoc,saturate(pixelToSampleUnitsScale*sampleCoc-offsetCoc+1.0));
}

// Wicked's disc: each sample's offset at the 4-ring radius 1, and its ring.
const disc=array<vec3<f32>,80>(
 vec3( 0.2310,-0.0957,1.0),vec3(-0.2310, 0.0957,1.0),vec3(-0.2310,-0.0957,1.0),vec3( 0.2310, 0.0957,1.0),
 vec3(-0.0957,-0.2310,1.0),vec3( 0.0957, 0.2310,1.0),vec3( 0.0957,-0.2310,1.0),vec3(-0.0957, 0.2310,1.0),
 vec3(-0.3536, 0.3536,2.0),vec3( 0.3536,-0.3536,2.0),vec3( 0.4619,-0.1913,2.0),vec3(-0.4619, 0.1913,2.0),
 vec3(-0.5000, 0.0000,2.0),vec3( 0.5000, 0.0000,2.0),vec3(-0.4619,-0.1913,2.0),vec3( 0.4619, 0.1913,2.0),
 vec3(-0.3536,-0.3536,2.0),vec3( 0.3536, 0.3536,2.0),vec3(-0.1913,-0.4619,2.0),vec3( 0.1913, 0.4619,2.0),
 vec3(-0.0000,-0.5000,2.0),vec3( 0.0000, 0.5000,2.0),vec3( 0.1913,-0.4619,2.0),vec3(-0.1913, 0.4619,2.0),
 vec3( 0.4566,-0.5950,3.0),vec3(-0.4566, 0.5950,3.0),vec3(-0.5950, 0.4566,3.0),vec3( 0.5950,-0.4566,3.0),
 vec3( 0.6929,-0.2870,3.0),vec3(-0.6929, 0.2870,3.0),vec3(-0.7436, 0.0979,3.0),vec3( 0.7436,-0.0979,3.0),
 vec3(-0.7436,-0.0979,3.0),vec3( 0.7436, 0.0979,3.0),vec3(-0.6929,-0.2870,3.0),vec3( 0.6929, 0.2870,3.0),
 vec3(-0.5950,-0.4566,3.0),vec3( 0.5950, 0.4566,3.0),vec3( 0.4566, 0.5950,3.0),vec3(-0.4566,-0.5950,3.0),
 vec3(-0.2870,-0.6929,3.0),vec3( 0.2870, 0.6929,3.0),vec3(-0.0979,-0.7436,3.0),vec3( 0.0979, 0.7436,3.0),
 vec3(-0.0979, 0.7436,3.0),vec3( 0.0979,-0.7436,3.0),vec3( 0.2870,-0.6929,3.0),vec3(-0.2870, 0.6929,3.0),
 vec3(-0.5556, 0.8315,4.0),vec3( 0.5556,-0.8315,4.0),vec3(-0.7071, 0.7071,4.0),vec3( 0.7071,-0.7071,4.0),
 vec3( 0.8315,-0.5556,4.0),vec3(-0.8315, 0.5556,4.0),vec3( 0.9239,-0.3827,4.0),vec3(-0.9239, 0.3827,4.0),
 vec3( 0.9808,-0.1951,4.0),vec3(-0.9808, 0.1951,4.0),vec3(-1.0000, 0.0000,4.0),vec3( 1.0000, 0.0000,4.0),
 vec3(-0.9808,-0.1951,4.0),vec3( 0.9808, 0.1951,4.0),vec3(-0.9239,-0.3827,4.0),vec3( 0.9239, 0.3827,4.0),
 vec3( 0.8315, 0.5556,4.0),vec3(-0.8315,-0.5556,4.0),vec3(-0.7071,-0.7071,4.0),vec3( 0.7071, 0.7071,4.0),
 vec3(-0.5556,-0.8315,4.0),vec3( 0.5556, 0.8315,4.0),vec3(-0.3827,-0.9239,4.0),vec3( 0.3827, 0.9239,4.0),
 vec3( 0.1951, 0.9808,4.0),vec3(-0.1951,-0.9808,4.0),vec3(-0.0000,-1.0000,4.0),vec3( 0.0000, 1.0000,4.0),
 vec3( 0.1951,-0.9808,4.0),vec3(-0.1951, 0.9808,4.0),vec3( 0.3827,-0.9239,4.0),vec3(-0.3827, 0.9239,4.0),
);
const ringSampleCount=array<u32,5>(0u,8u,24u,48u,80u);
const ringNormFactor=array<f32,5>(1.0,0.111111,0.040000,0.020408,0.012346);

// The tile pass over one row of a tile column: its pixels' smallest depth
// and largest and smallest circle of confusion.
@compute @workgroup_size(8,8,1)
fn dof_tile_horizontal(@builtin(global_invocation_id) DTid:vec3<u32>) {
 if any(DTid.xy>=textureDimensions(output)) {
  return;
 }
 let size=textureDimensions(input);
 var min_depth=DOF_MAX_DEPTH;
 var max_coc=0.0;
 var min_coc=1000000.0;
 for (var i=0u;i<DOF_TILESIZE;i+=1u) {
  let pixel=vec2(DTid.x*DOF_TILESIZE+i,DTid.y);
  if all(pixel<size) {
   let depth=dof_depth(vec2<f32>(pixel)+0.5);
   let coc=get_coc(depth);
   min_depth=min(min_depth,depth);
   max_coc=max(max_coc,coc);
   min_coc=min(min_coc,coc);
  }
 }
 textureStore(output,DTid.xy,vec4(min_depth,max_coc,min_coc,0.0));
}

// The tile pass down a tile column's rows.
@compute @workgroup_size(8,8,1)
fn dof_tile_vertical(@builtin(global_invocation_id) DTid:vec3<u32>) {
 if any(DTid.xy>=textureDimensions(output)) {
  return;
 }
 let dimensions=textureDimensions(tiles);
 var min_depth=DOF_MAX_DEPTH;
 var max_coc=0.0;
 var min_coc=1000000.0;
 for (var i=0u;i<DOF_TILESIZE;i+=1u) {
  let pixel=vec2(DTid.x,DTid.y*DOF_TILESIZE+i);
  if all(pixel<dimensions) {
   let tile=textureLoad(tiles,pixel,0);
   min_depth=min(min_depth,tile.x);
   max_coc=max(max_coc,tile.y);
   min_coc=min(min_coc,tile.z);
  }
 }
 textureStore(output,DTid.xy,vec4(min_depth,max_coc,min_coc,0.0));
}

// Each tile's 3x3 neighbourhood.
@compute @workgroup_size(8,8,1)
fn dof_neighborhood(@builtin(global_invocation_id) DTid:vec3<u32>) {
 if any(DTid.xy>=textureDimensions(output)) {
  return;
 }
 let dimensions=vec2<i32>(textureDimensions(tiles));
 var min_depth=DOF_MAX_DEPTH;
 var max_coc=0.0;
 var min_coc=1000000.0;
 for (var x=-1;x<=1;x+=1) {
  for (var y=-1;y<=1;y+=1) {
   let pixel=vec2<i32>(DTid.xy)+vec2(x,y);
   if all(pixel>=vec2(0)) && all(pixel<dimensions) {
    let tile=textureLoad(tiles,pixel,0);
    min_depth=min(min_depth,tile.x);
    max_coc=max(max_coc,tile.y);
    min_coc=min(min_coc,tile.z);
   }
  }
 }
 textureStore(output,DTid.xy,vec4(min_depth,max_coc,min_coc,0.0));
}

// The tile neighbourhood of half-size pixel `pixel`.
fn dof_neighborhood_of(pixel:vec2<u32>)->vec4<f32> {
 return textureLoad(tiles,min(2u*pixel/DOF_TILESIZE,textureDimensions(tiles)-1u),0);
}

// Presorts and prefilters each half-size pixel.
@compute @workgroup_size(8,8,1)
fn dof_prepass(@builtin(global_invocation_id) DTid:vec3<u32>) {
 let pixel=DTid.xy;
 if any(pixel>=textureDimensions(output)) {
  return;
 }
 // The half-size pixel's centre, in frame pixels: the corner its 2x2 share.
 let position=2.0*(vec2<f32>(pixel)+0.5);
 let neighborhood=dof_neighborhood_of(pixel);
 // Wicked's point sample at the half-size pixel's centre: the frame pixel
 // below and right of that corner, whose depth the presort takes too.
 let center_color=dof_input_texel(2u*pixel+1u);
 var prefilter=center_color;
 // DEPTHOFFIELD_EARLYEXIT: nothing reaches the tile; the presort is
 // Wicked's cleared target.
 if neighborhood.y<1.0 {
  textureStore(output,pixel,vec4(0.0));
  textureStore(output_b,pixel,vec4(prefilter,1.0));
  return;
 }
 let mindepth=neighborhood.x;
 let center_depth=dof_depth(position+0.5);
 let coc=get_coc(center_depth);
 let alpha=SampleAlpha(coc);
 let depthcmp=DepthCmp2(center_depth,mindepth);
 let backgroundFactor=alpha*depthcmp.x;
 let foregroundFactor=alpha*depthcmp.y;
 textureStore(output,pixel,vec4(coc,backgroundFactor,foregroundFactor,0.0));
 // Wicked's `if (backgroundFactor < 1)`, always true (SampleAlpha is at
 // most 0.64), is left out. In half-size pixels:
 let ringScale=0.5*coc;
 for (var i=0u;i<DOF_PREFILTER_TAPS;i+=1u) {
  let sample_position=position+2.0*ringScale*disc[i].xy;
  let depth=dof_depth(sample_position);
  let color=dof_input(sample_position);
  let weight=saturate(abs(depth-center_depth)*2.0);
  prefilter+=mix(color,center_color,weight);
 }
 prefilter*=ringNormFactor[1];
 textureStore(output_b,pixel,vec4(prefilter,1.0));
}

// The half-size texel at `position` (half-size pixels), edge texels beyond.
fn dof_texel(position:vec2<f32>)->vec2<u32> {
 let dimensions=textureDimensions(texture_prefilter);
 return min(vec2<u32>(max(position,vec2(0.0))),dimensions-1u);
}

// The near and far gather of each half-size pixel.
@compute @workgroup_size(8,8,1)
fn dof_main(@builtin(global_invocation_id) DTid:vec3<u32>) {
 let pixel=DTid.xy;
 let dimensions=textureDimensions(output);
 if any(pixel>=dimensions) {
  return;
 }
 let neighborhood=dof_neighborhood_of(pixel);
 let maxcoc=neighborhood.y;
 let center_color=textureLoad(texture_prefilter,pixel,0).rgb;
 // DEPTHOFFIELD_EARLYEXIT.
 if maxcoc<1.0 {
  textureStore(output,pixel,vec4(center_color,1.0));
  return;
 }
 let ringCount=clamp(u32(floor(maxcoc)),1u,min(settings.rings,DOF_MAX_RINGS));
 let samples=ringSampleCount[ringCount];
 let spreadScale=f32(ringCount)/maxcoc;
 // Ring k of ringCount at k/ringCount of maxcoc: the disc's ring k lies at
 // k/4 of its radius.
 let ringScale=f32(DOF_MAX_RINGS)/f32(ringCount)*maxcoc;
 let center=vec2<f32>(pixel)+0.5;
 let uv_scale=1.0/vec2<f32>(dimensions);
 var color:vec3<f32>;
 var alpha:f32;
 // DEPTHOFFIELD_CHEAP: circles within a pixel of each other average.
 if maxcoc-neighborhood.z<1.0 {
  color=center_color;
  for (var i=0u;i<DOF_MAX_SAMPLES;i+=1u) {
   if i>=samples {
    break;
   }
   let uv=(center+ringScale*disc[i].xy)*uv_scale;
   color+=textureSampleLevel(texture_prefilter,sampler_linear_clamp,uv,0.0).rgb;
  }
  color*=ringNormFactor[ringCount];
  alpha=0.0;
 } else {
  let center_presort=textureLoad(texture_presort,pixel,0).rgb;
  var background=center_presort.g*vec4(center_color,1.0);
  var foreground=center_presort.b*vec4(center_color,1.0);
  for (var i=0u;i<DOF_MAX_SAMPLES;i+=1u) {
   if i>=samples {
    break;
   }
   let offsetCoc=disc[i].z;
   let texel=dof_texel(center+ringScale*disc[i].xy);
   let sample_color=vec4(textureLoad(texture_prefilter,texel,0).rgb,1.0);
   let presort=textureLoad(texture_presort,texel,0).rgb;
   let spreadCmp=SpreadCmp(offsetCoc,presort.r,spreadScale);
   background+=presort.g*spreadCmp*sample_color;
   foreground+=presort.b*spreadCmp*sample_color;
  }
  background=vec4(background.rgb/max(0.00001,background.a),background.a);
  foreground=vec4(foreground.rgb/max(0.00001,foreground.a),foreground.a);
  alpha=saturate(ringNormFactor[ringCount]/SampleAlpha(maxcoc)*foreground.a);
  color=mix(background.rgb,foreground.rgb,alpha);
 }
 textureStore(output,pixel,vec4(color,alpha));
}

fn med3(a:vec4<f32>,b:vec4<f32>,c:vec4<f32>)->vec4<f32> {
 return max(min(a,b),min(max(a,b),c));
}

// The median of each half-size pixel's 3x3 neighbourhood, colour and alpha
// apart (Smith 1996's network of three-element sorts).
@compute @workgroup_size(8,8,1)
fn dof_postfilter(@builtin(global_invocation_id) DTid:vec3<u32>) {
 let pixel=DTid.xy;
 let dimensions=textureDimensions(output);
 if any(pixel>=dimensions) {
  return;
 }
 var p:array<vec4<f32>,9>;
 for (var i=0u;i<9u;i+=1u) {
  let offset=vec2<i32>(i32(i%3u)-1,i32(i/3u)-1);
  let texel=clamp(vec2<i32>(pixel)+offset,vec2(0),vec2<i32>(dimensions)-1);
  p[i]=textureLoad(texture_main,texel,0);
 }
 // Sort each row of three.
 for (var row=0u;row<3u;row+=1u) {
  let a=p[3u*row];
  let b=p[3u*row+1u];
  let c=p[3u*row+2u];
  p[3u*row]=min(min(a,b),c);
  p[3u*row+1u]=med3(a,b,c);
  p[3u*row+2u]=max(max(a,b),c);
 }
 // The largest of the rows' smallest, the smallest of their largest and
 // the median of their medians; their median is the nine's.
 let low=max(max(p[0],p[3]),p[6]);
 let high=min(min(p[2],p[5]),p[8]);
 let middle=med3(p[1],p[4],p[7]);
 textureStore(output,pixel,med3(low,middle,high));
}

// The focused frame: each frame pixel between its own colour and the
// half-size blur, by its circle of confusion, or its neighbourhood's in
// front of what lies behind it.
@compute @workgroup_size(8,8,1)
fn dof_upsample(@builtin(global_invocation_id) DTid:vec3<u32>) {
 let pixel=DTid.xy;
 if any(pixel>=textureDimensions(output)) {
  return;
 }
 let position=vec2<f32>(pixel)+0.5;
 let center_depth=dof_depth(position);
 let mindepth_maxcoc=textureLoad(tiles,min(pixel/DOF_TILESIZE,textureDimensions(tiles)-1u),0);
 let mindepth=mindepth_maxcoc.x;
 let maxcoc=mindepth_maxcoc.y;
 let fullres=finite_scene_color(textureLoad(input,pixel,0));
 let half_uv=0.5*position/vec2<f32>(textureDimensions(texture_postfilter));
 let halfres=textureSampleLevel(texture_postfilter,sampler_linear_clamp,half_uv,0.0);
 let alpha=halfres.a;
 let coc=get_coc(center_depth);
 let depthDelta=saturate(1.0-(center_depth-mindepth));
 let backgroundFactor=coc;
 let foregroundFactor=mix(maxcoc,coc,depthDelta);
 let combinedFactor=saturate(mix(backgroundFactor,foregroundFactor,alpha));
 textureStore(output,pixel,vec4(mix(fullres.rgb,halfres.rgb,combinedFactor),fullres.a));
}
