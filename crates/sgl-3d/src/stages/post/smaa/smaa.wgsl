// SMAA 1x, color edge detection, at the preset the pipeline constants set.
// Port of Three.js SMAANode.js (SMAA 1x Medium). The presets are SMAA.hlsl's
// (iryoku/smaa 27fad0b, SMAA_PRESET_*): each one's threshold and search
// steps, and for High and Ultra its diagonal and corner detection
// (SMAADecodeDiagBilinearAccess, SMAASearchDiag1 and 2, SMAAAreaDiag,
// SMAACalculateDiagWeights, SMAADetectHorizontalCornerPattern and
// SMAADetectVerticalCornerPattern), translated with Bevy 9d12036's WGSL port
// of them (crates/bevy_anti_alias/src/smaa/smaa.wesl, MIT OR Apache-2.0,
// src/LICENSE-bevy.txt) as a guide. Changes: SMAA 1x's zero subsample
// offsets are left out, and the corners take Three's search ends and
// distances, rounded as SMAA.hlsl rounds its own.
// Copyright and permission notices: LICENSE-three.txt and LICENSE-smaa.txt.

// SMAA_THRESHOLD and SMAA_MAX_SEARCH_STEPS; diagonal and corner detection
// run where SMAA_DISABLE_DIAG_DETECTION and SMAA_DISABLE_CORNER_DETECTION
// are not defined, with SMAA_MAX_SEARCH_STEPS_DIAG and SMAA_CORNER_ROUNDING.
override SMAA_THRESHOLD: f32;
override SMAA_MAX_SEARCH_STEPS: i32;
override SMAA_DIAG_DETECTION: bool;
override SMAA_MAX_SEARCH_STEPS_DIAG: i32;
override SMAA_CORNER_DETECTION: bool;
override SMAA_CORNER_ROUNDING: f32;
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
    out.o2 = vec4<f32>(out.o0.xz,out.o1.yw) + (vec4<f32>(-2.0,2.0,-2.0,2.0)*inverse_size.xxyy)*f32(SMAA_MAX_SEARCH_STEPS);
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
    var e = step(vec2<f32>(SMAA_THRESHOLD), d);
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
    for(var i=0; i<SMAA_MAX_SEARCH_STEPS; i++) {
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
    for(var i=0; i<SMAA_MAX_SEARCH_STEPS; i++) {
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
    for(var i=0; i<SMAA_MAX_SEARCH_STEPS; i++) {
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
    for(var i=0; i<SMAA_MAX_SEARCH_STEPS; i++) {
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
// SMAADecodeDiagBilinearAccess: the two edges a bilinear fetch at a 0.25
// offset blends, red to 0.25 or 1 and green to 0.75 or 1, as 0 or 1.
fn decode_diag_bilinear_access_2(blended: vec2<f32>) -> vec2<f32> {
    var e = blended;
    e.r = e.r * abs(5.0 * e.r - 5.0 * 0.75);
    return round(e);
}
fn decode_diag_bilinear_access_4(blended: vec4<f32>) -> vec4<f32> {
    var e = blended;
    let rb = e.rb * abs(5.0 * e.rb - 5.0 * 0.75);
    e.r = rb.x;
    e.b = rb.y;
    return round(e);
}
// SMAASearchDiag1: steps along `dir` while both edges continue the line;
// returns the steps taken and whether the last fetch held both, and the
// last fetch's edges in `e`.
fn search_diag_1(uv: vec2<f32>, dir: vec2<f32>, e: ptr<function, vec2<f32>>) -> vec2<f32> {
    var coord = vec4<f32>(uv, -1.0, 1.0);
    let t = vec3<f32>(inverse_size, 1.0);
    while coord.z < f32(SMAA_MAX_SEARCH_STEPS_DIAG - 1) && coord.w > 0.9 {
        coord = vec4<f32>(t * vec3<f32>(dir, 1.0) + coord.xyz, coord.w);
        *e = edge(coord.xy);
        coord.w = dot(*e, vec2<f32>(0.5));
    }
    return coord.zw;
}
// SMAASearchDiag2: as search_diag_1 for the other diagonal, fetching the
// pixel's top edge and its right neighbour's left edge in one bilinear
// access.
fn search_diag_2(uv: vec2<f32>, dir: vec2<f32>, e: ptr<function, vec2<f32>>) -> vec2<f32> {
    var coord = vec4<f32>(uv, -1.0, 1.0);
    coord.x += 0.25 * inverse_size.x;
    let t = vec3<f32>(inverse_size, 1.0);
    while coord.z < f32(SMAA_MAX_SEARCH_STEPS_DIAG - 1) && coord.w > 0.9 {
        coord = vec4<f32>(t * vec3<f32>(dir, 1.0) + coord.xyz, coord.w);
        *e = decode_diag_bilinear_access_2(edge(coord.xy));
        coord.w = dot(*e, vec2<f32>(0.5));
    }
    return coord.zw;
}
// SMAAAreaDiag: the areas for a diagonal of `distance` with crossing edges
// `e`, from the atlas's diagonal half (SMAA_AREATEX_MAX_DISTANCE_DIAG 20).
fn area_diag(distance: vec2<f32>, e: vec2<f32>) -> vec2<f32> {
    let pixel = vec2<f32>(1.0/160.0,1.0/560.0);
    var coord = pixel*(20.0*e+distance)+0.5*pixel;
    coord.x += 0.5;
    return textureSampleLevel(area,linear_sampler,coord,0.0).rg;
}
// SMAACalculateDiagWeights: the weights of the diagonal lines through a
// pixel with edge `e` at its north.
fn calculate_diag_weights(uv: vec2<f32>, e: vec2<f32>) -> vec2<f32> {
    let px = inverse_size;
    var weights = vec2<f32>(0.0);
    var d = vec4<f32>(0.0);
    var end = vec2<f32>(0.0);
    if e.r > 0.0 {
        let xz = search_diag_1(uv, vec2<f32>(-1.0, 1.0), &end);
        d.x = xz.x + f32(end.y > 0.9);
        d.z = xz.y;
    }
    let yw = search_diag_1(uv, vec2<f32>(1.0, -1.0), &end);
    d.y = yw.x;
    d.w = yw.y;
    if d.x + d.y > 2.0 {
        // The crossing edges, two at a time.
        let coords = vec4<f32>(-d.x + 0.25, d.x, d.y, -d.y - 0.25) * px.xyxy + uv.xyxy;
        let c = decode_diag_bilinear_access_4(vec4<f32>(
            textureSampleLevel(edges, linear_sampler, coords.xy, 0.0, vec2<i32>(-1, 0)).rg,
            textureSampleLevel(edges, linear_sampler, coords.zw, 0.0, vec2<i32>(1, 0)).rg)).yxwz;
        // Each side's crossing edges as one value, none where the line's
        // end was not found.
        let cc = select(2.0 * c.xz + c.yw, vec2<f32>(0.0), d.zw >= vec2<f32>(0.9));
        weights += area_diag(d.xy, cc);
    }
    let xz = search_diag_2(uv, vec2<f32>(-1.0, -1.0), &end);
    d.x = xz.x;
    d.z = xz.y;
    if textureSampleLevel(edges, linear_sampler, uv, 0.0, vec2<i32>(1, 0)).r > 0.0 {
        let yw = search_diag_2(uv, vec2<f32>(1.0, 1.0), &end);
        d.y = yw.x + f32(end.y > 0.9);
        d.w = yw.y;
    } else {
        d.y = 0.0;
        d.w = 0.0;
    }
    if d.x + d.y > 2.0 {
        let coords = vec4<f32>(-d.x, -d.x, d.y, d.y) * px.xyxy + uv.xyxy;
        let c = vec4<f32>(
            textureSampleLevel(edges, linear_sampler, coords.xy, 0.0, vec2<i32>(-1, 0)).g,
            textureSampleLevel(edges, linear_sampler, coords.xy, 0.0, vec2<i32>(0, -1)).r,
            textureSampleLevel(edges, linear_sampler, coords.zw, 0.0, vec2<i32>(1, 0)).gr);
        let cc = select(2.0 * c.xz + c.yw, vec2<f32>(0.0), d.zw >= vec2<f32>(0.9));
        weights += area_diag(d.xy, cc).gr;
    }
    return weights;
}
// SMAADetectHorizontalCornerPattern: less blending at a horizontal line's
// sharp corners, at its ends `coords` (left in xy, right in zw), `d` pixels
// away.
fn horizontal_corners(weights: vec2<f32>, coords: vec4<f32>, d: vec2<f32>) -> vec2<f32> {
    let left_right = step(d.xy, d.yx);
    // Less for pixels in the centre of a line.
    let rounding = (1.0 - SMAA_CORNER_ROUNDING / 100.0) * left_right / (left_right.x + left_right.y);
    var factor = vec2<f32>(1.0);
    factor.x -= rounding.x * textureSampleLevel(edges, linear_sampler, coords.xy, 0.0, vec2<i32>(0, 1)).r;
    factor.x -= rounding.y * textureSampleLevel(edges, linear_sampler, coords.zw, 0.0, vec2<i32>(1, 1)).r;
    factor.y -= rounding.x * textureSampleLevel(edges, linear_sampler, coords.xy, 0.0, vec2<i32>(0, -2)).r;
    factor.y -= rounding.y * textureSampleLevel(edges, linear_sampler, coords.zw, 0.0, vec2<i32>(1, -2)).r;
    return weights * saturate(factor);
}
// SMAADetectVerticalCornerPattern: as horizontal_corners for a vertical
// line, top in xy and bottom in zw.
fn vertical_corners(weights: vec2<f32>, coords: vec4<f32>, d: vec2<f32>) -> vec2<f32> {
    let left_right = step(d.xy, d.yx);
    let rounding = (1.0 - SMAA_CORNER_ROUNDING / 100.0) * left_right / (left_right.x + left_right.y);
    var factor = vec2<f32>(1.0);
    factor.x -= rounding.x * textureSampleLevel(edges, linear_sampler, coords.xy, 0.0, vec2<i32>(1, 0)).g;
    factor.x -= rounding.y * textureSampleLevel(edges, linear_sampler, coords.zw, 0.0, vec2<i32>(1, 1)).g;
    factor.y -= rounding.x * textureSampleLevel(edges, linear_sampler, coords.xy, 0.0, vec2<i32>(-2, 0)).g;
    factor.y -= rounding.y * textureSampleLevel(edges, linear_sampler, coords.zw, 0.0, vec2<i32>(-2, 1)).g;
    return weights * saturate(factor);
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
        if SMAA_DIAG_DETECTION {
            // Diagonals have north and west edges, so the north finds them;
            // a diagonal takes priority over horizontal and vertical lines.
            let diagonal = calculate_diag_weights(uv, e);
            if diagonal.r + diagonal.g != 0.0 {
                return vec4<f32>(diagonal, 0.0, 0.0);
            }
        }
        let left = search_left(o0.xy,ends.x,px);
        let right = search_right(o0.zw,ends.y,px);
        let e1 = edge(vec2<f32>(left,o1.y)).r;
        let e2 = edge(vec2<f32>(right+px.x,o1.y)).r;
        let d = abs(vec2<f32>(left,right)/px.x-v.pixel.x);
        let a = area_weight(sqrt(d),e1,e2);
        w = vec4<f32>(a,w.ba);
        if SMAA_CORNER_DETECTION {
            w = vec4<f32>(horizontal_corners(w.rg, vec4<f32>(left, uv.y, right, uv.y), round(d)), w.ba);
        }
    }
    if e.r>0.0 {
        let up = search_up(o1.xy,ends.z,px);
        let down = search_down(o1.zw,ends.w,px);
        let e1 = edge(vec2<f32>(o0.x,up)).g;
        let e2 = edge(vec2<f32>(o0.x,down+px.y)).g;
        let d = abs(vec2<f32>(up,down)/px.y-v.pixel.y);
        let a = area_weight(sqrt(d),e1,e2);
        w = vec4<f32>(w.rg,a);
        if SMAA_CORNER_DETECTION {
            w = vec4<f32>(w.rg, vertical_corners(w.ba, vec4<f32>(uv.x, up, uv.x, down), round(d)));
        }
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
