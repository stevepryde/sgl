// The frame's environment: its panorama and prefiltered PMREM atlas. Programs
// bind them through bind_lit or bind_unlit.
fn panorama_uv(direction:vec3<f32>,rotation:f32)->vec2<f32> {
 // Inverse scene rotation, matching Three.js background/environmentRotation.
 let c=cos(rotation);
 let s=sin(rotation);
 let d=vec3(c*direction.x-s*direction.z,direction.y,s*direction.x+c*direction.z);
 return vec2(atan2(d.z,d.x)/6.2831853+0.5,acos(clamp(d.y,-1.,1.))/3.14159265);
}
fn sample_environment(direction:vec3<f32>,roughness:f32)->vec3<f32> {
 let d=pmrem_direction(direction,frame.diffuse_environment_yaw);
 return pmrem_sample(environment_map,environment_sampler,d,roughness)*frame.diffuse_environment_intensity;
}
fn diffuse_environment(normal:vec3<f32>)->vec3<f32> {
 return sample_environment(normal,1.);
}
