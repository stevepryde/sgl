// The sky stage's bindings and the marches its multiple-scattering and
// sky-view passes share: Bevy 9d120361303727a66b62f31f0d053793af62417a
// crates/bevy_pbr/src/atmosphere/functions.wesl's sample_local_inscattering
// and raymarch_atmosphere (MIT OR Apache-2.0, src/LICENSE-bevy.txt), over
// shading/sky.wgsl's model. Departures from Bevy, beside shading/sky.wgsl's:
// 1. The lights are the stage's own (SkyUniform.lights): the frame's
//    directional lights as they shine above the air, colour times
//    illuminance, an absent or off light black; every light loop is capped
//    by FRAME_DIRECTIONAL_LIGHTS (AR-12).
// 2. raymarch_atmosphere's float sample counter is an integer one, capped by
//    SKY_VIEW_SAMPLES (AR-12), at Bevy's sample positions; it takes the
//    camera's position as Bevy's atmosphere space holds it (the planet's
//    centre at the origin, +Y up) and drops its unused uv.
// 3. Its ground term takes the transmittance toward the light from the
//    ground point's radius, as Wicked Engine 4323a33
//    (WickedEngine/shaders/skyAtmosphere.hlsli 746-754, MIT) and Hillaire's
//    sample do; Bevy reads its table at radius 0, which its mapping clamps
//    to the ground's zenith, whatever the light's elevation.
// The stage's tables (sampled, linear and clamped): transmittance and
// multiple scattering. Each pass writes its own table at binding 4.

// The sky-view march's steps (Bevy's sky_view_lut_samples): its loop's cap.
const SKY_VIEW_SAMPLES:u32=16u;

// One directional light as the sky's marches take it: toward the light, and
// its colour times its illuminance above the air (0 for no light).
struct SkyLight {
 direction_to_light:vec3<f32>,
 color:vec3<f32>,
}
// The stage's uniform: the medium, the frame's lights and the camera's
// distance from the planet's centre.
struct SkyUniform {
 atmosphere:SkyAtmosphere,
 lights:array<SkyLight,FRAME_DIRECTIONAL_LIGHTS>,
 camera_radius:f32,
}
@group(0) @binding(0) var<uniform> sky:SkyUniform;
@group(0) @binding(1) var transmittance_lut:texture_2d<f32>;
@group(0) @binding(2) var multiscattering_lut:texture_2d<f32>;
@group(0) @binding(3) var sky_lut_sampler:sampler;

// L_scat, equation 3 of the paper: the total single-order scattering toward
// the view at one point, plus the multiple-scattering table's share.
fn sample_local_inscattering(local_scattering:vec3<f32>,ray_dir:vec3<f32>,world_pos:vec3<f32>)->vec3<f32> {
 let local_r=length(world_pos);
 let local_up=normalize(world_pos);
 var inscattering=vec3(0.);
 for (var light_i=0u;light_i<FRAME_DIRECTIONAL_LIGHTS;light_i++) {
  let light=sky.lights[light_i];
  let mu_light=dot(light.direction_to_light,local_up);
  // -(L . V) == (L . -V). -V here is our ray direction, which points away
  // from the view instead of towards it (as is the convention for V).
  let neg_LdotV=dot(light.direction_to_light,ray_dir);
  let transmittance_to_light=sample_transmittance_lut(transmittance_lut,sky_lut_sampler,local_r,mu_light);
  let shadow_factor=transmittance_to_light*f32(!ray_intersects_ground(local_r,mu_light));
  let scattering_coeff=sample_scattering(sky.atmosphere,local_r,neg_LdotV);
  // Transmittance from scattering event to light source.
  let scattering_factor=shadow_factor*scattering_coeff;
  // Additive factor from the multiscattering LUT.
  let psi_ms=sample_multiscattering_lut(multiscattering_lut,sky_lut_sampler,local_r,mu_light);
  let multiscattering_factor=psi_ms*local_scattering;
  inscattering+=light.color*(scattering_factor+multiscattering_factor);
 }
 return inscattering;
}

struct RaymarchResult {
 inscattering:vec3<f32>,
 transmittance:vec3<f32>,
}

// The in-scattered radiance and transmittance along ray_dir from pos (the
// planet's centre at the origin) for at most t_max metres, in up to
// SKY_VIEW_SAMPLES steps; with ground, the sunlit ground where the ray meets
// it.
fn raymarch_atmosphere(pos:vec3<f32>,ray_dir:vec3<f32>,t_max:f32,ground:bool)->RaymarchResult {
 let r=length(pos);
 let up=normalize(pos);
 let mu=dot(ray_dir,up);
 // Optimization: reduce the sample count at close proximity to the scene.
 let sample_count=mix(1.,f32(SKY_VIEW_SAMPLES),saturate(t_max*0.01));
 let segment=get_raymarch_segment(r,mu);
 let t_start=segment.start;
 var t_end=segment.end;
 t_end=min(t_end,t_max);
 let t_total=t_end-t_start;
 var result:RaymarchResult;
 result.inscattering=vec3(0.);
 result.transmittance=vec3(1.);
 // Skip an invalid segment.
 if t_total<=0. {
  return result;
 }
 var prev_t=t_start;
 var optical_depth=vec3(0.);
 for (var i=0u;i<SKY_VIEW_SAMPLES;i++) {
  let s=f32(i);
  if s>=sample_count {
   break;
  }
  // Linear distribution from atmosphere entry to exit/ground.
  let t_i=t_start+t_total*(s+SKY_MIDPOINT_RATIO)/sample_count;
  let dt_i=(t_i-prev_t);
  prev_t=t_i;
  let sample_pos=pos+ray_dir*t_i;
  let local_r=length(sample_pos);
  let absorption=sample_density(sky.atmosphere,local_r,SKY_ABSORPTION_DENSITY);
  let scattering=sample_density(sky.atmosphere,local_r,SKY_SCATTERING_DENSITY);
  let extinction=absorption+scattering;
  let sample_optical_depth=extinction*dt_i;
  optical_depth+=sample_optical_depth;
  let sample_transmittance=exp(-sample_optical_depth);
  let inscattering=sample_local_inscattering(scattering,ray_dir,sample_pos);
  let s_int=(inscattering-inscattering*sample_transmittance)/max(extinction,SKY_MIN_EXTINCTION);
  result.inscattering+=result.transmittance*s_int;
  result.transmittance*=sample_transmittance;
  if all(result.transmittance<vec3(0.001)) {
   break;
  }
 }
 // Include reflected luminance from the planet's ground.
 if ground&&ray_intersects_ground(r,mu) {
  for (var light_i=0u;light_i<FRAME_DIRECTIONAL_LIGHTS;light_i++) {
   let light=sky.lights[light_i];
   let transmittance_to_ground=exp(-optical_depth);
   // The point on the sphere and its normal (up).
   let sphere_point=pos+ray_dir*t_end;
   let sphere_normal=normalize(sphere_point);
   let mu_light=dot(light.direction_to_light,sphere_normal);
   let transmittance_to_light=sample_transmittance_lut(transmittance_lut,sky_lut_sampler,length(sphere_point),mu_light);
   let light_luminance=transmittance_to_light*max(mu_light,0.)*light.color;
   // Normalized Lambert BRDF.
   let ground_luminance=transmittance_to_ground*sky.atmosphere.ground_albedo/SKY_PI;
   result.inscattering+=ground_luminance*light_luminance;
  }
 }
 return result;
}
