// Scene geometry's camera and probe-capture passes, all over pulled vertices
// (source_vs): the G-buffer (stable_fs and its fallbacks), lit color with
// motion (fs), lit color with its ambient diffuse, motion and exact primitive
// identity (source_fs), the G-buffer and source_fs's outputs at once
// (fused_opaque_fs), blended receivers as the surface (receiver_fs) and
// blended surfaces' colour (blended_fs). A masked material's pipelines
// discard the texels it cuts out in each opaque pass
// (material_alpha_discard), after the fragment's last derivative.
struct SceneOutput {
 @location(0) color:vec4<f32>,
 @location(1) motion:vec2<f32>,
}
struct StableMaterial {
 normal:vec4<f32>,
 material:vec4<f32>,
 f0:vec4<f32>,
 anisotropy:vec4<f32>,
}
fn stable_material(s:Surface)->StableMaterial {
 // Preserve both lobes: the base follows normal/bump maps, the coat geometry.
 var o:StableMaterial;
 o.normal=gbuffer_encode_normals(s.normal,s.geometry_normal);
 o.f0=gbuffer_encode_f0(mix(vec3(0.04),s.base.rgb,s.metallic),!s.unlit);
 o.material=gbuffer_encode_material(s.coat_roughness,s.roughness,s.coat,s.environment_scale);
 o.anisotropy=s.anisotropy;
 return o;
}
struct StableOutput {
 @location(0) normal:vec4<f32>,
 @location(1) material:vec4<f32>,
 @location(2) motion:vec2<f32>,
 @location(3) f0:vec4<f32>,
 @location(4) anisotropy:vec4<f32>,
}
fn stable_surface(i:Fragment,s:Surface)->StableOutput {
 let m=stable_material(s);
 return StableOutput(m.normal,m.material,gbuffer_encode_motion(i.current_clip,i.previous_clip),m.f0,m.anisotropy);
}
// The G-buffer records no emission.
fn stable_raster_surface(i:Fragment,front:bool)->Surface {
 return raster_surface(i,front,surface_base_color(i),vec3(0.),cluster_range(i.world,i.clip.xy));
}
@fragment fn stable_fs(i:Fragment,@builtin(front_facing) raster_front:bool)->StableOutput {
 let front=object_front_face(i,raster_front);
 let s=stable_raster_surface(i,front);
 material_alpha_discard(s.base.a);
 return stable_surface(i,s);
}
// A lit raster fragment: color with the surface's alpha, the ambient diffuse
// within it (shading/gbuffer.wgsl) and motion.
struct ShadedFragment {
 color:vec4<f32>,
 ambient:vec4<f32>,
 motion:vec2<f32>,
}
fn shade_surface(i:Fragment,raster_front:bool)->ShadedFragment {
 let front=object_front_face(i,raster_front);
 let base=surface_base_color(i);
 let emission=surface_emission(i);
 // Keep split lighting's early unlit exit before normal maps and BRDF sampling.
 var shaded:Shaded;
 if (material.flags&MATERIAL_UNLIT)!=0u {
  material_alpha_discard(base.a);
  shaded=shade_unlit(unlit_surface(base,emission));
 } else {
  let context=raster_context(i);
  let s=raster_surface(i,front,base,emission,context.clusters);
  material_alpha_discard(base.a);
  shaded=shade_lit(s,context);
 }
 return ShadedFragment(vec4(shaded.color,base.a),vec4(shaded.ambient,0.),gbuffer_encode_motion(i.current_clip,i.previous_clip));
}
// Probe captures, which keep the ambient diffuse in color.
@fragment fn fs(i:Fragment,@builtin(front_facing) front:bool)->SceneOutput {
 let s=shade_surface(i,front);
 return SceneOutput(s.color,s.motion);
}

