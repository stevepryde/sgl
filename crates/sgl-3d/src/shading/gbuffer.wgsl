// The G-buffer: what the opaque stage records for later stages, and its
// encodings (shading::gbuffer names the target formats).
//
// normal: RG the mapped base normal, BA the geometry coat normal, both signed
//  [-1, 1] world-space octahedral coordinates.
// material: coat perceptual roughness, base perceptual roughness, coat
//  strength, and the base lobe's reflectance at grazing incidence (F90,
//  surface_f90).
// f0: specular reflectance at normal incidence; in alpha, one 8-bit code:
//  0 where nothing lit was drawn (unlit materials and the clear); on a lit
//  surface, its material's occlusion in 126ths, plus 1, and 127 more where
//  it takes the baked scene lights (takes_baked_lights in
//  baked_lighting.wgsl): 1-127 and 128-254, decoded at the midpoints
//  (gbuffer_lit, gbuffer_takes_baked_lights, gbuffer_occlusion), as Godot
//  b130438 packs a flag beside 7-bit roughness in one 8-bit channel, its
//  halves apart (scene_forward_clustered.glsl, normal_roughness_output_buffer).
// anisotropy: RG the world anisotropy tangent, signed octahedral as normal
//  holds one (0 without anisotropy), B its strength, A the environment
//  scale, a material value the anisotropy pass of a device that cannot
//  write this target with the others also writes (gbuffer_encode_anisotropy).
// motion: current minus previous unjittered UV, +y down, at most two screens
//  along its longer axis (gbuffer_encode_motion).
// ambient: in rgb, the ambient light within lit colour (Shaded in
//  surface.wgsl) before occlusion, its diffuse share and the specular
//  multiple scattering it carries together; lit colour's alpha holds the
//  multiple scattering's share of it (gbuffer_encode_ambient,
//  gbuffer_multiscatter_share, gbuffer_ambient), which source completion
//  reads before writing alpha 1, and completion takes from lit colour what
//  its occlusion hides (occlusion_ambient). Zero where nothing lit was
//  drawn. In alpha, the irradiance volume's sky visibility a(n) at a lit
//  pixel, 1 where no volume lights it, which completion's occlusion of the
//  sky's specular reads.
// receiver (the Surface contract, specs/sgl3d-architecture.md): where a
//  blended receiver is the surface, RG its traced lobe's normal as `normal`
//  holds one and B that lobe's perceptual roughness. Read only under a
//  receiver, where the surface depth is nearer than the opaque depth.
//
// Octahedral unit-vector encoding from Bevy b56fc29d3016e641754765244b5ba3f9cc504671,
// crates/bevy_pbr/src/render/utils.wgsl (MIT, see LICENSE-bevy.txt).
// Modified: prefixed names, signed coordinates for float storage (no UNORM
// remap), octahedral_decode_signed, clamp in place of saturate.
// Cigolle et al. 2014, "A Survey of Efficient Representations for Independent
// Unit Vectors", https://jcgt.org/published/0003/02/01/paper.pdf.
fn gbuffer_octahedral_encode(v:vec3<f32>)->vec2<f32> {
 let n=v/(abs(v.x)+abs(v.y)+abs(v.z));
 let wrap=(1.-abs(n.yx))*select(vec2(-1.),vec2(1.),n.xy>vec2(0.));
 return select(wrap,n.xy,n.z>=0.);
}
fn gbuffer_octahedral_decode(v:vec2<f32>)->vec3<f32> {
 var n=vec3(v,1.-abs(v.x)-abs(v.y));
 let t=clamp(-n.z,0.,1.);
 n=vec3(n.xy+select(vec2(t),vec2(-t),n.xy>=vec2(0.)),n.z);
 return normalize(n);
}
fn gbuffer_encode_normals(base:vec3<f32>,coat:vec3<f32>)->vec4<f32> {
 return vec4(gbuffer_octahedral_encode(base),gbuffer_octahedral_encode(coat));
}
fn gbuffer_base_normal(packed:vec4<f32>)->vec3<f32> {
 return gbuffer_octahedral_decode(packed.xy);
}
fn gbuffer_coat_normal(packed:vec4<f32>)->vec3<f32> {
 return gbuffer_octahedral_decode(packed.zw);
}
// The normal of the lobe reflections trace (specular_traced_lobe): the
// coat's on a coated receiver, else the base's.
fn gbuffer_reflection_normal(packed:vec4<f32>,coat:f32)->vec3<f32> {
 let coated=specular_traced_lobe(coat)==SPECULAR_COAT;
 return gbuffer_octahedral_decode(select(packed.xy,packed.zw,coated));
}

