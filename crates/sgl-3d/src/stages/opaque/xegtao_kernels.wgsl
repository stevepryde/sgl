// XeGTAO, Copyright (C) 2016-2021 Intel Corporation, SPDX-License-Identifier: MIT.
// XeGTAO's pure kernels that ambient occlusion and screen-space indirect light
// both take, one owner for each (XeGTAO.hlsli a5b1686: XeGTAO_FastACos, the
// Hilbert R2 noise of SpatioTemporalNoise, where a slice's samples sit,
// XeGTAO_CalculateEdges and XeGTAO_PackEdges, and XeGTAO_Denoise's edge
// weights). Binds nothing.
fn fast_acos(v:f32)->f32 {
    let x=abs(v);
    let root=bitcast<f32>(0x1fbd1df5i+(bitcast<i32>(1.0-x)>>1u));
    let result=(-0.156583*x+1.570796)*root;
    return select(3.141593-result,result,v>=0.0);
}
// The R2 sequence at a cell's Hilbert index (hilbert_lut) in the frame
// `frame`, turned by 288 a frame over 64 frames.
fn xegtao_noise(hilbert_index:u32,frame:u32)->vec2<f32> {
    let index=hilbert_index+288u*(frame%64u);
    return fract(vec2(0.5)+f32(index)*vec2(0.75487766624669276005,0.5698402909980532659114));
}
// XeGTAO's pixelTooCloseThreshold (XeGTAO.hlsli 335): every sample lies at
// least this many pixels from its receiver, so none reads the receiver's
// own pixel, which tells it nothing.
const XEGTAO_PIXEL_TOO_CLOSE: f32 = 1.3;
// Where sample `step` of a slice's `steps` a side lies, as a share of the
// projected radius `radius_pixels`: (step + noise) / steps, `noise` in
// [0, 1), raised to `power`, then XeGTAO's pixel floor added, so the last
// sample reaches the radius (XeGTAO.hlsli 420 to 433: s = pow((step +
// stepNoise) / stepsPerSlice, sampleDistributionPower) + minS, minS =
// pixelTooCloseThreshold / screenspaceRadius; Bevy 9d12036 ssao.wesl 180
// and 181 square it the same way). A `noise` shared by a slice's steps keeps
// its samples in order outward.
fn xegtao_sample_s(step:u32,steps:u32,noise:f32,power:f32,radius_pixels:f32)->f32 {
    return pow((f32(step)+noise)/f32(steps),power)+XEGTAO_PIXEL_TOO_CLOSE/radius_pixels;
}
// A pixel's edges toward its left, right, top and bottom neighbours from its
// view depth `z` and theirs, 1 where the surface continues and 0 across a
// discontinuity, its slope allowed for.
fn xegtao_edges(z:f32,neighbours:vec4<f32>)->vec4<f32> {
    var edges=neighbours-vec4(z);
    let slopes=vec2(edges.y-edges.x,edges.w-edges.z)*0.5;
    edges=min(abs(edges),abs(edges+vec4(slopes.x,-slopes.x,slopes.y,-slopes.y)));
    return clamp(vec4(1.25)-edges/(z*0.011),vec4(0.0),vec4(1.0));
}
// Edges two bits each, left in the highest, as XeGTAO's R8 edges.
fn xegtao_pack_edges(edges:vec4<f32>)->u32 {
    return u32(dot(round(edges*2.9),vec4(64.0,16.0,4.0,1.0)));
}
fn xegtao_unpack_edges(packed:u32)->vec4<f32> {
    return vec4<f32>((vec4(packed)>>vec4(6u,4u,2u,0u))&vec4(3u))/3.0;
}
// XeGTAO_Denoise's weights of a pixel's cardinal neighbours (left, right,
// top, bottom) and corners (top-left, top-right, bottom-left, bottom-right)
// from its edges and theirs; the pixel itself weighs 1.2.
struct XeGtaoDenoiseWeights {
    cardinal:vec4<f32>,
    corners:vec4<f32>,
}
fn xegtao_denoise_weights(center:vec4<f32>,left:vec4<f32>,right:vec4<f32>,top:vec4<f32>,bottom:vec4<f32>)->XeGtaoDenoiseWeights {
    var edges=center*vec4(left.y,right.x,top.w,bottom.z);
    let edginess=clamp(1.5-dot(edges,vec4(1.0)),0.0,1.0)/1.5*0.5;
    edges=clamp(edges+edginess,vec4(0.0),vec4(1.0));
    let corners=0.425*vec4(edges.x*left.z+edges.z*top.x,edges.z*top.y+edges.y*right.z,edges.w*bottom.x+edges.x*left.w,edges.y*right.w+edges.w*bottom.y);
    return XeGtaoDenoiseWeights(edges,corners);
}
