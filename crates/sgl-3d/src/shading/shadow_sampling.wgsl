// Shadow-map filters every 2D shadow kind shares: a depth array, a
// comparison sampler, a shadow-map position, its reversed-Z depth, its layer
// and the map's rectangle in it, and the filter each receiver takes. Reads
// `frame` for the temporal filter's switch and frame count.
//
// Ports Bevy 9d12036 crates/bevy_pbr/src/render/shadow_sampling.wesl
// (sample_shadow_map_hardware, sample_shadow_map_castano_thirteen, map,
// random_rotation_matrix, calculate_uv_offset_scale_jimenez_fourteen,
// sample_shadow_map_jimenez_fourteen, sample_shadow_map) and from
// crates/bevy_pbr/src/render/utils.wesl the SPIRAL_OFFSET constants, MIT OR
// Apache-2.0 (src/LICENSE-bevy.txt).
// Changes: the map and sampler are parameters, so another shadow kind's map
// can share the filters; the filter is chosen by a parameter, from what
// receives the shadow, instead of a shader definition; no PCSS, WebGL 2 or array-less paths; and every tap is
// clamped to `bounds`, the map's rectangle in its texture (a light's faces in
// the local-light atlas), as Wicked Engine 4323a33's
// WickedEngine/shaders/shadowHF.hlsli sample_shadow clamps each sample to
// shadow_border_clamp's rectangle, MIT (src/LICENSE-wicked.txt).
//
// The fog's tap (sample_shadow_map_fog) ports Godot b130438's
// servers/rendering/renderer_rd/shaders/environment/volumetric_fog_process.glsl
// (MODE_DENSITY's directional shadow and INV_FOG_FADE), MIT
// (src/LICENSE-godot.txt). Changes: WebGPU filters a depth texture only by
// comparison, so the tap loads the four texels Godot's linear sampler
// filters and filters them itself.

const SPIRAL_OFFSET_0_=vec2<f32>(-0.7071,0.7071);
const SPIRAL_OFFSET_1_=vec2<f32>(-0.0000,-0.8750);
const SPIRAL_OFFSET_2_=vec2<f32>(0.5303,0.5303);
const SPIRAL_OFFSET_3_=vec2<f32>(-0.6250,-0.0000);
const SPIRAL_OFFSET_4_=vec2<f32>(0.3536,-0.3536);
const SPIRAL_OFFSET_5_=vec2<f32>(-0.0000,0.3750);
const SPIRAL_OFFSET_6_=vec2<f32>(-0.1768,-0.1768);
const SPIRAL_OFFSET_7_=vec2<f32>(0.1250,0.0000);

// Bevy's ShadowFilteringMethod: Hardware2x2, Gaussian and Temporal.
const SHADOW_FILTER_HARDWARE_2X2:u32=0u;
const SHADOW_FILTER_GAUSSIAN:u32=1u;
const SHADOW_FILTER_TEMPORAL:u32=2u;
// What receives a shadow: a probe capture's or ray hit's surface, which
// takes the static layers and the fixed kernel; the camera's surface, which
// takes the frame's maps with the filter its shadow quality chooses (one
// hardware tap, as Godot's hard filter; else the temporal kernel while
// temporal antialiasing resolves it, or the fixed one); and a point of the
// camera's fog, which has no side and takes the frame's maps with one tap
// that the fog's reprojection resolves: a local light's one hardware tap,
// as Bevy's volumetric fog samples it, and the directional cascade's
// Godot's fog tap (directional_shadow.wgsl).
const SHADOW_RECEIVER_CAPTURE:u32=0u;
const SHADOW_RECEIVER_CAMERA:u32=1u;
const SHADOW_RECEIVER_MEDIUM:u32=2u;
// The filter a receiver's shadows take.
fn shadow_filter(receiver:u32)->u32 {
 if receiver==SHADOW_RECEIVER_MEDIUM {
  return SHADOW_FILTER_HARDWARE_2X2;
 }
 if receiver==SHADOW_RECEIVER_CAMERA && (frame.flags&FRAME_HARDWARE_SHADOW_FILTER)!=0u {
  return SHADOW_FILTER_HARDWARE_2X2;
 }
 if receiver==SHADOW_RECEIVER_CAMERA && (frame.flags&FRAME_TEMPORAL_SHADOW_FILTER)!=0u {
  return SHADOW_FILTER_TEMPORAL;
 }
 return SHADOW_FILTER_GAUSSIAN;
}

