// A rasterized fragment's sample of the bound material's maps
// (specs/sgl3d-architecture.md, Material samplers). Group 2's one sampler,
// tex_sampler, samples every map binding in its state. A binding in another
// state is sampled here in the shader, as Vulkan's texel filtering and image
// level selection define it, where its bit (by MATERIAL_SLOT_*) is set in
// the pipeline constant material_shader_sampled_maps
// (view::pipelines::Variant::shader_sampled). Each call names its slot, so
// a pipeline compiles the shader path in for those bindings alone, and one
// of a material whose maps share one state pays nothing for it.
override material_shader_sampled_maps:u32=0u;
// Texel `p` of level `level` of `map`, `size` texels, wrapped by `state`.
fn material_sampling_load(map:texture_2d<f32>,p:vec2<i32>,level:i32,size:vec2<i32>,state:u32)->vec4<f32> {
 let x=sampler_wrap(p.x,size.x,sampler_wrap_mode(state,0u));
 let y=sampler_wrap(p.y,size.y,sampler_wrap_mode(state,1u));
 return textureLoad(map,vec2(x,y),level);
}
// Level `level` of `map` at `uv`: its texel there or, `linear`, the four
// nearest texel centres blended (Vulkan, Texel Nearest and Linear
// Filtering), each wrapped by `state`.
fn material_sampling_level(map:texture_2d<f32>,uv:vec2<f32>,level:i32,linear:bool,state:u32)->vec4<f32> {
 let size=vec2<i32>(textureDimensions(map,level));
 let p=uv*vec2<f32>(size);
 if !linear {
  return material_sampling_load(map,vec2<i32>(floor(p)),level,size,state);
 }
 let footprint=sampler_linear(p);
 let i=footprint.texel;
 let f=footprint.weight;
 let low=mix(material_sampling_load(map,i,level,size,state),material_sampling_load(map,i+vec2(1,0),level,size,state),f.x);
 let high=mix(material_sampling_load(map,i+vec2(0,1),level,size,state),material_sampling_load(map,i+vec2(1,1),level,size,state),f.x);
 return mix(low,high,f.y);
}
// `map` at `uv` under sampler state `state` with level-of-detail bias
// `bias`, as Vulkan's image level selection takes it: the level of detail
// is the log2 of the longer of uv's screen derivatives in level-0 texels
// (its isotropic ρmax) plus the bias. At 0 or below the map is magnified:
// level 0 through the magnification filter. Above, the minification filter
// takes level 0 without mipmaps, the nearest level (half rounding down) with
// nearest mip selection, else the two nearest levels blended. No
// anisotropic filtering.
fn material_sample_in_shader(map:texture_2d<f32>,uv:vec2<f32>,bias:f32,state:u32)->vec4<f32> {
 let size=vec2<f32>(textureDimensions(map));
 let lod=log2(max(length(dpdx(uv)*size),length(dpdy(uv)*size)))+bias;
 if !(lod>0.) {
  return material_sampling_level(map,uv,0,(state&SAMPLER_MAG_LINEAR)!=0u,state);
 }
 let linear=(state&SAMPLER_MIN_LINEAR)!=0u;
 let mip=sampler_mip_filter(state);
 if mip==SAMPLER_MIP_NONE {
  return material_sampling_level(map,uv,0,linear,state);
 }
 let last=i32(textureNumLevels(map))-1;
 let d=min(lod,f32(last));
 if mip==SAMPLER_MIP_NEAREST {
  return material_sampling_level(map,uv,i32(ceil(d+.5))-1,linear,state);
 }
 let level=i32(floor(d));
 let finer=material_sampling_level(map,uv,level,linear,state);
 let coarser=material_sampling_level(map,uv,min(level+1,last),linear,state);
 return mix(finer,coarser,d-floor(d));
}
// Whether the bound material's map binding `slot` (MATERIAL_SLOT_*)
// samples the second UV set, and whether through a transform: never in the
// pipelines of a material none of whose maps does (material_uv1_enabled,
// material_transforms_enabled), else as its record's map_uv_bits say.
fn material_map_on_uv1(slot:u32)->bool {
 return material_uv1_enabled && (material.map_uv_bits&(1u<<slot))!=0u;
}
fn material_map_transformed(slot:u32)->bool {
 return material_transforms_enabled && (material.map_uv_bits&(1u<<(slot+MAP_UV_TRANSFORMED_SHIFT)))!=0u;
}
// Where the bound material's map binding `slot` samples at a fragment
// whose first and second UV sets are `uv` and `uv1`
// (specs/sgl3d-architecture.md, Material map coordinates): its set, through
// its transform. The one resolution of every raster pass.
fn material_map_uv(slot:u32,uv:vec2<f32>,uv1:vec2<f32>)->vec2<f32> {
 var coordinates=uv;
 if material_map_on_uv1(slot) {
  coordinates=uv1;
 }
 if material_map_transformed(slot) {
  coordinates=map_uv_transform(material_bindings.map_uvs[slot],coordinates);
 }
 return coordinates;
}
// The bound material's map `map`, in binding `slot` (MATERIAL_SLOT_*), at
// `uv` with level-of-detail bias `bias`: through group 2's sampler, or in
// the shader where its state is another.
fn material_map_sample(map:texture_2d<f32>,slot:u32,uv:vec2<f32>,bias:f32)->vec4<f32> {
 if (material_shader_sampled_maps&(1u<<slot))!=0u {
  return material_sample_in_shader(map,uv,bias,material_map_sampler(material.map_samplers,slot));
 }
 return textureSampleBias(map,tex_sampler,uv,bias);
}
