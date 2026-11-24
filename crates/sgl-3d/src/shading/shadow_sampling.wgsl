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
// receives the shadow, instead of a shader definition; no WebGL 2 or
// array-less paths; and every tap is
// clamped to `bounds`, the map's rectangle in its texture (a light's faces in
// the local-light atlas), as Wicked Engine 4323a33's
// WickedEngine/shaders/shadowHF.hlsli sample_shadow clamps each sample to
// shadow_border_clamp's rectangle, MIT (src/LICENSE-wicked.txt).
//
// The soft filter (SHADOW_FILTER_SOFT, contact-hardening) follows Bevy's
// PCSS (shadow_sampling.wesl sample_shadow_map_pcss and
// search_for_blockers_in_shadow_map): a blocker search, then the Jimenez
// spiral at the penumbra's size, never below the fixed filter's and with no
// early return; and Godot b130438's soft shadows
// (servers/rendering/renderer_rd/shaders/scene_forward_lights_inc.glsl,
// sample_directional_soft_shadow and the omni and spot soft shadows, MIT,
// src/LICENSE-godot.txt): one turned kernel for the search and the filter,
// of eight taps, Godot's `Soft Low` penumbra_shadow_samples
// (renderer_scene_render_rd.cpp). Each shadow kind estimates its own
// penumbra (directional_shadow.wgsl, local_shadow.wgsl). Changes: the
// kernel is this file's spiral and interleaved-gradient rotation, not
// Godot's Vogel disc and hash, so the file keeps one temporal kernel; the
// search loads the nearest texels, as WebGPU filters a depth texture only by
// comparison, where Godot samples linearly; and a search that finds no
// blocker takes the fixed filter, as Bevy's does, not Godot's lit result:
// eight sparse taps over a search region wider than a thin caster would
// miss that caster and drop its contact shadow.
//
// The soft filter follows the receiver's plane (#841): each tap of its
// search and of its filter is compared at the depth the plane has where
// the tap lands, not at the receiver's one depth, so a surface tilted to a
// large light neither counts its own uphill texels as blockers nor puts
// its uphill taps beneath itself. It is the receiver plane depth bias of
// Isidoro, "Shadow Mapping: GPU-based Tips and Techniques" (GDC 2006), as
// Filament 935afa5 applied it in PCSS (shaders/src/surface_shadowing.fs,
// computeReceiverPlaneDepthBias and the z_bias of blockerSearchAndFilter
// and filterPCSS, Apache-2.0, src/LICENSE-filament.txt; the last revision
// with it, before EVSSM replaced that PCSS). Changes: the plane is the one
// through the offset receiver with its geometry normal, where Filament
// takes the shadow position's screen derivatives: the lookups take a
// position and a normal, and run in light loops and compute programs,
// where WGSL has no derivatives, and a cascade's or face's edge puts no
// discontinuity in a normal; the solve is Filament's inverse transpose,
// written as the cross product of the map-space derivatives along two
// directions across the plane, as DiligentFX f26cfe5 writes it
// (Shaders/Common/public/Shadows.fxh ComputeReceiverPlaneDepthBias,
// Apache-2.0, src/LICENSE-diligent.txt); the plane is followed no steeper
// than Diligent's clamp allows (receiver_plane_normal, #905); a tap
// clamped to its map's rectangle is compared at the plane's depth where
// it was read, where Filament biases by the unclamped offset; and the
// depth compared is kept in the map's range, beyond whose far plane an
// empty texel would count as a blocker. Each shadow kind gives its map's
// slope, and a cube follows the plane along each tap's ray
// (directional_shadow.wgsl, local_shadow.wgsl). The fixed filters compare
// at the receiver's one depth, as Bevy's do. Limit: the plane is the
// receiver's at the pixel, from its interpolated normal (not the triangle
// drawn into the map), so a receiver that curves, ends or meets another
// surface within the filter is followed only as far as it is flat: a
// smooth-shaded low-poly mesh keeps a little self-shadow on one side and
// misses an occluder very near it on the other, and a receiver can still
// find itself a blocker uphill, or what lies behind its far edge downhill,
// as the ground does a sunlit wall at the wall's foot under a widened
// penumbra.
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
// The soft filter's taps, for its blocker search and again for its filter:
// the spiral's eight offsets.
const SOFT_SHADOW_TAPS:u32=8u;
const SPIRAL_OFFSETS=array<vec2<f32>,SOFT_SHADOW_TAPS>(SPIRAL_OFFSET_0_,SPIRAL_OFFSET_1_,SPIRAL_OFFSET_2_,SPIRAL_OFFSET_3_,SPIRAL_OFFSET_4_,SPIRAL_OFFSET_5_,SPIRAL_OFFSET_6_,SPIRAL_OFFSET_7_);
// Spiral offset `tap`, below SOFT_SHADOW_TAPS.
fn spiral_offset(tap:u32)->vec2<f32> {
 return SPIRAL_OFFSETS[min(tap,SOFT_SHADOW_TAPS-1u)];
}

