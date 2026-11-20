// Screen-space indirect light (the architecture's Screen-space indirect
// light): the opaque stage's four dispatches at half the render size,
// rounded up, each texel's receiver the full-size pixel at its upper left.
// Provenance and the port's departures: screen_indirect/README.md.
//
// prefilter_sources: the bounce sources, the lit colour and normals at half
// size and its mips (SSRTCS.compute's light and normal pyramids).
// gather: the visibility-bitmask horizon search (Therrien, Levesque and
// Gilet 2023), CDRIN SSRT3 b38d821 HDRP/Shaders/Resources/SSRTCS.compute
// (CSMain's slice loop, HorizonSampling, ComputeOccludedBitfield; MIT,
// Copyright (c) 2024 CDRIN, src/LICENSE-ssrt3.txt) at SSRT_HDRP.cs's shipped
// steps, spacing, thickness and sectors, with Bevy 9d12036
// crates/bevy_pbr/src/ssao/ssao.wesl (updateSectors, processSample, the
// slice setup, the Hilbert and R2 noise; MIT, src/LICENSE-bevy.txt) as the
// WGSL model of the same search, over ambient occlusion's depth pyramid and
// Hilbert table, the G-buffer's normals and the bounce sources. Each
// sample's sectors weigh the measure GTAO integrates in a slice, within the
// radius (D-65); an emitter facing away gives none (Godot b130438
// servers/rendering/renderer_rd/shaders/effects/ssil.glsl 214; MIT,
// src/LICENSE-godot.txt).
// denoise: XeGTAO_Denoise's edge-aware kernel (xegtao_kernels.wgsl) over the
// light and coverage, with edges from the receivers' depths.
// accumulate: reprojection by the G-buffer's motion into the kept result
// (shading/screen_indirect.wgsl), at most SCREEN_INDIRECT_MOST_HISTORY
// frames, DiligentFX f26cfe5 SSAO_MAX_HISTORY_LENGTH (Apache-2.0,
// src/LICENSE-diligent.txt), as ambient occlusion's (AO-7).
struct Params {
    view: mat4x4<f32>,
    size: vec2<u32>,
    half_size: vec2<u32>,
    pixel_size: vec2<f32>,
    ndc_mul: vec2<f32>,
    ndc_add: vec2<f32>,
    jitter_delta: vec2<f32>,
    radius: f32,
    slices: u32,
    steps: u32,
    noise_index: u32,
    reset: u32,
}
// The most slices and steps a side a pixel takes, the High level, so its
// loops end whatever the parameters say: a pixel walks at most
// SCREEN_INDIRECT_MOST_SLICES * SCREEN_INDIRECT_SIDES *
// SCREEN_INDIRECT_MOST_STEPS = 48 samples (AR-12).
const SCREEN_INDIRECT_MOST_SLICES: u32 = 2u;
const SCREEN_INDIRECT_MOST_STEPS: u32 = 12u;
// The two sides of a slice's receiver.
const SCREEN_INDIRECT_SIDES: u32 = 2u;
// The sectors a slice's hemisphere is cut into: SSRTCS.compute's MAX_RAY,
// one bit each of a u32.
const SCREEN_INDIRECT_SECTORS: f32 = 32.0;
// How deep in metres a sample's surface is taken to be behind it:
// SSRT_HDRP.cs's default thickness, constant (linearThickness off).
const SCREEN_INDIRECT_THICKNESS: f32 = 1.0;
// The coarsest depth mip a sample reads: the pyramid's last.
const SCREEN_INDIRECT_LAST_MIP: u32 = 4u;
// The frames accumulated at most.
const SCREEN_INDIRECT_MOST_HISTORY: f32 = 16.0;
// The view depth ambient occlusion's pyramid gives the sky (view_depth's
// clamp, the largest half float).
const SCREEN_INDIRECT_SKY_DEPTH: f32 = 65504.0;
// The Hilbert curve's tile, texels across: ambient occlusion's table.
const SCREEN_INDIRECT_HILBERT_WIDTH: u32 = 64u;
// Four cardinal neighbours and four corners (AR-12).
const SCREEN_INDIRECT_DENOISE_NEIGHBOURS: u32 = 4u;
const PI: f32 = 3.1415926535897932384626433832795;
const HALF_PI: f32 = 1.57079632679489661923;

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var depth_pyramid: texture_2d<f32>;
@group(0) @binding(2) var normals: texture_2d<f32>;
@group(0) @binding(3) var lit_color: texture_2d<f32>;
@group(0) @binding(5) var hilbert_lut: texture_2d<u32>;
@group(0) @binding(6) var gathered_out: texture_storage_2d<rgba16float, write>;
@group(0) @binding(7) var edges_out: texture_storage_2d<r32uint, write>;
@group(0) @binding(8) var gathered: texture_2d<f32>;
@group(0) @binding(9) var edges: texture_2d<u32>;
@group(0) @binding(10) var denoised_out: texture_storage_2d<rgba16float, write>;
@group(0) @binding(11) var denoised: texture_2d<f32>;
@group(0) @binding(12) var depth: texture_depth_2d;
@group(0) @binding(13) var motion: texture_2d<f32>;
@group(0) @binding(14) var kept: texture_2d<u32>;
@group(0) @binding(15) var kept_out: texture_storage_2d<rgba32uint, write>;
@group(0) @binding(16) var<uniform> kept_view: ScreenIndirectView;
@group(0) @binding(17) var sources_out0: texture_storage_2d<rg32uint, write>;
@group(0) @binding(18) var sources_out1: texture_storage_2d<rg32uint, write>;
@group(0) @binding(19) var sources_out2: texture_storage_2d<rg32uint, write>;
@group(0) @binding(20) var sources_out3: texture_storage_2d<rg32uint, write>;
@group(0) @binding(21) var sources: texture_2d<u32>;