// Devices with the original 32-byte material budget write orientation in a
// second depth-equal material pass, retaining the complete anisotropic response.
struct LegacyStableOutput {
 @location(0) normal:vec4<f32>,
 @location(1) material:vec4<f32>,
 @location(2) motion:vec2<f32>,
 @location(3) f0:vec4<f32>,
}
@fragment fn stable_legacy_fs(i:Fragment,@builtin(front_facing) raster_front:bool)->LegacyStableOutput {
 let front=object_front_face(i,raster_front);
 let surface=stable_raster_surface(i,front);
 material_alpha_discard(surface.base.a);
 let s=stable_surface(i,surface);
 return LegacyStableOutput(s.normal,s.material,s.motion,s.f0);
}
@fragment fn anisotropy_fs(i:Fragment,@builtin(front_facing) raster_front:bool)->@location(0) vec4<f32> {
 let front=object_front_face(i,raster_front);
 let alpha=surface_base_color(i).a;
 var anisotropy=vec4(0.);
 if material.anisotropy_strength>0. {
  anisotropy=surface_anisotropy(i,front,surface_normal(i,front));
 }
 material_alpha_discard(alpha);
 return anisotropy;
}

// Nonindexed pulled vertices, with source identity carrying the primitive.
// Every camera opaque pass (G-buffer, lighting, fused) draws them, so the
// split and fused forms rasterize one primitive stream: an indexed draw of
// the same triangles changes screen-space derivatives (filtered roughness,
// normal maps, texture LOD) in 2x2 quads spanning two primitives. Godot and
// Wicked likewise share one vertex path between depth prepass and colour pass.
// Each instance of a draw reads its own object record (DrawInstance).
@vertex fn source_vs(@builtin(vertex_index) index:u32,drawn:DrawInstance)->Fragment {
 let v=scene_source_vertex(drawn.object,drawn.mesh,index);
 var o=vertex(drawn.object,Vertex(v.position,v.normal,v.uv,v.color,v.lightmap_uv,v.lightmap_bounds,v.tangent),v.previous_position);
 o.source_id=v.source_id;
 return o;
}
struct SourceOutput {
 @location(0) color:vec4<f32>,
 @location(1) ambient:vec4<f32>,
 @location(2) motion:vec2<f32>,
 @location(3) source_id:vec2<u32>,
}
@fragment fn source_fs(i:Fragment,@builtin(front_facing) front:bool)->SourceOutput {
 let s=shade_surface(i,front);
 return SourceOutput(s.color,s.ambient,s.motion,i.source_id);
}

// One depth owner writes both the stable inputs and their primary source.
struct FusedOpaqueOutput {
 @location(0) normal:vec4<f32>,
 @location(1) material:vec4<f32>,
 @location(2) motion:vec2<f32>,
 @location(3) f0:vec4<f32>,
 @location(4) color:vec4<f32>,
 @location(5) ambient:vec4<f32>,
 @location(6) source_id:vec2<u32>,
 @location(7) anisotropy:vec4<f32>,
}
@fragment fn fused_opaque_fs(i:Fragment,@builtin(front_facing) raster_front:bool)->FusedOpaqueOutput {
 let front=object_front_face(i,raster_front);
 let context=raster_context(i);
 let s=raster_surface(i,front,surface_base_color(i),surface_emission(i),context.clusters);
 material_alpha_discard(s.base.a);
 let stable=stable_surface(i,s);
 var shaded:Shaded;
 if s.unlit {
  shaded=shade_unlit(s);
 } else {
  shaded=shade_lit(s,context);
 }
 return FusedOpaqueOutput(stable.normal,stable.material,stable.motion,stable.f0,vec4(shaded.color,s.base.a),vec4(shaded.ambient,0.),i.source_id,stable.anisotropy);
}

