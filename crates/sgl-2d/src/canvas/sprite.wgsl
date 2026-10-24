// Instanced sprite shader (AR-6, R-6).
//
// A static unit quad (0,0)..(1,1) is expanded per-instance into a world-space
// textured quad by a 2x3 affine. The fragment stage samples the bound page
// (nearest or linear, `SpritePass::set_filter`), applies the modulate color,
// and **premultiplies alpha in-shader** (rgb *= a) so the pipeline's
// (src = One, dst = OneMinusSrcAlpha) blend is correct for straight-alpha
// PNGs.
//
// Three fragment entry points share the vertex stage:
// - fs_main: single-target (the screen/UI channel — bypasses lighting).
// - fs_scene: the world channel's MRT scene pass (R-6/PR-4). Besides the
//   albedo it writes:
//   * a screen-space **normal** target: the sprite's normal map decoded,
//     y flipped (normal maps are +G = up, screen space is y-down), re-encoded
//     to [0,1] and premultiplied by alpha — so translucent sprites lerp their
//     normal over what's below. Sprites without a normal map write the flat
//     +Z encoding.
//   * a **mask** target: rgb = the sprite's light-mask layers (1/2/4) and
//     a = the "has normal map" weight, both premultiplied by alpha — giving
//     the light pass fractional per-pixel layer coverage and normal weight.
//   Limitation (documented): normals are not rotated by sprite rotation nor
//   mirrored by flips — no normal-mapped sprite in the game rotates or flips
//   (tiles and the menu title background only, PR-4/PR-11).
// - fs_scene_linear: fs_scene with the page texel decoded sRGB → linear
//   before the modulate (the `LightingSpace::Linear` scene pipeline, whose
//   albedo target is half-float). The modulate is taken as linear; the
//   normal and mask outputs are identical.
//
// Flips are folded into the per-instance UV rect on the CPU (uv_min/uv_max
// may arrive swapped per axis), so there is no flip attribute here.
//
// Every page sample goes through `page_uv`: the interpolated UV clamped to
// half a texel inside the instance's UV rect. Interior fragments sample
// exactly where they would unclamped (a 1:1 sprite stays texel-exact), while
// an edge fragment under linear sampling can never blend in the
// neighbouring atlas entry or padding. Under nearest sampling the clamp
// never changes which texel is read.
//
// Shapes: an instance with shape.x > 0 is a signed-distance rounded rect
// instead of a textured quad (`shape_rgba`). Its UV rect carries the quad's
// local pixel extent (-e..e, centered), so `uv` interpolates to the local
// point; shape = (half width, half height, corner radius, border width) in
// those pixels. The edge coverage ramp is one target pixel wide (screen
// derivatives of the distance), the fill is `color`, the inner border band
// `border_color`, both premultiplied like the textured path. Every entry
// point samples and takes derivatives unconditionally (uniform control
// flow) and only then selects, so textured instances are unchanged. On the
// world channel a shape writes the flat normal and its light mask.

struct Camera {
    view_proj: mat4x4<f32>,
};

@group(0) @binding(0) var<uniform> camera: Camera;

@group(1) @binding(0) var page_tex: texture_2d<f32>;
@group(1) @binding(1) var page_samp: sampler;
// The page's companion normal map (same placement as the diffuse page; a
// 1×1 dummy when the page has no normals — only sampled with misc.w = 0).
@group(1) @binding(2) var page_normal_tex: texture_2d<f32>;

struct VertexInput {
    // Static unit-quad corner, in [0,1]^2.
    @location(0) corner: vec2<f32>,
};

struct InstanceInput {
    // Affine columns (rotation * size) + translation, in the channel's camera
    // coordinates (pixels; or the world camera's units on the world channel).
    @location(1) model_x: vec2<f32>,
    @location(2) model_y: vec2<f32>,
    @location(3) translation: vec2<f32>,
    // UV sub-rect in normalized page coords (either end may be the smaller
    // one when the sprite is flipped).
    @location(4) uv_min: vec2<f32>,
    @location(5) uv_max: vec2<f32>,
    // RGBA modulate (straight alpha), in the pipeline's color space.
    @location(6) color: vec4<f32>,
    // rgb = light-mask layer bits (1/2/4), w = has-normal-map flag (R-6).
    @location(7) misc: vec4<f32>,
    // Shape instances: half size, radius, border (zero for sprites).
    @location(8) shape: vec4<f32>,
    // Shape border RGBA (straight alpha).
    @location(9) border_color: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) misc: vec4<f32>,
    @location(3) @interpolate(flat) shape: vec4<f32>,
    @location(4) @interpolate(flat) border_color: vec4<f32>,
    // The instance's UV rect (uv_min, uv_max), for `page_uv`.
    @location(5) @interpolate(flat) uv_rect: vec4<f32>,
};

@vertex
fn vs_main(vert: VertexInput, inst: InstanceInput) -> VertexOutput {
    let local = vert.corner;
    let world = vec2<f32>(
        inst.model_x.x * local.x + inst.model_y.x * local.y + inst.translation.x,
        inst.model_x.y * local.x + inst.model_y.y * local.y + inst.translation.y,
    );

    var out: VertexOutput;
    out.clip_pos = camera.view_proj * vec4<f32>(world, 0.0, 1.0);
    out.uv = mix(inst.uv_min, inst.uv_max, local);
    out.color = inst.color;
    out.misc = inst.misc;
    out.shape = inst.shape;
    out.border_color = inst.border_color;
    out.uv_rect = vec4<f32>(inst.uv_min, inst.uv_max);
    return out;
}

