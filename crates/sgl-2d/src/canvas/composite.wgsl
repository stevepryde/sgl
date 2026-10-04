// Lighting composite (R-6, PR-4): the Godot add-mode combine.
//
//   final = albedo × (canvas_modulate + light_accum)
//
// i.e. `base·modulate + Σ light·base` — the base scene modulated by the
// CanvasModulate color, plus every light's accumulated contribution
// multiplied by the base albedo.
//
// Two fragment entry points, one per `LightingSpace`; both write the 8-bit
// sRGB-encoded composited target the screen/UI channel then draws over
// unlit and the letterbox blit presents:
// - fs_main (Gamma, R-13 parity, D-8): the whole 2D pipeline runs in sRGB
//   ("gamma") space like Godot — the albedo target, the modulate color and
//   the accumulated light contributions are all gamma-space values, so the
//   combine is a direct multiply-add clamped exactly as Godot's 8-bit
//   framebuffer would. With the default white modulate and an empty
//   accumulation this is the identity, so unlit scenes pass through
//   unchanged.
// - fs_main_linear (Linear): the albedo target is half-float linear, the
//   modulate and the accumulation are linear, so the multiply-add happens in
//   linear and the clamped result is sRGB-encoded exactly once, here.

struct Uniforms {
    // CanvasModulate, as given (sRGB for fs_main, linear for fs_main_linear).
    modulate: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var albedo_tex: texture_2d<f32>;
@group(0) @binding(2) var accum_tex: texture_2d<f32>;

struct VsOut {
    @builtin(position) clip_pos: vec4<f32>,
};

// Fullscreen triangle; the targets match the render target size, so the
// fragment reads them at its own pixel coordinate.
@vertex
fn vs_main(@builtin(vertex_index) vid: u32) -> VsOut {
    var out: VsOut;
    let x = f32(i32(vid) / 2) * 4.0 - 1.0;
    let y = f32(i32(vid) & 1) * 4.0 - 1.0;
    out.clip_pos = vec4<f32>(x, y, 0.0, 1.0);
    return out;
}

// albedo × (modulate + accum) at this fragment's pixel, clamped to [0, 1].
fn combine(pix: vec2<i32>) -> vec3<f32> {
    let albedo = textureLoad(albedo_tex, pix, 0);
    let accum = textureLoad(accum_tex, pix, 0);
    let combined = albedo.rgb * (u.modulate.rgb + accum.rgb);
    return clamp(combined, vec3<f32>(0.0), vec3<f32>(1.0));
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return vec4<f32>(combine(vec2<i32>(in.clip_pos.xy)), 1.0);
}

// IEC 61966-2-1 encode (inverse of the blit's decode) for the linear path.
fn linear_to_srgb3(c: vec3<f32>) -> vec3<f32> {
    let lo = c * 12.92;
    let hi = 1.055 * pow(c, vec3<f32>(1.0 / 2.4)) - 0.055;
    return select(hi, lo, c <= vec3<f32>(0.0031308));
}

@fragment
fn fs_main_linear(in: VsOut) -> @location(0) vec4<f32> {
    return vec4<f32>(linear_to_srgb3(combine(vec2<i32>(in.clip_pos.xy))), 1.0);
}
