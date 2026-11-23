// Selection outlines (stages::outline). The mask pass draws the selected
// instances depth-only, unjittered, at the output size, through the indexed
// casters (caster.wgsl); `outline_rows` then finds each pixel's nearest
// covered mask pixel along its row within the reach, and whether that
// pixel is in sight, and `outline_composite` the nearest over its column,
// covering the pixel by the distance between their centres: a separable
// distance search, exact within the reach. three.js r187's OutlineNode
// (examples/jsm/tsl/display/OutlineNode.js, MIT) is the structure
// followed: a mask of only the selected objects, a depth comparison into
// visible and hidden, an edge outside the mask composited with a visible
// and a hidden colour; its half-resolution edge detect and blur are
// replaced by this distance (D-79), and nothing of its code is copied.
struct Outline {
 // sRGB-encoded colours with straight alpha.
 visible:vec4<f32>,
 hidden:vec4<f32>,
 // Output pixels, 0..=32.
 width:f32,
 // ceil(width), at most OUTLINE_REACH_MAX.
 reach:u32,
 // The render size over the output size, which maps an output pixel to
 // the opaque depth, and how far the frame's jitter moved the depth's
 // samples, in render pixels.
 depth_scale:vec2<f32>,
 depth_shift:vec2<f32>,
 padding:vec2<f32>,
}
// The reach of Outline::MAX_WIDTH (32 output pixels): the cap on the row
// and column searches (AR-12), each at most OUTLINE_REACH_MAX + 1
// iterations of two taps however the uniform is written.
const OUTLINE_REACH_MAX:u32=32u;
// A row value: the distance to the nearest covered pixel along the row in
// its low seven bits, OUTLINE_NONE where none is within the reach, and
// OUTLINE_VISIBLE where that pixel is in sight.
const OUTLINE_DISTANCE:u32=127u;
const OUTLINE_NONE:u32=127u;
const OUTLINE_VISIBLE:u32=128u;
// The relative margin in device depth (as in view depth, under reversed-Z
// device depth near / distance) within which the selection counts as in
// sight of the opaque depth it is compared with.
const OUTLINE_DEPTH_MARGIN:f32=1e-4;
// Whether the output stores what is written in the sRGB encoding (a unorm
// attachment that does not encode), so the colours are written as given;
// else they are decoded to linear for the attachment.
override OUTLINE_ENCODES_SRGB:bool=false;

@group(0) @binding(0) var<uniform> outline:Outline;
@group(0) @binding(1) var outline_mask:texture_depth_2d;
@group(0) @binding(2) var outline_scene_depth:texture_depth_2d;
@group(0) @binding(3) var outline_row_distances:texture_2d<u32>;

// Whether the selection at output pixel `p`, at mask depth `depth`, is in
// sight: at or nearer than the farthest of the four opaque-depth texels
// whose jittered samples surround the pixel's centre, which bracket the
// selection's own surface wherever the opaque depth holds it (exactly where
// it is flat), whatever the jitter and scale between the two.
fn outline_in_sight(p:vec2<i32>,depth:f32)->bool {
 let size=vec2<i32>(textureDimensions(outline_scene_depth));
 let base=vec2<i32>(floor((vec2<f32>(p)+.5)*outline.depth_scale+outline.depth_shift-.5));
 let at=clamp(base,vec2(0),size-1);
 let next=clamp(base+1,vec2(0),size-1);
 let farthest=min(
  min(textureLoad(outline_scene_depth,at,0),textureLoad(outline_scene_depth,vec2(next.x,at.y),0)),
  min(textureLoad(outline_scene_depth,vec2(at.x,next.y),0),textureLoad(outline_scene_depth,next,0)),
 );
 return depth>=farthest*(1.-OUTLINE_DEPTH_MARGIN);
}

// The row value of the mask pixel `p` at `distance` from the pixel the
// search started at, or OUTLINE_NONE where the selection does not cover it.
fn outline_covered(p:vec2<i32>,distance:u32)->u32 {
 if p.x<0||p.x>=i32(textureDimensions(outline_mask).x) {
  return OUTLINE_NONE;
 }
 let depth=textureLoad(outline_mask,p,0);
 if depth<=0. {
  return OUTLINE_NONE;
 }
 return distance|select(0u,OUTLINE_VISIBLE,outline_in_sight(p,depth));
}

@fragment fn outline_rows(@builtin(position) position:vec4<f32>)->@location(0) u32 {
 let p=vec2<i32>(position.xy);
 let reach=min(outline.reach,OUTLINE_REACH_MAX);
 for (var distance=0u;distance<=OUTLINE_REACH_MAX;distance++) {
  if distance>reach {
   break;
  }
  let left=outline_covered(p-vec2(i32(distance),0),distance);
  let right=outline_covered(p+vec2(i32(distance),0),distance);
  // At equal distances, a pixel in sight wins.
  if left!=OUTLINE_NONE||right!=OUTLINE_NONE {
   return select(left,right,left==OUTLINE_NONE||(right&OUTLINE_VISIBLE)!=0u);
  }
 }
 return OUTLINE_NONE;
}

// The nearest covered pixel found so far: its squared distance (-1 for
// none) and whether it is in sight.
struct OutlineNearest {
 squared:i32,
 in_sight:bool,
}

// `nearest`, or the covered pixel nearest pixel (x, y) along its row where
// that is nearer to the pixel `dy` rows away; at equal distances, a pixel
// in sight wins.
fn outline_nearer(nearest:OutlineNearest,x:i32,y:i32,dy:u32)->OutlineNearest {
 if y<0||y>=i32(textureDimensions(outline_row_distances).y) {
  return nearest;
 }
 let row=textureLoad(outline_row_distances,vec2(x,y),0).r;
 let dx=row&OUTLINE_DISTANCE;
 if dx==OUTLINE_NONE {
  return nearest;
 }
 let squared=i32(dx*dx+dy*dy);
 let in_sight=(row&OUTLINE_VISIBLE)!=0u;
 if nearest.squared<0||squared<nearest.squared||(squared==nearest.squared&&in_sight) {
  return OutlineNearest(squared,in_sight);
 }
 return nearest;
}

@fragment fn outline_composite(@builtin(position) position:vec4<f32>)->@location(0) vec4<f32> {
 let p=vec2<i32>(position.xy);
 let reach=min(outline.reach,OUTLINE_REACH_MAX);
 // Searched outward from the pixel's own row: a covered pixel is inside
 // the selection, which the outline leaves alone, and a row farther than
 // the nearest so far cannot hold a nearer one.
 if (textureLoad(outline_row_distances,p,0).r&OUTLINE_DISTANCE)==0u {
  discard;
 }
 var nearest=outline_nearer(OutlineNearest(-1,false),p.x,p.y,0u);
 for (var dy=1u;dy<=OUTLINE_REACH_MAX;dy++) {
  if dy>reach||(nearest.squared>=0&&i32(dy*dy)>nearest.squared) {
   break;
  }
  nearest=outline_nearer(nearest,p.x,p.y-i32(dy),dy);
  nearest=outline_nearer(nearest,p.x,p.y+i32(dy),dy);
 }
 if nearest.squared<0 {
  discard;
 }
 let coverage=saturate(outline.width+1.-sqrt(f32(nearest.squared)));
 if coverage<=0. {
  discard;
 }
 let color=select(outline.hidden,outline.visible,nearest.in_sight);
 let rgb=select(srgb_to_linear(color.rgb),color.rgb,OUTLINE_ENCODES_SRGB);
 return vec4(rgb,color.a*coverage);
}
