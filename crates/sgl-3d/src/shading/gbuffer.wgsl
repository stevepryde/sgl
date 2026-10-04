// The G-buffer: what the opaque stage records for later stages, and its
// encodings (shading::gbuffer names the target formats).
//
// normal: RG the mapped base normal, BA the geometry coat normal, both signed
//  [-1, 1] world-space octahedral coordinates.
// material: coat perceptual roughness, base perceptual roughness, coat
//  strength, environment scale.
// f0: specular reflectance at normal incidence; alpha 1 on lit surfaces.
// anisotropy: world tangent in xyz, strength in w.
// motion: current minus previous unjittered UV, +y down, at most two screens
//  along its longer axis (gbuffer_encode_motion).
// ambient: in rgb, the ambient diffuse radiance within lit colour (Shaded in
//  surface.wgsl) before occlusion; source completion subtracts the share its
//  ambient visibility hides. Zero where nothing lit was drawn.
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
// The normal of the lobe reflections trace: the coat's on a coated receiver,
// else the base's.
fn gbuffer_reflection_normal(packed:vec4<f32>,coat:f32)->vec3<f32> {
 return gbuffer_octahedral_decode(select(packed.xy,packed.zw,coat>0.));
}

struct GBufferMaterial {
 coat_roughness:f32,
 roughness:f32,
 coat:f32,
 environment_scale:f32,
}
fn gbuffer_encode_material(coat_roughness:f32,roughness:f32,coat:f32,environment_scale:f32)->vec4<f32> {
 return vec4(coat_roughness,roughness,coat,environment_scale);
}
fn gbuffer_material(packed:vec4<f32>)->GBufferMaterial {
 return GBufferMaterial(packed.x,packed.y,packed.z,packed.w);
}
// The perceptual roughness of the lobe reflections trace: the coat's on a
// coated receiver, else the base's. Unlit receivers reflect nothing: 1, which
// no method traces.
fn gbuffer_traced_roughness(material:GBufferMaterial,lit:bool)->f32 {
 var roughness=material.roughness;
 if material.coat>0. {
  roughness=material.coat_roughness;
 }
 if !lit {
  roughness=1.;
 }
 return roughness;
}

fn gbuffer_encode_f0(f0:vec3<f32>,lit:bool)->vec4<f32> {
 return vec4(f0,select(0.,1.,lit));
}
fn gbuffer_lit(packed:vec4<f32>)->bool {
 return packed.a>=0.5;
}

// The motion of a point from its unjittered clip positions in this frame and
// the last submitted one, for geometry and the sky alike, at most
// GBUFFER_MOTION_LIMIT screens along its longer axis with its direction kept.
// Two screens is off-screen for every temporal consumer, even after the
// reflections' 3x3 search around the reprojected position, and finite in the
// half-float target. A previous position on or behind the previous camera's
// plane (w <= 0) has no place on screen: its motion is the limit, away from
// where its clip position points, as a point in front tends to as its w
// reaches zero. Wicked Engine 4323a33 (WickedEngine/shaders/
// visibility_velocityCS.hlsl) likewise writes velocity only for a positive
// previous w, clamped to one screen.
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
