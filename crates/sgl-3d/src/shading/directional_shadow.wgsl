// The shadowed directional light's cascades (Frame.shadow_cascades,
// view::cascades): which one a receiver takes, and its filtered visibility.
// Reads `view`, `frame`, `directional_shadow_map` and `shadow_sampler`.
//
// Ports Bevy 9d12036 crates/bevy_pbr/src/render/shadows.wesl
// (get_cascade_index, world_to_directional_light_local,
// sample_directional_cascade, fetch_directional_shadow), MIT OR Apache-2.0
// (src/LICENSE-bevy.txt). Changes: one light has cascades, the frame's; the
// biases are Bevy's DirectionalLight defaults rather than per-light values;
// no PCSS; and probe captures and ray hits, which have no camera depth,
// take the first cascade that holds them (directional_shadow_at).
//
// The camera's fog takes Godot b130438's lookup instead
// (directional_shadow_medium, from servers/rendering/renderer_rd/shaders/
// environment/volumetric_fog_process.glsl, MODE_DENSITY's directional
// lights, MIT, src/LICENSE-godot.txt): the one cascade at the point's view
// depth, no receiver offset, and the fog tap. Changes: beyond the last
// cascade the fog is unshadowed, as surfaces are, where Godot fades the
// last cascade out by the light's shadow fade.

// Bevy's DirectionalLight::DEFAULT_SHADOW_DEPTH_BIAS, in metres toward the
// light.
const DIRECTIONAL_SHADOW_DEPTH_BIAS:f32=0.02;
// Bevy's DirectionalLight::DEFAULT_SHADOW_NORMAL_BIAS, 1.8 texels along the
// normal, times SQRT_2 for the worst-case diagonal offset
// (crates/bevy_pbr/src/render/light.rs).
const DIRECTIONAL_SHADOW_NORMAL_BIAS:f32=2.5455844;
// A cascade's rectangle in its layer: the whole layer.
const DIRECTIONAL_SHADOW_BOUNDS:vec4<f32>=vec4(0.,0.,1.,1.);
// view::cascades::SHADOW_CASCADE_OVERLAP's twin: the share of a cascade's
// far bound that the next cascade overlaps and the shading blends across.
const SHADOW_CASCADE_OVERLAP:f32=0.2;

fn get_cascade_index(view_z:f32)->u32 {
 for(var i:u32=0u;i<frame.shadow_cascade_count;i=i+1u) {
  if -view_z<frame.shadow_cascades[i].far_bound {
   return i;
  }
 }
 return frame.shadow_cascade_count;
}

// Converts from world space to the uv position in the light's shadow map.
//
// The depth is stored in the return value's z coordinate. If the return value's
// w coordinate is 0.0, then we landed outside the shadow map entirely.
fn world_to_directional_light_local(cascade_index:u32,offset_position:vec4<f32>)->vec4<f32> {
 let cascade=frame.shadow_cascades[cascade_index];

 let offset_position_clip=cascade.clip_from_world*offset_position;
 if offset_position_clip.w<=0.0 {
  return vec4(0.0);
 }
 let offset_position_ndc=offset_position_clip.xyz/offset_position_clip.w;
 // No shadow outside the orthographic projection volume
 if any(offset_position_ndc.xy<vec2<f32>(-1.0)) || offset_position_ndc.z<0.0 || any(offset_position_ndc>vec3<f32>(1.0)) {
  return vec4(0.0);
 }

 // compute texture coordinates for shadow lookup, compensating for the Y-flip difference
 // between the NDC and texture coordinates
 let flip_correction=vec2<f32>(0.5,-0.5);
 let light_local=offset_position_ndc.xy*flip_correction+vec2<f32>(0.5,0.5);

 let depth=offset_position_ndc.z;

 return vec4(light_local,depth,1.0);
}

// The receiver at `frag_position` with `surface_normal`, offset by the
// cascade's bias, in cascade `cascade_index`'s map: w is 0 outside it.
fn directional_cascade_local(light_id:u32,cascade_index:u32,frag_position:vec3<f32>,surface_normal:vec3<f32>)->vec4<f32> {
 let toward_light=normalize(frame.directional_lights[light_id].direction_to_light);
 let texel_size=frame.shadow_cascades[cascade_index].texel_size;
 // The normal bias is scaled to the texel size.
 let offset_position=shadow_receiver_offset(frag_position,surface_normal,toward_light,texel_size,DIRECTIONAL_SHADOW_NORMAL_BIAS,DIRECTIONAL_SHADOW_DEPTH_BIAS);
 return world_to_directional_light_local(cascade_index,vec4(offset_position,1.));
}