// Bevy 9d12036 crates/bevy_pbr/src/render/rgb9e5.wesl (MIT,
// src/LICENSE-bevy.txt): EXT_texture_shared_exponent's shared-exponent
// colour, in which the bounce sources hold lit colour.
const RGB9E5_EXP_BIAS: i32 = 15;
const RGB9E5_MANTISSA_BITS: i32 = 9;
const RGB9E5_MANTISSA_VALUES: i32 = 512;
const MAX_RGB9E5: f32 = 65408.0;
fn floor_log2(x: f32) -> i32 {
    return i32((bitcast<u32>(x) & 0x7F800000u) >> 23u) - 127;
}
fn vec3_to_rgb9e5(rgb_in: vec3<f32>) -> u32 {
    let rgb = clamp(rgb_in, vec3(0.0), vec3(MAX_RGB9E5));
    let maxrgb = max(rgb.r, max(rgb.g, rgb.b));
    var exp_shared = max(-RGB9E5_EXP_BIAS - 1, floor_log2(maxrgb)) + 1 + RGB9E5_EXP_BIAS;
    var denom = exp2(f32(exp_shared - RGB9E5_EXP_BIAS - RGB9E5_MANTISSA_BITS));
    let maxm = i32(floor(maxrgb / denom + 0.5));
    if maxm == RGB9E5_MANTISSA_VALUES {
        denom *= 2.0;
        exp_shared += 1;
    }
    let n = vec3<u32>(floor(rgb / denom + 0.5));
    return (u32(exp_shared) << 27u) | (n.b << 18u) | (n.g << 9u) | n.r;
}
fn rgb9e5_to_vec3(v: u32) -> vec3<f32> {
    let scale = exp2(f32(i32(v >> 27u) - RGB9E5_EXP_BIAS - RGB9E5_MANTISSA_BITS));
    return vec3(f32(v & 511u), f32((v >> 9u) & 511u), f32((v >> 18u) & 511u)) * scale;
}

