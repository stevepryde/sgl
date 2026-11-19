// The sky-view table: Bevy 9d120361303727a66b62f31f0d053793af62417a
// crates/bevy_pbr/src/atmosphere/sky_view_lut.wesl (MIT OR Apache-2.0,
// src/LICENSE-bevy.txt): each texel is the in-scattered radiance toward a
// zenith angle and azimuth from the camera's radius, which the backdrop draw
// reads through sky_view_lut_r_mu_azimuth_to_uv. Departures from Bevy, beside
// shading/sky.wgsl's and common.wgsl's:
// 1. No world transform: the camera stands at SkyUniform.camera_radius
//    straight above the planet's centre, its ray directions in world axes
//    (Bevy's flat-world get_local_up), so AtmosphereTransforms goes.
// 2. Each texel is taken at its centre, as Bevy's other tables and Wicked
//    Engine 4323a33's sky view (skyAtmosphere_skyViewLutCS.hlsl 14-15) take
//    theirs; Bevy's idx / size is half a texel off the mapping its reader
//    inverts.
@group(0) @binding(4) var sky_view_lut_out:texture_storage_2d<rgba16float,write>;

@compute @workgroup_size(16,16,1)
fn sky_view_lut_main(@builtin(global_invocation_id) idx:vec3<u32>) {
 let size=vec2(SKY_VIEW_WIDTH,SKY_VIEW_HEIGHT);
 if any(idx.xy>=size) {
  return;
 }
 let uv=(vec2<f32>(idx.xy)+0.5)/vec2<f32>(size);
 let r=sky.camera_radius;
 let zenith_azimuth=sky_view_lut_uv_to_zenith_azimuth(r,uv);
 let ray_dir=zenith_azimuth_to_ray_dir(zenith_azimuth.x,zenith_azimuth.y);
 let cam_pos=vec3(0.,r,0.);
 let mu=ray_dir.y;
 let t_max=max_atmosphere_distance(r,mu);
 let result=raymarch_atmosphere(cam_pos,ray_dir,t_max,true);
 textureStore(sky_view_lut_out,idx.xy,vec4(result.inscattering,1.));
}
