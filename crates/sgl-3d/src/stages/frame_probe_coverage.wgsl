// Diagnostic only: the primary raster's geometry pixels (nonzero depth) and
// those without a lit primitive identity, with the first such pixel.
@group(0) @binding(0) var depth:texture_depth_2d;
@group(0) @binding(1) var identity:texture_2d<u32>;
@compute @workgroup_size(8,8) fn main(@builtin(global_invocation_id) id:vec3<u32>) {
 if any(id.xy>=textureDimensions(depth)) {
  return;
 }
 let p=vec2<i32>(id.xy);
 let z=textureLoad(depth,p,0);
 if z>0. {
  atomicAdd(&stats[24],1u);
  if textureLoad(identity,p,0).x==0u {
   if atomicAdd(&stats[25],1u)==0u {
    atomicStore(&stats[26],id.x);
    atomicStore(&stats[27],id.y);
    atomicStore(&stats[28],bitcast<u32>(z));
   }
  }
 }
}
