// Where a ray toward a light ends on it (the architecture's Ray-traced
// shadows), which ray-traced shadows and the dynamic GI volume's visibility
// rays draw alike (S3D-5): a point of a point or spot light's sphere of its
// radius, a point of a rectangle's face, or a direction within a
// directional light's disc. A size of 0 is the light's centre or
// direction: a hard shadow.
//
// Ports Wicked Engine 2ff1d9e's screenspaceshadowCS.hlsl 104–106, 124–127,
// 153–159 and 186–188 under RTSHADOW, with get_tangentspace and
// hemispherepoint_cos from globals.hlsli 1452–1478 (MIT,
// src/LICENSE-wicked.txt). Changed: a directional light's spread is its
// disc's radius at unit distance, the tangent of half its angular
// diameter, where Wicked takes the light's radius in direction units; and
// SGL3D's lights have no capsule length.
// An orthonormal basis about unit `normal`: Wicked's get_tangentspace, its
// rows the columns here, so that the basis times a vector is HLSL's mul of
// the vector by Wicked's matrix.
fn get_tangentspace(normal:vec3<f32>)->mat3x3<f32> {
 let helper=select(vec3(1.,0.,0.),vec3(0.,0.,1.),abs(normal.x)>.99);
 let tangent=normalize(cross(normal,helper));
 let binormal=normalize(cross(normal,tangent));
 return mat3x3(tangent,binormal,normal);
}
// A point on the unit hemisphere about +z, cosine-weighted, from `u` and
// `v` in [0, 1): its projection on the disc below it is uniform.
fn hemispherepoint_cos(u:f32,v:f32)->vec3<f32> {
 let phi=v*2.*3.14159265359;
 let cos_theta=sqrt(1.-u);
 let sin_theta=sqrt(1.-cos_theta*cos_theta);
 return vec3(cos(phi)*sin_theta,sin(phi)*sin_theta,cos_theta);
}
// The point of scene light `light` a ray from `position` ends at, drawn by
// `random` in [0, 1)²: a rectangle's face, or a point or spot light's
// sphere, on the hemisphere about the direction from `position` toward it.
fn light_ray_end(light:Light,position:vec3<f32>,random:vec2<f32>)->vec3<f32> {
 if light.shape==LIGHT_RECT {
  return light.position+light.half_width*(random.x*2.-1.)+light_rect_half_height(light)*(random.y*2.-1.);
 }
 let toward=light.position-position;
 if light.radius<=0. || dot(toward,toward)<=0. {
  return light.position;
 }
 return light.position+get_tangentspace(normalize(toward))*hemispherepoint_cos(random.x,random.y)*light.radius;
}
// The unit direction of a ray toward a directional light whose direction
// toward it is unit `toward` and whose disc has radius `disc_radius` at
// unit distance, drawn by `random` in [0, 1)².
fn directional_ray_direction(toward:vec3<f32>,disc_radius:f32,random:vec2<f32>)->vec3<f32> {
 if disc_radius<=0. {
  return toward;
 }
 return normalize(toward+get_tangentspace(toward)*hemispherepoint_cos(random.x,random.y)*disc_radius);
}