struct GBufferMaterial {
 coat_roughness:f32,
 roughness:f32,
 coat:f32,
 f90:f32,
}
fn gbuffer_encode_material(coat_roughness:f32,roughness:f32,coat:f32,f90:f32)->vec4<f32> {
 return vec4(coat_roughness,roughness,coat,f90);
}
fn gbuffer_material(packed:vec4<f32>)->GBufferMaterial {
 return GBufferMaterial(packed.x,packed.y,packed.z,packed.w);
}
// A surface's anisotropy, its world tangent and strength as Surface.anisotropy
// holds them, and its environment scale. The tangent, a unit vector where
// the strength is above 0, takes two channels, as the normals do, so the
// environment scale has the fourth.
fn gbuffer_encode_anisotropy(anisotropy:vec4<f32>,environment_scale:f32)->vec4<f32> {
 var tangent=vec2(0.);
 if anisotropy.w>0. {
  tangent=gbuffer_octahedral_encode(anisotropy.xyz);
 }
 return vec4(tangent,anisotropy.w,environment_scale);
}
// The anisotropy `packed` records, its tangent and strength as
// Surface.anisotropy holds them: zero without anisotropy.
fn gbuffer_anisotropy(packed:vec4<f32>)->vec4<f32> {
 if packed.z<=0. {
  return vec4(0.);
 }
 return vec4(gbuffer_octahedral_decode(packed.xy),packed.z);
}
fn gbuffer_environment_scale(packed:vec4<f32>)->f32 {
 return packed.w;
}
// The perceptual roughness of the lobe reflections trace
// (specular_traced_lobe): the coat's on a coated receiver, else the base's.
// Unlit receivers reflect nothing: 1, which no method traces.
fn gbuffer_traced_roughness(material:GBufferMaterial,lit:bool)->f32 {
 let coated=specular_traced_lobe(material.coat)==SPECULAR_COAT;
 var roughness=select(material.roughness,material.coat_roughness,coated);
 if !lit {
  roughness=1.;
 }
 return roughness;
}

// The lobe reflections trace: its normal and perceptual roughness.
struct GBufferTracedLobe {
 normal:vec3<f32>,
 roughness:f32,
}
// The traced lobe of a surface recorded as `normals`, `material` and `f0`.
fn gbuffer_traced_lobe(normals:vec4<f32>,material:vec4<f32>,f0:vec4<f32>)->GBufferTracedLobe {
 let decoded=gbuffer_material(material);
 return GBufferTracedLobe(gbuffer_reflection_normal(normals,decoded.coat),gbuffer_traced_roughness(decoded,gbuffer_lit(f0)));
}
fn gbuffer_encode_receiver(lobe:GBufferTracedLobe)->vec4<f32> {
 return vec4(gbuffer_octahedral_encode(lobe.normal),lobe.roughness,0.);
}
// Whether a pixel is under a receiver: its surface depth is nearer than its
// opaque depth (reversed Z).
fn gbuffer_under_receiver(surface_depth:f32,opaque_depth:f32)->bool {
 return surface_depth>opaque_depth;
}
// The traced lobe of the surface at a pixel: the receiver layer's
// `receiver` under a receiver, else that of the opaque surface the G-buffer
// records.
fn gbuffer_surface_lobe(surface_depth:f32,opaque_depth:f32,receiver:vec4<f32>,normals:vec4<f32>,material:vec4<f32>,f0:vec4<f32>)->GBufferTracedLobe {
 if gbuffer_under_receiver(surface_depth,opaque_depth) {
  return GBufferTracedLobe(gbuffer_octahedral_decode(receiver.xy),receiver.z);
 }
 return gbuffer_traced_lobe(normals,material,f0);
}

