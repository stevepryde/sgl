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
struct Distorted {
 @location(0) color:vec4<f32>,
 // Source UV minus output UV, as AMD's distortion lookup adds this offset
 // when reading the undistorted depth and motion buffers.
 @location(1) offset:vec2<f32>,
}
fn distort(v:Vertex)->Distorted {
 let pixel=vec2<i32>(v.p.xy);
 let original=textureLoad(source,pixel,0);
 if v.weight<=0. || all(v.displacement==vec2(0.)) || textureLoad(depth,pixel,0)>v.p.z {
  return Distorted(original,vec2(0.));
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
     return Distorted(original,vec2(0.));
    }
    if textureLoad(depth,tap,0)>v.p.z {
     return Distorted(original,vec2(0.));
    }
    color+=textureLoad(source,tap,0)*weight;
   }
  }
 }
 return Distorted(color,v.displacement*v.weight/vec2<f32>(size));
}
@fragment fn fs(v:Vertex)->@location(0) vec4<f32> {
 return distort(v).color;
}
@fragment fn fs_with_field(v:Vertex)->Distorted {
 return distort(v);
}
