// GGX prefiltering of a captured HDR cube into a cube with the perceptual
// roughness specular_probe_levels.wgsl assigns each mip. Filtered importance
// sampling (Colbert and Krivanek, GPU Gems 3, ch. 20) reads the captured
// cube's mip chain so a small HDR emitter contributes its angular footprint.
@group(0) @binding(0) var captured:texture_cube<f32>;
@group(0) @binding(1) var linear_sampler:sampler;
@group(0) @binding(2) var filtered:texture_storage_2d_array<rgba16float,write>;
@group(0) @binding(5) var<uniform> level:u32;
const PI:f32=3.14159265359;
fn radical_inverse(i:u32)->f32 {
 return f32(reverseBits(i))*2.3283064365386963e-10;
}
fn basis(n:vec3<f32>)->mat3x3<f32> {
 let up=select(vec3(0.,1.,0.),vec3(0.,0.,1.),abs(n.y)>0.99);
 let t=normalize(cross(up,n));
 return mat3x3(t,cross(n,t),n);
}
fn radiance(direction:vec3<f32>,lod:f32)->vec3<f32> {
 return textureSampleLevel(captured,linear_sampler,direction,lod).rgb;
}
fn sample_lod(direction:vec3<f32>,pdf:f32,count:u32)->f32 {
 let size=f32(textureDimensions(captured,0).x);
 let major=max(abs(direction.x),max(abs(direction.y),abs(direction.z)));
 let texel_solid_angle=4.*major*major*major/(size*size);
 return max(0.,0.5*log2(1./(f32(count)*pdf*texel_solid_angle)));
}
// The direction through a cube texel centre: WebGPU's (and D3D's) cube face
// orientation, the inverse of the lookup textureSampleLevel performs.
fn texel_direction(face:u32,uv:vec2<f32>)->vec3<f32> {
 let s=uv.x*2.-1.;
 let t=uv.y*2.-1.;
 switch face {
  case 0u {return vec3(1.,-t,-s);}
  case 1u {return vec3(-1.,-t,s);}
  case 2u {return vec3(s,1.,t);}
  case 3u {return vec3(s,-1.,-t);}
  case 4u {return vec3(s,-t,1.);}
  default {return vec3(-s,-t,-1.);}
 }
}
@compute @workgroup_size(8,8,1)
fn prefilter(@builtin(global_invocation_id) id:vec3<u32>) {
 let size=textureDimensions(filtered).x;
 if id.x>=size || id.y>=size || id.z>=6u {return;}
 let n=normalize(texel_direction(id.z,(vec2<f32>(id.xy)+0.5)/f32(size)));
 // A destination texel spans 2^level source texels: integrate that footprint
 // even for the sharp level, so thin emissive strips do not alias.
 let footprint_lod=log2(f32(textureDimensions(captured,0).x)/f32(size));
 if level==0u {
  textureStore(filtered,vec2<i32>(id.xy),i32(id.z),vec4(radiance(n,footprint_lod),1.));
  return;
 }
 let frame=basis(n);
 let count=64u;
 var total=vec3(0.);
 var weight=0.;
 let roughness=specular_probe_roughness(level);
 let alpha=roughness*roughness;
 for(var i=0u;i<count;i++) {
  let xi=vec2((f32(i)+0.5)/f32(count),radical_inverse(i));
  let angle=2.*PI*xi.y;
  let cos_theta=sqrt((1.-xi.x)/(1.+(alpha*alpha-1.)*xi.x));
  let sin_theta=sqrt(max(0.,1.-cos_theta*cos_theta));
  let h=frame*vec3(sin_theta*cos(angle),sin_theta*sin(angle),cos_theta);
  let l=normalize(2.*dot(n,h)*h-n);
  let ndotl=max(dot(n,l),0.);
  let denominator=cos_theta*cos_theta*(alpha*alpha-1.)+1.;
  let pdf=max(alpha*alpha/(4.*PI*denominator*denominator),0.000001);
  if ndotl>0. {
   total+=radiance(l,max(footprint_lod,sample_lod(l,pdf,count)))*ndotl;
   weight+=ndotl;
  }
 }
 textureStore(filtered,vec2<i32>(id.xy),i32(id.z),vec4(total/max(weight,0.0001),1.));
}

@group(0) @binding(3) var previous_mip:texture_2d_array<f32>;
@group(0) @binding(4) var next_mip:texture_storage_2d_array<rgba16float,write>;
@compute @workgroup_size(8,8,1)
fn mip_reduce(@builtin(global_invocation_id) id:vec3<u32>) {
 let size=textureDimensions(next_mip);
 if id.x>=size.x || id.y>=size.y || id.z>=6u {return;}
 let p=vec2<i32>(id.xy)*2;
 let color=textureLoad(previous_mip,p,i32(id.z),0)+textureLoad(previous_mip,p+vec2(1,0),i32(id.z),0)
          +textureLoad(previous_mip,p+vec2(0,1),i32(id.z),0)+textureLoad(previous_mip,p+vec2(1,1),i32(id.z),0);
 textureStore(next_mip,vec2<i32>(id.xy),i32(id.z),color*0.25);
}
