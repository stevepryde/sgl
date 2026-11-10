// A rasterized fragment's Surface: its evaluated material, decals and
// geometry-filtered roughness, shared by every camera and capture pass.
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
 s.view=material_view(i.world);
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
 // A coatless surface's coat normal is its base normal (Surface). A coated
 // record keeps its coat normal where its texel's coat is 0, as the
 // G-buffer records it (stable_material).
 if material.coat<=0. && s.coat<=0. {
  s.coat_normal=n;
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
