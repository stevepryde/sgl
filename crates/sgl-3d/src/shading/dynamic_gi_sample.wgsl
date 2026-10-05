// The dynamic GI volume's irradiance at a receiver, which the one
// determination of indirect diffuse light takes (surface.wgsl). Reads
// `frame`, `dynamic_gi_probes` and `baked_sampler`.
//
// Ports Wicked Engine df44c3db4c4927492bc9c791eac715d98d7ed091's
// ddgi_sample_irradiance (WickedEngine/shaders/ShaderInterop_DDGI.h
// 147–317, after Majercik et al. 2019: the eight probes about the point,
// weighted trilinearly from the cell's rest corner, by the smooth backface
// test and by Chebyshev visibility from the probe's depth moments, the
// weight floored and crushed), reading irradiance by the normal from
// revision 95e357f73f28d24e70ce7a1ab8f9fb954de457e4's colour map (251–257)
// in place of its spherical harmonics, MIT (src/LICENSE-wicked.txt).
// Changed: a probe not yet blended weighs nothing, and a receiver whose
// probes all weigh nothing keeps its fallback; the volume's share fades to
// nothing over the one spacing past its extent, as RTXGI's volume blend
// weight fades (volume_share.wgsl); f32 in place of half; the sampler is
// `baked_sampler`.
// Changed, the weights, to those NVIDIA RTXGI's DDGIGetVolumeIrradiance
// takes (practice only; its code is not copied): the whole wrap-shading weight
// (wrap^2 + 0.2) in place of Wicked's blend of a hard saturate(dot) test with
// it at smooth_backface 0.01, and a floor of 1e-6 in place of 0.01. Wicked's
// pair ties every probe of a receiver near a wall and facing it at the floor,
// those behind the receiver by the hard test and those beyond the wall by
// visibility, so it takes the light beyond the wall: at 0.3 m from a wall of
// an inward-facing room under a sky of radiance 1, 0.35 of it, where the wrap
// weight leaves 1e-4 and the floor 1e-8. RTXGI's further floor of 0.05 on the
// Chebyshev weight is not taken: it raises what an occluded probe gives.
// Changed, the visibility point: offset by Majercik et al. 2021's
// self-shadow bias (JCGT 10(2), equation 2: (n 0.2 + v 0.8) 0.75 times the
// least spacing times 0.3, v toward the viewer) in place of Wicked's 1 mm
// along the normal. The wrap weight lets the probes behind a surface weigh,
// and without the bias the surface shadows itself against the probes in
// front: a black floor under that sky took 0.96-0.99 of it, and takes
// 0.998 with the bias. A receiver nearer a wall than the bias, facing it and
// seen head-on, tests visibility from beyond the wall (0.3 m from that wall
// with probes 2 m apart: up to 0.11 of the sky; 1e-5 seen 60 degrees off its
// normal). The probes behind a small object's faces also return to it light
// it reflected, so a change in the bounce about it fades over tens of frames
// where Wicked's faded within a few.
// Majercik et al. 2021's TunableShadowBias, at its default.
const DDGI_SELF_SHADOW_BIAS:f32=.3;
// The volume's irradiance / PI at `position` along `normal`, seen from
// `view` (toward the viewer), in rgb, and in a its share of the receiver's
// indirect diffuse: 1 within the volume's extent, fading to 0 over the one
// spacing past it; 0 where the volume does not light the frame or no probe
// about the receiver has been blended.
fn dynamic_gi_irradiance(position:vec3<f32>,normal:vec3<f32>,view:vec3<f32>)->vec4<f32> {
 if (frame.flags&FRAME_DYNAMIC_GI)==0u {
  return vec4(0.);
 }
 let origin=frame.dynamic_gi_origin;
 let spacing=frame.dynamic_gi_spacing;
 let probes=frame.dynamic_gi_probes;
 let cells=(position-origin)/spacing;
 let last=vec3<f32>(probes-vec3(1u));
 let share=volume_share(cells,last);
 if share<=0. {
  return vec4(0.);
 }
 let size=vec2<f32>(textureDimensions(dynamic_gi_probes));
 let base_grid_coord=vec3<u32>(clamp(floor(cells),vec3(0.),last));
 // Taking the rest pose, as Wicked does.
 let reference_probe_pos=ddgi_probe_position_rest(base_grid_coord,origin,spacing);
 let alpha=saturate((position-reference_probe_pos)/spacing);
 // The self-shadow bias, from the receiver toward its viewer and normal.
 let bias=(normal*.2+view*.8)*(.75*min(spacing.x,min(spacing.y,spacing.z)))*DDGI_SELF_SHADOW_BIAS;
 var sum_irradiance=vec3(0.);
 var sum_weight=0.;
 for (var i=0u;i<8u;i++) {
  let offset=vec3(i,i>>1u,i>>2u)&vec3(1u);
  let probe_grid_coord=min(base_grid_coord+offset,probes-vec3(1u));
  let data=textureLoad(dynamic_gi_probes,ddgi_probe_data_pixel(probe_grid_coord,probes),0);
  if data.a<=0. {
   continue;
  }
  let probe_pos=ddgi_probe_position(probe_grid_coord,origin,spacing,data.rgb);
  let probe_to_point=position-probe_pos+bias;
  let dir=normalize(-probe_to_point);
  let trilinear=mix(1.-alpha,alpha,vec3<f32>(offset));
  var weight=1.;
  // Smooth backface test.
  let true_direction_to_probe=normalize(probe_pos-position);
  let wrap=max(.0001,(dot(true_direction_to_probe,normal)+1.)*.5);
  weight*=wrap*wrap+.2;
  // Moment visibility test.
  let depth_uv=ddgi_probe_uv(ddgi_probe_depth_pixel(probe_grid_coord,probes),DDGI_DEPTH_RESOLUTION,-dir,size);
  let dist_to_probe=length(probe_to_point);
  let moments=textureSampleLevel(dynamic_gi_probes,baked_sampler,depth_uv,0.).xy;
  let mean=moments.x;
  let variance=abs(mean*mean-moments.y);
  let behind=max(dist_to_probe-mean,0.);
  var chebyshev_weight=variance/(variance+behind*behind);
  chebyshev_weight=max(chebyshev_weight*chebyshev_weight*chebyshev_weight,0.);
  weight*=select(chebyshev_weight,1.,dist_to_probe<=mean);
  // Avoid zero weight.
  weight=max(.000001,weight);
  let color_uv=ddgi_probe_uv(ddgi_probe_color_pixel(probe_grid_coord,probes),DDGI_COLOR_RESOLUTION,normal,size);
  let probe_irradiance=textureSampleLevel(dynamic_gi_probes,baked_sampler,color_uv,0.).rgb;
  // Crush tiny weights but keep the curve continuous, before the
  // trilinear weights.
  let crush_threshold=.2;
  if weight<crush_threshold {
   weight*=weight*weight/(crush_threshold*crush_threshold);
  }
  weight*=trilinear.x*trilinear.y*trilinear.z;
  sum_irradiance+=weight*probe_irradiance;
  sum_weight+=weight;
 }
 if sum_weight<=0. {
  return vec4(0.);
 }
 return vec4(sum_irradiance/sum_weight,share);
}
