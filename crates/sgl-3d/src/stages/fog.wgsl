// The volumetric fog: the camera's froxel volume, lit and integrated each
// frame. Ported from Godot b130438's
// servers/rendering/renderer_rd/shaders/environment/volumetric_fog_process.glsl
// (MODE_DENSITY, the medium and the light it scatters in each froxel blended
// with the last frame's, MODE_FILTER, a Gaussian across each slice's x and
// then y, and MODE_FOG, the integration front to back),
// environment/volumetric_fog.glsl (a box FogVolume with
// scene/resources/3d/fog_material.cpp's FogMaterial) and environment/fog.cpp
// (volumetric_fog_update), MIT (src/LICENSE-godot.txt), after Hillaire 2015,
// "Physically Based and Unified Volumetric Rendering in Frostbite".
//
// Where it differs from Godot, and why:
// - The frame's medium, Godot's environment fog, takes a FogMaterial's
//   height falloff (uniform by default), so height fog needs no volume.
// - A fog volume is a box with a FogMaterial's density, albedo and edge
//   fade; emission (the frame medium's too), height falloff, density
//   textures, negative density and other shapes are not ported. The box's
//   0.1 m cull fade applies under the edge fade too, where Godot drops it
//   for a material that reads SDF, so a volume without edge fade still ends
//   at its box, where Godot's pow(0, 0) is undefined.
// - Each froxel sums in f32, in the scene's order, the volumes whose froxel
//   bounds hold it (fog.cpp's bounds, stages/fog/volume_froxels.rs), where
//   Godot dispatches each volume over its bounds into fixed-point atomics:
//   no clear, extra pass or packing. Godot's packing drops a volume's
//   density at or below 0.001, truncates it to 1/1024, clamps its albedo
//   weight to a density of 1 and its scattering to 1, and truncates that
//   scattering to 11/11/10 bits (volumetric_fog.glsl's DENSITY_USED); here
//   every medium scatters albedo × density, as Godot's environment medium
//   does and Hillaire's albedo defines.
// - Albedos are linear RGB, where Godot's fog colours are sRGB it converts.
// - No GI injection: Godot injects VoxelGI and SDFGI, which SGL3D lacks.
// - The lights are SGL3D's, in its units and falloff: the frame's two
//   directional lights, one with cascades, and the camera's clustered point,
//   spot and rectangle lights, baked ones too, through their records.
// - The cascades take Godot's fog tap (directional_shadow.wgsl) without its
//   fade to unshadowed toward the shadow distance: beyond the last cascade
//   the medium is unshadowed, as surfaces are, whose cascades have no fade.
// - A local light's shadow is one hardware 2×2 comparison tap
//   (shadow_sampling.wgsl), as Bevy's volumetric fog samples its shadow
//   maps, without Godot's fade behind the occluder: not ported, as the
//   atlas's perspective depth would need linearizing for its metres.
// - A rectangle scatters by the solid angle of its face, π times its form
//   factor about the direction to its centre, where Godot's area light takes
//   it about the direction to its nearest point and fades it in over 10 cm
//   in front of the face. Bounded by π, it needs no 1 m distance clamp,
//   which Godot gives area lights against jitter flicker.
// - No light has Godot's shadow_opacity (stevepryde/sgl#65).
// - The ambient is the frame's hemisphere fill and environment diffuse
//   averaged over the sphere, which an isotropic phase scatters, where Godot
//   samples its sky upward, at a mip chosen by the density, and along the
//   view, blended by |g|, mixed with its ambient colour by its sky
//   contribution.
// - Froxels lie where the camera's unjittered projection puts them, and in
//   the last frame's volume where its view-projection does, so a changing
//   projection reprojects too, where Godot interpolates frustum sizes from
//   the near plane and reprojects through the camera's transform alone.
// - A froxel without history (the first fog frame, a new size, the camera's
//   reset) keeps none, where Godot blends from a cleared volume, so its fog
//   fades in, and across a cut.
// - The history alternates two volumes instead of copying one: the filter's
//   y pass writes the one the injection has just reprojected, so the history
//   stays unfiltered, as Godot copies it before it filters. The filter skips
//   froxels outside the volume, whose stores WGSL leaves undefined.
// - Each filter invocation filters a run of 8 froxels along its pass's axis
//   from the 14 their taps reach, loading each once, where Godot's
//   invocation filters one froxel from 7 loads: 1.75 loads per froxel per
//   pass instead of 7.
//   Its sums are Godot's, term for term, through the same RGBA16F volume
//   between the passes. FidelityFX Blur 1.1 (FidelityFX SDK c6efa6b,
//   sdk/include/FidelityFX/gpu/blur/ffx_blur.h) shares taps the same way,
//   walking each thread group down its columns so each row's blur serves
//   every vertical tap reaching it, and reads its input through the texture
//   cache, its workgroup input cache (BLUR_ENABLE_INPUT_CACHE) off as
//   slower; here one invocation holds its run's taps in registers. Tiles in
//   workgroup memory, as Godot's screen-space reflection filter and Wicked
//   Engine's blur_gaussian_float4CS.hlsl use, measured slower than these
//   runs on an Apple M5 (stevepryde/sgl#70).
// - The integration steps along the view ray through each slice, where
//   Godot steps the slice's depth, so fog off the view's axis is as dense as
//   on it.
// - The sky takes the whole fog, without Godot's sky affect
//   (stevepryde/sgl#61), and nothing fogs under an orthographic camera,
//   which Godot's fog covers: froxels are placed by a perspective
//   projection.

