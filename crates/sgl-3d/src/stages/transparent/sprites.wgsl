// Sprite batches (stages/transparent/sprites.rs) under the unlit group 0:
// one instanced draw a batch, six vertices a sprite, each corner placed by
// the view's facing (shading/sprite.wgsl). Unlit: the image's texel, sampled
// as sRGB colour, times the sprite's tint, as three.js r187's
// SpriteMaterial's colour is its map times its colour and opacity
// (ShaderLib/sprite.glsl.js; MIT, stages/post/smaa/LICENSE-three.txt), then
// fogged where it lies.
@group(1) @binding(0) var<uniform> sprite_batch:SpriteBatch;
@group(1) @binding(1) var<storage,read> sprite_records:array<SpriteRecord>;
@group(1) @binding(2) var sprite_image:texture_2d<f32>;
@group(1) @binding(3) var sprite_sampler:sampler;
// A blended batch's sprites back to front in this view (group 2).
@group(2) @binding(0) var<storage,read> sprite_order:array<u32>;
// Whether a masked batch's colour marks transparency and composition: its
// sprites or look changed since the last submitted frame.
override sprite_marks:bool=false;

struct SpriteFragment {
 @builtin(position) @invariant clip:vec4<f32>,
 @location(0) uv:vec2<f32>,
 @location(1) tint:vec4<f32>,
 // Unjittered, this frame and the last submitted frame's (motion).
 @location(2) current_clip:vec4<f32>,
 @location(3) previous_clip:vec4<f32>,
}
// Sprite `index` of the batch, corner `vertex`.
fn sprite_vertex(index:u32,vertex:u32)->SpriteFragment {
 let sprite=sprite_records[index];
 let corner=sprite_quad_corner(vertex);
 let world=vec4(sprite_corner(sprite,sprite_batch,view.view,view.projection,corner),1.);
 var o:SpriteFragment;
 o.clip=view.view_projection*world;
 o.uv=sprite_uv(sprite,corner);
 o.tint=sprite.tint;
 // Motion: the camera's reprojection of the point the sprite covers this
 // frame, as Godot measures a billboard's (its vertex placed with this
 // frame's facing), exact while the view only translates.
 o.current_clip=view.stable_view_projection*world;
 o.previous_clip=view.previous_view_projection*world;
 return o;
}
// A masked batch's instance i draws sprite i.
@vertex fn sprite_vs(@builtin(vertex_index) vertex:u32,@builtin(instance_index) instance:u32)->SpriteFragment {
 return sprite_vertex(instance,vertex);
}
// A blended batch's instance i draws the sprite the view's order names.
@vertex fn sprite_ordered_vs(@builtin(vertex_index) vertex:u32,@builtin(instance_index) instance:u32)->SpriteFragment {
 return sprite_vertex(sprite_order[instance],vertex);
}
fn sprite_texel(i:SpriteFragment)->vec4<f32> {
 return textureSample(sprite_image,sprite_sampler,i.uv)*i.tint;
}
// A masked sprite's texel, its cut-out texels discarded (glTF's alpha
// cutoff).
fn sprite_masked(i:SpriteFragment)->vec3<f32> {
 let texel=sprite_texel(i);
 if texel.a<sprite_batch.cutoff {
  discard;
 }
 return texel.rgb;
}
// A masked batch's depth and motion (Frame item 7), over the opaque depth.
@fragment fn sprite_depth_fs(i:SpriteFragment)->@location(0) vec4<f32> {
 sprite_masked(i);
 return gbuffer_encode_velocity(i.current_clip,i.previous_clip);
}
// A masked batch's colour (Frame item 9), at the depth its depth pass wrote,
// fogged where it lies. A fragment's position w is one over its view
// depth.
@fragment fn sprite_masked_fs(i:SpriteFragment)->@location(0) vec4<f32> {
 return vec4(frame_fog(sprite_masked(i),i.clip.xy,1./i.clip.w),1.);
}
// A blended sprite, premultiplied as blended surfaces are and fogged where
// it lies.
fn sprite_blended(i:SpriteFragment)->vec4<f32> {
 let texel=sprite_texel(i);
 return vec4(frame_fog_premultiplied(texel.rgb*texel.a,texel.a,i.clip.xy,1./i.clip.w),texel.a);
}
@fragment fn sprite_blended_fs(i:SpriteFragment)->@location(0) vec4<f32> {
 return sprite_blended(i);
}
// An additive sprite: straight colour dimmed by fog transmittance alone
// (as additive glow is, D-44), multiplied by alpha in the blend stage.
fn sprite_additive(i:SpriteFragment)->vec4<f32> {
 let texel=sprite_texel(i);
 return vec4(frame_fog_premultiplied(texel.rgb,0.,i.clip.xy,1./i.clip.w),texel.a);
}
@fragment fn sprite_additive_fs(i:SpriteFragment)->@location(0) vec4<f32> {
 return sprite_additive(i);
}
// The temporal masks (view/targets.rs). A blended sprite marks as a blended
// surface does (geometry.wgsl's blended_masked_fs, after AMD's FSR
// documentation and sample; FidelityFX SDK 1.1.4, MIT, see
// LICENSE-amd-fidelityfx.txt): reactivity and transparency and composition
// by its alpha. A masked sprite has depth and motion of its own; a batch
// that changed since the last submitted frame marks transparency and
// composition 1 where it draws, as AMD's animated textures do
// (framework/rendermodules/animatedtextures/shaders/AnimatedTexture.hlsl),
// and an unchanged one marks nothing.
struct SpriteMasked {
 @location(0) color:vec4<f32>,
 @location(1) reactive:vec4<f32>,
 @location(2) composition:f32,
}
@fragment fn sprite_blended_masked_fs(i:SpriteFragment)->SpriteMasked {
 let color=sprite_blended(i);
 return SpriteMasked(color,temporal_reactive(color.a),color.a);
}
@fragment fn sprite_additive_masked_fs(i:SpriteFragment)->SpriteMasked {
 let color=sprite_additive(i);
 return SpriteMasked(color,temporal_reactive(color.a),color.a);
}
@fragment fn sprite_masked_masked_fs(i:SpriteFragment)->SpriteMasked {
 let color=vec4(frame_fog(sprite_masked(i),i.clip.xy,1./i.clip.w),1.);
 return SpriteMasked(color,temporal_reactive(0.),select(0.,1.,sprite_marks));
}
// Where a changed masked batch's sprites stood in the last submitted frame,
// the whole quad, its cut-out texels included (their cell may have changed):
// transparency and composition 1, colour and reactivity kept, so what the
// sprite left is rejected, as a masked material's footprint is marked when
// an edit changes its coverage.
@fragment fn sprite_footprint_fs(i:SpriteFragment)->SpriteMasked {
 return SpriteMasked(vec4(0.),vec4(0.),1.);
}