// Do the lookup, using HW 2x2 PCF and comparison
fn sample_shadow_map_hardware(shadow_map:texture_depth_2d_array,comparison:sampler_comparison,light_local:vec2<f32>,depth:f32,array_index:i32,bounds:vec4<f32>)->f32 {
 let sample_uv=clamp(light_local,bounds.xy,bounds.zw);
 return textureSampleCompareLevel(shadow_map,comparison,sample_uv,array_index,depth);
}

// https://web.archive.org/web/20230210095515/http://the-witness.net/news/2013/09/shadow-mapping-summary-part-1
fn sample_shadow_map_castano_thirteen(shadow_map:texture_depth_2d_array,comparison:sampler_comparison,light_local:vec2<f32>,depth:f32,array_index:i32,bounds:vec4<f32>)->f32 {
 let shadow_map_size=vec2<f32>(textureDimensions(shadow_map));
 let inv_shadow_map_size=1.0/shadow_map_size;

 let uv=light_local*shadow_map_size;
 var base_uv=floor(uv+0.5);
 let s=(uv.x+0.5-base_uv.x);
 let t=(uv.y+0.5-base_uv.y);
 base_uv-=0.5;
 base_uv*=inv_shadow_map_size;

 let uw0=(4.0-3.0*s);
 let uw1=7.0;
 let uw2=(1.0+3.0*s);

 let u0=(3.0-2.0*s)/uw0-2.0;
 let u1=(3.0+s)/uw1;
 let u2=s/uw2+2.0;

 let vw0=(4.0-3.0*t);
 let vw1=7.0;
 let vw2=(1.0+3.0*t);

 let v0=(3.0-2.0*t)/vw0-2.0;
 let v1=(3.0+t)/vw1;
 let v2=t/vw2+2.0;

 var sum=0.0;

 sum+=uw0*vw0*sample_shadow_map_hardware(shadow_map,comparison,base_uv+(vec2(u0,v0)*inv_shadow_map_size),depth,array_index,bounds);
 sum+=uw1*vw0*sample_shadow_map_hardware(shadow_map,comparison,base_uv+(vec2(u1,v0)*inv_shadow_map_size),depth,array_index,bounds);
 sum+=uw2*vw0*sample_shadow_map_hardware(shadow_map,comparison,base_uv+(vec2(u2,v0)*inv_shadow_map_size),depth,array_index,bounds);

 sum+=uw0*vw1*sample_shadow_map_hardware(shadow_map,comparison,base_uv+(vec2(u0,v1)*inv_shadow_map_size),depth,array_index,bounds);
 sum+=uw1*vw1*sample_shadow_map_hardware(shadow_map,comparison,base_uv+(vec2(u1,v1)*inv_shadow_map_size),depth,array_index,bounds);
 sum+=uw2*vw1*sample_shadow_map_hardware(shadow_map,comparison,base_uv+(vec2(u2,v1)*inv_shadow_map_size),depth,array_index,bounds);

 sum+=uw0*vw2*sample_shadow_map_hardware(shadow_map,comparison,base_uv+(vec2(u0,v2)*inv_shadow_map_size),depth,array_index,bounds);
 sum+=uw1*vw2*sample_shadow_map_hardware(shadow_map,comparison,base_uv+(vec2(u1,v2)*inv_shadow_map_size),depth,array_index,bounds);
 sum+=uw2*vw2*sample_shadow_map_hardware(shadow_map,comparison,base_uv+(vec2(u2,v2)*inv_shadow_map_size),depth,array_index,bounds);

 return sum*(1.0/144.0);
}

