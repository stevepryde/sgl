// Source completion: each opaque receiver's ambient occlusion of its ambient
// diffuse, and its environment and probe specular, which the forward pass
// excludes, as probe captures do; and the composition that blends a
// screen-space method's reflections over it.

// Source-completion camera. With SOURCE_FOG, the frame's fog volume fogs
// what completion and composition write; `fog` holds one over its length and
// over its detail spread (fog.wgsl), and the share of its fog the sky takes.
struct SourceCamera {
 inverse:mat4x4<f32>,
 eye:vec4<f32>,
 fog:vec4<f32>,
 forward:vec3<f32>,
 flags:u32,
}
const SOURCE_FOG:u32=1u;
// The frame's fog volume (stages/fog.wgsl) and its sampler.
@group(0) @binding(25) var source_fog_volume:texture_3d<f32>;
@group(0) @binding(26) var source_fog_sampler:sampler;
// False leaves out environment and probe specular (a diagnostics layer).
override application_environment_enabled:bool=true;

// The reflection environment: the prefiltered sky's yaw and intensity, the
// width of the screen-space method's fade in perceptual roughness, and the
// alpha roughness below which the method traces a lobe (reflections/mod.rs).
struct ReflectionEnvironment {
 yaw:f32,
 intensity:f32,
 fade:f32,
 traced:f32,
}
@group(0) @binding(8) var sky_map:texture_2d_array<f32>;
@group(0) @binding(10) var env_sampler:sampler;
@group(0) @binding(12) var<uniform> env:ReflectionEnvironment;
@group(0) @binding(13) var receiver_material:texture_2d<f32>;

