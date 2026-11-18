struct Sky {
 @builtin(position) clip:vec4<f32>,
 @location(0) uv:vec2<f32>,
}
@vertex fn sky_vs(@builtin(vertex_index) id:u32)->Sky {
 var o:Sky;
 let p=fullscreen_corner(id);
 o.clip=fullscreen_position(p);
 o.uv=p;
 return o;
}
struct SkyOutput {
 @location(0) color:vec4<f32>,
 @location(1) motion:vec4<f32>,
}
fn sky_color(i:Sky)->vec4<f32> {
 let world=view.inverse_view_projection*vec4(i.uv*2.-vec2(1.),1.,1.);
 let direction=normalize(world.xyz/world.w-view.eye);
 // Probe captures record the sky that reflections fall back to beyond the
 // probes (source completion's prefiltered sky at the reflection yaw and
 // intensity), not the camera's backdrop, so the two agree where they blend.
 if (view.flags&VIEW_PROBE_CAPTURE)!=0u {
  let d=pmrem_direction(direction,frame.reflection_yaw);
  return vec4(pmrem_sample(environment_map,environment_sampler,d,0.)*frame.reflection_intensity,1.);
 }
 if (frame.flags&FRAME_BACKDROP_COLOR)!=0u {
  return vec4(frame.backdrop_color,1.);
 }
 let uv=panorama_uv(direction,frame.backdrop_yaw);
 return vec4(textureSampleLevel(backdrop_map,environment_sampler,uv,0.).rgb*frame.backdrop_brightness,1.);
}
// Background at infinity moves with the camera's rotation only (Diligent's
// EnvMap.psh GetMotionVector, Bevy's background motion vectors), and with
// the panorama's own turn: its direction through this camera, and the
// direction the same point of the panorama lay in through the last frame's,
// encoded as geometry's motion is. The panorama turning from
// previous_backdrop_yaw to backdrop_yaw (panorama_uv's rotation about +Y)
// carries what shows in a direction now from that direction turned by
// previous_backdrop_yaw - backdrop_yaw, as an object's previous model
// matrix carries its vertices (neither upstream turns its environment in
// the motion). A projection that gives the direction no positive w (an
// orthographic camera's is 0) cannot place it on screen; the sky then
// records no motion.
fn sky_motion(i:Sky)->vec2<f32> {
 let world=view.inverse_view_projection*vec4(i.uv*2.-vec2(1.),1.,1.);
 let direction=world.xyz/world.w-view.eye;
 let current=view.stable_view_projection*vec4(direction,0.);
 if current.w<=0. {
  return vec2(0.);
 }
 let turn=frame.previous_backdrop_yaw-frame.backdrop_yaw;
 let c=cos(turn);
 let s=sin(turn);
 let was=vec3(c*direction.x+s*direction.z,direction.y,c*direction.z-s*direction.x);
 let previous=view.previous_view_projection*vec4(was,0.);
 return gbuffer_encode_motion(current,previous);
}
@fragment fn sky_fs(i:Sky)->SkyOutput {
 return SkyOutput(sky_color(i),vec4(sky_motion(i),0.,0.));
}
