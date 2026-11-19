// The multiple-scattering table: Bevy 9d120361303727a66b62f31f0d053793af62417a
// crates/bevy_pbr/src/atmosphere/multiscattering_lut.wesl (MIT OR
// Apache-2.0, src/LICENSE-bevy.txt), Hillaire 2020's section 5.5: each texel
// is Ψ_ms for a radius and a light's zenith cosine, from
// SKY_MULTISCATTERING_DIRECTIONS directions (the workgroup's depth), each
// marched in SKY_MULTISCATTERING_SAMPLES steps, then summed in shared memory.
// Departures from Bevy, beside shading/sky.wgsl's and common.wgsl's:
// 1. The light's zenith cosine is mu: its direction is
//    (0, mu, -sqrt(1 - mu²)), as Wicked Engine 4323a33 builds it
//    (WickedEngine/shaders/skyAtmosphere_multiScatteredLuminanceLutCS.hlsl
//    25, MIT) and Hillaire's sample does; Bevy's normalize(vec3(0, mu, -1))
//    has the zenith cosine mu / sqrt(1 + mu²).
// 2. The ground's reflection is Lambertian, albedo / π, and takes the
//    transmittance toward the light from the ground point's radius, as
//    Wicked Engine's (skyAtmosphere.hlsli 746-754) and Hillaire's do; Bevy
//    omits the 1 / π and reads its table at radius 0.
// 3. The sample count and the reduction's steps are named caps (AR-12).
const SKY_MULTISCATTERING_DIRECTIONS:u32=64u;
// log2 of SKY_MULTISCATTERING_DIRECTIONS: the reduction halves the sums this
// many times.
const SKY_MULTISCATTERING_REDUCTION_STEPS:u32=6u;
const SKY_MULTISCATTERING_SAMPLES:u32=20u;

const PHI_2:vec2<f32>=vec2(1.3247179572447460259609088,1.7548776662466927600495087);

@group(0) @binding(4) var multiscattering_lut_out:texture_storage_2d<rgba16float,write>;

fn s2_sequence(n:u32)->vec2<f32> {
 return fract(0.5+f32(n)*PHI_2);
}

// Lambert equal-area projection.
fn uv_to_sphere(uv:vec2<f32>)->vec3<f32> {
 let phi=SKY_PI_2*uv.y;
 let sin_lambda=2.*uv.x-1.;
 let cos_lambda=sqrt(1.-sin_lambda*sin_lambda);
 return vec3(cos_lambda*cos(phi),cos_lambda*sin(phi),sin_lambda);
}

// Shared memory for the workgroup's sums.
var<workgroup> multi_scat_shared_mem:array<vec3<f32>,SKY_MULTISCATTERING_DIRECTIONS>;
var<workgroup> l_shared_mem:array<vec3<f32>,SKY_MULTISCATTERING_DIRECTIONS>;

