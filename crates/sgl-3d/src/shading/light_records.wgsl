// The scene-light records every view shades from (group 0's `lights`, at
// each light's identity index) and each light's shadow in the local-light
// shadow atlas (group 0's `local_shadows`). Rust mirrors: shading/lights.rs;
// the layout test compares the two.
// One point, spot or rectangle light. A point or spot light's shading reads
// its first four rows; a rectangle's also its last.
struct Light {
 position:vec3<f32>,
 // Filament's falloff: one over the squared range.
 inverse_square_range:f32,
 // Linear RGB times intensity per steradian; a rectangle's luminance.
 color:vec3<f32>,
 // Godot's light_specular: the scale of the light's specular lobes.
 specular:f32,
 // A spot light's unit direction or a rectangle's unit normal; zero for a
 // point light.
 direction:vec3<f32>,
 range:f32,
 // Filament's spot scale and offset; 0 and 1 leave a point light and a
 // rectangle whole.
 spot_scale:f32,
 spot_offset:f32,
 // LIGHT_PUNCTUAL or LIGHT_RECT.
 shape:u32,
 padding:u32,
 // A rectangle's half width along its width's axis (Bevy's RectLight right
 // times half its width), and half its height along Bevy's up, the width's
 // axis crossed with its normal.
 half_width:vec3<f32>,
 half_height:f32,
}
// A rectangle's half height along its height's axis: its width's axis
// crossed with its normal (Bevy's RectLight up).
fn light_rect_half_height(rect:Light)->vec3<f32> {
 return normalize(cross(rect.half_width,rect.direction))*rect.half_height;
}
// A scene light's shadow in the local-light shadow atlas, at its identity's
// index in group 0's `local_shadows`: its faces' places in the atlas and
// how a point projects into them. A cube has six faces around its light
// (View::local_shadow_face), a spot one (View::spot_shadow).
struct LocalShadow {
 // A spot face's reversed-Z projection, from the world, out to the range.
 clip_from_world:mat4x4<f32>,
 // Each face's top-left corner in atlas UV: face 2i in xy, 2i+1 in zw.
 corners:array<vec4<f32>,3>,
 // A face's size in atlas UV.
 size:f32,
 // The near plane of the faces' projections, in metres.
 near:f32,
 // World metres per atlas texel at a metre along a face's axis.
 texel_scale:f32,
 // LOCAL_SHADOW_NONE, LOCAL_SHADOW_CUBE or LOCAL_SHADOW_SPOT.
 kind:u32,
 // 1 while its static layers hold its static casters; 0 for a light that
 // moved this frame, whose faces hold every caster.
 layers:u32,
 padding:array<u32,3>,
}
// Light.shape: a point or spot light, and a rectangle.
const LIGHT_PUNCTUAL:u32=0u;
const LIGHT_RECT:u32=1u;
// LocalShadow.kind: a light without a shadow, one with six cube faces, and
// a spot with one face.
const LOCAL_SHADOW_NONE:u32=0u;
const LOCAL_SHADOW_CUBE:u32=1u;
const LOCAL_SHADOW_SPOT:u32=2u;
