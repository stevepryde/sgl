// The volumetric fog's froxel volume (stages/fog.wgsl writes it): how its
// depth slices are spaced, where a point at a frame position and view depth
// samples it, and how its in-scattered light and transmittance fog what lies
// behind. Godot b130438's get_depth_at_pos
// (servers/rendering/renderer_rd/shaders/environment/volumetric_fog_process.glsl)
// and volumetric_fog_process and its fog composition
// (shaders/forward_clustered/scene_forward_clustered.glsl), MIT
// (src/LICENSE-godot.txt).

// The view depth at `unit` (0 to 1 across the volume's slices) of a volume
// reaching `length` metres with `detail_spread`.
fn fog_slice_depth(unit:f32,length:f32,detail_spread:f32)->f32 {
 return length*pow(unit,detail_spread);
}
// The volume coordinate of a point at `uv` on the frame and `view_depth`
// metres deep. Beyond the volume, the sky included, it is the last slice.
fn fog_volume_coordinate(uv:vec2<f32>,view_depth:f32,inverse_length:f32,inverse_detail_spread:f32)->vec3<f32> {
 let unit=clamp(view_depth*inverse_length,0.,1.);
 return vec3(uv,pow(unit,inverse_detail_spread));
}
// `color` seen through `fog`: the light the fog scatters toward the camera
// (rgb) and its transmittance (a).
fn fog_composite(color:vec3<f32>,fog:vec4<f32>)->vec3<f32> {
 return color*fog.a+fog.rgb;
}
// A fog volume (content::transient::FogVolume; Rust mirror
// shading::fog::FogVolumeRecord): a box's frame from the world, its half
// size, the density and albedo it adds and its edge fade, and the sphere
// about it.
struct FogVolumeRecord {
 local_from_world:mat4x4<f32>,
 half_size:vec3<f32>,
 density:f32,
 albedo:vec3<f32>,
 edge_fade:f32,
 center:vec3<f32>,
 radius_squared:f32,
}
