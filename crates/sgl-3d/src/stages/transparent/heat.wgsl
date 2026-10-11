// Sousa, GPU Gems 2 §19.1 snapshot/offset; §19.2 rejection with opaque depth
// replacing the alpha mask. Every contributing bilinear tap is checked.
struct View {
 camera:mat4x4<f32>,
 // Render pixels per scene pixel on each axis: displacement is authored in
 // scene pixels and this pass runs at the render size (below it under FSR2).
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
@fragment fn fs(v:Vertex)->@location(0) vec4<f32> {
 let pixel=vec2<i32>(v.p.xy);
 let original=textureLoad(source,pixel,0);
 if v.weight<=0. || all(v.displacement==vec2(0.)) || textureLoad(depth,pixel,0)>v.p.z {
  return original;
 }
 let sample_pixel=v.p.xy+v.displacement*v.weight-vec2(.5);
 let base=vec2<i32>(floor(sample_pixel));
 let f=fract(sample_pixel);
 let size=vec2<i32>(textureDimensions(source));
 var color=vec4(0.);
 for(var y=0;y<2;y++) {
  for(var x=0;x<2;x++) {
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