@compute @workgroup_size(1,1,SKY_MULTISCATTERING_DIRECTIONS)
fn multiscattering_lut_main(@builtin(global_invocation_id) global_id:vec3<u32>) {
 let uv=(vec2<f32>(global_id.xy)+0.5)/vec2(f32(SKY_MULTISCATTERING_SIZE));
 let r_mu=multiscattering_lut_uv_to_r_mu(uv);
 let light_dir=vec3(0.,r_mu.y,-sqrt(max(1.-r_mu.y*r_mu.y,0.)));
 let ray_dir=uv_to_sphere(s2_sequence(global_id.z));
 let ms_sample=sample_multiscattering_dir(r_mu.x,ray_dir,light_dir);
 // The contribution of this sample.
 let sphere_solid_angle=4.*SKY_PI;
 let sample_weight=sphere_solid_angle/f32(SKY_MULTISCATTERING_DIRECTIONS);
 multi_scat_shared_mem[global_id.z]=ms_sample.f_ms*sample_weight;
 l_shared_mem[global_id.z]=ms_sample.l_2*sample_weight;
 workgroupBarrier();
 // Parallel reduction, halving the sums each step.
 for (var k=0u;k<SKY_MULTISCATTERING_REDUCTION_STEPS;k++) {
  let step=SKY_MULTISCATTERING_DIRECTIONS>>(k+1u);
  if global_id.z<step {
   multi_scat_shared_mem[global_id.z]+=multi_scat_shared_mem[global_id.z+step];
   l_shared_mem[global_id.z]+=l_shared_mem[global_id.z+step];
  }
  workgroupBarrier();
 }
 if global_id.z>0u {
  return;
 }
 // The isotropic phase function.
 let f_ms=multi_scat_shared_mem[0]*SKY_FRAC_4_PI;
 let l_2=l_shared_mem[0]*SKY_FRAC_4_PI;
 // Equation 10 of the paper: the geometric series of infinite scattering.
 let psi_ms=l_2/(1.-f_ms);
 textureStore(multiscattering_lut_out,global_id.xy,vec4<f32>(psi_ms,1.));
}

struct MultiscatteringSample {
 l_2:vec3<f32>,
 f_ms:vec3<f32>,
}

fn sample_multiscattering_dir(r:f32,ray_dir:vec3<f32>,light_dir:vec3<f32>)->MultiscatteringSample {
 // The cosine of the view direction's zenith angle.
 let mu_view=ray_dir.y;
 let t_max=max_atmosphere_distance(r,mu_view);
 let dt=t_max/f32(SKY_MULTISCATTERING_SAMPLES);
 var optical_depth=vec3<f32>(0.);
 var l_2=vec3(0.);
 var f_ms=vec3(0.);
 var throughput=vec3(1.);
 for (var i=0u;i<SKY_MULTISCATTERING_SAMPLES;i++) {
  let t_i=dt*(f32(i)+0.5);
  let local_r=get_local_r(r,mu_view,t_i);
  let local_up=get_local_up(r,t_i,ray_dir);
  let absorption=sample_density(sky.atmosphere,local_r,SKY_ABSORPTION_DENSITY);
  let scattering=sample_density(sky.atmosphere,local_r,SKY_SCATTERING_DENSITY);
  let extinction=absorption+scattering;
  let sample_optical_depth=extinction*dt;
  let sample_transmittance=exp(-sample_optical_depth);
  optical_depth+=sample_optical_depth;
  let ms=scattering;
  let ms_int=(ms-ms*sample_transmittance)/max(extinction,SKY_MIN_EXTINCTION);
  f_ms+=throughput*ms_int;
  let mu_light=dot(light_dir,local_up);
  let transmittance_to_light=sample_transmittance_lut(transmittance_lut,sky_lut_sampler,local_r,mu_light);
  let shadow_factor=transmittance_to_light*f32(!ray_intersects_ground(local_r,mu_light));
  let s=scattering*shadow_factor*SKY_FRAC_4_PI;
  let s_int=(s-s*sample_transmittance)/max(extinction,SKY_MIN_EXTINCTION);
  l_2+=throughput*s_int;
  throughput*=sample_transmittance;
  if all(throughput<vec3(0.001)) {
   break;
  }
 }
 // Include reflected luminance from the planet's ground.
 if ray_intersects_ground(r,mu_view) {
  let transmittance_to_ground=exp(-optical_depth);
  let ground_point=vec3(0.,r,0.)+t_max*ray_dir;
  let local_up=normalize(ground_point);
  let mu_light=dot(light_dir,local_up);
  let transmittance_to_light=sample_transmittance_lut(transmittance_lut,sky_lut_sampler,length(ground_point),mu_light);
  let ground_luminance=transmittance_to_light*transmittance_to_ground*max(mu_light,0.)*sky.atmosphere.ground_albedo/SKY_PI;
  l_2+=ground_luminance;
 }
 return MultiscatteringSample(l_2,f_ms);
}
