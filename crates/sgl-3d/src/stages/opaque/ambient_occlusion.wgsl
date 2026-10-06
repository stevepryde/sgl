// XeGTAO, Copyright (C) 2016-2021 Intel Corporation, SPDX-License-Identifier: MIT.
// FP32 scalar-visibility port; exact authority and platform differences: ambient_occlusion/README.md.
struct Params {
    view: mat4x4<f32>,
    size: vec2<u32>,
    pixel_size: vec2<f32>,
    depth_unpack: vec2<f32>,
    ndc_mul: vec2<f32>,
    ndc_add: vec2<f32>,
    radius: f32,
    slices: u32,
    steps: u32,
}
// The most slices and steps a pixel takes, XeGTAO's Ultra preset, so its
// loops end whatever the parameters say.
const MOST_SLICES: u32 = 9u;
const MOST_STEPS: u32 = 3u;
// The Hilbert curve's tile, texels across: hilbert_lut's extent.
const HILBERT_WIDTH: u32 = 64u;
@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var raw_depth: texture_depth_2d;
@group(0) @binding(2) var input_depth: texture_2d<f32>;
@group(0) @binding(3) var output_depth0: texture_storage_2d<r32float, write>;
@group(0) @binding(4) var normals: texture_2d<f32>;
@group(0) @binding(5) var output_ao: texture_storage_2d<r32uint, write>;
@group(0) @binding(6) var input_ao: texture_2d<u32>;
@group(0) @binding(7) var output_depth1: texture_storage_2d<r32float, write>;
@group(0) @binding(8) var output_depth2: texture_storage_2d<r32float, write>;
@group(0) @binding(9) var output_depth3: texture_storage_2d<r32float, write>;
@group(0) @binding(10) var output_depth4: texture_storage_2d<r32float, write>;
@group(0) @binding(11) var hilbert_lut: texture_2d<u32>;
fn sat(x:f32)->f32 { return clamp(x,0.0,1.0); }
fn sat4(x:vec4<f32>)->vec4<f32> { return clamp(x,vec4(0.0),vec4(1.0)); }
fn bounded(q:vec2<i32>,size:vec2<u32>)->vec2<i32> { return clamp(q,vec2(0),vec2<i32>(size)-1); }
fn mip_size(mip:u32)->vec2<u32> { return max(vec2(1u),p.size >> vec2(mip)); }
fn depth_at(q:vec2<i32>,mip:u32)->f32 {
    return textureLoad(input_depth,bounded(q,mip_size(mip)),i32(mip)).x;
}
fn position(uv:vec2<f32>,z:f32)->vec3<f32> { return vec3((p.ndc_mul*uv+p.ndc_add)*z,z); }
fn view_depth(q:vec2<u32>)->f32 {
    let d=textureLoad(raw_depth,vec2<i32>(min(q,p.size-1u)),0);
    var z=65504.0;
    if d>0.0 {
        z=p.depth_unpack.x/(p.depth_unpack.y-d);
    }
    // Upstream ClampDepth uses #ifdef XE_GTAO_USE_HALF_FLOAT_PRECISION,
    // even when the selected FP32 mode defines that macro to zero.
    return clamp(z,0.0,65504.0);
}
fn filter_depth(d:vec4<f32>)->f32 {
    let farthest=max(max(d.x,d.y),max(d.z,d.w));
    let radius=0.75*p.radius*1.457;
    let range=0.615*radius;
    let weights=sat4((vec4(farthest)-d)*(-1.0/range)+vec4(radius*(1.0-0.615)/range+1.0));
    return dot(weights,d)/dot(weights,vec4(1.0));
}
// A parent's second child per axis: 1, or 0 where the child mip is one texel
// across there and the second child clamps onto the first, as loads clamp.
fn second_child(parent:vec2<u32>,child_mip:u32)->vec2<u32> {
    return min(parent*2u+1u,mip_size(child_mip)-1u)-parent*2u;
}
var<workgroup> scratch_depths:array<array<f32,8>,8>;
// XeGTAO_PrefilterDepths16x16 for mips 0-3: each 8x8 group filters a 16x16
// tile. Mip 4 would be a fifth storage texture, past WebGPU's default four
// per stage, so prefilter_depth4 writes it.
@compute @workgroup_size(8,8)
fn prefilter_depths(@builtin(global_invocation_id) id:vec3<u32>,@builtin(local_invocation_id) group_thread_id:vec3<u32>) {
    // MIP 0: the full-resolution view-space depth.
    let base_coord=id.xy;
    let pix_coord=base_coord*2u;
    let depth0=view_depth(pix_coord+vec2(0u,0u));
    let depth1=view_depth(pix_coord+vec2(1u,0u));
    let depth2=view_depth(pix_coord+vec2(0u,1u));
    let depth3=view_depth(pix_coord+vec2(1u,1u));
    store_depth0(pix_coord+vec2(0u,0u),depth0);
    store_depth0(pix_coord+vec2(1u,0u),depth1);
    store_depth0(pix_coord+vec2(0u,1u),depth2);
    store_depth0(pix_coord+vec2(1u,1u),depth3);
    // MIP 1: view_depth clamps the children as loads do.
    let dm1=filter_depth(vec4(depth0,depth1,depth2,depth3));
    if all(base_coord<mip_size(1u)) {
        textureStore(output_depth1,base_coord,vec4(dm1,0.0,0.0,0.0));
    }
    scratch_depths[group_thread_id.x][group_thread_id.y]=dm1;
    workgroupBarrier();
    // MIP 2
    if all(group_thread_id.xy%vec2(2u)==vec2(0u)) && all(base_coord/2u<mip_size(2u)) {
        let dm2=filter_scratch(group_thread_id.xy,1u,second_child(base_coord/2u,1u));
        textureStore(output_depth2,base_coord/2u,vec4(dm2,0.0,0.0,0.0));
        scratch_depths[group_thread_id.x][group_thread_id.y]=dm2;
    }
    workgroupBarrier();
    // MIP 3
    if all(group_thread_id.xy%vec2(4u)==vec2(0u)) && all(base_coord/4u<mip_size(3u)) {
        let dm3=filter_scratch(group_thread_id.xy,2u,second_child(base_coord/4u,2u));
        textureStore(output_depth3,base_coord/4u,vec4(dm3,0.0,0.0,0.0));
    }
}
fn store_depth0(q:vec2<u32>,z:f32) {
    if all(q<p.size) {
        textureStore(output_depth0,q,vec4(z,0.0,0.0,0.0));
    }
}
// The children a mip-1+ scratch texel at `at` filters, `stride` apart.
fn filter_scratch(at:vec2<u32>,stride:u32,second:vec2<u32>)->f32 {
    let far=at+second*stride;
    let in_tl=scratch_depths[at.x][at.y];
    let in_tr=scratch_depths[far.x][at.y];
    let in_bl=scratch_depths[at.x][far.y];
    let in_br=scratch_depths[far.x][far.y];
    return filter_depth(vec4(in_tl,in_tr,in_bl,in_br));
}
// Mip 4 from mip 3 (input_depth), XeGTAO's last DepthMIPFilter step.
@compute @workgroup_size(8,8)
fn prefilter_depth4(@builtin(global_invocation_id) id:vec3<u32>) {
    if any(id.xy>=mip_size(4u)) { return; }
    let s=vec2<i32>(id.xy*2u);
    let size3=mip_size(3u);
    let d=vec4(textureLoad(input_depth,bounded(s,size3),0).x,textureLoad(input_depth,bounded(s+vec2(1,0),size3),0).x,textureLoad(input_depth,bounded(s+vec2(0,1),size3),0).x,textureLoad(input_depth,bounded(s+vec2(1,1),size3),0).x);
    textureStore(output_depth4,id.xy,vec4(filter_depth(d),0.0,0.0,0.0));
}
fn fast_acos(v:f32)->f32 {
    let x=abs(v);
    let root=bitcast<f32>(0x1fbd1df5i+(bitcast<i32>(1.0-x)>>1u));
    let result=(-0.156583*x+1.570796)*root;
    return select(3.141593-result,result,v>=0.0);
}
fn noise(q:vec2<u32>)->vec2<f32> {
    let index=textureLoad(hilbert_lut,q%vec2(HILBERT_WIDTH),0).x;
    return fract(vec2(0.5)+f32(index)*vec2(0.75487766624669276005,0.5698402909980532659114));
}
fn sample_depth(uv:vec2<f32>,level:f32)->f32 {
    let mip=u32(clamp(floor(level+0.5),0.0,4.0));
    let size=mip_size(mip);
    return depth_at(vec2<i32>(floor(uv*vec2<f32>(size))),mip);
}
@compute @workgroup_size(8,8)
fn main_pass(@builtin(global_invocation_id) id:vec3<u32>) {
    if any(id.xy>=p.size) { return; }
    let q=vec2<i32>(id.xy);
    var z=depth_at(q,0u);
    var edges=vec4(depth_at(q+vec2(-1,0),0u),depth_at(q+vec2(1,0),0u),depth_at(q+vec2(0,-1),0u),depth_at(q+vec2(0,1),0u))-vec4(z);
    let slopes=vec2(edges.y-edges.x,edges.w-edges.z)*0.5;
    edges=min(abs(edges),abs(edges+vec4(slopes.x,-slopes.x,slopes.y,-slopes.y)));
    edges=sat4(vec4(1.25)-edges/(z*0.011));
    let packed=dot(round(edges*2.9),vec4(64.0,16.0,4.0,1.0));
    z*=0.99999;
    let uv=(vec2<f32>(id.xy)+0.5)*p.pixel_size;
    let center=position(uv,z);
    let view_vec=normalize(-center);
    let world_normal=gbuffer_base_normal(textureLoad(normals,q,0));
    let rh_normal=(p.view*vec4(world_normal,0.0)).xyz;
    let normal=vec3(rh_normal.xy,-rh_normal.z);
    let radius=p.radius*1.457;
    let falloff_range=0.615*radius;
    let falloff_mul=-1.0/falloff_range;
    let falloff_add=radius*(1.0-0.615)/falloff_range+1.0;
    let screen_radius=radius/(z*p.ndc_mul.x*p.pixel_size.x);
    var visibility=sat((10.0-screen_radius)/100.0)*0.5;
    let local_noise=noise(id.xy);
    let min_s=1.3/screen_radius;
    let slices=min(p.slices,MOST_SLICES);
    let steps=min(p.steps,MOST_STEPS);
    for(var slice=0u;slice<slices;slice++) {
        let phi=(f32(slice)+local_noise.x)/f32(slices)*3.1415926535897932384626433832795;
        let direction=vec3(cos(phi),sin(phi),0.0);
        let omega=vec2(direction.x,-direction.y)*screen_radius;
        let ortho=direction-dot(direction,view_vec)*view_vec;
        let axis=normalize(cross(ortho,view_vec));
        let projected=normal-axis*dot(normal,axis);
        let sign_norm=sign(dot(ortho,projected));
        var projected_length=length(projected);
        let cos_norm=sat(dot(projected,view_vec)/projected_length);
        let n=sign_norm*fast_acos(cos_norm);
        let low=vec2(cos(n+1.57079632679489661923),cos(n-1.57079632679489661923));
        var horizon=low;
        for(var step=0u;step<steps;step++) {
            let step_noise=fract(local_noise.y+f32(slice+step*steps)*0.6180339887498948482);
            let sample_s=pow((f32(step)+step_noise)/f32(steps),2.0)+min_s;
            var offset=sample_s*omega;
            let mip=clamp(log2(length(offset))-3.30,0.0,5.0);
            offset=round(offset)*p.pixel_size;
            let uv0=uv+offset;
            let uv1=uv-offset;
            let delta0=position(uv0,sample_depth(uv0,mip))-center;
            let delta1=position(uv1,sample_depth(uv1,mip))-center;
            let dist0=length(delta0);
            let dist1=length(delta1);
            let weight=clamp(vec2(dist0,dist1)*falloff_mul+falloff_add,vec2(0.0),vec2(1.0));
            let sample_horizon=vec2(dot(delta0/dist0,view_vec),dot(delta1/dist1,view_vec));
            horizon=max(horizon,mix(low,sample_horizon,weight));
        }
        projected_length=mix(projected_length,1.0,0.05);
        let h0=-fast_acos(horizon.y);
        let h1=fast_acos(horizon.x);
        let arc0=(cos_norm+2.0*h0*sin(n)-cos(2.0*h0-n))/4.0;
        let arc1=(cos_norm+2.0*h1*sin(n)-cos(2.0*h1-n))/4.0;
        visibility+=projected_length*(arc0+arc1);
    }
    visibility=max(0.03,pow(visibility/f32(slices),2.2));
    textureStore(output_ao,q,vec4(pack_working(u32(sat(visibility/1.5)*255.0+0.5),u32(packed)),0u,0u,0u));
}
fn working_at(q:vec2<i32>)->u32 { return textureLoad(input_ao,bounded(q,p.size),0).x; }
// XeGTAO's R8 working visibility and R8 packed edges in one R32Uint word:
// the visibility in bits 0-7, the edges in bits 8-15.
const WORKING_EDGES_SHIFT: u32 = 8u;
fn pack_working(visibility:u32,packed_edges:u32)->u32 { return visibility|(packed_edges<<WORKING_EDGES_SHIFT); }
fn unpack_edges(word:u32)->vec4<f32> {
    let packed=(word>>WORKING_EDGES_SHIFT)&255u;
    return vec4<f32>((vec4(packed)>>vec4(6u,4u,2u,0u))&vec4(3u))/3.0;
}
fn unpack_visibility(words:vec4<u32>)->vec4<f32> { return vec4<f32>(words&vec4((1u<<WORKING_EDGES_SHIFT)-1u))/255.0; }
// One pixel of XeGTAO_Denoise's final pass from its edges (centre, left,
// right, top, bottom) and visibility (centre, then the cardinal neighbours
// left, right, top, bottom, then the corners top-left, top-right,
// bottom-left, bottom-right).
fn denoise_pixel(center:vec4<f32>,left:vec4<f32>,right:vec4<f32>,top:vec4<f32>,bottom:vec4<f32>,ao:f32,cardinal:vec4<f32>,corners:vec4<f32>)->u32 {
    var edges=center*vec4(left.y,right.x,top.w,bottom.z);
    let edginess=sat(1.5-dot(edges,vec4(1.0)))/1.5*0.5;
    edges=sat4(edges+edginess);
    let diagonal=0.425*vec4(edges.x*left.z+edges.z*top.x,edges.z*top.y+edges.y*right.z,edges.w*bottom.x+edges.x*left.w,edges.y*right.w+edges.w*bottom.y);
    var sum=ao*1.2;
    var weight=1.2;
    // Keep upstream accumulation order.
    for(var i=0u;i<4u;i++) { sum+=edges[i]*cardinal[i]; weight+=edges[i]; }
    for(var i=0u;i<4u;i++) { sum+=diagonal[i]*corners[i]; weight+=diagonal[i]; }
    // R8_UINT typed UAV conversion saturates. R32_UINT needs that conversion explicitly.
    return min(255u,u32((sum/weight)*1.5*255.0+0.5));
}
// XeGTAO_Denoise: each invocation filters two horizontally adjacent pixels,
// loading the edges and visibility they share once.
@compute @workgroup_size(8,8)
fn denoise(@builtin(global_invocation_id) id:vec3<u32>) {
    let pix_coord_base=vec2<i32>(id.xy*vec2(2u,1u));
    if any(vec2<u32>(pix_coord_base)>=p.size) { return; }
    let q=pix_coord_base;
    // The working terms in the rows above, at and below the pair, from one
    // left of it to one right.
    let above=vec4(working_at(q+vec2(-1,-1)),working_at(q+vec2(0,-1)),working_at(q+vec2(1,-1)),working_at(q+vec2(2,-1)));
    let middle=vec4(working_at(q+vec2(-1,0)),working_at(q),working_at(q+vec2(1,0)),working_at(q+vec2(2,0)));
    let below=vec4(working_at(q+vec2(-1,1)),working_at(q+vec2(0,1)),working_at(q+vec2(1,1)),working_at(q+vec2(2,1)));
    let edges_l=unpack_edges(middle.x);
    let edges_c0=unpack_edges(middle.y);
    let edges_c1=unpack_edges(middle.z);
    let edges_r=unpack_edges(middle.w);
    let edges_t0=unpack_edges(above.y);
    let edges_t1=unpack_edges(above.z);
    let edges_b0=unpack_edges(below.y);
    let edges_b1=unpack_edges(below.z);
    let ao_above=unpack_visibility(above);
    let ao_middle=unpack_visibility(middle);
    let ao_below=unpack_visibility(below);
    let first=denoise_pixel(edges_c0,edges_l,edges_c1,edges_t0,edges_b0,ao_middle.y,vec4(ao_middle.x,ao_middle.z,ao_above.y,ao_below.y),vec4(ao_above.x,ao_above.z,ao_below.x,ao_below.z));
    textureStore(output_ao,q,vec4(first,0u,0u,0u));
    if u32(q.x+1)<p.size.x {
        let second=denoise_pixel(edges_c1,edges_c0,edges_r,edges_t1,edges_b1,ao_middle.z,vec4(ao_middle.y,ao_middle.w,ao_above.z,ao_below.z),vec4(ao_above.y,ao_above.w,ao_below.y,ao_below.w));
        textureStore(output_ao,q+vec2(1,0),vec4(second,0u,0u,0u));
    }
}