// The receiver pass (stages/transparent): a blended receiver of screen-space
// reflections as the surface at its pixels, drawn through source_vs as
// blended_fs draws it, over the surface depth it tests strictly nearer and
// writes: its traced lobe into the receiver layer and its unjittered motion
// into the G-buffer's, as HDRP's transparent depth prepass and motion
// vectors draw a material that receives SSR.
struct ReceiverOutput {
 @location(0) receiver:vec4<f32>,
 @location(1) motion:vec2<f32>,
}
@fragment fn receiver_fs(i:Fragment,@builtin(front_facing) raster_front:bool)->ReceiverOutput {
 let front=object_front_face(i,raster_front);
 let recorded=stable_material(stable_raster_surface(i,front));
 let lobe=gbuffer_traced_lobe(recorded.normal,recorded.material,recorded.f0);
 return ReceiverOutput(gbuffer_encode_receiver(lobe),gbuffer_encode_motion(i.current_clip,i.previous_clip));
}

// What the frame's screen-space method returned for a blended fragment's
// traced lobe (bind_blended.wgsl): the method's result where the fragment is
// a receiver that is the surface at its pixel, its depth equal to the
// surface depth there, as the receiver pass drew it through the same vertex
// entry (its position @invariant), primitive and depth state; for every
// other fragment, a receiver behind it included, none.
fn blended_traced_reflection(i:Fragment)->TracedReflection {
 let receives=(material.flags&MATERIAL_RECEIVES_SCREEN_SPACE_REFLECTIONS)!=0u;
 if !receives || blended_trace.cutoff<=0. {
  return untraced_reflection();
 }
 let pixel=vec2<i32>(i.clip.xy);
 if textureLoad(blended_surface_depth,pixel,0)!=i.clip.z {
  return untraced_reflection();
 }
 return TracedReflection(textureLoad(blended_reflections,pixel,0),blended_trace.cutoff,blended_trace.fade);
}
// Blended surfaces, which the transparent stage draws back to front over the
// beauty: lit as opaque surfaces are, with the probe and sky specular that
// source completion gives opaque ones (probe_environment), a receiver that
// is the surface composing the screen-space method's result into its traced
// lobe in its place, then fogged from the frame's fog volume where they lie,
// as Bevy 9d12036's forward transparent pass shades and fogs each fragment
// (crates/bevy_pbr/src/render/pbr.wesl) and Godot b130438's samples its
// volumetric fog (scene_forward_clustered.glsl), and blended with their
// alpha.
fn blended_color(i:Fragment,raster_front:bool)->vec4<f32> {
 let front=object_front_face(i,raster_front);
 let context=ShadeContext(i.clip.xy,SHADOW_RECEIVER_CAMERA,true,cluster_range(i.world,i.clip.xy),blended_traced_reflection(i));
 let s=raster_surface(i,front,surface_base_color(i),surface_emission(i),context.clusters);
 var shaded:Shaded;
 if s.unlit {
  shaded=shade_unlit(s);
 } else {
  shaded=shade_lit(s,context);
 }
 // A fragment's position w is one over its view depth.
 return vec4(frame_fog(shaded.color,i.clip.xy,1./i.clip.w),s.base.a);
}
@fragment fn blended_fs(i:Fragment,@builtin(front_facing) front:bool)->@location(0) vec4<f32> {
 return blended_color(i,front);
}
// FSR2's masks (view::targets::mask_targets): AMD's FSR documentation
// (FidelityFX SDK 1.1.4, MIT, see LICENSE-amd-fidelityfx.txt,
// docs/techniques/super-resolution-upscaler.md, "Reactive mask") asks that
// alpha-blended surfaces write their alpha as reactivity, clamped to 0.9,
// and AMD's FSR sample writes a translucent surface's alpha as its
// transparency and composition
// (framework/rendermodules/translucency/shaders/translucencyps.hlsl).
struct BlendedFsr2Masked {
 @location(0) color:vec4<f32>,
 @location(1) reactive:f32,
 @location(2) composition:f32,
}
@fragment fn blended_fsr2_masked_fs(i:Fragment,@builtin(front_facing) front:bool)->BlendedFsr2Masked {
 let color=blended_color(i,front);
 return BlendedFsr2Masked(color,min(color.a,.9),color.a);
}
