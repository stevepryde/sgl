// Source completion: each opaque receiver's ambient occlusion of its ambient
// light, and its environment and probe specular, which the forward pass
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

// A receiver's specular lobes (specular_lobes.wgsl) from its G-buffer at
// `world`, seen from the camera; `anisotropy` is its anisotropy target's
// texel (gbuffer_anisotropy).
fn source_lobes(normals:vec4<f32>,material:GBufferMaterial,f0:vec4<f32>,anisotropy:vec4<f32>,world:vec3<f32>,camera:SourceCamera)->array<SpecularLobe,2> {
 let view=normalize(camera.eye.xyz-world);
 let normal=gbuffer_base_normal(normals);
 let base_dfg=lookup_dfg(source_lookup_tables,env_sampler,specular_nv(normal,view),material.roughness);
 return specular_lobes(normal,gbuffer_coat_normal(normals),view,f0.rgb,material.f90,material.roughness,base_dfg,material.coat,material.coat_roughness,gbuffer_anisotropy(anisotropy),source_lookup_tables,env_sampler);
}
// A lobe's environment and probe specular at `world` along `lobe`, occluded
// (occlusion_environment): the probes' by the receiver's `visibility`
// (source_visibility), the sky's by that times the irradiance volume's
// `sky_visibility` a(n).
fn source_occluded_environment(world:vec3<f32>,lobe:SpecularLobe,coat:bool,environment_scale:f32,tile:u32,visibility:f32,sky_visibility:f32,f0:vec3<f32>)->vec3<f32> {
 let environment=source_environment(world,lobe.direction,lobe.roughness,environment_scale,tile);
 return occlusion_environment(lobe,coat,environment.probes,environment.sky,visibility,sky_visibility,f0);
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
// A lit receiver's ambient light (gbuffer_ambient) loses what its
// `visibility` (source_visibility) hides: its diffuse share linearly, its
// multiple scattering by its base lobe's specular occlusion
// (occlusion_ambient). A screen-space method's traced lobe takes no
// environment specular here; the method's composition adds it by
// confidence. Incident radiance includes both lobes so another receiver
// sees the fully lit source, independently of tracing. The ambient light's
// `sky_visibility` is the irradiance volume's a(n) at it. An opaque
// receiver's alpha is 1, as glTF's OPAQUE and MASK modes ignore its
// material's, where lit colour's alpha held its multiple scattering's share;
// the sky keeps `alpha`.
fn complete_source(scene:vec3<f32>,alpha:f32,ambient:GBufferAmbient,normals:vec4<f32>,material:GBufferMaterial,f0:vec4<f32>,anisotropy:vec4<f32>,z:f32,id:vec2<u32>,size:vec2<u32>,camera:SourceCamera,traced:f32,visibility:f32)->CompletedSource {
 var incoming=scene;
 var incident=scene;
 let sky_visibility=ambient.sky_visibility;
 var completed_alpha=alpha;
 // The sky lies beyond the fog volume.
 var view_depth=3.4e38;
 if z>0. {
  completed_alpha=1.;
  let world=source_world(z,id,size,camera);
  if gbuffer_lit(f0) {
   let lobes=source_lobes(normals,material,f0,anisotropy,world,camera);
   if visibility<1. {
    let base=lobes[SPECULAR_BASE];
    let multi_occlusion=occlusion_multiscatter(base.nv,base.roughness,visibility,f0.rgb);
    incoming=occlusion_ambient(incoming,ambient.diffuse,ambient.multi,visibility,multi_occlusion);
    incident=incoming;
   }
   let traced_lobe=specular_traced_lobe(material.coat);
   for(var lobe=SPECULAR_BASE;lobe<=SPECULAR_COAT;lobe++) {
    if lobe==SPECULAR_COAT && material.coat<=0. {
     continue;
    }
    let environment=source_occluded_environment(world,lobes[lobe],lobe==SPECULAR_COAT,gbuffer_environment_scale(anisotropy),source_probes(id),visibility,sky_visibility,f0.rgb)*lobes[lobe].response;
    incident+=environment;
    if lobe!=traced_lobe || !specular_traces(lobes[lobe].roughness,traced) {
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
 return CompletedSource(vec4(fog_composite(incoming,fog),completed_alpha),vec4(fog_composite(incident,fog),completed_alpha));
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
@group(0) @binding(18) var baked:texture_cube_array<f32>;
@group(0) @binding(19) var<storage,read> collection:ProbeCollection;
// Each tile's probe buckets (probe_culling.wgsl), from its first.
@group(0) @binding(21) var<storage,read> probe_tiles:array<u32>;
fn source_probes(p:vec2<u32>)->u32 {
 let tiles_x=(textureDimensions(source_depth).x+PROBE_TILE_SIZE-1u)/PROBE_TILE_SIZE;
 return ((p.y/PROBE_TILE_SIZE)*tiles_x+p.x/PROBE_TILE_SIZE)*PROBE_BUCKETS;
}
// Wicked's TiledLighting loads each bucket of the tile as it walks it.
fn source_environment(world:vec3<f32>,direction:vec3<f32>,rough:f32,sky_scale:f32,tile:u32)->EnvironmentSpecular {
 if !application_environment_enabled {
  return EnvironmentSpecular(vec3(0.),vec3(0.));
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
// A lit receiver's visibility at pixel `id` with G-buffer F0 `f0`: the
// lesser of its material's occlusion and the frame's ambient occlusion
// (occlusion_visibility).
fn source_visibility(id:vec2<u32>,f0:vec4<f32>)->f32 {
 return occlusion_visibility(gbuffer_occlusion(f0),source_ambient_visibility(id));
}
@group(0) @binding(14) var incident_output:texture_storage_2d<rgba16float,write>;
// The ambient light within source_scene before occlusion, and in alpha the
// irradiance volume's sky visibility; source_scene's alpha holds its
// multiple scattering's share (shading/gbuffer.wgsl). Completion takes what
// each lit receiver's visibility hides out of the beauty
// (complete_source), before adding specular.
@group(0) @binding(24) var source_ambient:texture_2d<f32>;
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
 let z=textureLoad(source_depth,p,0);
 let f0=textureLoad(source_f0,p,0);
 var normals=vec4(0.);
 var material=gbuffer_material(vec4(0.));
 var ambient=GBufferAmbient(vec3(0.),vec3(0.),1.);
 var visibility=1.;
 if z>0. && gbuffer_lit(f0) {
  normals=textureLoad(source_normal,p,0);
  material=gbuffer_material(textureLoad(receiver_material,p,0));
  ambient=gbuffer_ambient(textureLoad(source_ambient,p,0),c.a);
  visibility=source_visibility(id.xy,f0);
 }
 let completed=complete_source(max(vec3(0.),c.rgb),c.a,ambient,normals,material,f0,textureLoad(source_anisotropy,p,0),z,id.xy,textureDimensions(source_output),source_camera,env.traced,visibility);
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
// Godot's over 0.1), so no seam shows where a roughness crosses it. Under a
// blended receiver (the surface depth nearer than the opaque depth) the
// method traced the receiver, which composes its result itself, and world
// rays skipped the pixel: the opaque lobe takes its environment and probe
// specular alone.
@group(0) @binding(20) var screen_space:texture_2d<f32>;
// World-space reflection rays (world_reflections.wgsl): radiance premultiplied
// by the share of rays that hit what they reach (a); probes and sky fill the rest.
@group(0) @binding(23) var world_space:texture_2d<f32>;
// The surface depth (the Surface contract, specs/sgl3d-architecture.md).
@group(0) @binding(27) var source_surface_depth:texture_depth_2d;
@fragment fn compose_screen_space(@builtin(position) position:vec4<f32>)->@location(0) vec4<f32> {
 let p=vec2<i32>(position.xy);
 let z=textureLoad(source_depth,p,0);
 let f0=textureLoad(source_f0,p,0);
 if z<=0. || !gbuffer_lit(f0) {
  return vec4(0.);
 }
 let under_receiver=gbuffer_under_receiver(textureLoad(source_surface_depth,p,0),z);
 let id=vec2<u32>(p);
 let size=textureDimensions(source_depth);
 let world=source_world(z,id,size,source_camera);
 let material=gbuffer_material(textureLoad(receiver_material,p,0));
 let anisotropy=textureLoad(source_anisotropy,p,0);
 let lobes=source_lobes(textureLoad(source_normal,p,0),material,f0,anisotropy,world,source_camera);
 var reflected=vec4(0.);
 var world_hit=vec4(0.);
 if !under_receiver {
  reflected=textureLoad(screen_space,p,0);
  if all(id<textureDimensions(world_space)) {
   world_hit=textureLoad(world_space,p,0);
  }
 }
 let visibility=source_visibility(id,f0);
 var specular=vec3(0.);
 let traced_lobe=specular_traced_lobe(material.coat);
 let lobe=lobes[traced_lobe];
 if specular_traces(lobe.roughness,env.traced) {
  let sky_visibility=gbuffer_sky_visibility(textureLoad(source_ambient,p,0));
  let environment=source_occluded_environment(world,lobe,traced_lobe==SPECULAR_COAT,gbuffer_environment_scale(anisotropy),source_probes(id),visibility,sky_visibility,f0.rgb);
  let fallback=world_hit.rgb+environment*(1.-world_hit.a);
  let fade=specular_trace_fade(lobe.roughness,sqrt(env.traced),env.fade);
  specular=specular_traced(lobe,reflected,fade,fallback);
 }
 return vec4(specular*source_fog(source_view_depth(world,source_camera),id,size,source_camera).a,0.);
}
