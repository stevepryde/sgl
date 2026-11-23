// A spot light's measured profile: its intensity by direction, a grid
// packed in the decal atlas (scene/decals.rs, content/light_profile.rs),
// which scales its radiance wherever it reaches. Reads `decal_atlas` and
// `decal_sampler`.
//
// three.js r187 (7e89746a687bb88271253698fa108b1dd59825d0)
// src/nodes/lighting/IESSpotLightNode.js (getSpotAttenuation), MIT
// (src/stages/post/smaa/LICENSE-three.txt): the profile's texture sampled
// at its first level at the vertical angle from the spot's direction and
// the horizontal angle about it, each mapped between its first and last
// texel centres, the light's intensity times the value. Changes: the
// vertical angle is atan2 of the offset across and along the direction
// rather than acos of their cosine, which loses precision near the axis;
// the horizontal angle is about the profile's own 0° axis
// (SpotProfile::zero_azimuth), counterclockwise looking along the
// direction as LM-63's type C turns seen from above, where three.js takes
// the shadow camera's projection; the grid spans 0° to 360° with the 360°
// row its 0° one again, rather than wrapping a texture; and the value
// multiplies the spot's cone, which still bounds the light, where three.js
// replaces the cone's falloff with it.

// Whether the pipeline evaluates profiles: off while the scene holds no
// profiled light (view/pipelines/key.rs, stages/fog.rs), so a scene without
// one pays nothing for them.
override light_profiles_enabled:bool=true;
// A right angle and a full turn, in radians: the grid's vertical and
// horizontal spans (shading/lights.rs's profile rows).
const LIGHT_PROFILE_RIGHT_ANGLE:f32=1.5707963267948966;
const LIGHT_PROFILE_TURN:f32=6.283185307179586;

// What spot `light`'s profile scales its radiance by at `position`: its
// grid's value toward it, bilinear between samples; one for a light
// without a profile, which samples nothing.
fn light_profile(light:Light,position:vec3<f32>)->f32 {
 let lookup=light.profile_lookup;
 if !light_profiles_enabled || lookup.z==0. {
  return 1.;
 }
 let offset=position-light.position;
 let across=vec2(dot(offset,light.profile_zero),dot(offset,cross(light.profile_zero,light.direction)));
 let polar=min(atan2(length(across),dot(offset,light.direction)),LIGHT_PROFILE_RIGHT_ANGLE);
 var azimuth=0.;
 // One row (an axially symmetric profile) has no horizontal angle, and a
 // point on the axis none either.
 if lookup.w!=0. && any(across!=vec2(0.)) {
  azimuth=atan2(across.y,across.x);
  if azimuth<0. {
   azimuth+=LIGHT_PROFILE_TURN;
  }
 }
 return textureSampleLevel(decal_atlas,decal_sampler,lookup.xy+vec2(polar,azimuth)*lookup.zw,0.).r;
}
