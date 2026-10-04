// The frame's volumetric fog over what a draw shades at frame position
// `pixel` (a fragment's position xy) and `view_depth` metres deep (one over
// a perspective fragment's position w): its volume sampled where the point
// lies, while FRAME_FOG is set. Reads `view`,
// `frame`, `fog_volume` and `fog_sampler`.
fn frame_fog(color:vec3<f32>,pixel:vec2<f32>,view_depth:f32)->vec3<f32> {
 if (frame.flags&FRAME_FOG)==0u {
  return color;
 }
 let coordinate=fog_volume_coordinate(pixel/view.viewport,view_depth,frame.fog_inverse_length,frame.fog_inverse_detail_spread);
 return fog_composite(color,textureSampleLevel(fog_volume,fog_sampler,coordinate,0.));
}