fn mip_size(mip: u32) -> vec2<u32> {
    return max(vec2(1u), p.size >> vec2(mip));
}
// The pyramid's view depth at full-size pixel `q`.
fn receiver_depth(q: vec2<i32>) -> f32 {
    return textureLoad(depth_pyramid, clamp(q, vec2(0), vec2<i32>(p.size) - 1), 0).x;
}
// The pyramid's view depth of `mip` at `uv`, the texel that holds it, as
// SSRTCS.compute's point-sampled GetLinearDepth.
fn sample_depth(uv: vec2<f32>, mip: u32) -> f32 {
    let size = mip_size(mip);
    let q = clamp(vec2<i32>(floor(uv * vec2<f32>(size))), vec2(0), vec2<i32>(size) - 1);
    return textureLoad(depth_pyramid, q, i32(mip)).x;
}
// The point at `uv` and view depth `z` in XeGTAO's view space (x right, y
// up, z forward), the raster jitter undone, as ambient occlusion's position.
fn position(uv: vec2<f32>, z: f32) -> vec3<f32> {
    return vec3((p.ndc_mul * uv + p.ndc_add) * z, z);
}
// A world normal in that view space.
fn view_normal(world: vec3<f32>) -> vec3<f32> {
    let rh = (p.view * vec4(world, 0.0)).xyz;
    return vec3(rh.xy, -rh.z);
}
// The full-size pixel half-size texel `id` stands for.
fn receiver_pixel(id: vec2<u32>) -> vec2<u32> {
    return min(id * 2u, p.size - 1u);
}
// An angle from its cosine, exactly: SSRTCS.compute's GTAOFastAcos
// (Lagarde's polynomial) errs by up to 0.008 rad between its ends, so off
// the view axis a receiver's own tangent plane, whose samples lie on its
// horizon, covered a sliver of its horizon sectors.
fn horizon_acos(x: f32) -> f32 {
    return acos(clamp(x, -1.0, 1.0));
}
// The integral over a slice's in-plane angle `alpha` from its projected
// normal, from `low` to `high`, of cos(alpha) * sin(alpha + gamma), gamma
// the projected normal's signed angle from the view vector: the measure
// XeGTAO's arcs evaluate (XeGTAO.hlsli, the iarc terms), here per sector
// interval (D-65). F(alpha) = (2 alpha sin(gamma) - cos(2 alpha + gamma)) / 4
// differenced, written as (w sin(gamma) + sin(low + high + gamma) sin(w)) / 2
// with w = high - low, which does not cancel over a narrow interval.
fn slice_measure(low: f32, high: f32, gamma: f32) -> f32 {
    let width = high - low;
    return (width * sin(gamma) + sin(low + high + gamma) * sin(width)) / 2.0;
}

