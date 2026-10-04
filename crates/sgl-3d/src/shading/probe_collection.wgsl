// Baked specular probes (Lagarde and Zanuttini, "Local Image-based Lighting
// With Parallax-corrected Cubemaps", SIGGRAPH 2012). Consumers declare
// `collection` (ProbeCollection) and `baked` (the cube array).
//
// A probe's influence weight is Godot's per-axis blend distance
// (scene_forward_lights_inc.glsl reflection_process): one at `blend` inside
// each face of its influence box, falling linearly to zero at the face.
fn probe_influence(world:vec3<f32>,probe:BakedProbe)->f32 {
 let p=(probe.world_to_local*vec4(world,1.)).xyz;
 let inside=min(p-probe.influence_min.xyz,probe.influence_max.xyz-p);
 if any(inside<vec3(0.)) {
  return 0.;
 }
 let ramp=select(vec3(1.),clamp(inside/max(probe.blend.xyz,vec3(0.000001)),vec3(0.),vec3(1.)),probe.blend.xyz>vec3(0.));
 return ramp.x*ramp.y*ramp.z;
}
// Captures store the cube in the capture views' Z-mirrored convention
// (`View::probe_face`), with roughness by mip as specular_probe_levels.wgsl
// maps it.
fn probe_radiance(index:u32,world:vec3<f32>,direction:vec3<f32>,rough:f32,filter_sampler:sampler)->vec3<f32> {
 let probe=collection.probes[index];
 var d=direction;
 if probe.center.w>0.5 {
  d=probe_parallax_direction(world,direction,probe);
 }
 return textureSampleLevel(baked,filter_sampler,vec3(d.xy,-d.z),i32(index),specular_probe_lod(clamp(rough,0.,1.))).rgb;
}
fn collection_sky_radiance(sky:texture_2d_array<f32>,filter_sampler:sampler,direction:vec3<f32>,rough:f32,rotation:f32,strength:f32)->vec3<f32> {
 return pmrem_sample(sky,filter_sampler,pmrem_direction(direction,rotation),rough)*strength;
}
// A receiver's environment specular is walked as Wicked's TiledLighting walks
// a tile: over the 32-probe buckets the collection occupies
// (ShaderEntityIterator's first and last bucket), each bucket's probes in
// turn. Overlapping probes are normalised by their total weight (Lagarde
// 2012), so neighbours' linear blends partition exactly; where the total
// falls below one, as at the edge of the probes, the sky fills the rest
// (Godot's accumulation).
struct ProbeSum {
 radiance:vec3<f32>,
 weight:f32
}
fn collection_buckets()->u32 {
 return min((collection.counts.x+31u)/32u,PROBE_BUCKETS);
}
// Adds probe `index` if it contains the receiver.
fn collection_add(sum:ProbeSum,index:u32,world:vec3<f32>,direction:vec3<f32>,rough:f32,filter_sampler:sampler)->ProbeSum {
 var result=sum;
 let weight=probe_influence(world,collection.probes[index]);
 if weight>0. {
  result.radiance+=weight*probe_radiance(index,world,direction,rough,filter_sampler);
  result.weight+=weight;
 }
 return result;
}
// Adds the probes of bucket `bucket` in `bits` that contain the receiver.
fn collection_bucket(sum:ProbeSum,bucket:u32,bucket_bits:u32,world:vec3<f32>,direction:vec3<f32>,rough:f32,filter_sampler:sampler)->ProbeSum {
 var result=sum;
 var bits=bucket_bits;
 while bits!=0u {
  let bit=firstTrailingBit(bits);
  bits^=1u<<bit;
  result=collection_add(result,bucket*32u+bit,world,direction,rough,filter_sampler);
 }
 return result;
}
fn collection_resolve(sum:ProbeSum,direction:vec3<f32>,rough:f32,scale:f32,sky:texture_2d_array<f32>,filter_sampler:sampler,rotation:f32,strength:f32)->vec3<f32> {
 let coverage=min(sum.weight,1.);
 var result=vec3(0.);
 if sum.weight>0. {
  result=sum.radiance/sum.weight*coverage;
 }
 if coverage<1. {
  result+=collection_sky_radiance(sky,filter_sampler,direction,rough,rotation,strength)*(1.-coverage);
 }
 return result*scale;
}
