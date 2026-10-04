// Per-light additive pass + shadow-mask pass (R-6, PR-4).
//
// vs_light/fs_light: one instanced quad per light covering the light's world
// footprint (the cookie's scaled size, or ±radius for analytic lights), drawn
// through the world camera into the light-accumulation target with additive
// blending. The fragment computes Godot's add-mode contribution WITHOUT the
// base albedo (the composite multiplies it in):
//
//   intensity × color × energy × normal_factor × coverage × shadow
//
// - intensity: the sampled cookie (rgb×a) for cookie lights, or the
//   bevy_light_2d radial falloff (1 − s²)² / (1 + falloff·s²), s = dist /
//   radius, for analytic lights (params.z = radius > 0; a 1×1 white dummy is
//   bound as the cookie). Both are evaluated and `select`ed so the one
//   pipeline keeps uniform control flow.
// - normal_factor: max(dot(N, L), 0) with L = normalize(light - frag, height),
//   blended to 1 by the mask target's alpha (the per-pixel "has normal map"
//   weight) — Godot lights unmapped sprites at full strength.
// - coverage: the mask target's rgb (per-pixel light-layer coverage, layers
//   1/2/4) dotted with the light's layer selectors, clamped to 1.
// - shadow: a 3×3 box tap over the shared shadow-mask target (soft, PCF-ish
//   edge ≈ Godot's smooth filter), or 1 for shadowless lights.
//
// vs_shadow/fs_shadow: CPU-extruded occluder triangles (world space) written
// as 0 into the R8 shadow-mask target (cleared to 1 per shadowed light).
//
// The screen-space inputs (normal/mask/shadow targets) are the same size as
// the render target, so they are read with textureLoad at the fragment's own
// pixel coordinate.

const MAX_LIGHTS: u32 = 32u;

struct Light {
    // Light position xy, half footprint zw (the world camera's units).
    pos_half: vec4<f32>,
    // Light color rgb (as authored), energy in w.
    color_energy: vec4<f32>,
    // x = height (normal shading z), y = shadow flag, z = analytic radius
    // (0 = cookie light), w = analytic falloff.
    params: vec4<f32>,
    // rgb = item-mask layer selectors (layers 1/2/4), w pad.
    mask: vec4<f32>,
};

struct Uniforms {
    view_proj: mat4x4<f32>,
    // Logical target size (w, h); z = world → screen-down y sign (+1 for
    // y-down pixels, -1 for a y-up camera); w pad.
    screen: vec4<f32>,
    lights: array<Light, MAX_LIGHTS>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var normal_tex: texture_2d<f32>;
@group(0) @binding(2) var mask_tex: texture_2d<f32>;
@group(0) @binding(3) var shadow_tex: texture_2d<f32>;

@group(1) @binding(0) var cookie_tex: texture_2d<f32>;
@group(1) @binding(1) var cookie_samp: sampler;

struct LightVsOut {
    @builtin(position) clip_pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) world: vec2<f32>,
    @location(2) @interpolate(flat) index: u32,
};

@vertex
fn vs_light(
    @builtin(vertex_index) vid: u32,
    @builtin(instance_index) inst: u32,
) -> LightVsOut {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0), vec2<f32>(1.0, 1.0),
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 1.0), vec2<f32>(0.0, 1.0),
    );
    let corner = corners[vid];
    let light = u.lights[inst];
    let world = light.pos_half.xy + (corner - vec2<f32>(0.5, 0.5)) * light.pos_half.zw * 2.0;

    var out: LightVsOut;
    out.clip_pos = u.view_proj * vec4<f32>(world, 0.0, 1.0);
    // Corner (0,0) is the footprint's smallest world y — the screen bottom
    // under a y-up camera, so flip v there to keep the cookie upright.
    out.uv = vec2<f32>(corner.x, select(corner.y, 1.0 - corner.y, u.screen.z < 0.0));
    out.world = world;
    out.index = inst;
    return out;
}

@fragment
fn fs_light(in: LightVsOut) -> @location(0) vec4<f32> {
    let light = u.lights[in.index];
    let cookie = textureSample(cookie_tex, cookie_samp, in.uv);

    let bounds = vec2<i32>(u.screen.xy) - vec2<i32>(1, 1);
    let pix = clamp(vec2<i32>(in.clip_pos.xy), vec2<i32>(0, 0), bounds);
    let nrm = textureLoad(normal_tex, pix, 0);
    let msk = textureLoad(mask_tex, pix, 0);

    // Receiver coverage: sprite light_mask layers ∩ the light's item mask.
    let coverage = clamp(dot(msk.rgb, light.mask.rgb), 0.0, 1.0);

    // Normal shading, blended to full strength where no normal map exists.
    let nv = nrm.rgb * 2.0 - vec3<f32>(1.0);
    let nlen = length(nv);
    // The normal target is screen-space y-down: bring the world delta into
    // that frame (y negated under a y-up camera). `height` is in world units.
    let lv = vec3<f32>((light.pos_half.xy - in.world) * vec2<f32>(1.0, u.screen.z), light.params.x);
    var dotf = 1.0;
    if (nlen > 0.001) {
        dotf = max(dot(nv / nlen, normalize(lv)), 0.0);
    }
    let factor = mix(1.0, dotf, msk.a);

    // Occluder shadow: 3×3 box tap for a soft, PCF-ish edge.
    var lit = 0.0;
    for (var dy = -1; dy <= 1; dy = dy + 1) {
        for (var dx = -1; dx <= 1; dx = dx + 1) {
            let p = clamp(pix + vec2<i32>(dx, dy), vec2<i32>(0, 0), bounds);
            lit = lit + textureLoad(shadow_tex, p, 0).r;
        }
    }
    let shadow = mix(1.0, lit / 9.0, light.params.y);

    // Intensity: the cookie (the shipped cookies carry the falloff in alpha
    // over a constant color) or, for analytic lights, the radial falloff.
    // The quad spans ±radius (or ±half the cookie), so the normalized offset
    // |uv·2 − 1| is exactly s = dist / radius; s > 1 (the quad's corners)
    // gives 0. Both are computed unconditionally and selected by params.z.
    let q = in.uv * 2.0 - vec2<f32>(1.0);
    let s2 = dot(q, q);
    let bell = max(1.0 - s2, 0.0);
    let analytic = bell * bell / (1.0 + light.params.w * s2);
    let intensity = select(cookie.rgb * cookie.a, vec3<f32>(analytic), light.params.z > 0.0);
    let contrib =
        intensity * light.color_energy.rgb * light.color_energy.w * factor * coverage * shadow;
    return vec4<f32>(contrib, 0.0);
}

// --- Shadow mask pass --------------------------------------------------------

struct ShadowVsOut {
    @builtin(position) clip_pos: vec4<f32>,
};

@vertex
fn vs_shadow(@location(0) pos: vec2<f32>) -> ShadowVsOut {
    var out: ShadowVsOut;
    out.clip_pos = u.view_proj * vec4<f32>(pos, 0.0, 1.0);
    return out;
}

@fragment
fn fs_shadow() -> @location(0) vec4<f32> {
    return vec4<f32>(0.0, 0.0, 0.0, 0.0); // 0 = shadowed (target cleared to 1)
}