// The froxels, first to last, that the scene's fog volume at `volume` in
// fog_volumes reaches (stages/fog/volume_froxels.rs FogVolumeFroxels).
struct FogVolumeFroxels {
 first:vec3<u32>,
 volume:u32,
 last:vec3<u32>,
}

// One frame's froxel volume (stages/fog.rs FroxelVolumeUniform).
struct FroxelVolume {
 world_from_view:mat4x4<f32>,
 // The last frame's unjittered view-projection.
 previous_clip_from_world:mat4x4<f32>,
 // The unjittered projection's x and y scales, then its x and y offsets:
 // the view point at depth d under frame position ndc is
 // ((ndc + offset) * d / scale, -d).
 projection:vec4<f32>,
 size:vec3<u32>,
 // This frame's offset in FOG_HALTON.
 frame:u32,
 albedo:vec3<f32>,
 density:f32,
 render_size:vec2<f32>,
 length:f32,
 detail_spread:f32,
 height:f32,
 height_falloff:f32,
 anisotropy:f32,
 // The share of the last frame's volume a froxel keeps where it
 // reprojects; 0 without history.
 temporal_blend:f32,
 // The share of the frame's ambient light the medium scatters.
 ambient:f32,
 // How many of fog_volume_froxels hold this frame's.
 volume_count:u32,
}
@group(1) @binding(0) var<uniform> froxels:FroxelVolume;
// Injection: the last frame's froxels and the sampler that reprojects them,
// and this frame's.
@group(1) @binding(1) var previous_scattering:texture_3d<f32>;
@group(1) @binding(2) var history_sampler:sampler;
@group(1) @binding(3) var scattering:texture_storage_3d<rgba16float,write>;
@group(1) @binding(6) var<storage,read> fog_volumes:array<FogVolumeRecord>;
@group(1) @binding(9) var<storage,read> fog_volume_froxels:array<FogVolumeFroxels>;
// Filter: the froxels one pass reads and writes, and Godot's params.filter_axis
// (0 x, 1 y) it filters along.
@group(1) @binding(7) var source_map:texture_3d<f32>;
@group(1) @binding(8) var dest_map:texture_storage_3d<rgba16float,write>;
override filter_axis:u32=0u;
// Integration: this frame's froxels and the volume it integrates them into.
@group(1) @binding(4) var integrate_scattering:texture_3d<f32>;
@group(1) @binding(5) var integrated:texture_storage_3d<rgba16float,write>;

// Godot's halton_map: where in its froxel each frame samples while the
// froxel reprojects.
const FOG_HALTON:array<vec3<f32>,16>=array(
 vec3(.5,.33333333,.2),
 vec3(.25,.66666667,.4),
 vec3(.75,.11111111,.6),
 vec3(.125,.44444444,.8),
 vec3(.625,.77777778,.04),
 vec3(.375,.22222222,.24),
 vec3(.875,.55555556,.44),
 vec3(.0625,.88888889,.64),
 vec3(.5625,.03703704,.84),
 vec3(.3125,.37037037,.08),
 vec3(.8125,.7037037,.28),
 vec3(.1875,.14814815,.48),
 vec3(.6875,.48148148,.68),
 vec3(.4375,.81481481,.88),
 vec3(.9375,.25925926,.12),
 vec3(.03125,.59259259,.32),
);

// The fog energy at or below which Godot leaves a light out of the medium,
// shadow lookup and all (volumetric_fog_process.glsl's light loops).
const FOG_ENERGY_CUTOFF:f32=.001;