// The bounce sources: the lit colour and view-space normal the march
// reads, at half the render size and SCREEN_INDIRECT_SOURCE_MIPS levels,
// each texel the mean of the four below it, as SSRTCS.compute reads its
// light and normal pyramids at the depth's mip (HorizonSampling,
// _LightPyramidTexture and _NormalPyramidTexture at mipLevelOffset) and
// Godot b130438 reads a mipmapped copy of the frame (ss_effects.cpp
// copy_internal_texture_to_last_frame, ssil.glsl 211). Packed in one
// Rg32Uint texel, read with one load: the colour shared-exponent, the
// normal octahedral, two signed 16-bit coordinates.
const SCREEN_INDIRECT_SOURCE_MIPS: u32 = 4u;
// The full-size pixels a level-0 source texel is the mean of (AR-12).
const SCREEN_INDIRECT_SOURCE_CHILDREN: u32 = 4u;
struct Source {
    color: vec3<f32>,
    normal: vec3<f32>,
}
fn pack_source(source: Source) -> vec4<u32> {
    let normal = select(source.normal / length(source.normal), vec3(0.0, 0.0, -1.0), dot(source.normal, source.normal) <= 0.0);
    return vec4(vec3_to_rgb9e5(source.color), pack2x16snorm(gbuffer_octahedral_encode(normal)), 0u, 0u);
}
// The source at depth mip `mip`'s footprint about `uv`: the source level
// one below it, half the render size being the first.
fn load_source(uv: vec2<f32>, mip: u32) -> Source {
    let level = max(mip, 1u) - 1u;
    // The texture owns every level's size (screen_indirect.rs Targets).
    let size = textureDimensions(sources, level);
    let texel = clamp(vec2<i32>(uv * vec2<f32>(p.size) / f32(2u << level)), vec2(0), vec2<i32>(size) - 1);
    let packed = textureLoad(sources, texel, i32(level)).xy;
    return Source(rgb9e5_to_vec3(packed.x), gbuffer_octahedral_decode(unpack2x16snorm(packed.y)));
}
var<workgroup> source_tile: array<array<Source, 8>, 8>;
fn tile_mean(at: vec2<u32>, stride: u32) -> Source {
    let a = source_tile[at.x][at.y];
    let b = source_tile[at.x + stride][at.y];
    let c = source_tile[at.x][at.y + stride];
    let d = source_tile[at.x + stride][at.y + stride];
    return Source((a.color + b.color + c.color + d.color) * 0.25, a.normal + b.normal + c.normal + d.normal);
}
// Each 8x8 group builds an 8x8 tile of level 0 from the full-size targets
// and its 4x4, 2x2 and 1x1 above it, through workgroup memory, as XeGTAO's
// depth prefilter builds its mips.
@compute @workgroup_size(8, 8)
fn prefilter_sources(@builtin(global_invocation_id) id: vec3<u32>, @builtin(local_invocation_id) local: vec3<u32>) {
    var color = vec3(0.0);
    var normal = vec3(0.0);
    for (var corner = 0u; corner < SCREEN_INDIRECT_SOURCE_CHILDREN; corner++) {
        let q = vec2<i32>(min(id.xy * 2u + vec2(corner & 1u, corner >> 1u), p.size - 1u));
        color += textureLoad(lit_color, q, 0).rgb;
        normal += view_normal(gbuffer_base_normal(textureLoad(normals, q, 0)));
    }
    var source = Source(color * 0.25, normal);
    if all(id.xy < textureDimensions(sources_out0)) {
        textureStore(sources_out0, id.xy, pack_source(source));
    }
    source_tile[local.x][local.y] = source;
    workgroupBarrier();
    if all(local.xy % vec2(2u) == vec2(0u)) {
        source = tile_mean(local.xy, 1u);
        if all(id.xy / 2u < textureDimensions(sources_out1)) {
            textureStore(sources_out1, id.xy / 2u, pack_source(source));
        }
    }
    workgroupBarrier();
    if all(local.xy % vec2(2u) == vec2(0u)) {
        source_tile[local.x][local.y] = source;
    }
    workgroupBarrier();
    if all(local.xy % vec2(4u) == vec2(0u)) {
        source = tile_mean(local.xy, 2u);
        if all(id.xy / 4u < textureDimensions(sources_out2)) {
            textureStore(sources_out2, id.xy / 4u, pack_source(source));
        }
    }
    workgroupBarrier();
    if all(local.xy % vec2(4u) == vec2(0u)) {
        source_tile[local.x][local.y] = source;
    }
    workgroupBarrier();
    if all(local.xy == vec2(0u)) {
        source = tile_mean(local.xy, 4u);
        if all(id.xy / 8u < textureDimensions(sources_out3)) {
            textureStore(sources_out3, id.xy / 8u, pack_source(source));
        }
    }
}

