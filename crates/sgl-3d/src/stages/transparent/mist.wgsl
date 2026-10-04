// The mist: billboards at the scene's mist positions, shaded by drifting
// noise and fogged where they stand.
struct Mist {
 @builtin(position) clip:vec4<f32>,
 @location(0) uv:vec2<f32>,
}
@vertex fn mist_vs(@builtin(vertex_index) id:u32,@location(0) center:vec3<f32>)->Mist {
 let corners=array<vec2<f32>,6>(vec2(-.5,-.5),vec2(.5,-.5),vec2(.5,.5),vec2(-.5,-.5),vec2(.5,.5),vec2(-.5,.5));
 let p=corners[id];
 let vp=view.view_projection;
 let right=normalize(vec3(vp[0].x,vp[1].x,vp[2].x));
 let up=normalize(vec3(vp[0].y,vp[1].y,vp[2].y));
 var o:Mist;
 o.clip=vp*vec4(center+right*p.x*frame.mist_size.x+up*p.y*frame.mist_size.y,1.);
 o.uv=p+vec2(.5);
 return o;
}
fn mist_color(i:Mist)->vec4<f32> {
 let drift=i.uv*vec2(7.,3.)+vec2(frame.elapsed_seconds*.025,frame.elapsed_seconds*-.035);
 let noise=noise2(drift)*.325+.325+noise2(drift*2.1)*.125+.125+noise2(drift*4.3)*.05+.05;
 let alpha=(1.-smoothstep(.15,.52,length(i.uv-vec2(.5))))*smoothstep(.15,.7,noise)*frame.mist_opacity;
 let color=mix(frame.mist_thin_color,frame.mist_dense_color,noise);
 // A fragment's position w is one over its view depth.
 return vec4(frame_fog(color,i.clip.xy,1./i.clip.w),alpha);
}
@fragment fn mist_fs(i:Mist)->@location(0) vec4<f32> {
 return mist_color(i);
}
// FSR2's masks (fsr2.rs): mist is a translucent material in AMD's FSR sample
// (FidelityFX SDK 1.1.4, MIT, see LICENSE-amd-fidelityfx.txt,
// framework/rendermodules/translucency/shaders/translucencyps.hlsl:168-169 and
// samples/fsrapi/fsrapirendermodule.cpp:100-101): no reactivity, its alpha as
// transparency and composition.
struct MistFsr2Masked {
 @location(0) color:vec4<f32>,
 @location(1) reactive:f32,
 @location(2) composition:f32,
}
@fragment fn mist_fsr2_masked_fs(i:Mist)->MistFsr2Masked {
 let color=mist_color(i);
 return MistFsr2Masked(color,0.,color.a);
}
