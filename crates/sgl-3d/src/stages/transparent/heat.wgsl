// Sousa, GPU Gems 2 §19.1 snapshot/offset; §19.2 rejection with opaque depth
// replacing the alpha mask. Every contributing bilinear tap is checked.
// Bilinear interpolation has two taps per axis, four total across these nested loops (AR-12).
const HEAT_TAPS_PER_AXIS:i32=2;
struct View {
 camera:mat4x4<f32>,
 // Render pixels per scene pixel on each axis: displacement is authored in
 // scene pixels and this pass runs at the render size (below it under FSR).
 render_per_scene:vec2<f32>,
}
@group(0) @binding(0) var<uniform> view:View;
@group(0) @binding(1) var source:texture_2d<f32>;
@group(0) @binding(2) var depth:texture_depth_2d;
struct Vertex {
 @builtin(position) p:vec4<f32>,
 @location(0) displacement:vec2<f32>,
 @location(1) weight:f32,
}
@vertex fn vs(@location(0) position:vec3<f32>, @location(1) displacement:vec2<f32>, @location(2) weight:f32)->Vertex {
 return Vertex(view.camera*vec4(position,1.),displacement*view.render_per_scene,weight);
}
// Where the heat lies in front of the opaque depth, at weight above 0.
fn covers(v:Vertex)->bool {
 return v.weight>0. && textureLoad(depth,vec2<i32>(v.p.xy),0)<=v.p.z;
}
fn heat(v:Vertex)->vec4<f32> {
 let pixel=vec2<i32>(v.p.xy);
 let original=textureLoad(source,pixel,0);
 if !covers(v) || all(v.displacement==vec2(0.)) {
  return original;
 }
 let sample_pixel=v.p.xy+v.displacement*v.weight-vec2(.5);
 let base=vec2<i32>(floor(sample_pixel));
 let f=fract(sample_pixel);
 let size=vec2<i32>(textureDimensions(source));
 var color=vec4(0.);
 for(var y=0;y<HEAT_TAPS_PER_AXIS;y++) {
  for(var x=0;x<HEAT_TAPS_PER_AXIS;x++) {
   let weight=select(1.-f.x,f.x,x==1)*select(1.-f.y,f.y,y==1);
   if weight>0. {
    let tap=base+vec2(x,y);
    // Reject offscreen footprints; never clamp unrelated viewport-edge colors.
    if any(tap<vec2(0)) || any(tap>=size) {
     return original;
    }
    if textureLoad(depth,tap,0)>v.p.z {
     return original;
    }
    color+=textureLoad(source,tap,0)*weight;
   }
  }
 }
 return color;
}
@fragment fn fs(v:Vertex)->@location(0) vec4<f32> {
 return heat(v);
}
// FSR's masks (fsr.rs): the shimmer changes what a pixel shows while its
// motion follows the still surface behind, the case AMD's FSR documentation
// gives the transparency and composition mask, not reactivity
// (super-resolution-upscaler.md 131; FidelityFX SDK 1.1.4,
// MIT, see LICENSE-amd-fidelityfx.txt): its weight as transparency and
// composition where it covers. It marks no reactivity, so the pass does not
// attach that mask.
struct HeatFsrMasked {
 @location(0) color:vec4<f32>,
 @location(1) composition:f32,
}
@fragment fn fs_fsr_masked(v:Vertex)->HeatFsrMasked {
 return HeatFsrMasked(heat(v),select(0.,v.weight,covers(v)));
}