@compute @workgroup_size(8, 8)
fn gather(@builtin(global_invocation_id) id: vec3<u32>) {
    if any(id.xy >= p.half_size) {
        return;
    }
    let texel = vec2<i32>(id.xy);
    let pixel = receiver_pixel(id.xy);
    let q = vec2<i32>(pixel);
    let z = receiver_depth(q);
    // The edges toward the neighbouring texels' receivers, which the
    // denoiser keeps from blurring across.
    let neighbours = vec4(
        receiver_depth(q + vec2(-2, 0)),
        receiver_depth(q + vec2(2, 0)),
        receiver_depth(q + vec2(0, -2)),
        receiver_depth(q + vec2(0, 2)),
    );
    textureStore(edges_out, texel, vec4(xegtao_pack_edges(xegtao_edges(z, neighbours)), 0u, 0u, 0u));
    // The sky receives nothing.
    if z >= SCREEN_INDIRECT_SKY_DEPTH {
        textureStore(gathered_out, texel, vec4(0.0));
        return;
    }
    let uv = (vec2<f32>(pixel) + 0.5) * p.pixel_size;
    let center = position(uv, z);
    let view_vec = normalize(-center);
    let normal = view_normal(gbuffer_base_normal(textureLoad(normals, q, 0)));
    let local_noise = xegtao_noise(textureLoad(hilbert_lut, id.xy % vec2(SCREEN_INDIRECT_HILBERT_WIDTH), 0).x, p.noise_index);
    let slices = min(p.slices, SCREEN_INDIRECT_MOST_SLICES);
    let steps = min(p.steps, SCREEN_INDIRECT_MOST_STEPS);
    // HorizonSampling's stepRadius in pixels: the radius at the receiver's
    // depth over steps + 1, at least a pixel a step. The radius is projected
    // whole, as Bevy's and XeGTAO's are, so the search spans the reach that
    // cuts its samples; SSRT_HDRP.cs's _HalfProjScale halves it, and its
    // search spans half its radius across the view (D-65).
    let pixel_view_size = z * p.ndc_mul.x * p.pixel_size.x;
    let step_pixels = max(p.radius / pixel_view_size, f32(steps)) / f32(steps + 1u);
    var light = vec3(0.0);
    var coverage = 0.0;
    for (var slice = 0u; slice < slices; slice++) {
        let phi = (f32(slice) + local_noise.x) * PI / f32(slices);
        let direction = vec3(cos(phi), sin(phi), 0.0);
        // A pixel's step along the slice, in UV (y down).
        let omega = vec2(direction.x, -direction.y) * p.pixel_size;
        let ortho = direction - dot(direction, view_vec) * view_vec;
        let axis = normalize(cross(ortho, view_vec));
        let projected = normal - axis * dot(normal, axis);
        let sign_norm = sign(dot(ortho, projected));
        let projected_length = length(projected);
        // A normal across the slice projects to nothing and weighs nothing;
        // kept finite, as the kept light would keep a NaN.
        let cos_norm = clamp(dot(projected, view_vec) / max(projected_length, 1e-6), 0.0, 1.0);
        let gamma = sign_norm * horizon_acos(cos_norm);
        // The projected normal's sector coordinate (Bevy's n).
        let normal_sector = (HALF_PI - gamma) / PI;
        // Linear spacing, jittered per slice (SSRT3's initialRayStep).
        let jitter = fract(local_noise.y + f32(slice) * 0.6180339887498948482);
        var bitmask = 0u;
        for (var side = 0u; side < SCREEN_INDIRECT_SIDES; side++) {
            // Bevy's sampling_direction: -1 toward +omega, 1 toward -omega.
            let sampling_direction = select(1.0, -1.0, side == 0u);
            for (var step = 0u; step < steps; step++) {
                let offset = max(step_pixels * (f32(step) + jitter), f32(step + 1u));
                let sample_uv = uv - omega * offset * sampling_direction;
                // A sample off screen ends its side.
                if any(sample_uv <= vec2(0.0)) || any(sample_uv >= vec2(1.0)) {
                    break;
                }
                // The depth mip its distance in pixels selects, by XeGTAO's
                // rule, not SSRTCS.compute's step index, min((j + 1) / 2, 4),
                // set for its search half as wide in pixels (D-70).
                let mip = xegtao_depth_mip(offset, SCREEN_INDIRECT_LAST_MIP);
                let delta = position(sample_uv, sample_depth(sample_uv, mip)) - center;
                let distance = length(delta);
                // Beyond the radius a sample covers nothing (D-65), the sky
                // among them.
                if distance > p.radius || distance <= 0.0 {
                    continue;
                }
                let back = delta - view_vec * SCREEN_INDIRECT_THICKNESS;
                var horizons = vec2(horizon_acos(dot(delta / distance, view_vec)), horizon_acos(dot(normalize(back), view_vec)));
                horizons = clamp(vec2(normal_sector) - sampling_direction * horizons / PI, vec2(0.0), vec2(1.0));
                horizons = select(horizons.xy, horizons.yx, sampling_direction >= 0.0);
                let first = u32(horizons.x * SCREEN_INDIRECT_SECTORS);
                let count = u32(ceil((horizons.y - horizons.x) * SCREEN_INDIRECT_SECTORS));
                let sectors = insertBits(0u, 0xFFFFFFFFu, first, count);
                let covered = sectors & ~bitmask;
                bitmask |= sectors;
                let total = countOneBits(sectors);
                let newly = countOneBits(covered);
                if newly == 0u {
                    continue;
                }
                // The interval's slice measure, by the share of its sectors
                // nothing nearer covered.
                let alphas = (horizons - 0.5) * PI;
                let weight = projected_length * abs(slice_measure(alphas.x, alphas.y, gamma)) * f32(newly) / f32(total);
                coverage += weight;
                let source = load_source(sample_uv, mip);
                let facing = smoothstep(0.0, 0.1, -dot(source.normal, delta / distance));
                let color = source.color;
                light += weight * facing * color;
            }
        }
    }
    light /= f32(slices);
    coverage /= f32(slices);
    // One slice can cover more than the hemisphere's measure: keep the
    // light within the brightest sample's.
    if coverage > 1.0 {
        light /= coverage;
        coverage = 1.0;
    }
    textureStore(gathered_out, texel, vec4(light, coverage));
}