fn sample_directional_cascade(light_id:u32,cascade_index:u32,frag_position:vec3<f32>,surface_normal:vec3<f32>,frag_coord_xy:vec2<f32>,shadow_filtering:u32)->f32 {
 let light_local=directional_cascade_local(light_id,cascade_index,frag_position,surface_normal);
 if light_local.w==0.0 {
  return 1.0;
 }
 let texel_size=frame.shadow_cascades[cascade_index].texel_size;
 return sample_shadow_map(directional_shadow_map,shadow_sampler,light_local.xy,light_local.z,i32(cascade_index),DIRECTIONAL_SHADOW_BOUNDS,frag_coord_xy,texel_size,shadow_filtering);
}

// The camera's receivers: the cascade at the receiver's view depth, blended
// into the next across their overlap, with `shadow_filtering`.
fn fetch_directional_shadow(light_id:u32,frag_position:vec3<f32>,surface_normal:vec3<f32>,view_z:f32,frag_coord_xy:vec2<f32>,shadow_filtering:u32)->f32 {
 let cascade_index=get_cascade_index(view_z);

 if cascade_index>=frame.shadow_cascade_count {
  return 1.0;
 }

 var shadow=sample_directional_cascade(light_id,cascade_index,frag_position,surface_normal,frag_coord_xy,shadow_filtering);

 // Blend with the next cascade, if there is one.
 let next_cascade_index=cascade_index+1u;
 if next_cascade_index<frame.shadow_cascade_count {
  let this_far_bound=frame.shadow_cascades[cascade_index].far_bound;
  let next_near_bound=(1.0-SHADOW_CASCADE_OVERLAP)*this_far_bound;
  if -view_z>=next_near_bound {
   let next_shadow=sample_directional_cascade(light_id,next_cascade_index,frag_position,surface_normal,frag_coord_xy,shadow_filtering);
   shadow=mix(shadow,next_shadow,(-view_z-next_near_bound)/(this_far_bound-next_near_bound));
  }
 }
 return shadow;
}

// Probe captures and ray hits, which have no camera depth: the first
// cascade whose map holds the receiver, filtered with the fixed kernel.
fn directional_shadow_at(light_id:u32,frag_position:vec3<f32>,surface_normal:vec3<f32>)->f32 {
 for(var cascade_index:u32=0u;cascade_index<frame.shadow_cascade_count;cascade_index=cascade_index+1u) {
  let light_local=directional_cascade_local(light_id,cascade_index,frag_position,surface_normal);
  if light_local.w!=0.0 {
   let texel_size=frame.shadow_cascades[cascade_index].texel_size;
   return sample_shadow_map(directional_shadow_map,shadow_sampler,light_local.xy,light_local.z,i32(cascade_index),DIRECTIONAL_SHADOW_BOUNDS,vec2(0.),texel_size,SHADOW_FILTER_GAUSSIAN);
  }
 }
 return 1.0;
}

// The camera's fog at `position`, `view_z` deep: the cascade at that depth,
// unoffset, and its fog tap.
fn directional_shadow_medium(position:vec3<f32>,view_z:f32)->f32 {
 let cascade_index=get_cascade_index(view_z);
 if cascade_index>=frame.shadow_cascade_count {
  return 1.0;
 }
 let light_local=world_to_directional_light_local(cascade_index,vec4(position,1.));
 if light_local.w==0.0 {
  return 1.0;
 }
 // Godot's shadow_z_range: the cascade's depth in metres, one over its
 // orthographic projection's depth per metre.
 let clip_from_world=frame.shadow_cascades[cascade_index].clip_from_world;
 let depth_per_metre=vec3(clip_from_world[0].z,clip_from_world[1].z,clip_from_world[2].z);
 let z_range=inverseSqrt(dot(depth_per_metre,depth_per_metre));
 return sample_shadow_map_fog(directional_shadow_map,light_local.xy,light_local.z,i32(cascade_index),DIRECTIONAL_SHADOW_BOUNDS,z_range);
}

// Directional light `light_id`'s shadow at `receiver` (SHADOW_RECEIVER_*).
// The camera's surfaces see it at `pixel` (Bevy's selection by view depth,
// with their filter), its fog as Godot's fog does; a probe capture or ray
// hit sees the first cascade that holds it.
fn directional_shadow_visibility(light_id:u32,position:vec3<f32>,normal:vec3<f32>,pixel:vec2<f32>,receiver:u32)->f32 {
 if receiver!=SHADOW_RECEIVER_CAPTURE {
  let view_z=(view.view*vec4(position,1.)).z;
  if receiver==SHADOW_RECEIVER_MEDIUM {
   return directional_shadow_medium(position,view_z);
  }
  return fetch_directional_shadow(light_id,position,normal,view_z,pixel,shadow_filter(receiver));
 }
 return directional_shadow_at(light_id,position,normal);
}
