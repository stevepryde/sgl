// A light as it reaches one receiver point, as Filament's Light: the unit
// direction toward the light, the radiance it brings, the fraction of it
// that is unoccluded, the scale of its specular lobes (Godot's
// light_specular; 1 is physical), and its size: the radius of the sphere it
// shines from over the distance to its centre, or a directional light's
// disc radius, which widens its highlights (pbr_sized_light), 0 for a point.
// Each light builds one for the point, and
// surface_direct_light (surface.wgsl) shades it. A sample with zero
// visibility does not reach the point. A rectangle's names its scene light,
// whose face surface_direct_light integrates, and its radiance is the face's
// luminance; any other light's is NO_RECT_LIGHT.
struct LightSample {
 direction:vec3<f32>,
 radiance:vec3<f32>,
 visibility:f32,
 specular:f32,
 rect:u32,
 size:f32,
}
const NO_RECT_LIGHT:u32=0xffffffffu;
