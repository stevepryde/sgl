// SMAA 1x Medium, color edge detection. Port of Three.js SMAANode.js.
// Copyright and permission notices: LICENSE-three.txt and LICENSE-smaa.txt.
@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var linear_sampler: sampler;
@group(0) @binding(2) var edges: texture_2d<f32>;
@group(0) @binding(3) var area: texture_2d<f32>;
@group(0) @binding(4) var search: texture_2d<f32>;
@group(0) @binding(5) var point_sampler: sampler;
@group(0) @binding(6) var weights: texture_2d<f32>;
@group(0) @binding(7) var<uniform> inverse_size: vec2<f32>;

struct Varyings {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) o0: vec4<f32>,
    @location(2) o1: vec4<f32>,
    @location(3) o2: vec4<f32>,
    @location(4) pixel: vec2<f32>,
}
fn fullscreen(index: u32) -> Varyings {
    // Same triangle and UVs as Three's QuadGeometry. Offsets must be computed
    // before interpolation: recomputing them per fragment changes searches at
    // discontinuous edges, even when the expressions are algebraically equal.
    let uv = array(vec2<f32>(0.0,-1.0),vec2<f32>(0.0,1.0),vec2<f32>(2.0,1.0))[index];
    var out: Varyings;
    out.position = vec4<f32>(uv * vec2<f32>(2.0,-2.0) + vec2<f32>(-1.0,1.0),0.0,1.0);
    out.uv = uv;
    out.pixel = uv / inverse_size;
    return out;
}
@vertex fn detect_vertex(@builtin(vertex_index) index: u32) -> Varyings {
    var out = fullscreen(index);
    out.o0 = out.uv.xyxy + inverse_size.xyxy * vec4<f32>(-1.0,0.0,0.0,-1.0);
    out.o1 = out.uv.xyxy + inverse_size.xyxy * vec4<f32>(1.0,0.0,0.0,1.0);
    out.o2 = out.uv.xyxy + inverse_size.xyxy * vec4<f32>(-2.0,0.0,0.0,-2.0);
    return out;
}
@vertex fn calculate_vertex(@builtin(vertex_index) index: u32) -> Varyings {
    var out = fullscreen(index);
    out.o0 = out.uv.xyxy + inverse_size.xyxy * vec4<f32>(-0.25,-0.125,1.25,-0.125);
    out.o1 = out.uv.xyxy + inverse_size.xyxy * vec4<f32>(-0.125,-0.25,-0.125,1.25);
    out.o2 = vec4<f32>(out.o0.xz,out.o1.yw) + (vec4<f32>(-2.0,2.0,-2.0,2.0)*inverse_size.xxyy)*8.0;
    return out;
}
@vertex fn blend_vertex(@builtin(vertex_index) index: u32) -> Varyings {
    var out = fullscreen(index);
    out.o1 = out.uv.xyxy + inverse_size.xyxy * vec4<f32>(1.0,0.0,0.0,1.0);
    return out;
}
fn color(uv: vec2<f32>) -> vec4<f32> {
    return textureSampleLevel(source, linear_sampler, uv, 0.0);
}
fn edge(uv: vec2<f32>) -> vec2<f32> {
    return textureSampleLevel(edges, linear_sampler, uv, 0.0).rg;
}
fn difference(a: vec3<f32>, b: vec3<f32>) -> f32 {
    let d = abs(a-b);
    return max(d.r, max(d.g,d.b));
}
@fragment fn detect(v: Varyings) -> @location(0) vec4<f32> {
    let uv = v.uv;
    let c = color(uv).rgb;
    let d = vec2<f32>(difference(c, color(v.o0.xy).rgb),
                      difference(c, color(v.o0.zw).rgb));
    var e = step(vec2<f32>(0.1), d);
    if dot(e,vec2<f32>(1.0)) == 0.0 { return vec4<f32>(0.0); }
    let right = difference(c,color(v.o1.xy).rgb);
    let bottom = difference(c,color(v.o1.zw).rgb);
    let left2 = difference(c,color(v.o2.xy).rgb);
    let top2 = difference(c,color(v.o2.zw).rgb);
    let largest = max(max(d.x,d.y),max(max(right,bottom),max(left2,top2)));
    e *= step(vec2<f32>(0.5*largest),d);
    return vec4<f32>(e,0.0,0.0);
}
fn search_length(e: vec2<f32>, bias: f32) -> f32 {
    return 255.0 * textureSampleLevel(search,point_sampler,vec2<f32>(bias+e.x*0.5,e.y),0.0).r;
}
// Keep upstream arithmetic sequencing; collecting these corrections changes
// subtexel lookup rounding and can flip the final neighborhood direction.
// Explicit fma preserves Three WebGPU's final correction and prevents the
// Metal compiler from reassociating px * 255 * the atlas sample differently.
fn search_left(start: vec2<f32>, end: f32, px: vec2<f32>) -> f32 {
    var coord = start;
    var e = vec2<f32>(0.0,1.0);
    for(var i=0; i<8; i++) {
        e = edge(coord);
        coord = coord - vec2<f32>(2.0,0.0) * px;
        if coord.x<=end || e.g<=0.8281 || e.r!=0.0 { break; }
    }
    coord.x += 0.25 * px.x;
    coord.x += px.x;
    coord.x += 2.0 * px.x;
    coord.x = fma(-px.x,search_length(e,0.0),coord.x);
    return coord.x;
}
fn search_right(start: vec2<f32>, end: f32, px: vec2<f32>) -> f32 {
    var coord = start;
    var e = vec2<f32>(0.0,1.0);
    for(var i=0; i<8; i++) {
        e = edge(coord);
        coord = coord + vec2<f32>(2.0,0.0) * px;
        if coord.x>=end || e.g<=0.8281 || e.r!=0.0 { break; }
    }
    coord.x -= 0.25 * px.x;
    coord.x -= px.x;
    coord.x -= 2.0 * px.x;
    coord.x = fma(px.x,search_length(e,0.5),coord.x);
    return coord.x;
}
fn search_up(start: vec2<f32>, end: f32, px: vec2<f32>) -> f32 {
    var coord = start;
    var e = vec2<f32>(1.0,0.0);
    for(var i=0; i<8; i++) {
        e = edge(coord);
        coord = coord + vec2<f32>(0.0,-2.0) * px;
        if coord.y<=end || e.r<=0.8281 || e.g!=0.0 { break; }
    }
    coord.y += 0.25 * px.y;
    coord.y += px.y;
    coord.y += 2.0 * px.y;
    coord.y = fma(-px.y,search_length(e.gr,0.0),coord.y);
    return coord.y;
}
fn search_down(start: vec2<f32>, end: f32, px: vec2<f32>) -> f32 {
    var coord = start;
    var e = vec2<f32>(1.0,0.0);
    for(var i=0; i<8; i++) {
        e = edge(coord);
        coord = coord - vec2<f32>(0.0,-2.0) * px;
        if coord.y>=end || e.r<=0.8281 || e.g!=0.0 { break; }
    }
    coord.y -= 0.25 * px.y;
    coord.y -= px.y;
    coord.y -= 2.0 * px.y;
    coord.y = fma(px.y,search_length(e.gr,0.5),coord.y);
    return coord.y;
}
fn area_weight(distance: vec2<f32>, e1: f32, e2: f32) -> vec2<f32> {
    // SMAA 1x uses subpixel index zero. The full upstream atlas is retained.
    let pixel = vec2<f32>(1.0/160.0,1.0/560.0);
    let coord = pixel*(16.0*round(4.0*vec2<f32>(e1,e2))+distance)+0.5*pixel;
    return textureSampleLevel(area,linear_sampler,coord,0.0).rg;
}
@fragment fn calculate(v: Varyings) -> @location(0) vec4<f32> {
    let px = inverse_size;
    let uv = v.uv;
    let e = edge(uv);
    let o0 = v.o0;
    let o1 = v.o1;
    let ends = v.o2;
    var w = vec4<f32>(0.0);
    if e.g>0.0 {
        let left = search_left(o0.xy,ends.x,px);
        let right = search_right(o0.zw,ends.y,px);
        let e1 = edge(vec2<f32>(left,o1.y)).r;
        let e2 = edge(vec2<f32>(right+px.x,o1.y)).r;
        let a = area_weight(sqrt(abs(vec2<f32>(left,right)/px.x-v.pixel.x)),e1,e2);
        w = vec4<f32>(a,w.ba);
    }
    if e.r>0.0 {
        let up = search_up(o1.xy,ends.z,px);
        let down = search_down(o1.zw,ends.w,px);
        let e1 = edge(vec2<f32>(o0.x,up)).g;
        let e2 = edge(vec2<f32>(o0.x,down+px.y)).g;
        let a = area_weight(sqrt(abs(vec2<f32>(up,down)/px.y-v.pixel.y)),e1,e2);
        w = vec4<f32>(w.rg,a);
    }
    return w;
}
@fragment fn blend(v: Varyings) -> @location(0) vec4<f32> {
    let px = inverse_size;
    let uv = v.uv;
    let center = textureSampleLevel(weights,linear_sampler,uv,0.0);
    let a = vec4<f32>(center.r,
        textureSampleLevel(weights,linear_sampler,v.o1.zw,0.0).g,
        center.b,textureSampleLevel(weights,linear_sampler,v.o1.xy,0.0).a);
    if dot(a,vec4<f32>(1.0))<0.00001 { return color(uv); }
    var offset = vec2<f32>(select(-a.b,a.a,a.a>a.b),select(-a.r,a.g,a.g>a.r));
    if abs(offset.x)>abs(offset.y) { offset.y=0.0; } else { offset.x=0.0; }
    return mix(color(uv),color(uv+sign(offset)*px),max(abs(offset.x),abs(offset.y)));
}
