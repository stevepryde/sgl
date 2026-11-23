// The HDR effect contract (the package README's "HDR effects";
// specs/sgl3d-architecture.md, HDR effect contract): what a game's HDR
// effect (Scene::add_hdr_effect) receives. Its module defines
// `struct EffectParams`, a uniform block (FrameInput::hdr_effect's
// parameters), and
//   fn hdr_effect(color:vec3<f32>,ctx:EffectContext,params:EffectParams)->vec3<f32>
// which SGL3D calls once for each pixel of the antialiased frame, after
// TAA or FSR and before depth of field, motion blur, bloom and the tone map.
// `color` is the pixel's colour, linear scene-referred RGB (sRGB primaries)
// in the lights' units before exposure, at most SCENE_COLOR_MAX; the effect
// returns the pixel's colour in the same units, which SGL3D bounds to
// 0..SCENE_COLOR_MAX (a channel that is negative or not finite reads 0).
// It may call effect_color, effect_view_depth and effect_distance at any
// `uv`, which the program's provider declares (hdr_effect_pass.wgsl), and
// reads SCENE_DEPTH_FAR (view_depth.wgsl); nothing else SGL3D declares is
// the contract. The pattern is three.js r187's render pipeline, a node
// over a scene pass's colour and depth before the output transform
// (src/renderers/common/RenderPipeline.js, src/nodes/display/PassNode.js).
// The depth is this frame's at the render size, jittered under TAA and
// FSR, where the colour is resolved: at a silhouette the texel under a
// pixel can change from frame to frame.
//
//   fn effect_color(uv:vec2<f32>)->vec3<f32>
//     The antialiased frame at `uv`, bilinear, clamped to its edge.
//   fn effect_view_depth(uv:vec2<f32>)->f32
//     The view depth in metres, along the camera's forward axis, of the
//     surface (opaque, or a blended receiver of screen-space reflections)
//     under `uv`: the depth texel there, unfiltered. SCENE_DEPTH_FAR at the
//     sky.
//   fn effect_distance(uv:vec2<f32>)->f32
//     The metres along the view ray through `uv` to that surface: from the
//     eye under a perspective projection, from the camera's plane under an
//     orthographic one. SCENE_DEPTH_FAR at the sky.
//
// What hdr_effect evaluates at: the pixel's centre `uv` over the frame,
// (0,0) its top-left corner and (1,1) its bottom-right; that centre in the
// frame's pixels (`pixel`) and the frame's `size` in pixels, the scene size,
// which differs from the depth's under FSR (the functions above map `uv`
// to both); the pixel's `view_depth` and `distance`, as effect_view_depth
// and effect_distance give them at `uv`; its view ray's unit direction
// `ray` and the `eye`, in the render frame's world space (eye + ray ×
// distance is the surface under a perspective projection); the frame's
// `exposure`, the multiplier the tone map applies to the colour after the
// chain, so color × exposure is what it would take (automatic exposure
// metered the frame before the effect, so it does not answer what the
// effect changes); the frame's `time` (FrameInput::elapsed_seconds in f32,
// which loses precision over a long session) and `phase`, where that time
// falls in the hour over which material animation repeats exactly, 0..1,
// as the shader contract's.
struct EffectContext {
 uv:vec2<f32>,
 pixel:vec2<f32>,
 size:vec2<f32>,
 view_depth:f32,
 distance:f32,
 ray:vec3<f32>,
 exposure:f32,
 eye:vec3<f32>,
 time:f32,
 phase:f32,
}