fn henyey_greenstein(cos_theta:f32,g:f32)->f32 {
 // 1 / (4 * PI)
 let k=.0795774715459;
 return k*(1.-g*g)/pow(1.+g*g-2.*g*cos_theta,1.5);
}
// Neither infinite nor NaN, judged by the exponent bits so no fast-math
// assumption removes the test.
fn fog_finite(value:vec4<f32>)->bool {
 let exponent=bitcast<vec4<u32>>(value)&vec4(0x7f800000u);
 return all(exponent!=vec4(0x7f800000u));
}
// The frame position of a froxel's unit x and y, with y down the frame.
// It and froxel_view_position have their Rust inverse in
// stages/fog/volume_froxels.rs, which bounds the fog volumes' froxels.
fn froxel_ndc(unit:vec2<f32>)->vec2<f32> {
 return vec2(unit.x*2.-1.,1.-unit.y*2.);
}
fn froxel_view_position(ndc:vec2<f32>,depth:f32)->vec3<f32> {
 return vec3((ndc+froxels.projection.zw)*depth/froxels.projection.xy,-depth);
}
// The world position at a froxel's unit coordinates.
fn froxel_world(unit:vec3<f32>)->vec3<f32> {
 let depth=fog_slice_depth(unit.z,froxels.length,froxels.detail_spread);
 return (froxels.world_from_view*vec4(froxel_view_position(froxel_ndc(unit.xy),depth),1.)).xyz;
}
// The frame's ambient light that an isotropic medium scatters: the mean of
// the radiance from every direction. The hemisphere fill and environment
// diffuse each give a surface's diffuse radiance for its normal, so the mean
// of an upward and a downward one's is the sphere's for light that varies
// with height alone.
fn fog_ambient()->vec3<f32> {
 let up=vec3(0.,1.,0.);
 let sky=frame.hemisphere_sky_color;
 let ground=frame.hemisphere_ground_color;
 let intensity=frame.hemisphere_intensity;
 let hemisphere=(pbr_hemisphere(up,sky,ground,intensity)+pbr_hemisphere(-up,sky,ground,intensity))/(2.*3.14159265359);
 return hemisphere+(diffuse_environment(up)+diffuse_environment(-up))*.5;
}
// The solid angle of rectangle `rect`'s face from a point `toward` its
// centre: the face's form factor about that direction, times PI.
fn fog_rect_solid_angle(rect:Light,position:vec3<f32>,toward:vec3<f32>)->f32 {
 let identity=mat3x3(vec3(1.,0.,0.),vec3(0.,1.,0.),vec3(0.,0.,1.));
 let face=rect_light_frame(toward,toward,rect.position-position,rect.half_width,light_rect_half_height(rect));
 return 3.14159265359*ltc_integrate_quad(face,identity);
}
// The density fog volume `volume` adds at `position`: Godot's box FogVolume
// (its signed distance and cull mask) with its FogMaterial's edge fade.
fn fog_volume_density(volume:FogVolumeRecord,position:vec3<f32>)->f32 {
 let offset=position-volume.center;
 if dot(offset,offset)>volume.radius_squared {
  return 0.;
 }
 let local=(volume.local_from_world*vec4(position,1.)).xyz;
 let q=abs(local)-volume.half_size;
 let sdf=length(max(q,vec3(0.)))+min(max(q.x,max(q.y,q.z)),0.);
 var density=volume.density*(1.-smoothstep(-.1,0.,sdf));
 if volume.edge_fade>0. {
  let side=min(volume.half_size.x,min(volume.half_size.y,volume.half_size.z));
  density*=pow(clamp(-sdf/side,0.,1.),volume.edge_fade);
 }
 return density;
}
// What scene light `index` scatters toward the camera, along `view_ray`,
// per unit of scattering at a point in the medium: none, without a shadow
// lookup, at a fog energy Godot skips.
fn fog_scene_light(index:u32,position:vec3<f32>,view_ray:vec3<f32>,pixel:vec2<f32>)->vec3<f32> {
 let fog_energy=lights[index].fog_energy;
 if fog_energy<=FOG_ENERGY_CUTOFF {
  return vec3(0.);
 }
 let sample=scene_light_sample(index,position,vec3(0.),pixel,SHADOW_RECEIVER_MEDIUM);
 if sample.visibility<=0. {
  return vec3(0.);
 }
 var light=sample.radiance*sample.visibility;
 if sample.rect!=NO_RECT_LIGHT {
  light*=fog_rect_solid_angle(lights[sample.rect],position,sample.direction);
 }
 return light*henyey_greenstein(dot(view_ray,sample.direction),froxels.anisotropy)*fog_energy;
}

// The ambient light the medium scatters, the same at every froxel: one
// invocation of each workgroup samples it.
var<workgroup> fog_ambient_light:vec3<f32>;

