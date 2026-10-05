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
    mip: u32,
    padding: vec2<u32>,
}
// The most slices and steps a pixel takes, XeGTAO's Ultra preset, so its
// loops end whatever the parameters say.
const MOST_SLICES: u32 = 9u;
const MOST_STEPS: u32 = 3u;
@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var raw_depth: texture_depth_2d;
@group(0) @binding(2) var input_depth: texture_2d<f32>;
@group(0) @binding(3) var output_depth: texture_storage_2d<r32float, write>;
@group(0) @binding(4) var normals: texture_2d<f32>;
@group(0) @binding(5) var output_ao: texture_storage_2d<r32uint, write>;
@group(0) @binding(6) var output_edges: texture_storage_2d<r32float, write>;
@group(0) @binding(7) var input_ao: texture_2d<u32>;
@group(0) @binding(8) var input_edges: texture_2d<f32>;
fn sat(x:f32)->f32 { return clamp(x,0.0,1.0); }
fn sat4(x:vec4<f32>)->vec4<f32> { return clamp(x,vec4(0.0),vec4(1.0)); }
fn bounded(q:vec2<i32>,size:vec2<u32>)->vec2<i32> { return clamp(q,vec2(0),vec2<i32>(size)-1); }
fn depth_at(q:vec2<i32>,mip:u32)->f32 {
    return textureLoad(input_depth,bounded(q,max(vec2(1u),p.size >> vec2(mip))),i32(mip)).x;
}
fn position(uv:vec2<f32>,z:f32)->vec3<f32> { return vec3((p.ndc_mul*uv+p.ndc_add)*z,z); }
fn filter_depth(d:vec4<f32>)->f32 {
    let farthest=max(max(d.x,d.y),max(d.z,d.w));
    let radius=0.75*p.radius*1.457;
    let range=0.615*radius;
    let weights=sat4((vec4(farthest)-d)*(-1.0/range)+vec4(radius*(1.0-0.615)/range+1.0));
    return dot(weights,d)/dot(weights,vec4(1.0));
}
@compute @workgroup_size(8,8)
fn prefilter(@builtin(global_invocation_id) id:vec3<u32>) {
    let size=max(vec2(1u),p.size >> vec2(p.mip));
    if any(id.xy>=size) { return; }
    let q=vec2<i32>(id.xy);
    if p.mip==0u {
        let d=textureLoad(raw_depth,q,0);
        var z=65504.0;
        if d>0.0 {
            z=p.depth_unpack.x/(p.depth_unpack.y-d);
        }
        // Upstream ClampDepth uses #ifdef XE_GTAO_USE_HALF_FLOAT_PRECISION,
        // even when the selected FP32 mode defines that macro to zero.
        textureStore(output_depth,q,vec4(clamp(z,0.0,65504.0),0.0,0.0,0.0));
    } else {
        let s=q*2;
        let m=p.mip-1u;
        let size_in=max(vec2(1u),p.size >> vec2(m));
        let d=vec4(textureLoad(input_depth,bounded(s,size_in),0).x,textureLoad(input_depth,bounded(s+vec2(1,0),size_in),0).x,textureLoad(input_depth,bounded(s+vec2(0,1),size_in),0).x,textureLoad(input_depth,bounded(s+vec2(1,1),size_in),0).x);
        textureStore(output_depth,q,vec4(filter_depth(d),0.0,0.0,0.0));
    }
}
fn fast_acos(v:f32)->f32 {
    let x=abs(v);
    let root=bitcast<f32>(0x1fbd1df5i+(bitcast<i32>(1.0-x)>>1u));
    let result=(-0.156583*x+1.570796)*root;
    return select(3.141593-result,result,v>=0.0);
}
fn noise(q:vec2<u32>)->vec2<f32> {
    var pos=q;
    var index=0u;
    for(var level=32u;level>0u;level/=2u) {
        let rx=select(0u,1u,(pos.x&level)>0u);
        let ry=select(0u,1u,(pos.y&level)>0u);
        index+=level*level*((3u*rx)^ry);
        if ry==0u {
            if rx==1u { pos=vec2(63u)-pos; }
            pos=pos.yx;
        }
    }
    return fract(vec2(0.5)+f32(index)*vec2(0.75487766624669276005,0.5698402909980532659114));
}
fn sample_depth(uv:vec2<f32>,level:f32)->f32 {
    let mip=u32(clamp(floor(level+0.5),0.0,4.0));
    let size=max(vec2(1u),p.size>>vec2(mip));
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
    textureStore(output_edges,q,vec4(packed/255.0,0.0,0.0,0.0));
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
    textureStore(output_ao,q,vec4(u32(sat(visibility/1.5)*255.0+0.5),0u,0u,0u));
}
fn edge_at(q:vec2<i32>)->vec4<f32> {
    let packed=u32(textureLoad(input_edges,bounded(q,p.size),0).x*255.5);
    return vec4<f32>((vec4(packed)>>vec4(6u,4u,2u,0u))&vec4(3u))/3.0;
}
fn ao_at(q:vec2<i32>)->f32 { return f32(textureLoad(input_ao,bounded(q,p.size),0).x)/255.0; }
@compute @workgroup_size(8,8)
fn denoise(@builtin(global_invocation_id) id:vec3<u32>) {
    if any(id.xy>=p.size) { return; }
    let q=vec2<i32>(id.xy);
    let left=edge_at(q+vec2(-1,0));
    let right=edge_at(q+vec2(1,0));
    let top=edge_at(q+vec2(0,-1));
    let bottom=edge_at(q+vec2(0,1));
    var edges=edge_at(q)*vec4(left.y,right.x,top.w,bottom.z);
    let edginess=sat(1.5-dot(edges,vec4(1.0)))/1.5*0.5;
    edges=sat4(edges+edginess);
    let diagonal=0.425*vec4(edges.x*left.z+edges.z*top.x,edges.z*top.y+edges.y*right.z,edges.w*bottom.x+edges.x*left.w,edges.y*right.w+edges.w*bottom.y);
    var sum=ao_at(q)*1.2;
    var weight=1.2;
    let cardinal=vec4(ao_at(q+vec2(-1,0)),ao_at(q+vec2(1,0)),ao_at(q+vec2(0,-1)),ao_at(q+vec2(0,1)));
    let corners=vec4(ao_at(q+vec2(-1,-1)),ao_at(q+vec2(1,-1)),ao_at(q+vec2(-1,1)),ao_at(q+vec2(1,1)));
    // Keep upstream accumulation order.
    for(var i=0u;i<4u;i++) { sum+=edges[i]*cardinal[i]; weight+=edges[i]; }
    for(var i=0u;i<4u;i++) { sum+=diagonal[i]*corners[i]; weight+=diagonal[i]; }
    // R8_UINT typed UAV conversion saturates. R32_UINT needs that conversion explicitly.
    textureStore(output_ao,q,vec4(min(255u,u32((sum/weight)*1.5*255.0+0.5)),0u,0u,0u));
}
