// AMD FidelityFX SDK 1.1.4, revision c6efa6bf7f2027b3ec94f28578bb5965eabb9e55.
// Port of gpu/fsr1/ffx_fsr1.h: fsrEasuSetFloat, fsrEasuTapFloat,
// ffxFsrEasuFloat and FsrRcasF. Copyright (C) 2024 Advanced Micro Devices, Inc.
// MIT, see ../../LICENSE-amd-fidelityfx.txt. Godot b130438's
// effects/fsr_upscale.glsl supplies the reference EASU -> RCAS schedule.
// Portable f32 fragment passes, no optional device features. Explicit
// clamped loads replace gathers. AMD's bit reciprocal approximations
// are retained; a zero gradient remains finite. Input and intermediate colour are
// gamma 2 as AMD requires perceptual colour; the final result is linear.
// ffx_core_gpu_common.h scalar approximation helpers, kept bit-for-bit.
fn fsr1_rcp(value:f32)->f32 {
 return bitcast<f32>(0x7ef07ebbu-bitcast<u32>(value));
}
fn fsr1_rcp_medium(value:f32)->f32 {
 let b=bitcast<f32>(0x7ef19fffu-bitcast<u32>(value));
 return b*(-b*value+2.);
}
fn fsr1_rsqrt(value:f32)->f32 {
 return bitcast<f32>(0x5f347d74u-(bitcast<u32>(value)>>1u));
}
fn fsr1_load_linear(p:vec2<i32>)->vec3<f32> {
 let last=vec2<i32>(textureDimensions(scene))-vec2(1);
 return textureLoad(scene,clamp(p,vec2(0),last),0).rgb;
}
fn fsr1_load(p:vec2<i32>)->vec3<f32> {
 return sqrt(clamp(fsr1_load_linear(p),vec3(0.),vec3(1.)));
}
fn fsr1_luma(c:vec3<f32>)->f32 {
 return c.b*.5+(c.r*.5+c.g);
}
// fsrEasuSetFloat, bilinear weight supplied by the caller.
fn fsr1_edge(w:f32,a:f32,b:f32,c:f32,d:f32,e:f32)->vec3<f32> {
 let dir=vec2(d-b,e-a);
 let magnitude=max(vec2(abs(d-c),abs(e-c)),vec2(abs(c-b),abs(c-a)));
 let len=clamp(abs(dir)*vec2(fsr1_rcp(magnitude.x),fsr1_rcp(magnitude.y)),vec2(0.),vec2(1.));
 return vec3(dir*w,dot(len,len)*w);
}
// fsrEasuTapFloat. RGB weighted colour and weight are accumulated together.
fn fsr1_tap(off:vec2<f32>,dir:vec2<f32>,len:vec2<f32>,lob:f32,clp:f32,c:vec3<f32>)->vec4<f32> {
 let v=vec2(dot(off,dir),dot(off,vec2(-dir.y,dir.x)))*len;
 let d2=min(dot(v,v),clp);
 let weight_b=(2./5.)*d2-1.;
 let weight_a=lob*d2-1.;
 let weight=((25./16.)*weight_b*weight_b-(25./16.-1.))*weight_a*weight_a;
 return vec4(c*weight,weight);
}
@fragment fn fsr1_easu(vertex:Output)->@location(0) vec4<f32> {
 // FsrEasuCon for a whole input texture: uv is output pixel centre/output size.
 let position=vertex.uv*vec2<f32>(textureDimensions(scene))-.5;
 let fp=vec2<i32>(floor(position));
 let pp=fract(position);
 let b=fsr1_load(fp+vec2(0,-1));
 let bL=fsr1_luma(b);
 let c=fsr1_load(fp+vec2(1,-1));
 let cL=fsr1_luma(c);
 let i=fsr1_load(fp+vec2(-1,1));
 let iL=fsr1_luma(i);
 let j=fsr1_load(fp+vec2(0,1));
 let jL=fsr1_luma(j);
 let f=fsr1_load(fp+vec2(0,0));
 let fL=fsr1_luma(f);
 let e=fsr1_load(fp+vec2(-1,0));
 let eL=fsr1_luma(e);
 let k=fsr1_load(fp+vec2(1,1));
 let kL=fsr1_luma(k);
 let l=fsr1_load(fp+vec2(2,1));
 let lL=fsr1_luma(l);
 let h=fsr1_load(fp+vec2(2,0));
 let hL=fsr1_luma(h);
 let g=fsr1_load(fp+vec2(1,0));
 let gL=fsr1_luma(g);
 let o=fsr1_load(fp+vec2(1,2));
 let oL=fsr1_luma(o);
 let n=fsr1_load(fp+vec2(0,2));
 let nL=fsr1_luma(n);
 let edge=fsr1_edge((1.-pp.x)*(1.-pp.y),bL,eL,fL,gL,jL)
          +fsr1_edge(pp.x*(1.-pp.y),cL,fL,gL,hL,kL)
          +fsr1_edge((1.-pp.x)*pp.y,fL,iL,jL,kL,nL)
          +fsr1_edge(pp.x*pp.y,gL,jL,kL,lL,oL);
 var dir=edge.xy;
 let dir_r=dot(dir,dir);
 if dir_r<1./32768. {
  dir=vec2(1.,dir.y);
 } else {
  dir*=fsr1_rsqrt(dir_r);
 }
 let len=(edge.z*.5)*(edge.z*.5);
 let stretch=dot(dir,dir)*fsr1_rcp(max(abs(dir.x),abs(dir.y)));
 let len2=vec2(1.+(stretch-1.)*len,1.-.5*len);
 let lob=.5+((1./4.-.04)-.5)*len;
 let clp=fsr1_rcp(lob);
 let min4=min(min(f,g),min(j,k));
 let max4=max(max(f,g),max(j,k));
 var accumulated=vec4(0.);
 accumulated+=fsr1_tap(vec2(0.,-1.)-pp,dir,len2,lob,clp,b);
 accumulated+=fsr1_tap(vec2(1.,-1.)-pp,dir,len2,lob,clp,c);
 accumulated+=fsr1_tap(vec2(-1.,1.)-pp,dir,len2,lob,clp,i);
 accumulated+=fsr1_tap(vec2(0.,1.)-pp,dir,len2,lob,clp,j);
 accumulated+=fsr1_tap(vec2(0.,0.)-pp,dir,len2,lob,clp,f);
 accumulated+=fsr1_tap(vec2(-1.,0.)-pp,dir,len2,lob,clp,e);
 accumulated+=fsr1_tap(vec2(1.,1.)-pp,dir,len2,lob,clp,k);
 accumulated+=fsr1_tap(vec2(2.,1.)-pp,dir,len2,lob,clp,l);
 accumulated+=fsr1_tap(vec2(2.,0.)-pp,dir,len2,lob,clp,h);
 accumulated+=fsr1_tap(vec2(1.,0.)-pp,dir,len2,lob,clp,g);
 accumulated+=fsr1_tap(vec2(1.,2.)-pp,dir,len2,lob,clp,o);
 accumulated+=fsr1_tap(vec2(0.,2.)-pp,dir,len2,lob,clp,n);
 return vec4(clamp(accumulated.rgb/accumulated.a,min4,max4),1.);
}
// FsrRcasF without optional denoise, matching Godot's default path.
fn fsr1_rcas(p:vec2<i32>)->vec3<f32> {
 // EASU already wrote gamma-2 colour: do not convert again.
 let b=fsr1_load_linear(p+vec2(0,-1));
 let d=fsr1_load_linear(p+vec2(-1,0));
 let e=fsr1_load_linear(p);
 let f=fsr1_load_linear(p+vec2(1,0));
 let h=fsr1_load_linear(p+vec2(0,1));
 let mn4=min(min(b,d),min(f,h));
 let mx4=max(max(b,d),max(f,h));
 // At a flat black/white channel the unconstrained limiter is zero.
 let hit_min=mn4/select(vec3(1.),4.*mx4,mx4>vec3(0.));
 let hit_max=(vec3(1.)-mx4)/select(vec3(-1.),4.*mn4-4.,mn4<vec3(1.));
 let lobes=max(-hit_min,hit_max);
 // FsrRcasCon: fixed 0.2 stops, the FSR sample's 0.8 sharpness.
 let lobe=max(-(.25-1./16.),min(max(max(lobes.r,lobes.g),lobes.b),0.))*exp2(-.2);
 let color=clamp((lobe*b+lobe*d+lobe*h+lobe*f+e)*fsr1_rcp_medium(4.*lobe+1.),vec3(0.),vec3(1.));
 return color*color;
}