// F0 with whether the surface is `lit`, whether it takes the baked scene
// lights (`takes_baked`), which the ray-traced shadow trace reads to cast no
// ray toward a baked light at a receiver that never takes it, and its
// material's `occlusion` (material_occlusion), which source completion
// takes with the frame's ambient occlusion. An unlit surface records 0
// whatever it would take.
const GBUFFER_OCCLUSION_STEPS:f32=126.;
fn gbuffer_encode_f0(f0:vec3<f32>,lit:bool,takes_baked:bool,occlusion:f32)->vec4<f32> {
 var code=0.;
 if lit {
  code=1.+round(saturate(occlusion)*GBUFFER_OCCLUSION_STEPS)+select(0.,127.,takes_baked);
 }
 return vec4(f0,code/255.);
}
fn gbuffer_lit(packed:vec4<f32>)->bool {
 return packed.a*255.>=.5;
}
// Whether a lit surface takes baked scene lights (Light::baked), as the
// lighting pass decides it for the surface's fragment.
fn gbuffer_takes_baked_lights(packed:vec4<f32>)->bool {
 return packed.a*255.>=127.5;
}
// A lit surface's material occlusion, to 1/126.
fn gbuffer_occlusion(packed:vec4<f32>)->f32 {
 let code=round(packed.a*255.);
 let steps=code-select(1.,128.,gbuffer_takes_baked_lights(packed));
 return saturate(steps/GBUFFER_OCCLUSION_STEPS);
}

// The motion of a point from its unjittered clip positions in this frame and
// the last submitted one, for geometry and the sky alike, at most
// GBUFFER_MOTION_LIMIT screens along its longer axis with its direction kept.
// Two screens is off-screen for every temporal consumer, even after the
// reflections' 3x3 vicinity search moves the reprojected position a few
// texels, and finite in the half-float target. A previous position on or
// behind the previous camera's plane (w <= 0) has no place on screen: its
// motion is the limit, away from where its clip position points, as a point
// in front tends to as its w reaches zero. Wicked Engine 4323a33
// (WickedEngine/shaders/visibility_velocityCS.hlsl) likewise writes velocity
// only for a positive previous w, clamped to one screen. The current w must
// be positive, as a rasterized fragment's is; callers handle a point the
// current camera cannot place on screen.
const GBUFFER_MOTION_LIMIT:f32=2.;
fn gbuffer_encode_motion(current_clip:vec4<f32>,previous_clip:vec4<f32>)->vec2<f32> {
 let current=current_clip.xy/current_clip.w*vec2(0.5,-0.5);
 // The motion times the previous w, finite as w reaches zero; at or behind
 // the plane only the previous position's direction remains.
 let previous_w=max(previous_clip.w,0.);
 let scaled=current*previous_w-previous_clip.xy*vec2(0.5,-0.5);
 let longest=max(abs(scaled.x),abs(scaled.y));
 if previous_w>0. && longest<=GBUFFER_MOTION_LIMIT*previous_w {
  return scaled/previous_w;
 }
 // Directly behind the previous camera no direction remains.
 if longest==0. {
  return vec2(GBUFFER_MOTION_LIMIT,0.);
 }
 return scaled/longest*GBUFFER_MOTION_LIMIT;
}

// The ambient target's texel for a lit pixel whose ambient light holds the
// diffuse share `diffuse` and the specular multiple scattering `multi`, and
// whose irradiance volume sky visibility is `sky_visibility`.
fn gbuffer_encode_ambient(diffuse:vec3<f32>,multi:vec3<f32>,sky_visibility:f32)->vec4<f32> {
 return vec4(max(diffuse,vec3(0.))+max(multi,vec3(0.)),sky_visibility);
}
// The multiple scattering's share of that ambient light, by luminance, which
// lit colour's alpha holds: 0 where it holds none.
fn gbuffer_multiscatter_share(diffuse:vec3<f32>,multi:vec3<f32>)->f32 {
 let total=luminance(max(diffuse,vec3(0.))+max(multi,vec3(0.)));
 if total<=0. {
  return 0.;
 }
 return saturate(luminance(max(multi,vec3(0.)))/total);
}
// A lit pixel's ambient light from its ambient texel `ambient` and its
// multiple scattering's `share` (lit colour's alpha): the diffuse share and
// the multiple scattering, each of the light's colour, exact where either is
// none, and the sky visibility.
struct GBufferAmbient {
 diffuse:vec3<f32>,
 multi:vec3<f32>,
 sky_visibility:f32,
}
fn gbuffer_ambient(ambient:vec4<f32>,share:f32)->GBufferAmbient {
 let light=max(ambient.rgb,vec3(0.));
 let multi=light*saturate(share);
 return GBufferAmbient(light-multi,multi,ambient.a);
}
