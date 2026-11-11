// DiligentFX SSAO_ComputeTemporalAccumulation.fx, f26cfe5b901bf180c4a3c9bbd4d5df0b96536d4b.
// Apache-2.0; see LICENSE-diligent.txt and ambient_occlusion/README.md.
struct Params {
    size: vec2<u32>,
    jitter_delta: vec2<f32>,
    depth_unpack: vec2<f32>,
    reset: u32,
    padding: u32,
}
@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var current_ao: texture_2d<u32>;
@group(0) @binding(2) var depth: texture_depth_2d;
@group(0) @binding(3) var motion: texture_2d<f32>;
// Visibility, accumulated frame count, positive view-space depth, unused.
@group(0) @binding(4) var previous: texture_2d<f32>;
@group(0) @binding(5) var history: texture_storage_2d<rgba32float, write>;
@group(0) @binding(6) var output: texture_storage_2d<r32uint, write>;

// DiligentFX's bilinear footprint and 3x3 variance neighborhood (AR-12).
const REPROJECTION_TAPS: u32 = 4u;
const STATISTIC_TAPS: u32 = 9u;
const MAX_HISTORY: f32 = 16.0;
const DEPTH_THRESHOLD: f32 = 0.01;

fn current_at(q: vec2<i32>) -> f32 {
    return f32(textureLoad(current_ao, clamp(q, vec2(0), vec2<i32>(p.size)-1), 0).x)/255.0;
}

@compute @workgroup_size(8,8)
fn resolve(@builtin(global_invocation_id) id: vec3<u32>) {
    if any(id.xy >= p.size) { return; }
    let q = vec2<i32>(id.xy);
    let d = textureLoad(depth, q, 0);
    let curr = current_at(q);
    var value = curr;
    var count = 1.0;
    var linear_depth = 0.0;
    if d > 0.0 {
        linear_depth = p.depth_unpack.x/(d+p.depth_unpack.y);
        let uv = (vec2<f32>(id.xy)+0.5)/vec2<f32>(p.size);
        let velocity = textureLoad(motion, q, 0);
        let previous_uv = uv-velocity.xy+p.jitter_delta;
        let position = previous_uv*vec2<f32>(p.size)-0.5;
        let base = vec2<i32>(floor(position));
        let f = fract(position);
        let weights = vec4((1.0-f.x)*(1.0-f.y),f.x*(1.0-f.y),(1.0-f.x)*f.y,f.x*f.y);
        let offsets = array<vec2<i32>,4>(vec2(0,0),vec2(1,0),vec2(0,1),vec2(1,1));
        // The geometry's depth motion includes object movement, unlike
        // reconstructing the current world point in the previous camera.
        let expected_depth = linear_depth+velocity.z;
        var total = 0.0;
        var accumulated = vec2(0.0);
        if p.reset == 0u && velocity.w > 0.0 && expected_depth > 0.0 {
            for(var i=0u;i<REPROJECTION_TAPS;i++) {
                let at = base+offsets[i];
                if all(at>=vec2(0)) && all(at<vec2<i32>(p.size)) {
                    let old = textureLoad(previous,at,0);
                    if old.z>0.0 && abs(1.0-expected_depth/old.z)<DEPTH_THRESHOLD {
                        accumulated += vec2(old.x,min(old.y+1.0,MAX_HISTORY))*weights[i];
                        total += weights[i];
                    }
                }
            }
        }
        if total>0.01 {
            accumulated /= total;
            var moments = vec2(0.0);
            for(var i=0u;i<STATISTIC_TAPS;i++) {
                let sample = current_at(q+vec2(i32(i/3u)-1,i32(i%3u)-1));
                moments += vec2(sample,sample*sample);
            }
            moments /= 9.0;
            let deviation = sqrt(max(0.0,moments.y-moments.x*moments.x));
            let motion_factor = clamp(1.025-length(velocity.xy*vec2(f32(p.size.x)/f32(p.size.y),1.0))*128.0,0.0,1.0);
            let gamma = mix(0.5,2.5,motion_factor*motion_factor);
            let inside = abs(accumulated.x-moments.x)<gamma*deviation;
            count = select(max(1.0,motion_factor*accumulated.y),accumulated.y,inside);
            value = mix(accumulated.x,curr,1.0/count);
        }
    }
    textureStore(history,q,vec4(value,count,linear_depth,0.0));
    textureStore(output,q,vec4(u32(clamp(value,0.0,1.0)*255.0+0.5),0u,0u,0u));
}