fn map(min1:f32,max1:f32,min2:f32,max2:f32,value:f32)->f32 {
 return min2+(value-min1)*(max2-min2)/(max1-min1);
}

// Creates a random rotation matrix using interleaved gradient noise.
//
// See: https://www.iryoku.com/next-generation-post-processing-in-call-of-duty-advanced-warfare/
fn random_rotation_matrix(scale:vec2<f32>,temporal:bool)->mat2x2<f32> {
 let random_angle=2.0*3.141592653589793*interleaved_gradient_noise(scale,select(1u,frame.frame_count,temporal));
 let m=vec2(sin(random_angle),cos(random_angle));
 return mat2x2(
  m.y,-m.x,
  m.x,m.y
 );
}

// Calculates the distance between spiral samples for the given texel size and
// penumbra size. This is used for the Jimenez '14 (i.e. temporal) variant of
// shadow sampling.
fn calculate_uv_offset_scale_jimenez_fourteen(shadow_map:texture_depth_2d_array,texel_size:f32,blur_size:f32)->vec2<f32> {
 let shadow_map_size=vec2<f32>(textureDimensions(shadow_map));

 // Empirically chosen fudge factor to make PCF look better across different CSM cascades
 let f=map(0.00390625,0.022949219,0.015,0.035,texel_size);
 return f*blur_size/(texel_size*shadow_map_size);
}

fn sample_shadow_map_jimenez_fourteen(shadow_map:texture_depth_2d_array,comparison:sampler_comparison,light_local:vec2<f32>,depth:f32,array_index:i32,bounds:vec4<f32>,frag_coord_xy:vec2<f32>,texel_size:f32,blur_size:f32,temporal:bool)->f32 {
 let rotation_matrix=random_rotation_matrix(frag_coord_xy,temporal);
 let uv_offset_scale=calculate_uv_offset_scale_jimenez_fourteen(shadow_map,texel_size,blur_size);

 // https://www.iryoku.com/next-generation-post-processing-in-call-of-duty-advanced-warfare (slides 120-135)
 let sample_offset0=(rotation_matrix*SPIRAL_OFFSET_0_)*uv_offset_scale;
 let sample_offset1=(rotation_matrix*SPIRAL_OFFSET_1_)*uv_offset_scale;
 let sample_offset2=(rotation_matrix*SPIRAL_OFFSET_2_)*uv_offset_scale;
 let sample_offset3=(rotation_matrix*SPIRAL_OFFSET_3_)*uv_offset_scale;
 let sample_offset4=(rotation_matrix*SPIRAL_OFFSET_4_)*uv_offset_scale;
 let sample_offset5=(rotation_matrix*SPIRAL_OFFSET_5_)*uv_offset_scale;
 let sample_offset6=(rotation_matrix*SPIRAL_OFFSET_6_)*uv_offset_scale;
 let sample_offset7=(rotation_matrix*SPIRAL_OFFSET_7_)*uv_offset_scale;

 var sum=0.0;
 sum+=sample_shadow_map_hardware(shadow_map,comparison,light_local+sample_offset0,depth,array_index,bounds);
 sum+=sample_shadow_map_hardware(shadow_map,comparison,light_local+sample_offset1,depth,array_index,bounds);
 sum+=sample_shadow_map_hardware(shadow_map,comparison,light_local+sample_offset2,depth,array_index,bounds);
 sum+=sample_shadow_map_hardware(shadow_map,comparison,light_local+sample_offset3,depth,array_index,bounds);
 sum+=sample_shadow_map_hardware(shadow_map,comparison,light_local+sample_offset4,depth,array_index,bounds);
 sum+=sample_shadow_map_hardware(shadow_map,comparison,light_local+sample_offset5,depth,array_index,bounds);
 sum+=sample_shadow_map_hardware(shadow_map,comparison,light_local+sample_offset6,depth,array_index,bounds);
 sum+=sample_shadow_map_hardware(shadow_map,comparison,light_local+sample_offset7,depth,array_index,bounds);
 return sum/8.0;
}

