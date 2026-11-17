// The motion's tile maximum and its 3x3 tile neighbourhood maximum:
// Wicked Engine's motion blur tile passes, after Jimenez 2014 ("Next
// Generation Post Processing in Call of Duty: Advanced Warfare") and McGuire
// et al. 2012 ("A Reconstruction Filter for Plausible Motion Blur"). Motion
// blur (stages/motion_blur.wgsl) reduces its blur vectors with them at the
// scene size after antialiasing, and TAA's motion reach
// (stages/antialiasing/reach.rs) the frame's motion in pixels at the render
// size before it, each on its own targets (view/motion_tiles.rs).
//
// Ports Wicked Engine 4323a33 WickedEngine/shaders/
// motionblur_tileMaxVelocity_horizontalCS.hlsl,
// motionblur_tileMaxVelocity_verticalCS.hlsl and
// motionblur_neighborhoodMaxVelocityCS.hlsl, MIT (src/LICENSE-wicked.txt).
// Changes:
// - Velocities are the motion target's times `velocity_scale`, in pixels of
//   the frame being tiled: blur vectors for motion blur (the frame's size
//   times the shutter), the motion in pixels for the reach (the render
//   size). Wicked scales UV velocity by its strength and the reciprocal
//   resolution.
// - Each velocity is at most two tiles long, as McGuire et al. 2012 clamp
//   each velocity's radius to the tile size: a 3x3 tile neighbourhood
//   carries a velocity one tile. Wicked does not clamp.
// - Each tile texture holds the largest velocity in xy and the smallest in
//   zw; tiles beyond the frame's edge are its edge tiles, and samples beyond
//   it its edge pixels.
// - Each loop runs a tile's MOTIONBLUR_TILESIZE pixels or its 3x3 tiles,
//   pipeline-overridable constants set from Rust ones (AR-12).
struct MotionTiles {
 // Frame pixels per unit of the motion target.
 velocity_scale:vec2<f32>,
 // The motion target's size over the frame's: motion texels per frame
 // pixel.
 render_scale:vec2<f32>,
}
@group(0) @binding(0) var<uniform> motion_tiles:MotionTiles;
@group(0) @binding(1) var texture_velocity:texture_2d<f32>;
// The frame being tiled, whose size the tiles cover.
@group(0) @binding(3) var input:texture_2d<f32>;
// The previous tile pass's result.
@group(0) @binding(4) var tiles:texture_2d<f32>;
// This pass's tiles.
@group(0) @binding(5) var output:texture_storage_2d<rgba16float,write>;

// Frame pixels per tile side (shading/motion_tiles.rs TILE_SIZE).
override MOTIONBLUR_TILESIZE:u32;

// The blur vector at `pixel` of the frame, edge pixels beyond it, at most
// two tiles long.
fn motionblur_velocity(pixel:vec2<i32>)->vec2<f32> {
 let frame_pixel=clamp(pixel,vec2(0),vec2<i32>(textureDimensions(input))-1);
 let texel=min(vec2<u32>((vec2<f32>(frame_pixel)+0.5)*motion_tiles.render_scale),textureDimensions(texture_velocity)-1u);
 let velocity=textureLoad(texture_velocity,texel,0).xy*motion_tiles.velocity_scale;
 let longest=2.0*f32(MOTIONBLUR_TILESIZE);
 return velocity*min(1.0,longest/max(length(velocity),1e-30));
}

@compute @workgroup_size(8,8,1)
fn motionblur_tileMaxVelocity_horizontal(@builtin(global_invocation_id) DTid:vec3<u32>) {
 if any(DTid.xy>=textureDimensions(output)) {
  return;
 }
 let tile_upperleft=vec2(DTid.x*MOTIONBLUR_TILESIZE,DTid.y);
 var max_magnitude=0.0;
 var max_velocity=vec2(0.0);
 var min_magnitude=100000.0;
 var min_velocity=vec2(100000.0);
 for (var i=0u;i<MOTIONBLUR_TILESIZE;i+=1u) {
  let pixel=vec2(tile_upperleft.x+i,tile_upperleft.y);
  let velocity=motionblur_velocity(vec2<i32>(pixel));
  let magnitude=length(velocity);
  if magnitude>max_magnitude {
   max_magnitude=magnitude;
   max_velocity=velocity;
  }
  if magnitude<min_magnitude {
   min_magnitude=magnitude;
   min_velocity=velocity;
  }
 }
 textureStore(output,DTid.xy,vec4(max_velocity,min_velocity));
}

@compute @workgroup_size(8,8,1)
fn motionblur_tileMaxVelocity_vertical(@builtin(global_invocation_id) DTid:vec3<u32>) {
 if any(DTid.xy>=textureDimensions(output)) {
  return;
 }
 let tile_upperleft=vec2(DTid.x,DTid.y*MOTIONBLUR_TILESIZE);
 let last_row=textureDimensions(tiles).y-1u;
 var max_magnitude=0.0;
 var max_velocity=vec2(0.0);
 var min_magnitude=100000.0;
 var min_velocity=vec2(100000.0);
 for (var i=0u;i<MOTIONBLUR_TILESIZE;i+=1u) {
  let pixel=vec2(tile_upperleft.x,min(tile_upperleft.y+i,last_row));
  let tile=textureLoad(tiles,pixel,0);
  var velocity=tile.xy;
  var magnitude=length(velocity);
  if magnitude>max_magnitude {
   max_magnitude=magnitude;
   max_velocity=velocity;
  }
  velocity=tile.zw;
  magnitude=length(velocity);
  if magnitude<min_magnitude {
   min_magnitude=magnitude;
   min_velocity=velocity;
  }
 }
 textureStore(output,DTid.xy,vec4(max_velocity,min_velocity));
}

@compute @workgroup_size(8,8,1)
fn motionblur_neighborhoodMaxVelocity(@builtin(global_invocation_id) DTid:vec3<u32>) {
 let dim=textureDimensions(output);
 if any(DTid.xy>=dim) {
  return;
 }
 var max_magnitude=0.0;
 var max_velocity=vec2(0.0);
 var min_magnitude=100000.0;
 var min_velocity=vec2(100000.0);
 for (var x=-1;x<=1;x+=1) {
  for (var y=-1;y<=1;y+=1) {
   let tile=textureLoad(tiles,clamp(vec2<i32>(DTid.xy)+vec2(x,y),vec2(0),vec2<i32>(dim)-1),0);
   var velocity=tile.xy;
   var magnitude=length(velocity);
   if magnitude>max_magnitude {
    max_magnitude=magnitude;
    max_velocity=velocity;
   }
   velocity=tile.zw;
   magnitude=length(velocity);
   if magnitude<min_magnitude {
    min_magnitude=magnitude;
    min_velocity=velocity;
   }
  }
 }
 textureStore(output,DTid.xy,vec4(max_velocity,min_velocity));
}