// Bevy's ShadowFilteringMethod: Hardware2x2, Gaussian and Temporal.
const SHADOW_FILTER_HARDWARE_2X2:u32=0u;
const SHADOW_FILTER_GAUSSIAN:u32=1u;
const SHADOW_FILTER_TEMPORAL:u32=2u;
// The temporal filter widened to each light's penumbra (contact-hardening),
// where Settings::soft_shadows is in effect. A light of no size takes the
// temporal filter.
const SHADOW_FILTER_SOFT:u32=3u;
// What receives a shadow: a probe capture's or ray hit's surface, which
// takes the static layers and the fixed kernel; the camera's surface, which
// takes the frame's maps with the filter its shadow quality chooses (one
// hardware tap, as Godot's hard filter; else the temporal kernel while
// temporal antialiasing resolves it, widened to each light's penumbra
// where Settings::soft_shadows is on, or the fixed one); and a point of the
// camera's fog, which has no side and takes the frame's maps with one tap
// that the fog's reprojection resolves: a local light's one hardware tap,
// as Bevy's volumetric fog samples it, and the directional cascade's
// Godot's fog tap (directional_shadow.wgsl). A dynamic GI probe ray's hit
// is a fourth kind, which takes no map: the visibility of its one light is
// a ray (surface_ray.wgsl's probe_hit_light).
const SHADOW_RECEIVER_CAPTURE:u32=0u;
const SHADOW_RECEIVER_CAMERA:u32=1u;
const SHADOW_RECEIVER_MEDIUM:u32=2u;
const SHADOW_RECEIVER_PROBE_HIT:u32=3u;
// A light's shadow at its shadow opacity, Godot b130438's shadow_opacity,
// MIT (src/LICENSE-godot.txt): its visibility blended toward unshadowed,
// mix(1, shadow, opacity), as Godot's surfaces take it
// (servers/rendering/renderer_rd/shaders/scene_forward_lights_inc.glsl and
// forward_clustered/scene_forward_clustered.glsl) and its fog
// (environment/volumetric_fog_process.glsl's mix(1 - opacity, 1, shadow),
// the same blend). At or below SHADOW_OPACITY_CUTOFF Godot looks up no
// shadow: the light is unshadowed.
const SHADOW_OPACITY_CUTOFF:f32=.001;
fn shadow_opacity_visibility(visibility:f32,opacity:f32)->f32 {
 return mix(1.,visibility,opacity);
}
// The filter a receiver's shadows take.
fn shadow_filter(receiver:u32)->u32 {
 if receiver==SHADOW_RECEIVER_MEDIUM {
  return SHADOW_FILTER_HARDWARE_2X2;
 }
 if receiver==SHADOW_RECEIVER_CAMERA && (frame.flags&FRAME_HARDWARE_SHADOW_FILTER)!=0u {
  return SHADOW_FILTER_HARDWARE_2X2;
 }
 if receiver==SHADOW_RECEIVER_CAMERA && (frame.flags&FRAME_SOFT_SHADOW_FILTER)!=0u {
  return SHADOW_FILTER_SOFT;
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

// The spiral, turned per pixel (and per frame where `temporal`), its unit
// offsets `uv_offset_scale` long in UV, each tap compared at the depth the
// receiver's plane has there (`slope`, receiver_plane_depth; zero compares
// every tap at `depth`).
fn sample_shadow_map_jimenez_fourteen(shadow_map:texture_depth_2d_array,comparison:sampler_comparison,light_local:vec2<f32>,depth:f32,slope:vec2<f32>,array_index:i32,bounds:vec4<f32>,frag_coord_xy:vec2<f32>,uv_offset_scale:vec2<f32>,temporal:bool)->f32 {
 let rotation_matrix=random_rotation_matrix(frag_coord_xy,temporal);

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
 sum+=sample_shadow_map_plane(shadow_map,comparison,light_local,sample_offset0,depth,slope,array_index,bounds);
 sum+=sample_shadow_map_plane(shadow_map,comparison,light_local,sample_offset1,depth,slope,array_index,bounds);
 sum+=sample_shadow_map_plane(shadow_map,comparison,light_local,sample_offset2,depth,slope,array_index,bounds);
 sum+=sample_shadow_map_plane(shadow_map,comparison,light_local,sample_offset3,depth,slope,array_index,bounds);
 sum+=sample_shadow_map_plane(shadow_map,comparison,light_local,sample_offset4,depth,slope,array_index,bounds);
 sum+=sample_shadow_map_plane(shadow_map,comparison,light_local,sample_offset5,depth,slope,array_index,bounds);
 sum+=sample_shadow_map_plane(shadow_map,comparison,light_local,sample_offset6,depth,slope,array_index,bounds);
 sum+=sample_shadow_map_plane(shadow_map,comparison,light_local,sample_offset7,depth,slope,array_index,bounds);
 return sum/8.0;
}

// Bevy's sample_shadow_map for `shadow_filtering`: one hardware 2x2 tap; the fixed
// Castano '13 kernel; or the Jimenez '14 spiral turned per pixel and per
// frame for TAA or FSR to resolve, which the soft filter is without a
// penumbra.
fn sample_shadow_map(shadow_map:texture_depth_2d_array,comparison:sampler_comparison,light_local:vec2<f32>,depth:f32,array_index:i32,bounds:vec4<f32>,frag_coord_xy:vec2<f32>,texel_size:f32,shadow_filtering:u32)->f32 {
 if shadow_filtering==SHADOW_FILTER_HARDWARE_2X2 {
  return sample_shadow_map_hardware(shadow_map,comparison,light_local,depth,array_index,bounds);
 }
 if shadow_filtering==SHADOW_FILTER_TEMPORAL || shadow_filtering==SHADOW_FILTER_SOFT {
  return sample_shadow_map_jimenez_fourteen(shadow_map,comparison,light_local,depth,vec2(0.),array_index,bounds,frag_coord_xy,calculate_uv_offset_scale_jimenez_fourteen(shadow_map,texel_size,1.0),true);
 }
 return sample_shadow_map_castano_thirteen(shadow_map,comparison,light_local,depth,array_index,bounds);
}

// The soft filter's rule (Bevy's floored blur size): the temporal spiral
// at the larger of the fixed filter's radius and `penumbra`, the
// penumbra's radius in UV, along the receiver's plane (`slope`). A
// penumbra of 0, for no blocker, is the fixed temporal filter's radius.
fn sample_shadow_map_soft(shadow_map:texture_depth_2d_array,comparison:sampler_comparison,light_local:vec2<f32>,depth:f32,slope:vec2<f32>,array_index:i32,bounds:vec4<f32>,frag_coord_xy:vec2<f32>,texel_size:f32,penumbra:vec2<f32>)->f32 {
 let fixed=calculate_uv_offset_scale_jimenez_fourteen(shadow_map,texel_size,1.0);
 return sample_shadow_map_jimenez_fourteen(shadow_map,comparison,light_local,depth,slope,array_index,bounds,frag_coord_xy,soft_shadow_radius(fixed,penumbra),true);
}
// The soft filter's radius: the penumbra's, never below the fixed
// filter's `fixed`.
fn soft_shadow_radius(fixed:vec2<f32>,penumbra:vec2<f32>)->vec2<f32> {
 return max(fixed,penumbra);
}

// The steepest the soft filter follows a receiver's plane: DiligentFX
// f26cfe5's default fReceiverPlaneDepthBiasClamp (Shaders/Common/public/
// BasicStructures.fxh 53, applied in Shadows.fxh 190-193), metres toward
// the light per metre across the direction to it. An unbounded plane
// nearly edge on to its light compares the uphill taps metres nearer the
// light, past any occluder there, and counts them lit: a wall under a
// roof that the sun meets within a degree of edge on took 0.2 to 0.5 of
// the sun through the roof (#905). Bounded, a plane steeper than this, one
// the light grazes within 5.7 degrees of edge on, is followed at this
// steepness and counts some of its own uphill texels as blockers again.
// Open ground, which reads 1 unbounded, reads under a sun of the default
// 0.53 degree disc 0.91 at 2 degrees high, 0.98 at 3 and 1 from 4; under
// a 2 degree disc 0.64 to 0.71, 0.74 to 0.87 and 0.89 to 0.94 at 2, 3 and
// 4 degrees high; under 6 and 10 degree discs 0.22 to 0.86 from 2 to 5
// degrees high; and 1 from 6 degrees up. The fixed filters read 0.81 to
// 0.89 there up to 8 degrees. Limit: the bound still lets a tap r metres
// across reach 10 r nearer the light, so a receiver nearly edge on less
// than ten filter radii (never below the fixed filter's, which grows with
// the map's texels) beneath an occluder still sees past it: points 0.3 to
// 0.5 m beneath a roof that a light of radius 0.5 m, 2.5 m above the roof,
// meets edge on take 0.08 to 0.17 of it (0.34 to 0.49 unbounded), where
// lights of 0.1 and 0.2 m take under 0.01.
const RECEIVER_PLANE_STEEPEST:f32=10.;
// The normal of the plane the soft filter follows for a receiver with unit
// `normal`, lit from the unit direction `toward_light`: `normal` where its
// plane rises at most RECEIVER_PLANE_STEEPEST toward the light per metre
// across the direction to it, else turned toward the light, on its side
// of the edge-on plane, until it rises that much. Diligent clamps the
// slope along each of its map's axes; here the plane's steepness is bounded
// whatever its heading, as a cube's taps lie in no one map's axes. A zero
// normal (a point in the fog) stays zero.
fn receiver_plane_normal(normal:vec3<f32>,toward_light:vec3<f32>)->vec3<f32> {
 let along=dot(normal,toward_light);
 let across=normal-toward_light*along;
 let least=length(across)/RECEIVER_PLANE_STEEPEST;
 if abs(along)>=least {
  return normal;
 }
 return normalize(across+toward_light*(least*select(-1.,1.,along>=0.)));
}
// Two directions across the plane of unit `normal`, neither a unit vector
// nor zero: the normal crossed with the axis it lies least along, and with
// that.
fn receiver_plane_directions(normal:vec3<f32>)->mat2x3<f32> {
 let along=abs(normal);
 let axis=select(select(vec3(0.,0.,1.),vec3(0.,1.,0.),along.y<=along.z),vec3(1.,0.,0.),along.x<=along.y && along.x<=along.z);
 let first=cross(normal,axis);
 return mat2x3(first,cross(normal,first));
}
// The least of a plane's normal along a map's depth that
// receiver_plane_depth_slope divides by: far below any plane a light sees
// but edge on, it only keeps the quotient finite (a zero normal's slope is
// 0 / 0 without it). Callers bound the plane's steepness first
// (receiver_plane_normal); receiver_plane_depth keeps what the slope gives
// in the map's range.
const RECEIVER_PLANE_EDGE_ON:f32=1e-20;
// Filament's computeReceiverPlaneDepthBias (Isidoro's chain rule): the
// depth a plane gains per unit along each of a map's two axes, from its
// normal `plane` in the map's space (the two axes, then depth), which is
// the cross product of the map-space derivatives along two directions
// across it.
fn receiver_plane_depth_slope(plane:vec3<f32>)->vec2<f32> {
 return -plane.xy/(select(-1.,1.,plane.z>=0.)*max(abs(plane.z),RECEIVER_PLANE_EDGE_ON));
}
// The depth the plane of a receiver at `depth` with `slope` has `offset`
// from it in UV (Filament's z_bias), within the map's range: beyond its far
// plane an empty texel would be nearer the light than the plane.
fn receiver_plane_depth(depth:f32,slope:vec2<f32>,offset:vec2<f32>)->f32 {
 return saturate(depth+dot(slope,offset));
}
// One hardware tap `offset` from `light_local`, clamped to `bounds`,
// compared at the depth the receiver's plane has where it lands.
fn sample_shadow_map_plane(shadow_map:texture_depth_2d_array,comparison:sampler_comparison,light_local:vec2<f32>,offset:vec2<f32>,depth:f32,slope:vec2<f32>,array_index:i32,bounds:vec4<f32>)->f32 {
 let sample_uv=clamp(light_local+offset,bounds.xy,bounds.zw);
 return textureSampleCompareLevel(shadow_map,comparison,sample_uv,array_index,receiver_plane_depth(depth,slope,sample_uv-light_local));
}

// The soft filter's blocker test at one tap: the depth of the texel nearest
// `light_local`, clamped to `bounds`, as (depth, 1) where it blocks a
// receiver at `depth` (reversed Z: nearer the light, greater), else 0.
// WebGPU filters a depth texture only by comparison, so the texel is
// loaded.
fn shadow_blocker_texel(shadow_map:texture_depth_2d_array,light_local:vec2<f32>,depth:f32,array_index:i32,bounds:vec4<f32>)->vec2<f32> {
 let size=vec2<i32>(textureDimensions(shadow_map));
 let uv=clamp(light_local,bounds.xy,bounds.zw);
 let texel=clamp(vec2<i32>(floor(uv*vec2<f32>(size))),vec2(0),size-1);
 let blocker=textureLoad(shadow_map,texel,array_index,0);
 return select(vec2(0.),vec2(blocker,1.),blocker>depth);
}

// The blocker test at the tap `offset` from `light_local` on a 2D map,
// clamped to `bounds`, for the receiver's plane where it lands.
fn shadow_plane_blocker(shadow_map:texture_depth_2d_array,light_local:vec2<f32>,offset:vec2<f32>,depth:f32,slope:vec2<f32>,array_index:i32,bounds:vec4<f32>)->vec2<f32> {
 let uv=clamp(light_local+offset,bounds.xy,bounds.zw);
 return shadow_blocker_texel(shadow_map,uv,receiver_plane_depth(depth,slope,uv-light_local),array_index,bounds);
}

// The soft filter's blocker search on one 2D map: the spiral turned as the
// filter turns it, its unit offsets `search` long in UV. The mean depth of
// the texels that block the plane of a receiver at `depth` with `slope`,
// and how many do.
fn search_shadow_blockers(shadow_map:texture_depth_2d_array,light_local:vec2<f32>,depth:f32,slope:vec2<f32>,array_index:i32,bounds:vec4<f32>,frag_coord_xy:vec2<f32>,search:vec2<f32>)->vec2<f32> {
 let rotation=random_rotation_matrix(frag_coord_xy,true);
 // The SOFT_SHADOW_TAPS offsets, unrolled as the spiral filter's are.
 var sum=shadow_plane_blocker(shadow_map,light_local,(rotation*SPIRAL_OFFSET_0_)*search,depth,slope,array_index,bounds);
 sum+=shadow_plane_blocker(shadow_map,light_local,(rotation*SPIRAL_OFFSET_1_)*search,depth,slope,array_index,bounds);
 sum+=shadow_plane_blocker(shadow_map,light_local,(rotation*SPIRAL_OFFSET_2_)*search,depth,slope,array_index,bounds);
 sum+=shadow_plane_blocker(shadow_map,light_local,(rotation*SPIRAL_OFFSET_3_)*search,depth,slope,array_index,bounds);
 sum+=shadow_plane_blocker(shadow_map,light_local,(rotation*SPIRAL_OFFSET_4_)*search,depth,slope,array_index,bounds);
 sum+=shadow_plane_blocker(shadow_map,light_local,(rotation*SPIRAL_OFFSET_5_)*search,depth,slope,array_index,bounds);
 sum+=shadow_plane_blocker(shadow_map,light_local,(rotation*SPIRAL_OFFSET_6_)*search,depth,slope,array_index,bounds);
 sum+=shadow_plane_blocker(shadow_map,light_local,(rotation*SPIRAL_OFFSET_7_)*search,depth,slope,array_index,bounds);
 return vec2(sum.x/max(sum.y,1.),sum.y);
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