// Bevy's sample_shadow_map for `shadow_filtering`: one hardware 2x2 tap; the fixed
// Castano '13 kernel; or the Jimenez '14 spiral turned per pixel and per
// frame for TAA or FSR2 to resolve.
fn sample_shadow_map(shadow_map:texture_depth_2d_array,comparison:sampler_comparison,light_local:vec2<f32>,depth:f32,array_index:i32,bounds:vec4<f32>,frag_coord_xy:vec2<f32>,texel_size:f32,shadow_filtering:u32)->f32 {
 if shadow_filtering==SHADOW_FILTER_HARDWARE_2X2 {
  return sample_shadow_map_hardware(shadow_map,comparison,light_local,depth,array_index,bounds);
 }
 if shadow_filtering==SHADOW_FILTER_TEMPORAL {
  return sample_shadow_map_jimenez_fourteen(shadow_map,comparison,light_local,depth,array_index,bounds,frag_coord_xy,texel_size,1.0,true);
 }
 return sample_shadow_map_castano_thirteen(shadow_map,comparison,light_local,depth,array_index,bounds);
}

// Godot's INV_FOG_FADE: how fast the light a point in the fog receives
// fades, per metre it lies behind its occluder.
const INV_FOG_FADE:f32=10.;
// Godot's fog tap: the occluder's depth at `light_local`, linearly filtered
// with the map's edge clamped, and the light faded exponentially with the
// metres the receiver at `depth` lies behind it, `z_range` metres per
// unit of depth.
fn sample_shadow_map_fog(shadow_map:texture_depth_2d_array,light_local:vec2<f32>,depth:f32,array_index:i32,bounds:vec4<f32>,z_range:f32)->f32 {
 let size=vec2<i32>(textureDimensions(shadow_map));
 let texel=clamp(light_local,bounds.xy,bounds.zw)*vec2<f32>(size)-.5;
 let weight=fract(texel);
 let corner=vec2<i32>(floor(texel));
 let columns=clamp(vec2(corner.x,corner.x+1),vec2(0),vec2(size.x-1));
 let rows=clamp(vec2(corner.y,corner.y+1),vec2(0),vec2(size.y-1));
 let first_row=mix(textureLoad(shadow_map,vec2(columns.x,rows.x),array_index,0),textureLoad(shadow_map,vec2(columns.y,rows.x),array_index,0),weight.x);
 let second_row=mix(textureLoad(shadow_map,vec2(columns.x,rows.y),array_index,0),textureLoad(shadow_map,vec2(columns.y,rows.y),array_index,0),weight.x);
 let occluder=mix(first_row,second_row,weight.y);
 // Reversed Z: a receiver behind its occluder is deeper, nearer 0.
 return exp(min(0.,depth-occluder)*z_range*INV_FOG_FADE);
}

// Bevy's receiver offset (shadows.wesl sample_directional_cascade): along
// the surface normal by `normal_bias` shadow-map texels of `texel_size`
// metres, and toward the light by `depth_bias` metres, so a surface does not
// shadow itself and a caster's shadow stays attached to it.
fn shadow_receiver_offset(position:vec3<f32>,surface_normal:vec3<f32>,toward_light:vec3<f32>,texel_size:f32,normal_bias:f32,depth_bias:f32)->vec3<f32> {
 let normal_offset=normal_bias*texel_size*surface_normal;
 let depth_offset=depth_bias*toward_light;
 return position+normal_offset+depth_offset;
}
