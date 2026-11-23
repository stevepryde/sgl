// The HDR effect's pass (stages/hdr_effect.rs): a full-screen fragment at
// each pixel of the antialiased frame, at the scene size, which builds the
// pixel's EffectContext (hdr_effect_contract.wgsl), calls the game's
// hdr_effect, composed after this module, and bounds what it returns. Its
// bindings and the contract's functions over them.
struct HdrEffectFrame {
 // The camera's projection, unjittered (FrameInput::camera), whose depth
 // rows give view depth (view_depth.wgsl), and its inverse.
 projection:mat4x4<f32>,
 view_from_clip:mat4x4<f32>,
 // The inverse of the camera's view: view-space directions to the render
 // frame's world space.
 world_from_view:mat4x4<f32>,
 eye:vec3<f32>,
 time:f32,
 // The antialiased frame's size in pixels (the scene size).
 size:vec2<f32>,
 // shading::material::animation_phase of the frame's time.
 phase:f32,
}
@group(0) @binding(0) var<uniform> effect_frame:HdrEffectFrame;
@group(0) @binding(1) var<uniform> effect_params:EffectParams;
// The antialiased frame at the scene size.
@group(0) @binding(2) var effect_scene:texture_2d<f32>;
// The surface depth at the render size (the Surface contract).
@group(0) @binding(3) var effect_depth:texture_depth_2d;
// The frame's exposure multiplier (stages::exposure), 1×1.
@group(0) @binding(4) var effect_exposure:texture_2d<f32>;
// Linear filtering, clamped to the edge.
@group(0) @binding(5) var effect_sampler:sampler;
fn effect_color(uv:vec2<f32>)->vec3<f32> {
 return finite_scene_color(textureSampleLevel(effect_scene,effect_sampler,uv,0.)).rgb;
}
// The device depth of the render-size texel under `uv`, clamped to the
// target: `uv` spans the same view at the scene and the render size.
fn effect_device_depth(uv:vec2<f32>)->f32 {
 let dimensions=textureDimensions(effect_depth);
 let last=vec2<i32>(dimensions)-vec2(1);
 let texel=clamp(vec2<i32>(floor(uv*vec2<f32>(dimensions))),vec2(0),last);
 return textureLoad(effect_depth,texel,0);
}
fn effect_view_depth(uv:vec2<f32>)->f32 {
 return projected_view_depth(effect_frame.projection,effect_device_depth(uv));
}
// The unit view-space direction of the view ray through `uv`, away from the
// camera: between two of its points the inverse projection gives, at the
// near plane's device depth (1, reversed-Z) and at 0.5, which lie in front
// of the camera under perspective and orthographic projections alike.
fn effect_view_ray(uv:vec2<f32>)->vec3<f32> {
 let ndc=vec2(uv.x*2.-1.,1.-uv.y*2.);
 let near=effect_frame.view_from_clip*vec4(ndc,1.,1.);
 let farther=effect_frame.view_from_clip*vec4(ndc,.5,1.);
 let ray=normalize(farther.xyz/farther.w-near.xyz/near.w);
 return select(ray,-ray,ray.z>0.);
}
// A view depth as a distance along `ray`, a unit view-space ray: the ray
// meets view depth d at d over its cosine with the view axis, from the eye
// under a perspective projection (whose rays leave it) and from the
// camera's plane under an orthographic one (whose rays are the axis).
fn effect_along(view_depth:f32,ray:vec3<f32>)->f32 {
 if view_depth>=SCENE_DEPTH_FAR {
  return SCENE_DEPTH_FAR;
 }
 return min(view_depth/max(-ray.z,1e-6),SCENE_DEPTH_FAR);
}
fn effect_distance(uv:vec2<f32>)->f32 {
 return effect_along(effect_view_depth(uv),effect_view_ray(uv));
}
// What the frame keeps of a channel the game returned: within
// 0..SCENE_COLOR_MAX, and 0 where it is not finite, which a min or max
// would leave undefined (finite.wgsl).
fn effect_bound(value:f32)->f32 {
 return select(0.,clamp(value,0.,SCENE_COLOR_MAX),finite(value));
}
@fragment fn hdr_effect_fs(@builtin(position) position:vec4<f32>)->@location(0) vec4<f32> {
 let texel=finite_scene_color(textureLoad(effect_scene,vec2<i32>(position.xy),0));
 let view_ray=effect_view_ray(position.xy/effect_frame.size);
 var ctx:EffectContext;
 ctx.uv=position.xy/effect_frame.size;
 ctx.pixel=position.xy;
 ctx.size=effect_frame.size;
 ctx.view_depth=effect_view_depth(ctx.uv);
 ctx.distance=effect_along(ctx.view_depth,view_ray);
 ctx.ray=normalize((effect_frame.world_from_view*vec4(view_ray,0.)).xyz);
 ctx.exposure=textureLoad(effect_exposure,vec2(0),0).r;
 ctx.eye=effect_frame.eye;
 ctx.time=effect_frame.time;
 ctx.phase=effect_frame.phase;
 let color=hdr_effect(texel.rgb,ctx,effect_params);
 return vec4(effect_bound(color.r),effect_bound(color.g),effect_bound(color.b),texel.a);
}
