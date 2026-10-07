// Scene geometry's camera and probe-capture passes, all over pulled vertices
// (source_vs): the G-buffer (stable_fs and its fallbacks), lit color with
// motion (fs), lit color with its ambient light, motion and exact primitive
// identity (source_fs), the G-buffer and source_fs's outputs at once
// (fused_opaque_fs), blended receivers as the surface (receiver_fs),
// blended surfaces' colour (blended_fs) and FSR2's transparency and
// composition mask over moving opaque surfaces (fsr2_composition_fs). A
// masked material's pipelines discard the texels it cuts out in each opaque
// pass (material_alpha_discard), after the fragment's last derivative.
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
 // Preserve both lobes: the base follows normal/bump maps, the coat its
 // clearcoat normal map, else the geometry normal.
 var o:StableMaterial;
 o.normal=gbuffer_encode_normals(s.normal,s.coat_normal);
 o.f0=gbuffer_encode_f0(surface_f0(s),!s.unlit,takes_baked_lights(s.baked,s.lightmap_uv,s.moving),s.occlusion);
 o.material=gbuffer_encode_material(s.coat_roughness,s.roughness,s.coat,surface_f90(s));
 o.anisotropy=gbuffer_encode_anisotropy(s.anisotropy,s.environment_scale);
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
// A lit raster fragment: its shading, the surface's alpha and motion.
struct ShadedFragment {
 shaded:Shaded,
 alpha:f32,
 motion:vec2<f32>,
}
// A camera view's lit colour and ambient target texel for `shaded`: its
// ambient light, and the multiple scattering's share of it in lit colour's
// alpha, which source completion reads (shading/gbuffer.wgsl).
fn camera_color(shaded:Shaded)->vec4<f32> {
 return vec4(shaded.color,gbuffer_multiscatter_share(shaded.ambient,shaded.multi));
}
fn camera_ambient(shaded:Shaded)->vec4<f32> {
 return gbuffer_encode_ambient(shaded.ambient,shaded.multi,shaded.sky_visibility);
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
 return ShadedFragment(shaded,base.a,gbuffer_encode_motion(i.current_clip,i.previous_clip));
}
// Probe captures, which keep their ambient light, occluded by their
// material's occlusion, in color.
@fragment fn fs(i:Fragment,@builtin(front_facing) front:bool)->SceneOutput {
 let s=shade_surface(i,front);
 return SceneOutput(vec4(s.shaded.color,s.alpha),s.motion);
}

// Devices with the original 32-byte material budget write orientation and
// the environment scale, a material value, in a second depth-equal material
// pass, retaining the complete anisotropic response.
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
 return gbuffer_encode_anisotropy(anisotropy,material.environment_scale);
}

// Nonindexed pulled vertices, with source identity carrying the primitive.
// Every camera opaque pass (G-buffer, lighting, fused) draws them, so the
// split and fused forms rasterize one primitive stream: an indexed draw of
// the same triangles changes screen-space derivatives (filtered roughness,
// normal maps, texture LOD) in 2x2 quads spanning two primitives. Godot and
// Wicked likewise share one vertex path between depth prepass and colour pass.
// Each instance of a draw reads its own object record (DrawInstance); a
// GPU-built draw's instance is a section, whose vertices past its triangles
// are dummies.
@vertex fn source_vs(@builtin(vertex_index) draw_vertex:u32,drawn:DrawInstance)->Fragment {
 if drawn_dummy(drawn,draw_vertex) {
  var dummy:Fragment;
  dummy.clip=SCENE_DUMMY_CLIP;
  return dummy;
 }
 let v=scene_source_vertex(drawn.object,drawn.mesh,drawn_index(drawn,draw_vertex));
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
 return SourceOutput(camera_color(s.shaded),camera_ambient(s.shaded),s.motion,i.source_id);
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
 return FusedOpaqueOutput(stable.normal,stable.material,stable.motion,stable.f0,camera_color(shaded),camera_ambient(shaded),i.source_id,stable.anisotropy);
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
 let context=ShadeContext(i.clip.xy,SHADOW_RECEIVER_CAMERA,true,true,cluster_range(i.world,i.clip.xy),blended_traced_reflection(i));
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
// FSR2's transparency and composition mask over an opaque or masked
// surface whose shading moves where its geometry stands still (its
// material's normal layers; stages/transparent), drawn at the G-buffer's
// depth: 1, as AMD's FSR sample's animated textures write it, its depth and
// motion a static surface's but its contents changing
// (framework/rendermodules/animatedtextures/shaders/AnimatedTexture.hlsl;
// FidelityFX SDK 1.1.4, MIT, see LICENSE-amd-fidelityfx.txt). The reactive
// mask at location 0 is kept (view::targets::composition_targets). A masked
// material's cut-out texels are not its surface.
@fragment fn fsr2_composition_fs(i:Fragment)->@location(1) f32 {
 material_alpha_discard(surface_base_color(i).a);
 return 1.;
}
