// Application extension: baked static Lambertian diffuse for fixed emitters.
// A static surface takes it from one of two baked maps of the same encoding
// (scene::static_lighting): the lightmap, through its material UV and the
// lightmap's chart transform, or the irradiance atlas, through its lightmap
// UV. Each holds irradiance / PI and an optional directional lobe, filtered
// bilinearly by the hardware through `baked_sampler`, at mip 0 as Bevy's
// `lightmap.wesl`, Godot's forward shaders and Wicked's `LightMapping` sample
// their lightmaps.
override baked_lighting_enabled:bool=true;
// Constant+linear (L0/L1) irradiance polynomial relative to the baked value:
// E(n)=color*max(a+w.n,0), evaluated as dot((w,a),(n,1)) as in Ramamoorthi and
// Hanrahan (SIGGRAPH 2001) and Sloan, "Stupid Spherical Harmonics Tricks"
// (GDC 2008, appendix A10). Packed xyz=w/8+.5, packed w=a/2; [.5;4] is neutral.
fn directional_irradiance(color:vec3<f32>,direction:vec4<f32>,normal:vec3<f32>)->vec3<f32> {
 let lobe=vec4(direction.xyz*8.-4.,direction.w*2.);
 return color*max(dot(lobe,vec4(normalize(normal),1.)),0.);
}
// Layer `layer` of a baked map at `uv`: its irradiance times `scale`, shaped
// by its lobe for `normal`. An all-zero lobe is a layer without one.
fn baked_map_irradiance(irradiance_map:texture_2d_array<f32>,direction_map:texture_2d_array<f32>,uv:vec2<f32>,layer:i32,scale:f32,normal:vec3<f32>)->vec3<f32> {
 let color=textureSampleLevel(irradiance_map,baked_sampler,uv,layer,0.).rgb*scale;
 let direction=textureSampleLevel(direction_map,baked_sampler,uv,layer,0.);
 if all(direction==vec4(0.)) {
  return color;
 }
 return directional_irradiance(color,direction,normal);
}
fn ambient_cube_irradiance(cube:array<vec4<f32>,6>,normal:vec3<f32>)->vec3<f32> {
 let n=normalize(normal);
 let weights=n*n;
 return cube[select(1u,0u,n.x>=0.)].rgb*weights.x
  +cube[select(3u,2u,n.y>=0.)].rgb*weights.y
  +cube[select(5u,4u,n.z>=0.)].rgb*weights.z;
}
// Where a receiver's baked diffuse comes from: the one determination of
// which receivers have baked lighting. A moving instance takes its ambient
// cube. A static one takes the lightmap where its material is lightmapped,
// else the irradiance atlas where one is installed and the receiver has a
// chart there: a lightmap UV other than the (0,0) or negative UVs an
// unassigned surface carries. Outside a cropped chart's bounds the chart is
// black, which is still the atlas's baked lighting: the determination never
// reads the bounds, so neither a crop nor the UVs a triangle's edge quads
// extrapolate past them give a static receiver baked lights to loop over.
// With baked lighting off, no receiver has any.
const BAKED_NONE:u32=0u;
const BAKED_AMBIENT_CUBE:u32=1u;
const BAKED_LIGHTMAP:u32=2u;
const BAKED_ATLAS:u32=3u;
fn baked_diffuse_source(lightmapped:bool,atlas_uv:vec2<f32>,moving:bool)->u32 {
 if !baked_lighting_enabled || (frame.flags&FRAME_BAKED_LIGHTING)==0u {
  return BAKED_NONE;
 }
 if moving {
  return BAKED_AMBIENT_CUBE;
 }
 if lightmapped {
  return BAKED_LIGHTMAP;
 }
 if (frame.flags&FRAME_IRRADIANCE_ATLAS)==0u || all(atlas_uv==vec2(0.)) || any(atlas_uv<vec2(0.)) {
  return BAKED_NONE;
 }
 return BAKED_ATLAS;
}
// Whether a receiver takes baked scene lights (Light::baked): those without
// baked lighting, whose light no lightmap or atlas chart already holds, as
// Godot skips a BAKE_STATIC light only where the instance uses a lightmap.
// They are the moving instances and the static receivers baked_diffuse_source
// gives no baked map.
fn takes_baked_lights(lightmapped:bool,atlas_uv:vec2<f32>,moving:bool)->bool {
 let source=baked_diffuse_source(lightmapped,atlas_uv,moving);
 return source==BAKED_NONE || source==BAKED_AMBIENT_CUBE;
}
fn surface_fixed_irradiance(lightmapped:bool,uv:vec2<f32>,atlas_uv:vec2<f32>,bounds:vec4<f32>,normal:vec3<f32>,front:bool,moving:bool,cube:array<vec4<f32>,6>)->vec3<f32> {
 let source=baked_diffuse_source(lightmapped,atlas_uv,moving);
 if source==BAKED_NONE {
  return vec3(0.);
 }
 if source==BAKED_AMBIENT_CUBE {
  return ambient_cube_irradiance(cube,normal);
 }
 if source==BAKED_LIGHTMAP {
  // The sampler clamps the chart's X and repeats its Y.
  let chart_uv=uv*frame.lightmap_chart.xy+frame.lightmap_chart.zw;
  return baked_map_irradiance(static_lightmap,static_lightmap_direction,chart_uv,0,1.,normal);
 }
 if any(atlas_uv<bounds.xy) || any(atlas_uv>bounds.zw) {
  return vec3(0.);
 }
 let half_texel=vec2(0.5)/vec2<f32>(textureDimensions(static_irradiance_atlas));
 let bounded=clamp(atlas_uv,half_texel,vec2(1.)-half_texel);
 return baked_map_irradiance(static_irradiance_atlas,static_direction_atlas,bounded,select(1,0,front),frame.fixed_irradiance_scale,normal);
}