fn gathered_at(q: vec2<i32>) -> vec4<f32> {
    return textureLoad(gathered, clamp(q, vec2(0), vec2<i32>(p.half_size) - 1), 0);
}
fn edges_at(q: vec2<i32>) -> vec4<f32> {
    return xegtao_unpack_edges(textureLoad(edges, clamp(q, vec2(0), vec2<i32>(p.half_size) - 1), 0).x);
}

@compute @workgroup_size(8, 8)
fn denoise(@builtin(global_invocation_id) id: vec3<u32>) {
    if any(id.xy >= p.half_size) {
        return;
    }
    let q = vec2<i32>(id.xy);
    let weights = xegtao_denoise_weights(edges_at(q), edges_at(q + vec2(-1, 0)), edges_at(q + vec2(1, 0)), edges_at(q + vec2(0, -1)), edges_at(q + vec2(0, 1)));
    let cardinal = array(vec2(-1, 0), vec2(1, 0), vec2(0, -1), vec2(0, 1));
    let corners = array(vec2(-1, -1), vec2(1, -1), vec2(-1, 1), vec2(1, 1));
    var sum = gathered_at(q) * 1.2;
    var weight = 1.2;
    for (var i = 0u; i < SCREEN_INDIRECT_DENOISE_NEIGHBOURS; i++) {
        sum += weights.cardinal[i] * gathered_at(q + cardinal[i]);
        weight += weights.cardinal[i];
    }
    for (var i = 0u; i < SCREEN_INDIRECT_DENOISE_NEIGHBOURS; i++) {
        sum += weights.corners[i] * gathered_at(q + corners[i]);
        weight += weights.corners[i];
    }
    textureStore(denoised_out, q, sum / weight);
}

@compute @workgroup_size(8, 8)
fn accumulate(@builtin(global_invocation_id) id: vec3<u32>) {
    if any(id.xy >= p.half_size) {
        return;
    }
    let texel = vec2<i32>(id.xy);
    let pixel = receiver_pixel(id.xy);
    let q = vec2<i32>(pixel);
    if textureLoad(depth, q, 0) <= 0.0 {
        // Nothing to keep at the sky.
        textureStore(kept_out, texel, vec4(0u));
        return;
    }
    let z = receiver_depth(q);
    let world_normal = gbuffer_base_normal(textureLoad(normals, q, 0));
    let current = textureLoad(denoised, texel, 0);
    var value = current;
    var count = 1.0;
    let velocity = textureLoad(motion, q, 0);
    let expected_depth = z + velocity.z;
    let uv = (vec2<f32>(pixel) + 0.5) * p.pixel_size;
    let previous_uv = uv - velocity.xy + p.jitter_delta;
    // No history behind the previous camera or off its screen (SI-4), as a
    // fragment's read (screen_indirect_read).
    let on_screen = all(previous_uv >= vec2(0.0)) && all(previous_uv <= vec2(1.0));
    if p.reset == 0u && velocity.w > 0.0 && expected_depth > 0.0 && on_screen {
        let previous = screen_indirect_position(kept_view, previous_uv, expected_depth);
        let taps = screen_indirect_taps(kept, kept_view, previous_uv, previous, kept_view.rotation * world_normal);
        if taps.weight > 0.0 {
            count = min(taps.count + 1.0, SCREEN_INDIRECT_MOST_HISTORY);
            value = mix(taps.value, current, 1.0 / count);
        }
    }
    // The normal in this frame's view space (x right, y up, -z forward),
    // the space the next frame's readers find the kept texels in.
    let normal = (p.view * vec4(world_normal, 0.0)).xyz;
    textureStore(kept_out, texel, screen_indirect_pack(ScreenIndirectKept(value, z, count, normal)));
}