// A receiver's specular lobe: its split-sum response, reflection direction,
// perceptual roughness and N.V, and whether a screen-space method traces it.
struct SourceLobe {
 weight:vec3<f32>,
 ray:vec3<f32>,
 rough:f32,
 nv:f32,
 traced:bool,
}
// The base lobe and, for a coated receiver, the coat lobe. A screen-space
// method traces the coat of a coated receiver, else the base, when its alpha
// roughness is below `traced`.
fn source_lobes(normals:vec4<f32>,material:GBufferMaterial,f0:vec4<f32>,anisotropy:vec4<f32>,world:vec3<f32>,camera:SourceCamera,traced:f32)->array<SourceLobe,2> {
 let n=gbuffer_base_normal(normals);
 let coat=gbuffer_coat_normal(normals);
 let v=normalize(camera.eye.xyz-world);
 let nv=max(dot(n,v),0.);
 let coat_nv=max(dot(coat,v),0.);
 let coat_ray=reflect(-v,coat);
 let coat_f=pbr_coat_fresnel(coat,v,material.coat);
 let base_ray=pbr_anisotropy_reflection(n,v,anisotropy,material.roughness);
 let base=SourceLobe(pbr_three_single_scatter(f0.rgb,source_brdf(nv,material.roughness))*(1.-coat_f),base_ray,material.roughness,nv,material.coat<=0. && material.roughness*material.roughness<traced);
 let coat_lobe=SourceLobe(pbr_three_single_scatter(vec3(0.04),source_brdf(coat_nv,material.coat_roughness))*material.coat,normalize(mix(coat_ray,coat,pow(material.coat_roughness,4.))),material.coat_roughness,coat_nv,material.coat>0. && material.coat_roughness*material.coat_roughness<traced);
 return array(base,coat_lobe);
}
// Specular occlusion of a lobe's environment and probe specular by the
// receiver's ambient visibility, as Filament's desktop default evaluates it
// (ef1a133 shaders/src/surface_ambient_occlusion.fs SpecularAO_Lagarde and
// gtaoMultiBounce, applied as surface_light_indirect.fs evaluateIBL and
// evaluateClearCoatIBL do; Apache-2.0, see LICENSE-filament.txt. Modified:
// translated to WGSL). Lagarde and de Rousiers 2014, "Moving Frostbite to
// PBR", with GTAO's multi-bounce on the base lobe's F0 (Jimenez et al. 2016).
// Screen-space hits are visible surfaces and stay unoccluded, as in Filament.
fn source_specular_occlusion(lobe:SourceLobe,coat:bool,visibility:f32,f0:vec3<f32>)->vec3<f32> {
 let alpha=lobe.rough*lobe.rough;
 let ao=clamp(pow(lobe.nv+visibility,exp2(-16.*alpha-1.))-1.+visibility,0.,1.);
 if coat {
  return vec3(ao);
 }
 let a=2.0404*f0-vec3(.3324);
 let b=-4.7951*f0+vec3(.6417);
 let c=2.7552*f0+vec3(.6903);
 return max(vec3(ao),((ao*a+b)*ao+c)*ao);
}
fn source_world(z:f32,id:vec2<u32>,size:vec2<u32>,camera:SourceCamera)->vec3<f32> {
 let uv=(vec2<f32>(id)+vec2(0.5))/vec2<f32>(size);
 let h=camera.inverse*vec4(uv*vec2(2.,-2.)+vec2(-1.,1.),z,1.);
 return h.xyz/h.w;
}
// The fog between the camera and a receiver `view_depth` metres deep at
// pixel `id` of `size`: the light it scatters toward the camera (rgb) and its
// transmittance (a); none without SOURCE_FOG.
fn source_fog(view_depth:f32,id:vec2<u32>,size:vec2<u32>,camera:SourceCamera)->vec4<f32> {
 if (camera.flags&SOURCE_FOG)==0u {
  return vec4(0.,0.,0.,1.);
 }
 let uv=(vec2<f32>(id)+vec2(.5))/vec2<f32>(size);
 let coordinate=fog_volume_coordinate(uv,view_depth,camera.fog.x,camera.fog.y);
 return textureSampleLevel(source_fog_volume,source_fog_sampler,coordinate,0.);
}
// The view depth of a receiver at `world`.
fn source_view_depth(world:vec3<f32>,camera:SourceCamera)->f32 {
 return dot(world-camera.eye.xyz,camera.forward);
}
struct CompletedSource {
 color:vec4<f32>,
 incident:vec4<f32>,
}
// A screen-space method's traced lobe takes no environment specular here; the
// method's composition adds it by confidence. Incident radiance includes both
// lobes so another receiver sees the fully lit source, independently of tracing.
fn complete_source(incoming_value:vec3<f32>,alpha:f32,normals:vec4<f32>,material:GBufferMaterial,f0:vec4<f32>,anisotropy:vec4<f32>,z:f32,id:vec2<u32>,size:vec2<u32>,camera:SourceCamera,traced:f32)->CompletedSource {
 var incoming=incoming_value;
 var incident=incoming_value;
 // The sky lies beyond the fog volume.
 var view_depth=3.4e38;
 if z>0. {
  let world=source_world(z,id,size,camera);
  if gbuffer_lit(f0) {
   let lobes=source_lobes(normals,material,f0,anisotropy,world,camera,traced);
   let visibility=source_ambient_visibility(id);
   for(var i=0u;i<2u;i++) {
    if i==1u && material.coat<=0. {
     continue;
    }
    let environment=source_environment(world,lobes[i].ray,lobes[i].rough,material.environment_scale,source_probes(id))*lobes[i].weight*source_specular_occlusion(lobes[i],i==1u,visibility,f0.rgb);
    incident+=environment;
    if !lobes[i].traced {
     incoming+=environment;
    }
   }
  }
  view_depth=source_view_depth(world,camera);
 }
 var fog=source_fog(view_depth,id,size,camera);
 if z<=0. {
  // The sky takes its sky affect of the fog: Godot b130438's
  // mix(sky, fogged sky, volumetric_fog_sky_affect)
  // (servers/rendering/renderer_rd/shaders/environment/sky.glsl), MIT
  // (src/LICENSE-godot.txt), as one mix of the fog with no fog.
  fog=mix(vec4(0.,0.,0.,1.),fog,camera.fog.z);
 }
 return CompletedSource(vec4(fog_composite(incoming,fog),alpha),vec4(fog_composite(incident,fog),alpha));
}

