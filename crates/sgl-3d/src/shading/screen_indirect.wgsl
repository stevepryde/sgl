// Screen-space indirect light's kept result (the architecture's
// Screen-space indirect light): its layout, the similarity test that
// chooses the texels a point takes from it, and the read. The opaque
// stage's pass writes it and reads it back as it accumulates
// (stages/opaque/screen_indirect.wgsl); the next frame's camera lighting
// reads it through camera_screen_indirect.wgsl. It binds nothing: its
// callers pass the kept result and the view it was kept in.
//
// The kept result is one Rgba32Uint texture at half the render size,
// rounded up, so a tap is one load. Each texel stands for its receiver, the
// full-size pixel at its upper left, and holds, as pairs of half floats, the
// light as irradiance / PI in rgb and the coverage, and the receiver's
// positive view depth and the frames accumulated (0: nothing kept); and the
// receiver's normal in the view space of the frame that kept it, octahedral
// as the G-buffer's (gbuffer_octahedral_encode) in two signed 16-bit
// coordinates (screen_indirect_pack).
// Bevy 9d12036 crates/bevy_solari/src/realtime/gbuffer_utils.wesl
// pixel_dissimilar (38–44; MIT, src/LICENSE-bevy.txt): two points are
// dissimilar where the distance of one from the other's tangent plane, over
// its view depth, exceeds 0.003, or their normals oppose.
const SCREEN_INDIRECT_PLANE_DISTANCE:f32=.003;
// The least sum of the passing taps' bilinear weights that keeps anything:
// DiligentFX's TotalWeight > 0.01, as ambient occlusion's history (AO-6).
const SCREEN_INDIRECT_LEAST_WEIGHT:f32=.01;
// A read's bilinear footprint, the taps its loop walks (AR-12).
const SCREEN_INDIRECT_TAPS:u32=4u;
// The light and coverage a point takes.
struct ScreenIndirect {
 light:vec3<f32>,
 coverage:f32,
}
fn screen_indirect_none()->ScreenIndirect {
 return ScreenIndirect(vec3(0.),0.);
}
// A kept texel's contents.
struct ScreenIndirectKept {
 // Light in rgb, coverage in a.
 value:vec4<f32>,
 depth:f32,
 count:f32,
 // In the view space of the frame that kept it.
 normal:vec3<f32>,
}
fn screen_indirect_pack(kept:ScreenIndirectKept)->vec4<u32> {
 return vec4(pack2x16float(kept.value.rg),pack2x16float(kept.value.ba),pack2x16float(vec2(kept.depth,kept.count)),pack2x16snorm(gbuffer_octahedral_encode(kept.normal)));
}
fn screen_indirect_unpack(texel:vec4<u32>)->ScreenIndirectKept {
 let geometry=unpack2x16float(texel.z);
 return ScreenIndirectKept(vec4(unpack2x16float(texel.x),unpack2x16float(texel.y)),geometry.x,geometry.y,gbuffer_octahedral_decode(unpack2x16snorm(texel.w)));
}
// The camera of the frame the kept result was written in, the last finished
// frame's, in this frame's render frame: the rotation from world space to
// its view space (x right, y up, -z forward), its projection's x and y
// scales, and the render size the kept result was written at. Rust layout:
// stages::opaque::screen_indirect::KeptView.
struct ScreenIndirectView {
 rotation:mat3x3<f32>,
 projection_scale:vec2<f32>,
 render_size:vec2<f32>,
}
// The point at `uv` (y down) and positive view depth `depth` of `view`, in
// its view space. A centred projection: the feature runs under no other.
fn screen_indirect_position(view:ScreenIndirectView,uv:vec2<f32>,depth:f32)->vec3<f32> {
 let ndc=vec2(uv.x*2.-1.,1.-uv.y*2.);
 return vec3(ndc/view.projection_scale*depth,-depth);
}
// Whether `other`, with `other_normal`, may stand for `position`, with
// `normal`, all in one view space: pixel_dissimilar's negation, the plane
// `normal`'s and the depth `position`'s.
fn screen_indirect_similar(position:vec3<f32>,normal:vec3<f32>,other:vec3<f32>,other_normal:vec3<f32>)->bool {
 let plane=abs(dot(normal,other-position));
 return plane<=SCREEN_INDIRECT_PLANE_DISTANCE*(-position.z) && dot(normal,other_normal)>=0.;
}
// What the kept result holds for a point at `uv` of `view` that lies at
// `position` with `normal` in its view space: the light and coverage
// (`value`) and the frames accumulated (`count`) of the texels about it
// whose count is above 0 and that pass the similarity test, their bilinear
// weights renormalised, and the sum of those weights (`weight`); all 0 where
// that sum is SCREEN_INDIRECT_LEAST_WEIGHT or less.
struct ScreenIndirectTaps {
 value:vec4<f32>,
 count:f32,
 weight:f32,
}
fn screen_indirect_taps(kept:texture_2d<u32>,view:ScreenIndirectView,uv:vec2<f32>,position:vec3<f32>,normal:vec3<f32>)->ScreenIndirectTaps {
 let size=vec2<i32>(textureDimensions(kept));
 // Texel k's receiver is the full-size pixel 2k.
 let at=(uv*view.render_size-.5)*.5;
 let base=vec2<i32>(floor(at));
 let f=at-floor(at);
 let weights=vec4((1.-f.x)*(1.-f.y),f.x*(1.-f.y),(1.-f.x)*f.y,f.x*f.y);
 var taps=ScreenIndirectTaps(vec4(0.),0.,0.);
 for (var i=0u;i<SCREEN_INDIRECT_TAPS;i++) {
  let texel=base+vec2(i32(i&1u),i32(i>>1u));
  if any(texel<vec2(0)) || any(texel>=size) || weights[i]<=0. {
   continue;
  }
  let tap=screen_indirect_unpack(textureLoad(kept,texel,0));
  if tap.count<=0. {
   continue;
  }
  let receiver=(vec2<f32>(texel*2)+.5)/view.render_size;
  let other=screen_indirect_position(view,receiver,tap.depth);
  if !screen_indirect_similar(position,normal,other,tap.normal) {
   continue;
  }
  taps.value+=tap.value*weights[i];
  taps.count+=tap.count*weights[i];
  taps.weight+=weights[i];
 }
 if taps.weight<=SCREEN_INDIRECT_LEAST_WEIGHT {
  return ScreenIndirectTaps(vec4(0.),0.,0.);
 }
 return ScreenIndirectTaps(taps.value/taps.weight,taps.count/taps.weight,taps.weight);
}
// The light a point whose last frame's clip position, unjittered, was
// `previous_clip` and whose world normal is `normal` takes from `kept`,
// kept in `view`: none where that position lay behind the camera or off
// screen, or no texel about it passes (screen_indirect_taps).
fn screen_indirect_read(kept:texture_2d<u32>,view:ScreenIndirectView,previous_clip:vec4<f32>,normal:vec3<f32>)->ScreenIndirect {
 if previous_clip.w<=0. {
  return screen_indirect_none();
 }
 let ndc=previous_clip.xy/previous_clip.w;
 if any(abs(ndc)>vec2(1.)) {
  return screen_indirect_none();
 }
 let uv=vec2(ndc.x*.5+.5,.5-ndc.y*.5);
 let position=screen_indirect_position(view,uv,previous_clip.w);
 let taps=screen_indirect_taps(kept,view,uv,position,view.rotation*normal);
 return ScreenIndirect(taps.value.rgb,taps.value.a);
}
