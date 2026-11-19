// The aerial volume: Bevy 9d120361303727a66b62f31f0d053793af62417a
// crates/bevy_pbr/src/atmosphere/aerial_view_lut.wesl (MIT OR Apache-2.0,
// src/LICENSE-bevy.txt), Hillaire 2020's camera volume: over the camera's
// frustum, AERIAL_SIDE columns across and down and AERIAL_SLICES deep, each
// texel what lies between the camera and its slice's end along its
// column's view ray, marched through the medium with
// common.wgsl's sample_local_inscattering (the lights above the air, through
// the transmittance and multiple-scattering tables). The fog's integration
// composes it into the frame's one fog volume. Departures from Bevy, each for
// the reason given:
// 1. Slices spaced by the square, as Wicked Engine 4323a33 spaces its camera
//    volume (shading/sky.wgsl aerial_slice_distance), not linearly: the first
//    ends 31.25 m out instead of 1 km, where near haze is decided.
// 2. Linear radiance (rgb), not its logarithm: the reader interpolates the
//    same data either way, and the squared slices already concentrate them
//    where the radiance changes fastest.
// 3. The mean of the three channels' transmittance in alpha, as Bevy's own
//    path without dual-source blending and Wicked's camera volume
//    (skyAtmosphere_cameraVolumeLutCS.hlsl) keep it: the encoding of the
//    fog's integrated volume, which composes the two. Bevy's native path
//    blends RGB transmittance in a full-screen pass blended surfaces never
//    see.
// 4. March distances are multiplied by the game's strength
//    (Sky::aerial_perspective), positions and steps alike: distance
//    scaling, so a small world reads as a large one, the air at each sample
//    as far away and as high as the scaled distance puts it (the practice
//    of Unreal's documented Aerial Perspective View Distance Scale). This
//    departs from Wicked Engine 4323a33's aerialPerspectiveScale, which
//    multiplies only each step's optical depth (skyAtmosphere.hlsli 650)
//    at unscaled positions, so its strength thickens the air at the
//    camera's height rather than reaching farther through the atmosphere.
// 5. The rays are the fog's froxel columns' (shading/fog.wgsl
//    fog_froxel_view_position), from the unjittered projection, so the
//    integration reads each column's own ray; the camera stands at
//    SkyUniform.camera_radius straight above the planet's centre.
// 6. The step count is the named cap AERIAL_SLICE_SAMPLES and the light
//    loops FRAME_DIRECTIONAL_LIGHTS (AR-12).

// Bevy's aerial_view_lut_samples: the march's steps a slice, its inner
// loop's cap.
const AERIAL_SLICE_SAMPLES:u32=10u;

// The camera the volume lies over: its view to world, and its unjittered
// projection as shading::fog::froxel_projection packs it; the game's
// strength, Sky::aerial_perspective.
struct SkyAerial {
 world_from_view:mat4x4<f32>,
 projection:vec4<f32>,
 strength:f32,
}
@group(0) @binding(9) var<uniform> aerial:SkyAerial;
@group(0) @binding(10) var aerial_view_lut_out:texture_storage_3d<rgba16float,write>;

@compute @workgroup_size(8,8,1)
fn aerial_view_lut_main(@builtin(global_invocation_id) idx:vec3<u32>) {
 let size=vec2(AERIAL_SIDE);
 if any(idx.xy>=size) {
  return;
 }
 let uv=(vec2<f32>(idx.xy)+.5)/vec2<f32>(size);
 let view_ray=fog_froxel_view_position(aerial.projection,fog_froxel_ndc(uv),1.);
 let ray_dir=normalize((aerial.world_from_view*vec4(view_ray,0.)).xyz);
 let world_pos=vec3(0.,sky.camera_radius,0.);
 var prev_t=0.;
 var total_inscattering=vec3(0.);
 var throughput=vec3(1.);
 for (var slice_i=0u;slice_i<AERIAL_SLICES;slice_i++) {
  let start=select(0.,aerial_slice_distance(f32(slice_i)-1.),slice_i>0u);
  let end=aerial_slice_distance(f32(slice_i));
  for (var step_i=0u;step_i<AERIAL_SLICE_SAMPLES;step_i++) {
   let t_i=mix(start,end,(f32(step_i)+SKY_MIDPOINT_RATIO)/f32(AERIAL_SLICE_SAMPLES))*aerial.strength;
   let dt=t_i-prev_t;
   prev_t=t_i;
   let sample_pos=world_pos+ray_dir*t_i;
   let local_r=length(sample_pos);
   let absorption=sample_density(sky.atmosphere,local_r,SKY_ABSORPTION_DENSITY);
   let scattering=sample_density(sky.atmosphere,local_r,SKY_SCATTERING_DENSITY);
   let extinction=absorption+scattering;
   let sample_optical_depth=extinction*dt;
   let sample_transmittance=exp(-sample_optical_depth);
   // One segment of the integral.
   let inscattering=sample_local_inscattering(scattering,ray_dir,sample_pos);
   // Analytical integration of the single scattering term in the radiance
   // transfer equation.
   let s_int=(inscattering-inscattering*sample_transmittance)/max(extinction,SKY_MIN_EXTINCTION);
   total_inscattering+=throughput*s_int;
   throughput*=sample_transmittance;
   if all(throughput<vec3(0.001)) {
    break;
   }
  }
  let mean_transmittance=(throughput.r+throughput.g+throughput.b)/3.;
  textureStore(aerial_view_lut_out,vec3(idx.xy,slice_i),vec4(total_inscattering,mean_transmittance));
 }
}
