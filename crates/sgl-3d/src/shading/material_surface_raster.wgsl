// A rasterized fragment's material (raster_material, which the material's
// shader function finishes): material textures
// sampled with the view's mip bias, normal and bump maps along screen derivatives,
// a normal map's scrolling layers where the frame's time puts them,
// roughness filtered by the geometry normal's variance. Camera geometry and
// shader-driven masked casters evaluate their material through this owner.
// Reads `view`, `frame`, object records, the material and the tier's maps
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
// Where the bound material's map binding `slot` samples at fragment `i`
// (material_map_uv).
fn surface_map_uv(i:Fragment,slot:u32)->vec2<f32> {
 return material_map_uv(slot,i.uv,i.uv1);
}
// The tangent frame the texel of normal-map binding `slot` (the normal
// map's or the clearcoat normal map's) is taken on: the frame of the
// coordinates it was sampled at (specs/sgl3d-architecture.md, Material map
// coordinates). Where the material is anisotropic and the binding samples
// the authored frame's UV set (the normal map's, the first without one),
// the authored tangent frame carried through the binding's transform
// (map_uv_authored_frame), as KHR_materials_clearcoat takes the coat's on
// the base's (the Khronos glTF Sample Renderer 0686eb2's NormalInfo t and b,
// material_info.glsl 196–207); else three.js r185's derivative cotangent
// frame (TangentUtils) of the binding's coordinates, made glTF's frame
// (map_uv_texel_frame), which follows their set, rotation and mirroring by
// construction.
fn surface_map_frame(i:Fragment,front:bool,slot:u32)->mat3x3<f32> {
 let normal_set=(material.maps&MATERIAL_MAP_NORMAL)!=0u && material_map_on_uv1(MATERIAL_SLOT_RELIEF);
 if material.anisotropy_strength>0. && material_map_on_uv1(slot)==normal_set {
  let authored=surface_tangent_frame(i,front);
  if !material_map_transformed(slot) {
   return authored;
  }
  return map_uv_authored_frame(material_bindings.map_uvs[slot],authored);
 }
 let face=select(-1.0,1.0,front);
 let n=surface_geometry_normal(i,front);
 let dx=dpdx(i.world);
 let dy=dpdy(i.world);
 let uv=surface_map_uv(i,slot);
 let uv_dx=dpdx(uv);
 let uv_dy=dpdy(uv);
 // Three185.1 TangentUtils uses GLSL dFdy; its WebGPU builder lowers that
 // to -dpdy. Negate BOTH world and UV Y derivatives. The cotangent frame
 // is scaled by dot(cross(dx,-dy),n), positive wherever n faces the view,
 // an accepted back's flipped normal included, so the UV-weighted sums of
 // a and b below are the surface's own gradients of U and V on either
 // side; face reverses them on a back, to (-T,-B,-N) as
 // surface_tangent_frame, ray_map_frame and the Khronos glTF Sample
 // Renderer 0686eb2 (material_info.glsl 164–171) give.
 let a=cross(-dy,n);
 let b=cross(n,dx);
 return map_uv_texel_frame((a*uv_dx.x-b*uv_dy.x)*face,(a*uv_dx.y-b*uv_dy.y)*face,n);
}
fn surface_normal(i:Fragment,front:bool)->vec3<f32> {
 let face=select(-1.0,1.0,front);
 var n=surface_geometry_normal(i,front);
 if normal_maps_enabled && (material.maps&MATERIAL_MAP_NORMAL)!=0u {
  var mapped:vec3<f32>;
  let uv=surface_map_uv(i,MATERIAL_SLOT_RELIEF);
  if (material.flags&MATERIAL_NORMAL_LAYERS)!=0u {
   let phase=frame.animation_phase;
   let first=material_map_sample(relief_map,MATERIAL_SLOT_RELIEF,material_normal_layer_uv(material.normal_layers[0],uv,phase),view.mip_bias);
   let second=material_map_sample(relief_map,MATERIAL_SLOT_RELIEF,material_normal_layer_uv(material.normal_layers[1],uv,phase),view.mip_bias);
   mapped=material_layered_normal(material,first,second);
  } else {
   mapped=material_mapped_normal(material,material_map_sample(relief_map,MATERIAL_SLOT_RELIEF,uv,view.mip_bias));
  }
  n=normalize(surface_map_frame(i,front,MATERIAL_SLOT_RELIEF)*mapped);
 }
 // Three185.1 MaterialNode.NORMAL selects normalMap OR ELSE bumpMap: a bump
 // map is in effect only without a normal map (scene::materials::maps), so
 // its bit alone selects it, with the normal-map stage disabled too.
 if bump_maps_enabled && (material.maps&MATERIAL_MAP_BUMP)!=0u {
  // Three185.1 BumpMapNode: forward samples along GLSL screen derivatives,
  // with normalized position derivatives so authored bump does not scale
  // with world-space pixel size. WebGPU lowers GLSL dFdy to -dpdy.
  let uv=surface_map_uv(i,MATERIAL_SLOT_RELIEF);
  let steps=pbr_bump_steps(uv);
  let height=material_map_sample(relief_map,MATERIAL_SLOT_RELIEF,uv,0.).r;
  let forward=vec2(material_map_sample(relief_map,MATERIAL_SLOT_RELIEF,uv+steps[0],0.).r,material_map_sample(relief_map,MATERIAL_SLOT_RELIEF,uv+steps[1],0.).r);
  n=pbr_bump_normal(i.world,n,height,forward,material.bump_scale,face);
 }
 return n;
}
// The coat's normal (Surface.coat_normal): its clearcoat normal map's on
// its binding's frame (surface_map_frame), where the map is in effect and
// the normal-map diagnostic switch is on, else the geometry normal.
fn surface_coat_normal(i:Fragment,front:bool)->vec3<f32> {
 if normal_maps_enabled && (material.maps&MATERIAL_MAP_COAT_NORMAL)!=0u {
  return normalize(surface_map_frame(i,front,MATERIAL_SLOT_COAT_NORMAL)*material_coat_normal(material,material_coat_normal_texel(i.uv,i.uv1)));
 }
 return surface_geometry_normal(i,front);
}
// The anisotropy direction and strength around the mapped normal `n`.
fn surface_anisotropy(i:Fragment,front:bool,n:vec3<f32>)->vec4<f32> {
 return pbr_resolve_anisotropy(n,surface_tangent_frame(i,front),material.anisotropy_strength,material.anisotropy_rotation,(material.maps&MATERIAL_MAP_ANISOTROPY)!=0u,material_anisotropy_texel(i.uv,i.uv1));
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
// Each map is read where its binding samples (surface_map_uv).
// Each Extended map is sampled only where its factor or flag leaves it
// anything to scale.
fn raster_material(i:Fragment,front:bool)->MaterialSurface {
 let mr=material_map_sample(mr_map,MATERIAL_SLOT_METALLIC_ROUGHNESS,surface_map_uv(i,MATERIAL_SLOT_METALLIC_ROUGHNESS),view.mip_bias);
 let emission=material_map_sample(emission_map,MATERIAL_SLOT_EMISSION,surface_map_uv(i,MATERIAL_SLOT_EMISSION),view.mip_bias).rgb;
 var coat=vec4(1.);
 var coat_roughness=vec4(1.);
 if material.coat>0. {
  coat=material_clearcoat_texel(i.uv,i.uv1);
  coat_roughness=material_coat_roughness_texel(i.uv,i.uv1);
 }
 var transmission=vec4(1.);
 if (material.flags&MATERIAL_TRANSMISSIVE)!=0u {
  transmission=material_transmission_texel(i.uv,i.uv1);
 }
 var thickness=vec4(1.);
 if (material.flags&MATERIAL_TRANSMISSIVE)!=0u || material.diffuse_transmission>0. {
  thickness=material_thickness_texel(i.uv,i.uv1);
 }
 let texels=MaterialTexels(material_base_color(i.uv,i.uv1,i.color),emission,mr,coat,coat_roughness,transmission,thickness);
 let recorded=material_texel_surface(texels,surface_normal(i,front));
 return material_surface(recorded,material_surface_context(i,front),material_shader_params(false));
}
// What the material's shader function evaluates a fragment at
// (material_context).
fn material_surface_context(i:Fragment,front:bool)->SurfaceContext {
 return material_context(fragment_object(i),i.world,surface_geometry_normal(i,front),i.uv,i.color,i.custom,i.instance,front,i.clip.xy);
}