// The page UV to sample: `in.uv` kept half a texel inside the instance's UV
// rect (whose ends may be swapped by flips).
fn page_uv(in: VertexOutput) -> vec2<f32> {
    let half = 0.5 / vec2<f32>(textureDimensions(page_tex));
    let lo = min(in.uv_rect.xy, in.uv_rect.zw);
    let hi = max(in.uv_rect.xy, in.uv_rect.zw);
    return clamp(in.uv, lo + half, hi - half);
}

// Premultiplied color of a shape instance at its local point `in.uv`.
// Called in uniform control flow (it takes derivatives) for every instance;
// the result is only used when `in.shape.x > 0`.
fn shape_rgba(in: VertexOutput) -> vec4<f32> {
    let half = in.shape.xy;
    let r = clamp(in.shape.z, 0.0, min(half.x, half.y));
    let q = abs(in.uv) - half + vec2<f32>(r);
    let d = length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0) - r;
    // Local pixels per target pixel along the distance gradient.
    let aa = max(length(vec2<f32>(dpdx(d), dpdy(d))), 1e-6);
    let outer = clamp(0.5 - d / aa, 0.0, 1.0);
    let b = in.shape.w;
    let inner = select(1.0, clamp(0.5 - (d + b) / aa, 0.0, 1.0), b > 0.0);
    let fill = vec4<f32>(in.color.rgb * in.color.a, in.color.a);
    let edge = vec4<f32>(in.border_color.rgb * in.border_color.a, in.border_color.a);
    return mix(edge, fill, inner) * outer;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let texel = textureSample(page_tex, page_samp, page_uv(in));
    let shape = shape_rgba(in);
    if in.shape.x > 0.0 {
        return shape;
    }
    var rgba = texel * in.color;
    // Premultiply alpha for the (One, OneMinusSrcAlpha) blend.
    return vec4<f32>(rgba.rgb * rgba.a, rgba.a);
}

// --- World-channel MRT scene pass (R-6) --------------------------------------

struct SceneOutput {
    @location(0) albedo: vec4<f32>,
    @location(1) normal: vec4<f32>,
    @location(2) mask: vec4<f32>,
};

// The MRT outputs for a page `texel` sampled at `uv` (already in the
// pipeline's color space): premultiplied albedo, screen-space normal, light
// mask.
fn scene_output(in: VertexOutput, uv: vec2<f32>, texel: vec4<f32>) -> SceneOutput {
    var rgba = texel * in.color;
    let a = rgba.a;

    var out: SceneOutput;
    out.albedo = vec4<f32>(rgba.rgb * a, a);

    // Screen-space normal: decode the map, flip G (+up → -y on screen),
    // re-encode; flat +Z where the instance has no normal map. Sampled
    // unconditionally (uniform control flow), selected by the flag.
    let nm = textureSample(page_normal_tex, page_samp, uv).rgb * 2.0 - vec3<f32>(1.0);
    let mapped = vec3<f32>(nm.x, -nm.y, nm.z) * 0.5 + vec3<f32>(0.5);
    let enc = mix(vec3<f32>(0.5, 0.5, 1.0), mapped, in.misc.w);
    out.normal = vec4<f32>(enc * a, a);

    // Light-layer coverage + normal weight, premultiplied.
    out.mask = vec4<f32>(in.misc.rgb, in.misc.w) * a;
    return out;
}

// The MRT outputs for a shape: its premultiplied color as albedo, the flat
// normal and its light-mask layers, both weighted by coverage.
fn shape_scene_output(in: VertexOutput, rgba: vec4<f32>) -> SceneOutput {
    let a = rgba.a;
    var out: SceneOutput;
    out.albedo = rgba;
    out.normal = vec4<f32>(vec3<f32>(0.5, 0.5, 1.0) * a, a);
    out.mask = vec4<f32>(in.misc.rgb * a, 0.0);
    return out;
}

@fragment
fn fs_scene(in: VertexOutput) -> SceneOutput {
    let uv = page_uv(in);
    let out = scene_output(in, uv, textureSample(page_tex, page_samp, uv));
    let shape = shape_rgba(in);
    if in.shape.x > 0.0 {
        return shape_scene_output(in, shape);
    }
    return out;
}

// IEC 61966-2-1 decode (the same function the letterbox blit uses).
fn srgb_to_linear3(c: vec3<f32>) -> vec3<f32> {
    let lo = c / 12.92;
    let hi = pow((c + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4));
    return select(hi, lo, c <= vec3<f32>(0.04045));
}

// Linear-lighting scene pass: pages hold sRGB-encoded pixels (raw
// Rgba8Unorm), so decode rgb before the modulate; alpha is coverage.
@fragment
fn fs_scene_linear(in: VertexOutput) -> SceneOutput {
    let uv = page_uv(in);
    let texel = textureSample(page_tex, page_samp, uv);
    let out = scene_output(in, uv, vec4<f32>(srgb_to_linear3(texel.rgb), texel.a));
    let shape = shape_rgba(in);
    if in.shape.x > 0.0 {
        return shape_scene_output(in, shape);
    }
    return out;
}
