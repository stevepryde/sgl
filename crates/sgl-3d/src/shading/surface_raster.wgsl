// A rasterized fragment's Surface (surface.wgsl): material textures sampled
// with the view's mip bias, normal and bump maps along screen derivatives,
// the view's decals (decals.wgsl) over a lit material, and roughness filtered
// by the geometry normal's variance. Every geometry pass evaluates materials
// through these, so the G-buffer, split lighting and the fused pass share
// their equations. Reads `view`, the fragment's object record, the material
// and the decal bindings.
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
 return normalize(i.normal)*select(-1.0,1.0,front);
}
fn surface_normal(i:Fragment,front:bool)->vec3<f32> {
 let face=select(-1.0,1.0,front);
 var n=surface_geometry_normal(i,front);
 let dx=dpdx(i.world);
 let dy=dpdy(i.world);
 let uv_dx=dpdx(i.uv);
 let uv_dy=dpdy(i.uv);
 if normal_maps_enabled && (material.flags&MATERIAL_NORMAL_MAP)!=0u {
  // Three185.1 TangentUtils uses GLSL dFdy; its WebGPU builder lowers that
  // to -dpdy. Negate BOTH world and UV Y derivatives, then reverse the
  // tangent/bitangent on accepted backs (normal already carries face).
  let a=cross(-dy,n);
  let b=cross(n,dx);
  let tangent=(a*uv_dx.x-b*uv_dy.x)*face;
  let bitangent=(a*uv_dx.y-b*uv_dy.y)*face;
  let scale=inverseSqrt(max(max(dot(tangent,tangent),dot(bitangent,bitangent)),0.0000001));
  var mapped=textureSampleBias(normal_map,tex_sampler,i.uv,view.mip_bias).xyz*2.-vec3(1.);
  mapped=vec3(mapped.xy*material.normal_scale,mapped.z);
  if material.anisotropy_strength>0. {
   n=normalize(surface_tangent_frame(i,front)*mapped);
  } else {
   n=normalize(mat3x3(tangent*scale,bitangent*scale,n)*mapped);
  }
 }
 // Three185.1 MaterialNode.NORMAL selects normalMap OR ELSE bumpMap.
 // Diagnostic stage disabling must not select the material's unused bump map.
 if (material.flags&MATERIAL_NORMAL_MAP)==0u && bump_maps_enabled && (material.flags&MATERIAL_BUMP_MAP)!=0u {
  // Three185.1 BumpMapNode: forward samples along GLSL screen derivatives,
  // with normalized position derivatives so authored bump does not scale
  // with world-space pixel size. WebGPU lowers GLSL dFdy to -dpdy.
  n=pbr_bump_normal(bump_map,tex_sampler,i.world,n,i.uv,material.bump_scale,face);
 }
 return n;
}
// The anisotropy direction and strength around the mapped normal `n`.
fn surface_anisotropy(i:Fragment,front:bool,n:vec3<f32>)->vec4<f32> {
 return pbr_resolve_anisotropy(n,surface_tangent_frame(i,front),material.anisotropy_strength,material.anisotropy_rotation,(material.flags&MATERIAL_ANISOTROPY_MAP)!=0u,textureSampleBias(anisotropy_map,tex_sampler,i.uv,view.mip_bias).rgb);
}
fn surface_base_color(i:Fragment)->vec4<f32> {
 return material_base_color(i.uv,i.color);
}
fn surface_emission(i:Fragment)->vec3<f32> {
 var emission=material.emission*textureSampleBias(emission_map,tex_sampler,i.uv,view.mip_bias).rgb;
 if !instance_emission_enabled && (objects[fragment_object(i)].flags&OBJECT_STATIC)==0u {
  emission=vec3(0.);
 }
 return emission;
}
// Its decals are those of `clusters`, the cluster that holds it.
fn raster_surface(i:Fragment,front:bool,base:vec4<f32>,emission:vec3<f32>,clusters:ClusterRange)->Surface {
 let object=fragment_object(i);
 let mr=textureSampleBias(mr_map,tex_sampler,i.uv,view.mip_bias);
 let geometry_normal=surface_geometry_normal(i,front);
 let unlit=(material.flags&MATERIAL_UNLIT)!=0u;
 // The atlas's mips follow the position's derivatives under the view's mip
 // bias, as material samples follow their UVs'.
 let bias=exp2(view.mip_bias);
 let position_dx=dpdx(i.world)*bias;
 let position_dy=dpdy(i.world)*bias;
 var decaled=DecalSurface(base.rgb,surface_normal(i,front),material.roughness*mr.g,material.metallic*mr.b);
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
 s.coat_normal=geometry_normal;
 s.base=vec4(decaled.base,base.a);
 s.metallic=decaled.metallic;
 s.roughness=surface_roughness(decaled.roughness,n,i);
 s.coat=material.coat;
 s.coat_roughness=surface_roughness(material.coat_roughness,n,i);
 s.anisotropy=anisotropy;
 s.emission=emission;
 s.environment_scale=material.environment_scale;
 s.unlit=unlit;
 s.front=front;
 s.moving=(objects[object].flags&OBJECT_STATIC)==0u;
 s.baked=baked_material.x!=0u;
 s.uv=i.uv;
 s.lightmap_uv=i.lightmap_uv;
 s.lightmap_bounds=i.lightmap_bounds;
 s.baked_irradiance=objects[object].baked_irradiance;
 return s;
}
// The view's pixel and the cluster that holds the fragment. A probe
// capture is not the frame's camera and adds its own environment specular.
fn raster_context(i:Fragment)->ShadeContext {
 let capture=(view.flags&VIEW_PROBE_CAPTURE)!=0u;
 return ShadeContext(i.clip.xy,!capture,capture,cluster_range(i.world,i.clip.xy),untraced_reflection());
}
