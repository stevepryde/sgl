// The volumetric fog's froxel volume (stages/fog.wgsl writes it): how its
// depth slices are spaced, where a point at a frame position and view depth
// samples it, and how its in-scattered light and transmittance fog what lies
// behind. Godot b130438's get_depth_at_pos
// (servers/rendering/renderer_rd/shaders/environment/volumetric_fog_process.glsl)
// and volumetric_fog_process and its fog composition
// (shaders/forward_clustered/scene_forward_clustered.glsl), MIT
// (src/LICENSE-godot.txt).

// The frame position of a froxel column's unit x and y, with y down the
// frame, and the view-space point `depth` metres deep along that column's
// ray through `projection`, the unjittered projection's x and y scales and
// x and y offsets as shading::fog::froxel_projection packs them. The one
// column ray: the fog's froxels and the sky's aerial volume
// (stages/sky/aerial_view_lut.wgsl) both take it, so the integration reads
// each column's own air. Their Rust inverse is in
// stages/fog/volume_froxels.rs, which bounds the fog volumes' froxels.
fn fog_froxel_ndc(unit:vec2<f32>)->vec2<f32> {
 return vec2(unit.x*2.-1.,1.-unit.y*2.);
}
fn fog_froxel_view_position(projection:vec4<f32>,ndc:vec2<f32>,depth:f32)->vec3<f32> {
 return vec3((ndc+projection.zw)*depth/projection.xy,-depth);
}
// The view depth at `unit` (0 to 1 across the volume's slices) of a volume
// reaching `length` metres with `detail_spread`. Its Rust inverse bounds the
// fog volumes' froxels (stages/fog/volume_froxels.rs).
fn fog_slice_depth(unit:f32,length:f32,detail_spread:f32)->f32 {
 return length*pow(unit,detail_spread);
}
// The frame's one fog volume (stages/fog.wgsl integrate writes it): its
// slices, in order, the fog's own (fog_slices, none while the fog does not
// run), at fog_slice_depth over its length; under a procedural sky's aerial
// perspective, aerial_slices more from the fog's length (0 without the fog)
// to aerial_reach of view depth, spaced by the square, each the whole fog
// over the air to its depth; and with them one last slice, the fog alone
// over its whole length, which only the sky takes. Rust mirror:
// shading::fog::FogLayout. A uniform holds it last, so nothing follows it
// within the 16 bytes WGSL rounds a uniform's struct member up to.
struct FogLayout {
 // One over the fog's length (0 while the fog does not run) and over its
 // detail spread.
 inverse_length:f32,
 inverse_detail_spread:f32,
 // How far the aerial slices reach, metres of view depth.
 aerial_reach:f32,
 fog_slices:u32,
 aerial_slices:u32,
}
// The volume's slices.
fn fog_volume_slices(volume_layout:FogLayout)->u32 {
 return volume_layout.fog_slices+select(0u,volume_layout.aerial_slices+1u,volume_layout.aerial_slices>0u);
}
// The fog's length, metres: 0 while it does not run.
fn fog_layout_length(volume_layout:FogLayout)->f32 {
 return select(0.,1./volume_layout.inverse_length,volume_layout.fog_slices>0u);
}
// Where among the volume's slices a point `view_depth` metres deep lies, in
// slices (slice i's centre at i): the fog's slices by fog_slice_depth over
// its length, then the aerial slices, slice k at
// fog length + (reach - fog length) × ((k + 0.5) / aerial_slices)², which
// meet the fog's half a slice past its last; past the last aerial slice it
// stays there. The one mapping from a depth to a slice, which the fog's
// integration inverts (fog_layout_aerial_depth).
fn fog_volume_slice(view_depth:f32,volume_layout:FogLayout)->f32 {
 let fog=f32(volume_layout.fog_slices);
 let length=fog_layout_length(volume_layout);
 if view_depth<=length {
  return fog*pow(clamp(view_depth*volume_layout.inverse_length,0.,1.),volume_layout.inverse_detail_spread)-.5;
 }
 let aerial=f32(volume_layout.aerial_slices);
 let beyond=clamp((view_depth-length)/max(volume_layout.aerial_reach-length,1.),0.,1.);
 return min(fog-.5+aerial*sqrt(beyond),fog+aerial-1.);
}
// The view depth aerial slice `slice` of the volume holds (the inverse of
// fog_volume_slice past the fog's length).
fn fog_layout_aerial_depth(slice:u32,volume_layout:FogLayout)->f32 {
 let length=fog_layout_length(volume_layout);
 let unit=(f32(slice)+.5)/f32(volume_layout.aerial_slices);
 return length+(volume_layout.aerial_reach-length)*unit*unit;
}
// The froxel volume's coordinate of a point at `uv` on the frame and
// `view_depth` metres deep, a volume reaching one over `inverse_length` metres
// with one over `inverse_detail_spread`'s spread: beyond it, it is the last
// slice. The fog's injection reprojects its froxels through it. Its slice's
// Rust mirror bounds the fog volumes' froxels (stages/fog/volume_froxels.rs).
fn fog_froxel_coordinate(uv:vec2<f32>,view_depth:f32,inverse_length:f32,inverse_detail_spread:f32)->vec3<f32> {
 let unit=clamp(view_depth*inverse_length,0.,1.);
 return vec3(uv,pow(unit,inverse_detail_spread));
}
// The frame's fog volume's coordinate of a point at `uv` on the frame and
// `view_depth` metres deep. Without aerial slices it is the froxel volume's
// (beyond the fog's length, the sky included, the last slice), as it always
// was; with them, fog_volume_slice's.
fn fog_volume_coordinate(uv:vec2<f32>,view_depth:f32,volume_layout:FogLayout)->vec3<f32> {
 if volume_layout.aerial_slices==0u {
  return fog_froxel_coordinate(uv,view_depth,volume_layout.inverse_length,volume_layout.inverse_detail_spread);
 }
 return vec3(uv,(fog_volume_slice(view_depth,volume_layout)+.5)/f32(fog_volume_slices(volume_layout)));
}
// The volume coordinate the sky takes: the last slice, which holds the fog
// alone (the sky-view table already holds the air), or, without aerial
// slices, the fog's last.
fn fog_sky_coordinate(uv:vec2<f32>,volume_layout:FogLayout)->vec3<f32> {
 if volume_layout.aerial_slices==0u {
  return vec3(uv,1.);
 }
 let slices=f32(fog_volume_slices(volume_layout));
 return vec3(uv,(slices-.5)/slices);
}
// Where a planar reflector's reflected view's medium begins (the
// architecture's Frame, "Planar reflections"): at the reflector's `plane`
// (unit normal, d, positive on the reflecting side), which a reflected
// view's fog takes and every other view's holds zero. Its light passes
// through the medium between the plane and what it reflects; the viewer's
// own fog takes the stretch from its eye to the mirror. The one owner of
// that rule, which the fog's injection (whether a froxel holds medium, and
// whether it is wholly out of it) and its integration (where a column's air
// begins) take.
// Whether `plane` bounds the medium: a reflected view's, not zero.
fn fog_has_plane(plane:vec4<f32>)->bool {
 return any(plane!=vec4(0.));
}
// Whether the point `position` lies in the medium: on the plane's
// reflecting side, or anywhere without a plane.
fn fog_in_medium(position:vec3<f32>,plane:vec4<f32>)->bool {
 return !fog_has_plane(plane) || dot(plane.xyz,position)+plane.w>=0.;
}
// Whether a froxel lies wholly out of the medium, behind `view_plane`, the
// plane in the view's space: the froxel spans view depths `near` to `far`
// and, across the frame, the column positions `low` to `high` (frame
// position plus the projection's offset, at whose depth d the view point is
// (position × d / `scale`, -d), fog_froxel_view_position). The plane's
// signed distance is linear across the froxel's box, so it is greatest at a
// corner: at depth d, d (across · position - view_plane.z) + view_plane.w,
// where across is view_plane.xy / scale and position the column corner its
// signs pick, and so at near or at far. Out only where that greatest
// distance is negative: a froxel that touches the plane is in.
fn fog_froxel_behind(view_plane:vec4<f32>,scale:vec2<f32>,low:vec2<f32>,high:vec2<f32>,near:f32,far:f32)->bool {
 let across=view_plane.xy/scale;
 let corner=select(min(low,high),max(low,high),across>vec2(0.));
 let per_depth=dot(across,corner)-view_plane.z;
 return max(near*per_depth,far*per_depth)+view_plane.w<0.;
}
// How far along the ray from `origin` in the unit `direction` the medium
// begins: 0 without a plane or from an origin on its reflecting side, where
// the ray crosses the plane from behind it, and FOG_NEVER for a ray that
// never reaches it.
fn fog_medium_begins(origin:vec3<f32>,direction:vec3<f32>,plane:vec4<f32>)->f32 {
 let side=dot(plane.xyz,origin)+plane.w;
 if !fog_has_plane(plane) || side>=0. {
  return 0.;
 }
 let toward=dot(plane.xyz,direction);
 if toward<=0. {
  return FOG_NEVER;
 }
 return -side/toward;
}
// A distance no ray reaches.
const FOG_NEVER:f32=1e30;
// `color` seen through `fog`: the light the fog scatters toward the camera
// (rgb) and its transmittance (a).
fn fog_composite(color:vec3<f32>,fog:vec4<f32>)->vec3<f32> {
 return fog_composite_premultiplied(color,1.,fog);
}
// fog_composite of `color` premultiplied by a coverage of `alpha`, as a
// blended surface draws it over what lies behind (premultiplied blending):
// the light the fog scatters over the covered share alone, what lies behind
// it already holding its own, as Filament ef1a133 fogs a transparent surface
// (shaders/src/surface_main.fs 84, fogColor.rgb *= fragColor.a; Apache-2.0,
// src/LICENSE-filament.txt). Filament's fade mode adds its in-scatter whole
// (83-84); a coverage-blended surface takes it over its covered share too,
// which is Porter and Duff's over of the fogged surface, as SGL3D's straight
// alpha blending composed it before.
fn fog_composite_premultiplied(color:vec3<f32>,alpha:f32,fog:vec4<f32>)->vec3<f32> {
 return color*fog.a+fog.rgb*alpha;
}
// A fog volume (content::transient::FogVolume; Rust mirror
// shading::fog::FogVolumeRecord): a box's frame from the world, its half
// size, the density and albedo it adds and its edge fade, and the sphere
// about it.
struct FogVolumeRecord {
 local_from_world:mat4x4<f32>,
 half_size:vec3<f32>,
 density:f32,
 albedo:vec3<f32>,
 edge_fade:f32,
 center:vec3<f32>,
 radius_squared:f32,
}
