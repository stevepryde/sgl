// The dynamic GI volume's probes (the architecture's Dynamic diffuse GI):
// where each probe of a lattice of `probes` lies, and where it is in the one
// RGBA16F probe texture that holds them all, whose size shading::dynamic_gi
// owns. The texture has three regions, top to bottom:
//
// depth: each probe's bordered 18x18 octahedral map of the mean and the mean
//  square of the distances its rays travel, in rg, at column (x + y nx) 18
//  and row z 18, as Wicked's depth texture lays them out;
// irradiance: each probe's bordered 8x8 octahedral map of irradiance / PI,
//  in rgb, two slabs of the lattice's z to each band of eight rows: slab z's
//  tile at column ((z % 2) nx ny + x + y nx) 8 and row 18 nz + (z / 2) 8;
// data: one texel per probe, eighteen slabs to a row: its relocated offset
//  in half spacings in rgb, and in a 1 once it has been blended while it is
//  active (stages/dynamic_gi/common.wgsl's ddgi_probe_active), at column
//  (z % 18) nx ny + x + y nx and row 18 nz + 8 ceil(nz / 2) + z / 18.
//
// A probe's place in each region, and its index in the stage's buffers, are
// those of its stored coordinate: its lattice coordinate plus the volume's
// scroll, wrapping, so a scroll moves no probe (ddgi_probe_stored).
//
// Ports Wicked Engine df44c3db4c4927492bc9c791eac715d98d7ed091,
// WickedEngine/shaders/ShaderInterop_DDGI.h (DDGI_DEPTH_RESOLUTION,
// DDGI_DEPTH_TEXELS, ddgi_probe_coord, ddgi_probe_index,
// ddgi_probe_position_rest, ddgi_probe_position, ddgi_probe_depth_pixel,
// ddgi_probe_depth_uv) and globals.hlsli (signNotZero, encode_oct,
// decode_oct, flatten3D, unflatten3D), with the colour map of revision
// 95e357f73f28d24e70ce7a1ab8f9fb954de457e4 (DDGI_COLOR_RESOLUTION,
// DDGI_COLOR_TEXELS, ddgi_probe_color_pixel, ddgi_probe_color_uv), MIT
// (src/LICENSE-wicked.txt). Changed: one texture holds the colour and depth
// maps and the probe data Wicked keeps in its probe buffer, its colour
// slabs paired to fill the depth region's width; the lattice is an
// argument; f32 in place of half. Added: probes stored by their lattice
// coordinate plus the volume's scroll, wrapping, as NVIDIA RTXGI's infinite
// scrolling volume stores its probes (DDGIGetScrollingProbeIndex and its
// probe scroll offsets; practice only, its code not copied).
const DDGI_COLOR_RESOLUTION:u32=6u;
const DDGI_COLOR_TEXELS:u32=8u;
const DDGI_DEPTH_RESOLUTION:u32=16u;
const DDGI_DEPTH_TEXELS:u32=18u;
// The data region's slabs to a row: the depth region's width over the
// lattice's columns.
const DDGI_DATA_SLABS:u32=18u;
// Texels to a row of the dynamic GI stage's ray list and ray results.
const DDGI_RAY_ROW:u32=2048u;
// The most rays a probe traces a frame at any quality, which bounds every
// loop over a probe's rays whatever the volume says.
const DDGI_MOST_RAYS:u32=256u;
// The fixed rays each probe traces a frame beside its others, which classify
// it and are not blended.
const DDGI_FIXED_RAYS_PER_FRAME:u32=4u;
fn ddgi_sign_not_zero(v:vec2<f32>)->vec2<f32> {
 return select(vec2(-1.),vec2(1.),v>=vec2(0.));
}
// A unit direction's octahedral coordinates, in [-1, 1].
fn ddgi_encode_oct(v:vec3<f32>)->vec2<f32> {
 let p=v.xy*(1./(abs(v.x)+abs(v.y)+abs(v.z)));
 return select(p,(1.-abs(p.yx))*ddgi_sign_not_zero(p),v.z<=0.);
}
fn ddgi_decode_oct(e:vec2<f32>)->vec3<f32> {
 var v=vec3(e,1.-abs(e.x)-abs(e.y));
 if v.z<0. {
  v=vec3((1.-abs(v.yx))*ddgi_sign_not_zero(v.xy),v.z);
 }
 return normalize(v);
}
fn ddgi_probe_coord(index:u32,probes:vec3<u32>)->vec3<u32> {
 let slab=probes.x*probes.y;
 let z=index/slab;
 let rest=index-z*slab;
 return vec3(rest%probes.x,rest/probes.x,z);
}
// Where the probe at lattice coordinate `coord` is stored, with the volume
// scrolled by `scroll` (in [0, probes) on each axis).
fn ddgi_probe_stored(coord:vec3<u32>,probes:vec3<u32>,scroll:vec3<u32>)->vec3<u32> {
 return (coord+scroll)%probes;
}
// The lattice coordinate of the probe stored at `stored`.
fn ddgi_probe_lattice(stored:vec3<u32>,probes:vec3<u32>,scroll:vec3<u32>)->vec3<u32> {
 return (stored+probes-scroll)%probes;
}
fn ddgi_probe_index(coord:vec3<u32>,probes:vec3<u32>)->u32 {
 return coord.z*probes.x*probes.y+coord.y*probes.x+coord.x;
}
fn ddgi_probe_position_rest(coord:vec3<u32>,origin:vec3<f32>,spacing:vec3<f32>)->vec3<f32> {
 return origin+vec3<f32>(coord)*spacing;
}
// A probe moved from rest by its relocated `offset`, in half spacings.
fn ddgi_probe_position(coord:vec3<u32>,origin:vec3<f32>,spacing:vec3<f32>,offset:vec3<f32>)->vec3<f32> {
 return ddgi_probe_position_rest(coord,origin,spacing)+offset*spacing*.5;
}
// A probe's column of tiles: its x and y, as Wicked's probeCoord.xz layout
// lays slabs of y side by side.
fn ddgi_probe_column(coord:vec3<u32>,probes:vec3<u32>)->u32 {
 return coord.x+coord.y*probes.x;
}
// The first interior texel of a probe's depth map, inside its border.
fn ddgi_probe_depth_pixel(coord:vec3<u32>,probes:vec3<u32>)->vec2<u32> {
 return vec2(ddgi_probe_column(coord,probes),coord.z)*DDGI_DEPTH_TEXELS+vec2(1u);
}
// The first interior texel of a probe's irradiance map, inside its border.
fn ddgi_probe_color_pixel(coord:vec3<u32>,probes:vec3<u32>)->vec2<u32> {
 let slab=(coord.z%2u)*probes.x*probes.y;
 let row=probes.z*DDGI_DEPTH_TEXELS+(coord.z/2u)*DDGI_COLOR_TEXELS;
 return vec2((slab+ddgi_probe_column(coord,probes))*DDGI_COLOR_TEXELS,row)+vec2(1u);
}
fn ddgi_probe_data_pixel(coord:vec3<u32>,probes:vec3<u32>)->vec2<u32> {
 let row=probes.z*DDGI_DEPTH_TEXELS+(probes.z+1u)/2u*DDGI_COLOR_TEXELS+coord.z/DDGI_DATA_SLABS;
 return vec2((coord.z%DDGI_DATA_SLABS)*probes.x*probes.y+ddgi_probe_column(coord,probes),row);
}
// Ray `ray` of probe `probe` among all the probes' rays, each probe's
// `max_rays` and then its fixed rays.
fn ddgi_ray_slot(probe:u32,ray:u32,max_rays:u32)->u32 {
 return probe*(max_rays+DDGI_FIXED_RAYS_PER_FRAME)+ray;
}
// The texel of the ray list and ray results that holds ray `ray` of all
// the probes' rays.
fn ddgi_ray_texel(ray:u32)->vec2<u32> {
 return vec2(ray%DDGI_RAY_ROW,ray/DDGI_RAY_ROW);
}
// Where `direction` lies in a probe's map of `resolution` interior texels
// from `pixel`, in UV of a texture of `size` texels: Wicked's
// ddgi_probe_color_uv and ddgi_probe_depth_uv.
fn ddgi_probe_uv(pixel:vec2<u32>,resolution:u32,direction:vec3<f32>,size:vec2<f32>)->vec2<f32> {
 let at=vec2<f32>(pixel)+(ddgi_encode_oct(normalize(direction))*.5+.5)*f32(resolution);
 return at/size;
}