// Each froxel's extinction (a) and the light its medium scatters toward the
// camera per metre (rgb), blended with where it was in the last frame's.
@compute @workgroup_size(4,4,4) fn inject(@builtin(global_invocation_id) id:vec3<u32>,@builtin(local_invocation_index) local:u32) {
 if local==0u {
  fog_ambient_light=fog_ambient()*froxels.ambient;
 }
 workgroupBarrier();
 if any(id>=froxels.size) {
  return;
 }
 let size=vec3<f32>(froxels.size);
 var unit=(vec3<f32>(id)+vec3(.5))/size;
 var reprojected=vec4(0.);
 var reproject_amount=0.;
 if froxels.temporal_blend>0. {
  let previous=froxels.previous_clip_from_world*vec4(froxel_world(unit),1.);
  // The last frame's clip w is its view depth.
  if previous.w>0. {
   let previous_uv=vec2(previous.x,-previous.y)/previous.w*.5+vec2(.5);
   let previous_unit=fog_volume_coordinate(previous_uv,previous.w,1./froxels.length,1./froxels.detail_spread);
   if all(previous_unit>vec3(0.)) && all(previous_unit<vec3(1.)) {
    reprojected=textureSampleLevel(previous_scattering,history_sampler,previous_unit,0.);
    reproject_amount=froxels.temporal_blend;
    // Only a froxel that reprojects jitters.
    unit=(vec3<f32>(id)+FOG_HALTON[froxels.frame])/size;
   }
  }
 }
 let position=froxel_world(unit);
 let view_ray=normalize(position-view.eye);
 let pixel=unit.xy*froxels.render_size;
 var density=froxels.density*clamp(exp2(-froxels.height_falloff*(position.y-froxels.height)),0.,1.);
 // The scattering: each medium's albedo weighted by its density, as Godot
 // weights its environment medium's.
 var albedo=froxels.albedo*density;
 for(var index=0u;index<froxels.volume_count;index++) {
  let reached=fog_volume_froxels[index];
  if any(id<reached.first) || any(id>reached.last) {
   continue;
  }
  let volume=fog_volumes[reached.volume];
  let added=fog_volume_density(volume,position);
  density+=added;
  albedo+=volume.albedo*added;
 }
 var light=vec3(0.);
 if density>.00005 {
  for(var index=0u;index<2u;index++) {
   let directional=frame.directional_lights[index];
   if directional.illuminance<=0. || directional.fog_energy<=FOG_ENERGY_CUTOFF {
    continue;
   }
   let toward=normalize(directional.direction_to_light);
   var shadow=1.;
   if (directional.flags&DIRECTIONAL_LIGHT_SHADOW)!=0u {
    shadow=directional_shadow_visibility(index,position,vec3(0.),pixel,SHADOW_RECEIVER_MEDIUM);
   }
   light+=directional.color*directional.illuminance*shadow*henyey_greenstein(dot(view_ray,toward),froxels.anisotropy)*directional.fog_energy;
  }
  light+=fog_ambient_light;
  let range=cluster_range(position,pixel);
  let end=range.first+range.live+range.baked;
  for(var at=range.first;at<end;at++) {
   light+=fog_scene_light(cluster_item(at),position,view_ray,pixel);
  }
 }
 var froxel=vec4(light*albedo,density);
 if !fog_finite(froxel) {
  froxel=select(vec4(0.),reprojected,fog_finite(reprojected));
 } else if fog_finite(reprojected) {
  froxel=mix(froxel,reprojected,reproject_amount);
 }
 textureStore(scattering,id,clamp(froxel,vec4(0.),vec4(65504.)));
}

// Godot's MODE_FILTER sum: the 7-tap Gaussian of the froxels from three
// before a froxel along filter_axis to three after it, in Godot's order.
fn filter_gauss(t0:vec4<f32>,t1:vec4<f32>,t2:vec4<f32>,t3:vec4<f32>,t4:vec4<f32>,t5:vec4<f32>,t6:vec4<f32>) -> vec4<f32> {
 const gauss=array<f32,7>(.071303,.131514,.189879,.214607,.189879,.131514,.071303);
 var accum=vec4(0.);
 accum+=t0*gauss[0];
 accum+=t1*gauss[1];
 accum+=t2*gauss[2];
 accum+=t3*gauss[3];
 accum+=t4*gauss[4];
 accum+=t5*gauss[5];
 accum+=t6*gauss[6];
 return accum;
}

