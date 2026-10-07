// The transmission provider of the Extended binding tier: the frame behind
// a transmissive surface from the transparent stage's mipmapped copy of the
// composed frame (bind_blended_extended.wgsl), where the draw's group 3
// holds it (BlendedTrace.transmission), filtered through lit group 0's
// trilinear clamp-to-edge sampler (decal_sampler). A program composes it or
// transmission_basic.wgsl, exactly one; transmission.wgsl calls it.
//
// Mipped bicubic texture filtering by N8 (https://www.shadertoy.com/view/Dl2SDW)
// as three.js r185 ports it, textureBicubicLevel and its weights
// (src/nodes/accessors/TextureBicubic.js 6-75; commit 2431a09f, MIT,
// stages/post/smaa/LICENSE-three.txt), translated to WGSL, its helpers
// prefixed. Changed: each level read is clamped to the copy's last, where
// three.js reads the size of a level past it.

// Whether the copy holds this frame's composed frame.
fn transmission_frame_held()->bool {
 return blended_trace.transmission!=0u;
}
// The copy's size in texels, at level 0.
fn transmission_frame_size()->vec2<f32> {
 return vec2<f32>(textureDimensions(blended_transmission,0));
}
fn bicubic_w0(a:f32)->f32 {
 return (1./6.)*(a*(a*(-a+3.)-3.)+1.);
}
fn bicubic_w1(a:f32)->f32 {
 return (1./6.)*(a*(a*(3.*a-6.))+4.);
}
fn bicubic_w2(a:f32)->f32 {
 return (1./6.)*(a*(a*(-3.*a+3.)+3.)+1.);
}
fn bicubic_w3(a:f32)->f32 {
 return (1./6.)*(a*a*a);
}
// g0 and g1 are the two amplitude functions.
fn bicubic_g0(a:f32)->f32 {
 return bicubic_w0(a)+bicubic_w1(a);
}
fn bicubic_g1(a:f32)->f32 {
 return bicubic_w2(a)+bicubic_w3(a);
}
// h0 and h1 are the two offset functions.
fn bicubic_h0(a:f32)->f32 {
 return -1.+bicubic_w1(a)/(bicubic_w0(a)+bicubic_w1(a));
}
fn bicubic_h1(a:f32)->f32 {
 return 1.+bicubic_w3(a)/(bicubic_w2(a)+bicubic_w3(a));
}
// The copy at `uv` and level `lod`, of whose size `texelSize` holds the
// reciprocal in xy and the texels in zw: four bilinear taps.
fn bicubic(uv:vec2<f32>,texelSize:vec4<f32>,lod:f32)->vec4<f32> {
 let uvScaled=uv*texelSize.zw+.5;
 let iuv=floor(uvScaled);
 let fuv=fract(uvScaled);
 let g0x=bicubic_g0(fuv.x);
 let g1x=bicubic_g1(fuv.x);
 let h0x=bicubic_h0(fuv.x);
 let h1x=bicubic_h1(fuv.x);
 let h0y=bicubic_h0(fuv.y);
 let h1y=bicubic_h1(fuv.y);
 let p0=(vec2(iuv.x+h0x,iuv.y+h0y)-.5)*texelSize.xy;
 let p1=(vec2(iuv.x+h1x,iuv.y+h0y)-.5)*texelSize.xy;
 let p2=(vec2(iuv.x+h0x,iuv.y+h1y)-.5)*texelSize.xy;
 let p3=(vec2(iuv.x+h1x,iuv.y+h1y)-.5)*texelSize.xy;
 let a=bicubic_g0(fuv.y)*(g0x*textureSampleLevel(blended_transmission,decal_sampler,p0,lod)+g1x*textureSampleLevel(blended_transmission,decal_sampler,p1,lod));
 let b=bicubic_g1(fuv.y)*(g0x*textureSampleLevel(blended_transmission,decal_sampler,p2,lod)+g1x*textureSampleLevel(blended_transmission,decal_sampler,p3,lod));
 return a+b;
}
// The copy at `uv`, filtered bicubically at level `lod` (three.js's
// textureBicubicLevel): the two levels about it, each bicubic, mixed.
fn transmission_frame_sample(uv:vec2<f32>,lod:f32)->vec4<f32> {
 let last=f32(textureNumLevels(blended_transmission)-1u);
 let fine=min(floor(lod),last);
 let coarse=min(ceil(lod),last);
 let fLodSize=vec2<f32>(textureDimensions(blended_transmission,u32(fine)));
 let cLodSize=vec2<f32>(textureDimensions(blended_transmission,u32(coarse)));
 let fSample=bicubic(uv,vec4(1./fLodSize,fLodSize),fine);
 let cSample=bicubic(uv,vec4(1./cLodSize,cLodSize),coarse);
 return mix(fSample,cSample,fract(lod));
}
