// Letterbox blit (PR-1): sample the offscreen scene target — which holds
// gamma-space (sRGB-encoded) values, like Godot's framebuffer (D-11) — and
// write it to the sRGB swapchain. The swapchain's *_Srgb format re-encodes
// on store, so the fragment decodes first; the round trip is value-preserving
// and this is the only place any transfer-function conversion happens.
// The fragment runs inside the letterbox viewport; the black bars come from
// the swapchain clear outside it. Nearest sampling keeps the logical image
// crisp.

struct VsOut {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@group(0) @binding(0) var scene_tex: texture_2d<f32>;
@group(0) @binding(1) var scene_samp: sampler;

// Fullscreen triangle; map clip -> uv (y flipped: uv row 0 = top).
@vertex
fn vs_main(@builtin(vertex_index) vid: u32) -> VsOut {
    var out: VsOut;
    let x = f32(i32(vid) / 2) * 4.0 - 1.0;
    let y = f32(i32(vid) & 1) * 4.0 - 1.0;
    out.clip_pos = vec4<f32>(x, y, 0.0, 1.0);
    // Clip (-1..1, y up) -> uv (0..1, y down).
    out.uv = vec2<f32>(x * 0.5 + 0.5, 0.5 - y * 0.5);
    return out;
}

// IEC 61966-2-1 decode (inverse of the swapchain's encode-on-store).
fn srgb_to_linear3(c: vec3<f32>) -> vec3<f32> {
    let lo = c / 12.92;
    let hi = pow((c + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4));
    return select(hi, lo, c <= vec3<f32>(0.04045));
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let scene = textureSample(scene_tex, scene_samp, in.uv);
    return vec4<f32>(srgb_to_linear3(scene.rgb), 1.0);
}

// WebGPU canvas formats are non-sRGB. The scene already stores the exact
// gamma-space values we want the browser to present, so write them unchanged.
@fragment
fn fs_main_unorm(in: VsOut) -> @location(0) vec4<f32> {
    let scene = textureSample(scene_tex, scene_samp, in.uv);
    return vec4<f32>(scene.rgb, 1.0);
}
