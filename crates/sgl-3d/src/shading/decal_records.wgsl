// A decal's record, at its identity's index in group 0's `decals` (Scene::
// add_decal). Rust mirror: shading/decals.rs; the layout test compares the
// two.
struct Decal {
 // From the world to the box's decal space, Godot b130438's DecalData.xform:
 // x and z across its images from 0 to 1 (the images' U and V), y from -1
 // at its lower face to 1 at its upper.
 decal_from_world:mat4x4<f32>,
 // The box's unit axes in the world: its normal map's tangent frame
 // (Godot's normal_xform), y also the direction its normal fade measures
 // from (Godot's normal); and its fades.
 x_axis:vec3<f32>,
 upper_fade:f32,
 y_axis:vec3<f32>,
 lower_fade:f32,
 z_axis:vec3<f32>,
 normal_fade:f32,
 // Linear RGB and alpha that multiply its base colour image.
 color:vec4<f32>,
 // Where each of its images is in the decal atlas: offset in xy and size in
 // zw, in atlas UV; all zero for a map it does not have.
 base_color_rect:vec4<f32>,
 normal_rect:vec4<f32>,
 metallic_roughness_rect:vec4<f32>,
 base_color_mix:f32,
 padding:array<f32,3>,
}
