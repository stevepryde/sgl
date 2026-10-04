// Decals (scene/decals.rs): boxes that project images from the scene's decal
// atlas onto the lit surfaces inside them, changing their base colour,
// normal, roughness and metallic before they are lit. Reads `clusters`,
// `decals`, `decal_atlas` and `decal_sampler`.
//
// Godot b130438's clustered decals
// (servers/rendering/renderer_rd/shaders/forward_clustered/scene_forward_clustered.glsl,
// "process decals"), MIT (src/LICENSE-godot.txt). Changes: a cluster lists
// its decals after its lights, as Bevy 9d12036's clusterable objects share
// one list, where Godot keeps a bitmask per element type; decals are in
// world space, where Godot's are in view space; the atlas holds linear
// values, so one binding serves colour and data where Godot binds sRGB and
// linear views; decals bring no emission or occlusion, which SGL3D's
// surfaces do not take from maps; a fade of zero leaves coverage whole
// rather than raising zero to the zeroth power at the box's faces; and the
// gradients that select the atlas's mips take the view's mip bias, as
// material samples do. Not ported: Godot's `cull_mask` (every lit surface
// in a box takes it), its order by distance from the camera with
// `sorting_offset` (decals apply in identity order), and its distance
// fade.

// Whether the pipeline applies decals: off while the scene holds none
// (view/pipelines.rs), so a scene without them pays nothing for them.
override decals_enabled:bool=true;

// The surface values decals change.
struct DecalSurface {
 base:vec3<f32>,
 normal:vec3<f32>,
 // Perceptual roughness, before the builder filters it.
 roughness:f32,
 metallic:f32,
}
// The atlas texel of `rect` at decal UV `uv`, along the UV's screen
// gradients `ddx` and `ddy`; zero gradients sample level 0.
fn decal_texel(rect:vec4<f32>,uv:vec2<f32>,ddx:vec2<f32>,ddy:vec2<f32>)->vec4<f32> {
 return textureSampleGrad(decal_atlas,decal_sampler,uv*rect.zw+rect.xy,ddx*rect.zw,ddy*rect.zw);
}
// `surface` at `position`, whose geometry normal is `geometry_normal`, under
// the decals of `range`, in its order. `position_dx` and `position_dy` are
// the position's screen derivatives, which select the atlas's mips (Godot's
// simulated derivatives); a ray hit, which has none, passes zero.
fn decal_surface(surface:DecalSurface,range:ClusterRange,position:vec3<f32>,geometry_normal:vec3<f32>,position_dx:vec3<f32>,position_dy:vec3<f32>)->DecalSurface {
 if !decals_enabled {
  return surface;
 }
 var result=surface;
 let first=range.first+range.live+range.baked;
 for (var at=first;at<first+range.decals;at++) {
  let decal=decals[cluster_item(at)];
  let local=(decal.decal_from_world*vec4(position,1.)).xyz;
  if any(local<vec3(0.,-1.,0.)) || any(local>vec3(1.)) {
   continue;
  }
  var fade=1.;
  let face_fade=select(decal.lower_fade,decal.upper_fade,local.y>0.);
  if face_fade>0. {
   fade=pow(1.-abs(local.y),face_fade);
  }
  if decal.normal_fade>0. {
   fade*=smoothstep(decal.normal_fade,1.,dot(geometry_normal,decal.y_axis)*.5+.5);
  }
  let ddx=(decal.decal_from_world*vec4(position_dx,0.)).xz;
  let ddy=(decal.decal_from_world*vec4(position_dy,0.)).xz;
  var color=decal_texel(decal.base_color_rect,local.xz,ddx,ddy)*decal.color;
  color.a*=fade;
  result.base=mix(result.base,color.rgb,color.a*decal.base_color_mix);
  if any(decal.normal_rect!=vec4(0.)) {
   var mapped=decal_texel(decal.normal_rect,local.xz,ddx,ddy).xyz;
   // The map's +Y is toward the image's top, the box's -Z.
   mapped=vec3(mapped.xy*vec2(2.,-2.)-vec2(1.,-1.),0.);
   mapped.z=sqrt(max(0.,1.-dot(mapped.xy,mapped.xy)));
   let decal_normal=decal.x_axis*mapped.x+decal.y_axis*mapped.z+decal.z_axis*mapped.y;
   result.normal=normalize(mix(result.normal,decal_normal,color.a));
  }
  if any(decal.metallic_roughness_rect!=vec4(0.)) {
   let metallic_roughness=decal_texel(decal.metallic_roughness_rect,local.xz,ddx,ddy);
   result.roughness=mix(result.roughness,metallic_roughness.g,color.a);
   result.metallic=mix(result.metallic,metallic_roughness.b,color.a);
  }
 }
 return result;
}