@group(0) @binding(0) var source_scene:texture_2d<f32>;
@group(0) @binding(2) var source_output:texture_storage_2d<rgba16float,write>;
@group(0) @binding(3) var source_normal:texture_2d<f32>;
@group(0) @binding(4) var source_f0:texture_2d<f32>;
@group(0) @binding(5) var source_depth:texture_depth_2d;
@group(0) @binding(6) var<uniform> source_camera:SourceCamera;
// Lit group 0's lookup tables, for the DFG table (lookup_tables.wgsl).
@group(0) @binding(7) var source_lookup_tables:texture_2d_array<f32>;
@group(0) @binding(17) var source_anisotropy:texture_2d<f32>;
fn source_brdf(nv:f32,rough:f32)->vec2<f32> {
 return lookup_dfg(source_lookup_tables,env_sampler,nv,rough);
}
@group(0) @binding(18) var baked:texture_cube_array<f32>;
@group(0) @binding(19) var<storage,read> collection:ProbeCollection;
// Each tile's probe buckets (probe_culling.wgsl), from its first.
@group(0) @binding(21) var<storage,read> probe_tiles:array<u32>;
fn source_probes(p:vec2<u32>)->u32 {
 let tiles_x=(textureDimensions(source_depth).x+PROBE_TILE_SIZE-1u)/PROBE_TILE_SIZE;
 return ((p.y/PROBE_TILE_SIZE)*tiles_x+p.x/PROBE_TILE_SIZE)*PROBE_BUCKETS;
}
// Wicked's TiledLighting loads each bucket of the tile as it walks it.
fn source_environment(world:vec3<f32>,direction:vec3<f32>,rough:f32,sky_scale:f32,tile:u32)->vec3<f32> {
 if !application_environment_enabled {
  return vec3(0.);
 }
 var sum=ProbeSum(vec3(0.),0.);
 for(var bucket=0u;bucket<collection_buckets();bucket++) {
  sum=collection_bucket(sum,bucket,probe_tiles[tile+bucket],world,direction,rough,env_sampler);
 }
 return collection_resolve(sum,direction,rough,sky_scale,sky_map,env_sampler,env.yaw,env.intensity);
}
// XeGTAO visibility, or neutral 255 when ambient occlusion is off.
@group(0) @binding(22) var source_ambient_occlusion:texture_2d<u32>;
fn source_ambient_visibility(id:vec2<u32>)->f32 {
 let p=min(id,textureDimensions(source_ambient_occlusion)-vec2(1u));
 return f32(textureLoad(source_ambient_occlusion,p,0).x)/255.;
}
@group(0) @binding(14) var incident_output:texture_storage_2d<rgba16float,write>;
// The ambient diffuse within source_scene before occlusion (shading/gbuffer.wgsl).
@group(0) @binding(24) var source_ambient:texture_2d<f32>;
// True while ambient occlusion runs: completion takes the share of each lit
// receiver's ambient diffuse that its ambient visibility hides out of the
// beauty, before adding specular.
override diffuse_occlusion_enabled:bool=false;
// `beauty` without the share of its ambient diffuse `ambient` that
// `visibility` hides, never negative. Ambient occlusion weights only ambient
// diffuse light, which the beauty holds as one additive term, so occluding it
// here equals occluding it while shading, as Bevy's deferred lighting applies
// SSAO in a screen pass over its G-buffer (9d12036
// crates/bevy_pbr/src/deferred/deferred_lighting.wesl:65-77).
fn source_diffuse_occlusion(beauty:vec3<f32>,ambient:vec3<f32>,visibility:f32)->vec3<f32> {
 return max(vec3(0.),beauty-(1.-visibility)*ambient);
}
// False writes only the composite: no screen-space method runs to read the
// incident radiance, and incident_output is a stand-in (reflections/source.rs).
override incident_radiance_enabled:bool=true;
// Keep fully lit incident radiance separate from the composition base, which
// excludes the traced lobe until compose_screen_space replaces it by confidence.
// Filament 0e1ad7d4 filament/src/details/Renderer.cpp exports
// colorPassOutput.linearColor for SSR history; Godot's screen_space_reflection.glsl samples
// source_last_frame. We complete current-frame environment lighting here instead
// of feeding back previous-frame SSR. No upstream code is copied in this split.
@compute @workgroup_size(8,8) fn main(@builtin(global_invocation_id) id:vec3<u32>) {
 if any(id.xy>=textureDimensions(source_output)) {
  return;
 }
 let p=vec2<i32>(id.xy);
 let c=textureLoad(source_scene,p,0);
 var incoming=max(vec3(0.),c.rgb);
 let z=textureLoad(source_depth,p,0);
 let f0=textureLoad(source_f0,p,0);
 var normals=vec4(0.);
 var material=gbuffer_material(vec4(0.));
 if z>0. && gbuffer_lit(f0) {
  normals=textureLoad(source_normal,p,0);
  material=gbuffer_material(textureLoad(receiver_material,p,0));
  if diffuse_occlusion_enabled {
   incoming=source_diffuse_occlusion(incoming,textureLoad(source_ambient,p,0).rgb,source_ambient_visibility(id.xy));
  }
 }
 let completed=complete_source(incoming,c.a,normals,material,f0,textureLoad(source_anisotropy,p,0),z,id.xy,textureDimensions(source_output),source_camera,env.traced);
 textureStore(source_output,p,completed.color);
 if incident_radiance_enabled {
  textureStore(incident_output,p,completed.incident);
 }
}

