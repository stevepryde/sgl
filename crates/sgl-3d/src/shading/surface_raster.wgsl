// A rasterized fragment's material (raster_material, which the material's
// shader function finishes) and Surface (surface.wgsl): material textures
// sampled with the view's mip bias, normal and bump maps along screen derivatives,
// a normal map's scrolling layers where the frame's time puts them,
// the view's decals (decals.wgsl) over a lit material, and roughness filtered
// by the geometry normal's variance. Every geometry pass evaluates materials
// through these, so the G-buffer, split lighting and the fused pass share
// their equations. Reads `view`, `frame`, the fragment's object record, the
// material and the decal bindings, and a map of the Extended binding tier
// through the program's material-map provider (material_maps_basic.wgsl,
// material_maps_extended.wgsl).
fn surface_tangent_frame(i:Fragment,front:bool)->mat3x3<f32> {
 let f=pbr_tangent_frame(normalize(i.normal),i.tangent);
 let side=select(-1.,1.,front);
 return mat3x3(f[0]*side,f[1]*side,f[2]*side);
}
fn surface_roughness(rough:f32,n:vec3<f32>,i:Fragment)->f32 {
 let geometry_normal=normalize((view.view*vec4(i.normal,0.)).xyz);
 return pbr_filtered_roughness(rough,geometry_normal);
}
fn surface_geometry_normal(i:Fragment,front:bool)->vec3<f32> {
 return side_normal(i.normal,front);
}
// The tangent frame a normal map's texel is taken on, the base's and the
// coat's alike, as KHR_materials_clearcoat takes the coat's on the base's
// (the Khronos glTF Sample Renderer 0686eb2's NormalInfo t and b,
// material_info.glsl 196–207): the authored tangent frame where the
// material is anisotropic, else three.js r185's derivative cotangent frame
// (TangentUtils), its axes scaled alike.
fn surface_map_frame(i:Fragment,front:bool)->mat3x3<f32> {
 if material.anisotropy_strength>0. {
  return surface_tangent_frame(i,front);
 }
 let face=select(-1.0,1.0,front);
 let n=surface_geometry_normal(i,front);
 let dx=dpdx(i.world);
 let dy=dpdy(i.world);
 let uv_dx=dpdx(i.uv);
 let uv_dy=dpdy(i.uv);
 // Three185.1 TangentUtils uses GLSL dFdy; its WebGPU builder lowers that
 // to -dpdy. Negate BOTH world and UV Y derivatives, then reverse the
 // tangent/bitangent on accepted backs (normal already carries face).
 let a=cross(-dy,n);
 let b=cross(n,dx);
 let tangent=(a*uv_dx.x-b*uv_dy.x)*face;
 let bitangent=(a*uv_dx.y-b*uv_dy.y)*face;
 let scale=inverseSqrt(max(max(dot(tangent,tangent),dot(bitangent,bitangent)),0.0000001));
 return mat3x3(tangent*scale,bitangent*scale,n);
}
fn surface_normal(i:Fragment,front:bool)->vec3<f32> {
 let face=select(-1.0,1.0,front);
 var n=surface_geometry_normal(i,front);
 if normal_maps_enabled && (material.maps&MATERIAL_MAP_NORMAL)!=0u {
  var mapped:vec3<f32>;
  if (material.flags&MATERIAL_NORMAL_LAYERS)!=0u {
   let phase=frame.animation_phase;
   let first=textureSampleBias(relief_map,tex_sampler,material_normal_layer_uv(material.normal_layers[0],i.uv,phase),view.mip_bias);
   let second=textureSampleBias(relief_map,tex_sampler,material_normal_layer_uv(material.normal_layers[1],i.uv,phase),view.mip_bias);
   mapped=material_layered_normal(material,first,second);
  } else {
   mapped=material_mapped_normal(material,textureSampleBias(relief_map,tex_sampler,i.uv,view.mip_bias));
  }
  n=normalize(surface_map_frame(i,front)*mapped);
 }
 // Three185.1 MaterialNode.NORMAL selects normalMap OR ELSE bumpMap: a bump
 // map is in effect only without a normal map (scene::materials::maps), so
 // its bit alone selects it, with the normal-map stage disabled too.
 if bump_maps_enabled && (material.maps&MATERIAL_MAP_BUMP)!=0u {
  // Three185.1 BumpMapNode: forward samples along GLSL screen derivatives,
  // with normalized position derivatives so authored bump does not scale
  // with world-space pixel size. WebGPU lowers GLSL dFdy to -dpdy.
  n=pbr_bump_normal(relief_map,tex_sampler,i.world,n,i.uv,material.bump_scale,face);
 }
 return n;
}
// The coat's normal (Surface.coat_normal): its clearcoat normal map's on
// the base map's frame (surface_map_frame), where the map is in effect and
// the normal-map diagnostic switch is on, else the geometry normal.
fn surface_coat_normal(i:Fragment,front:bool)->vec3<f32> {
 if normal_maps_enabled && (material.maps&MATERIAL_MAP_COAT_NORMAL)!=0u {
  return normalize(surface_map_frame(i,front)*material_coat_normal(material,material_coat_normal_texel(i.uv)));
 }
 return surface_geometry_normal(i,front);
}
// The anisotropy direction and strength around the mapped normal `n`.
fn surface_anisotropy(i:Fragment,front:bool,n:vec3<f32>)->vec4<f32> {
 return pbr_resolve_anisotropy(n,surface_tangent_frame(i,front),material.anisotropy_strength,material.anisotropy_rotation,(material.maps&MATERIAL_MAP_ANISOTROPY)!=0u,material_anisotropy_texel(i.uv));
}
// The fragment's emitted light `emission`, none from a moving instance
// while the instance emission diagnostics layer is off.
fn raster_emission(i:Fragment,emission:vec3<f32>)->vec3<f32> {
 if !instance_emission_enabled && (objects[fragment_object(i)].flags&OBJECT_STATIC)==0u {
  return vec3(0.);
 }
 return emission;
}
// A rasterized fragment's material (shader_contract.wgsl's MaterialSurface):
// its record and maps at the fragment (material_texel_surface), the normal
// mapped (surface_normal), as the material's shader function then makes it
// (material_surface, with its context, material_surface_context). Every
// raster pass that reads the material, lit and caster alike, takes it from
// here; a material without a shader takes its record and maps unchanged.
// Each Extended map is sampled only where its factor or flag leaves it
// anything to scale.
fn raster_material(i:Fragment,front:bool)->MaterialSurface {
 let mr=textureSampleBias(mr_map,tex_sampler,i.uv,view.mip_bias);
 let emission=textureSampleBias(emission_map,tex_sampler,i.uv,view.mip_bias).rgb;
 var coat=vec4(1.);
 var coat_roughness=vec4(1.);
 if material.coat>0. {
  coat=material_clearcoat_texel(i.uv);
  coat_roughness=material_coat_roughness_texel(i.uv);
 }
 var transmission=vec4(1.);
 if (material.flags&MATERIAL_TRANSMISSIVE)!=0u {
  transmission=material_transmission_texel(i.uv);
 }
 var thickness=vec4(1.);
 if (material.flags&MATERIAL_TRANSMISSIVE)!=0u || material.diffuse_transmission>0. {
  thickness=material_thickness_texel(i.uv);
 }
 let texels=MaterialTexels(material_base_color(i.uv,i.color),emission,mr,coat,coat_roughness,transmission,thickness);
 let recorded=material_texel_surface(texels,surface_normal(i,front));
 return material_surface(recorded,material_surface_context(i,front),material_shader_params(false));
}
// What the material's shader function evaluates a fragment at
// (material_context).
fn material_surface_context(i:Fragment,front:bool)->SurfaceContext {
 return material_context(fragment_object(i),i.world,surface_geometry_normal(i,front),i.uv,i.color,i.custom,i.instance,front,i.clip.xy);
}
// Its decals are those of `clusters`, the cluster that holds it; its
// material `m` (raster_material).
fn raster_surface(i:Fragment,front:bool,m:MaterialSurface,clusters:ClusterRange)->Surface {
 let object=fragment_object(i);
 let geometry_normal=surface_geometry_normal(i,front);
 let unlit=(material.flags&MATERIAL_UNLIT)!=0u;
 // The atlas's mips follow the position's derivatives under the view's mip
 // bias, as material samples follow their UVs'.
 let bias=exp2(view.mip_bias);
 let position_dx=dpdx(i.world)*bias;
 let position_dy=dpdy(i.world)*bias;
 var decaled=DecalSurface(m.base_color.rgb,m.normal,m.roughness,m.metallic);
 if !unlit {
  decaled=decal_surface(decaled,clusters,i.world,geometry_normal,position_dx,position_dy);
 }
 let n=decaled.normal;
 var anisotropy=vec4(0.);
 if material.anisotropy_strength>0. {
  anisotropy=surface_anisotropy(i,front,n);
 }
 var s:Surface;
 s.position=i.world;
 s.view=normalize(view.eye-i.world);
 s.normal=n;
 s.geometry_normal=geometry_normal;
 s.base=vec4(decaled.base,m.base_color.a);
 s.metallic=decaled.metallic;
 s.dielectric_f0=material_surface_f0(material,m.ior,m.specular);
 s.specular=m.specular;
 s.roughness=surface_roughness(decaled.roughness,n,i);
 // The coat's normal is the record's coat's, taken only where the record
 // has a coat: a uniform branch, since its frame takes derivatives
 // (surface_map_frame), which WGSL allows only in uniform control flow.
 s.coat=m.clearcoat;
 s.coat_normal=geometry_normal;
 if material.coat>0. {
  s.coat_normal=surface_coat_normal(i,front);
 }
 s.coat_roughness=surface_roughness(m.coat_roughness,n,i);
 // The sheen's and the diffuse transmission's maps are sampled only where
 // their factor leaves them anything to scale. The sheen's roughness is
 // filtered as the base's is, as Filament ef1a133 filters it
 // (surface_shading_lit.fs 154–160).
 var sheen_roughness=material.sheen_roughness;
 if any(material.sheen>vec3(0.)) {
  s.sheen=material_sheen(material,material_sheen_color_texel(i.uv));
  sheen_roughness=material_sheen_roughness(material,material_sheen_roughness_texel(i.uv));
 }
 s.sheen_roughness=surface_roughness(sheen_roughness,n,i);
 s.diffuse_transmission_color=material.diffuse_transmission_color;
 if material.diffuse_transmission>0. {
  s.diffuse_transmission=material_diffuse_transmission(material,material_diffuse_transmission_texel(i.uv));
  s.diffuse_transmission_color=material_diffuse_transmission_color(material,material_diffuse_transmission_color_texel(i.uv));
  // Its volume, which the transmitted lobe lies behind and crosses.
  s.volume_thickness=transmission_world_thickness(m.thickness,objects[object].model);
  s.volume_attenuation=m.attenuation;
 }
 s.anisotropy=anisotropy;
 s.emission=raster_emission(i,m.emission);
 s.environment_scale=material.environment_scale;
 s.occlusion=m.occlusion;
 s.unlit=unlit;
 s.front=front;
 s.moving=(objects[object].flags&OBJECT_STATIC)==0u;
 s.baked=baked_material.x!=0u;
 s.uv=i.uv;
 s.lightmap_uv=i.lightmap_uv;
 s.lightmap_bounds=i.lightmap_bounds;
 s.baked_irradiance=objects[object].baked_irradiance;
 if films_enabled && material.iridescence>0. {
  let strength=material_iridescence(material,material_iridescence_texel(i.uv));
  let thickness=material_iridescence_thickness(material,material_iridescence_thickness_texel(i.uv));
  s.film=surface_film(s,strength,material.iridescence_ior,thickness);
 }
 return s;
}
// The view's pixel and the cluster that holds the fragment. A probe
// capture is not the frame's camera: it adds its own environment specular
// and occludes by its material's occlusion itself.
fn raster_context(i:Fragment)->ShadeContext {
 let capture=(view.flags&VIEW_PROBE_CAPTURE)!=0u;
 return ShadeContext(i.clip.xy,select(SHADOW_RECEIVER_CAMERA,SHADOW_RECEIVER_CAPTURE,capture),capture,capture,cluster_range(i.world,i.clip.xy),untraced_reflection());
}
