struct Backdrop {
 @builtin(position) clip:vec4<f32>,
 @location(0) uv:vec2<f32>,
}
@vertex fn backdrop_vs(@builtin(vertex_index) id:u32)->Backdrop {
 var o:Backdrop;
 let p=fullscreen_corner(id);
 o.clip=fullscreen_position(p);
 o.uv=p;
 return o;
}
struct BackdropOutput {
 @location(0) color:vec4<f32>,
 @location(1) motion:vec4<f32>,
}
// Each directional light's disc toward ray_dir_ws, through the air, where the
// planet does not hide it: Bevy 9d120361303727a66b62f31f0d053793af62417a
// crates/bevy_pbr/src/atmosphere/functions.wesl's sample_sun_radiance (MIT OR
// Apache-2.0, src/LICENSE-bevy.txt), its antialiased edge and its ground
// test. The CPU packs each disc's radiance, the light's colour times its
// illuminance times the air's transmittance toward it over the disc's solid
// angle (`view::frame_uniform`), so the backdrop needs no transmittance
// table; an absent or off light's disc is black.
fn sample_sun_radiance(ray_dir_ws:vec3<f32>)->vec3<f32> {
 let r=frame.sky_camera_radius;
 let mu_view=ray_dir_ws.y;
 let shadow_factor=f32(!ray_intersects_ground(r,mu_view));
 var sun_radiance=vec3(0.);
 for (var light_i=0u;light_i<FRAME_DIRECTIONAL_LIGHTS;light_i++) {
  let disc=frame.sky_discs[light_i];
  // Any nonzero length; an off light's zero direction stays zero.
  let toward=frame.directional_lights[light_i].direction_to_light;
  let direction_to_light=toward*inverseSqrt(max(dot(toward,toward),1e-30));
  let neg_LdotV=dot(direction_to_light,ray_dir_ws);
  let angle_to_sun=fast_acos(clamp(neg_LdotV,-1.,1.));
  let w=max(0.5*fwidth(angle_to_sun),1e-6);
  let factor=1.-smoothstep(disc.a-w,disc.a+w,angle_to_sun);
  sun_radiance+=disc.rgb*factor*shadow_factor;
 }
 return sun_radiance;
}
// The procedural sky (FRAME_BACKDROP_SKY) toward direction: the sky stage's
// sky-view table, which backdrop_map holds, from the camera's radius, plus
// the lights' discs (Bevy's render_sky.wesl, its sky branch). No yaw turns
// it; scene colour is scene-referred, so Bevy's view.exposure factor goes.
fn sky_backdrop(direction:vec3<f32>)->vec3<f32> {
 let inscattering=sample_sky_view_lut(backdrop_map,environment_sampler,frame.sky_camera_radius,direction);
 return inscattering+sample_sun_radiance(direction);
}
fn backdrop_color(i:Backdrop)->vec4<f32> {
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
 if (frame.flags&FRAME_BACKDROP_SKY)!=0u {
  return finite_scene_color(vec4(sky_backdrop(direction)*frame.backdrop_brightness,1.));
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
// the motion). A procedural sky is not turned: its yaw is 0 in both frames.
// A projection that gives the direction no positive w (an orthographic
// camera's is 0) cannot place it on screen; the backdrop then records no
// motion.
fn sky_motion(i:Backdrop)->vec2<f32> {
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
@fragment fn backdrop_fs(i:Backdrop)->BackdropOutput {
 return BackdropOutput(backdrop_color(i),vec4(sky_motion(i),0.,0.));
}
