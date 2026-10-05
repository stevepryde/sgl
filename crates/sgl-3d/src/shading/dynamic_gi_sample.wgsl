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
// Wicked's DDGI::smooth_backface (wiScene.h).
const DDGI_SMOOTH_BACKFACE:f32=.01;
// The volume's irradiance / PI at `position` along `normal` in rgb, and in
// a its share of the receiver's indirect diffuse: 1 within the volume's
// extent, fading to 0 over the one spacing past it; 0 where the volume does
// not light the frame or no probe about the receiver has been blended.
fn dynamic_gi_irradiance(position:vec3<f32>,normal:vec3<f32>)->vec4<f32> {
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
  let probe_to_point=position-probe_pos+normal*.001;
  let dir=normalize(-probe_to_point);
  let trilinear=mix(1.-alpha,alpha,vec3<f32>(offset));
  var weight=1.;
  // Smooth backface test.
  let true_direction_to_probe=normalize(probe_pos-position);
  let wrap=max(.0001,(dot(true_direction_to_probe,normal)+1.)*.5);
  weight*=mix(saturate(dot(dir,normal)),wrap*wrap+.2,DDGI_SMOOTH_BACKFACE);
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
  weight=max(.01,weight);
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