// Godot's MODE_FILTER: each froxel's 7-tap Gaussian along filter_axis of
// this frame's froxels, clamped to the volume's edges. Each invocation
// filters the 8 froxels from pos along the axis (stages/fog.rs FILTER_RUN)
// from the 14 their taps reach, each loaded once. The taps are written out:
// a loop over an array of them measured nearly 3× slower.
@compute @workgroup_size(8,8,1) fn filter_froxels(@builtin(global_invocation_id) id:vec3<u32>) {
 const filter_dir=array(vec3(1,0,0),vec3(0,1,0),vec3(0,0,1));
 let offset=filter_dir[filter_axis];
 let last=vec3<i32>(froxels.size)-vec3(1);
 let pos=vec3<i32>(id)*(vec3(1)+offset*7);
 if any(pos>last) {
  return;
 }
 let t0=textureLoad(source_map,clamp(pos-offset*3,vec3(0),last),0);
 let t1=textureLoad(source_map,clamp(pos-offset*2,vec3(0),last),0);
 let t2=textureLoad(source_map,clamp(pos-offset,vec3(0),last),0);
 let t3=textureLoad(source_map,pos,0);
 let t4=textureLoad(source_map,clamp(pos+offset,vec3(0),last),0);
 let t5=textureLoad(source_map,clamp(pos+offset*2,vec3(0),last),0);
 let t6=textureLoad(source_map,clamp(pos+offset*3,vec3(0),last),0);
 let t7=textureLoad(source_map,clamp(pos+offset*4,vec3(0),last),0);
 let t8=textureLoad(source_map,clamp(pos+offset*5,vec3(0),last),0);
 let t9=textureLoad(source_map,clamp(pos+offset*6,vec3(0),last),0);
 let t10=textureLoad(source_map,clamp(pos+offset*7,vec3(0),last),0);
 let t11=textureLoad(source_map,clamp(pos+offset*8,vec3(0),last),0);
 let t12=textureLoad(source_map,clamp(pos+offset*9,vec3(0),last),0);
 let t13=textureLoad(source_map,clamp(pos+offset*10,vec3(0),last),0);
 // The run stops at the volume's edge.
 let run=min(dot(last-pos,offset)+1,8);
 textureStore(dest_map,pos,filter_gauss(t0,t1,t2,t3,t4,t5,t6));
 if run>1 {
  textureStore(dest_map,pos+offset,filter_gauss(t1,t2,t3,t4,t5,t6,t7));
 }
 if run>2 {
  textureStore(dest_map,pos+offset*2,filter_gauss(t2,t3,t4,t5,t6,t7,t8));
 }
 if run>3 {
  textureStore(dest_map,pos+offset*3,filter_gauss(t3,t4,t5,t6,t7,t8,t9));
 }
 if run>4 {
  textureStore(dest_map,pos+offset*4,filter_gauss(t4,t5,t6,t7,t8,t9,t10));
 }
 if run>5 {
  textureStore(dest_map,pos+offset*5,filter_gauss(t5,t6,t7,t8,t9,t10,t11));
 }
 if run>6 {
  textureStore(dest_map,pos+offset*6,filter_gauss(t6,t7,t8,t9,t10,t11,t12));
 }
 if run>7 {
  textureStore(dest_map,pos+offset*7,filter_gauss(t7,t8,t9,t10,t11,t12,t13));
 }
}

// Each column's light scattered toward the camera (rgb) and transmittance
// (a) from the camera to each slice's centre.
@compute @workgroup_size(8,8,1) fn integrate(@builtin(global_invocation_id) id:vec3<u32>) {
 if any(id.xy>=froxels.size.xy) {
  return;
 }
 let unit=(vec2<f32>(id.xy)+vec2(.5))/vec2<f32>(froxels.size.xy);
 // Metres along the column's view ray per metre of depth.
 let ray_scale=length(froxel_view_position(froxel_ndc(unit),1.));
 var accumulated=vec4(0.,0.,0.,1.);
 var previous_depth=0.;
 for(var z=0u;z<froxels.size.z;z++) {
  let position=vec3(id.xy,z);
  let froxel=textureLoad(integrate_scattering,position,0);
  let depth=fog_slice_depth((f32(z)+.5)/f32(froxels.size.z),froxels.length,froxels.detail_spread);
  // Beer-Lambert over the step, and the light scattered within it,
  // integrated against its own extinction (Hillaire 2015).
  let transmittance=exp(-(depth-previous_depth)*ray_scale*froxel.a);
  accumulated=vec4(accumulated.rgb+(froxel.rgb-froxel.rgb*transmittance)/max(froxel.a,.00001)*accumulated.a,accumulated.a*transmittance);
  previous_depth=depth;
  textureStore(integrated,position,select(vec4(0.),clamp(accumulated,vec4(0.),vec4(65504.)),fog_finite(accumulated)));
 }
}