// Screen-space composition, after a screen-space method ran (vertex entry
// fullscreen_vs): the traced lobe's specular is its split-sum response times
// the method's radiance (rgb, premultiplied by its confidence a) plus this
// receiver's specular-occluded environment and probe specular for the rest,
// then fogged as source completion fogs: by the fog's transmittance alone,
// since completion added the light it scatters. The method fades out over the last
// env.fade of perceptual roughness below its cutoff (Bevy's SSR over 0.05,
// Godot's over 0.1), so no seam shows where a roughness crosses it.
@group(0) @binding(20) var screen_space:texture_2d<f32>;
// World-space reflection rays (world_reflections.wgsl): radiance premultiplied
// by the share of rays that hit a moving object (a); probes and sky fill the rest.
@group(0) @binding(23) var world_space:texture_2d<f32>;
@fragment fn compose_screen_space(@builtin(position) position:vec4<f32>)->@location(0) vec4<f32> {
 let p=vec2<i32>(position.xy);
 let z=textureLoad(source_depth,p,0);
 let f0=textureLoad(source_f0,p,0);
 if z<=0. || !gbuffer_lit(f0) {
  return vec4(0.);
 }
 let id=vec2<u32>(p);
 let size=textureDimensions(source_depth);
 let world=source_world(z,id,size,source_camera);
 let material=gbuffer_material(textureLoad(receiver_material,p,0));
 var lobes=source_lobes(textureLoad(source_normal,p,0),material,f0,textureLoad(source_anisotropy,p,0),world,source_camera,env.traced);
 let reflected=textureLoad(screen_space,p,0);
 var world_hit=vec4(0.);
 if all(id<textureDimensions(world_space)) {
  world_hit=textureLoad(world_space,p,0);
 }
 let visibility=source_ambient_visibility(id);
 var specular=vec3(0.);
 for(var i=0u;i<2u;i++) {
  if lobes[i].traced {
   let environment=source_environment(world,lobes[i].ray,lobes[i].rough,material.environment_scale,source_probes(id))*source_specular_occlusion(lobes[i],i==1u,visibility,f0.rgb);
   let fallback=world_hit.rgb+environment*(1.-world_hit.a);
   let cutoff=sqrt(env.traced);
   let fade=1.-smoothstep(cutoff-env.fade,cutoff,lobes[i].rough);
   specular+=(reflected.rgb*fade+fallback*(1.-reflected.a*fade))*lobes[i].weight;
  }
 }
 return vec4(specular*source_fog(source_view_depth(world,source_camera),id,size,source_camera).a,0.);
}
