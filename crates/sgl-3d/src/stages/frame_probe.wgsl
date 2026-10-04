// Diagnostic only: classify raw IEEE values, never modify the rendered image.
@group(0) @binding(0) var current:texture_2d<f32>;
@group(0) @binding(1) var source:texture_2d<f32>;
struct Params {
 stage:u32,
 compare_source:u32,
 pad:vec2<u32>,
}
@group(0) @binding(3) var<uniform> params:Params;
fn finite(v:vec3<u32>)->bool {
 return all((v&vec3(0x7f800000u))!=vec3(0x7f800000u));
}
fn save(base:u32,p:vec2<u32>,v:vec3<u32>,s:vec3<u32>) {
 atomicStore(&stats[base],p.x);
 atomicStore(&stats[base+1u],p.y);
 for(var i=0u;i<3u;i++) {
  atomicStore(&stats[base+2u+i],v[i]);
  atomicStore(&stats[base+5u+i],s[i]);
 }
}
@compute @workgroup_size(8,8) fn main(@builtin(global_invocation_id) id:vec3<u32>) {
 if any(id.xy>=textureDimensions(current)) {
  return;
 }
 let v=bitcast<vec3<u32>>(textureLoad(current,vec2<i32>(id.xy),0).rgb);
 let s=bitcast<vec3<u32>>(textureLoad(source,vec2<i32>(id.xy),0).rgb);
 let base=params.stage*32u;
 atomicStore(&stats[base+31u],1u);
 let zero=all((v&vec3(0x7fffffffu))==vec3(0u));
 if zero {
  atomicAdd(&stats[base],1u);
 }
 if !finite(v) {
  if atomicAdd(&stats[base+1u],1u)==0u {
   save(base+8u,id.xy,v,s);
  }
 }
 if finite(v) && any(((v&vec3(0x80000000u))!=vec3(0u)) & ((v&vec3(0x7fffffffu))!=vec3(0u))) {
  atomicAdd(&stats[base+2u],1u);
 }
 if any((v&vec3(0x7fffffffu))>vec3(0x7f800000u)) {
  atomicAdd(&stats[base+5u],1u);
 }
 if any(v==vec3(0x7f800000u)) {
  atomicAdd(&stats[base+6u],1u);
 }
 if any(v==vec3(0xff800000u)) {
  atomicAdd(&stats[base+7u],1u);
 }
 let source_positive=finite(s) && any((s>vec3(0u)) & (s<vec3(0x7f800000u)));
 if params.compare_source!=0u && zero && source_positive {
  if atomicAdd(&stats[base+3u],1u)==0u {
   save(base+16u,id.xy,v,s);
  }
 }
 for(var i=0u;i<3u;i++) {
  if v[i]<0x7f800000u {
   atomicMax(&stats[base+4u],v[i]);
  }
 }
}
