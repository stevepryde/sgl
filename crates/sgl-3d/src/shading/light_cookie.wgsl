// A spot light's cookie: the image it projects, packed in the decal atlas
// (scene/decals.rs), which scales its radiance wherever it reaches. Reads
// `decal_atlas` and `decal_sampler`.
//
// Godot b130438's spot projector
// (servers/rendering/renderer_rd/shaders/scene_forward_lights_inc.glsl
// 910–932), MIT (src/LICENSE-godot.txt): the image lies in the decal atlas
// at the record's rect, is sampled along the gradients found by projecting
// the position plus each of its screen derivatives and subtracting, and
// scales the light's colour by its rgb times its alpha. The projection is
// Bevy 9d12036's spot light texture (crates/bevy_pbr/src/render/
// pbr_lighting.wesl 820–838, through the light's transform that
// crates/bevy_pbr/src/decal/clustered.rs 346–372 extracts), MIT OR Apache-2.0
// (src/LICENSE-bevy.txt): from the light's own frame, its square reaching
// the outer angle's tangent each way. Changes: Godot projects through the
// spot's shadow matrix, which it writes only for a shadowed light; this
// projects from the record's frame whether or not the light casts a
// shadow, as Wicked Engine 4323a33 writes a masked spot's matrix with or
// without a shadow (WickedEngine/wiRenderer.cpp 5028–5034), MIT
// (src/LICENSE-wicked.txt); every receiver of the light takes it, the fog
// too, as Wicked's volumetric spot pass does
// (shaders/volumetricLight_SpotPS.hlsl 125–132), where Godot's fog,
// SDFGI and VoxelGI ignore projectors; the atlas UV is clamped half a
// texel inside the image's rect, which Godot leaves to the border; Bevy
// reads the image's red alone, where this, as Godot and Wicked
// (shaders/lightingHF.hlsli 331–340), takes its colour times its alpha;
// and the atlas holds linear values, so one view serves where Godot binds
// an sRGB one (decal_atlas.rs).

// Whether the pipeline samples cookies: off while the scene holds no
// cookied light (view/pipelines/key.rs), so a scene without one pays nothing
// for them. Pipelines that do not specialise on the scene's content (the
// fog) leave it on, and a light without a cookie samples nothing there.
override light_cookies_enabled:bool=true;
// The least depth along the spot's direction a projection divides by: a
// point the light reaches lies in front of it, but a screen derivative's
// far end may not, which then selects the coarsest level rather than an
// infinite gradient.
const LIGHT_COOKIE_NEAREST_DEPTH:f32=1e-4;

// Where `offset` from spot `light`'s position falls in its projector
// square, in image UV: u along its right (light.cookie_right), v down from
// its top, (0.5, 0.5) on its axis and 0 and 1 at the outer angle across it.
fn light_cookie_uv(light:Light,offset:vec3<f32>)->vec2<f32> {
 let up=cross(light.cookie_right,light.direction);
 let depth=max(dot(offset,light.direction),LIGHT_COOKIE_NEAREST_DEPTH);
 let scale=.5*light.cookie_cotangent/depth;
 return vec2(.5+dot(offset,light.cookie_right)*scale,.5-dot(offset,up)*scale);
}
// What spot `light`'s cookie scales its radiance by at `position`: its
// image's linear rgb times its alpha, at the level `position_dx` and
// `position_dy` select (the position's screen derivatives in raster, zero
// at a ray hit, which samples the first level, the froxel's extent in the
// fog); one for a light without a cookie, which samples nothing.
fn light_cookie(light:Light,position:vec3<f32>,position_dx:vec3<f32>,position_dy:vec3<f32>)->vec3<f32> {
 let rect=light.cookie_rect;
 if !light_cookies_enabled || all(rect==vec4(0.)) {
  return vec3(1.);
 }
 let offset=position-light.position;
 let uv=light_cookie_uv(light,offset);
 let uv_dx=light_cookie_uv(light,offset+position_dx)-uv;
 let uv_dy=light_cookie_uv(light,offset+position_dy)-uv;
 let half_texel=.5/vec2<f32>(textureDimensions(decal_atlas));
 let atlas_uv=clamp(uv*rect.zw+rect.xy,rect.xy+half_texel,rect.xy+rect.zw-half_texel);
 let texel=textureSampleGrad(decal_atlas,decal_sampler,atlas_uv,uv_dx*rect.zw,uv_dy*rect.zw);
 return texel.rgb*texel.a;
}
