// WGSL port of DiligentFX Shaders/PostProcess/TemporalAntiAliasing/private/TAA_ComputeTemporalAccumulation.fx
// (https://github.com/DiligentGraphics/DiligentFX, revision
// f26cfe5b901bf180c4a3c9bbd4d5df0b96536d4b). Copyright Diligent Graphics LLC,
// licensed under the Apache License, Version 2.0 (vendor/DiligentFX/License.txt).
// Modified: translated from HLSL to WGSL; see crates/sgl-post-fx/README.md.
// DFX-19 adds the still-pixel history of Bevy's TAA,
// crates/bevy_anti_alias/src/taa/taa.wesl (https://github.com/bevyengine/bevy,
// revision 92a29e701a6b0bf8846484c3999c2ba97d90dd06), MIT licensed
// (LICENSE-bevy.txt).
// DFX-14 adds the motion-difference rejection, speed-scaled variance box and
// history clip of Godot's TAA, servers/rendering/renderer_rd/shaders/effects/taa_resolve.glsl
// and servers/rendering/renderer_rd/effects/taa.cpp
// (https://github.com/godotengine/godot, revision
// b13043816a0f234985030ec035363a005bc86c32), MIT licensed (LICENSE-godot.txt);
// taa_resolve.glsl is based on Spartan Engine's TAA, Copyright (c) 2016-2022
// Panos Karabelas, MIT licensed (LICENSE-spartan.txt).
// DFX-13 finds the closest motion vectors in the resolve, as taa_resolve.glsl
// does (get_closest_pixel_velocity_3x3), with the search of DiligentFX's
// Shaders/Common/private/ComputeClosestMotion.fx.

#include "BasicStructures.fxh"
#include "FullScreenTriangleVSOutput.fxh"
#include "PostFX_Common.fxh"
#include "TemporalAntiAliasingStructures.fxh"

#define FLT_EPS 5.960464478e-8

#if POSTFX_OPTION_INVERTED_DEPTH
    #define DepthFarPlane  0.0
#else
    #define DepthFarPlane  1.0
#endif // POSTFX_OPTION_INVERTED_DEPTH

// PROVENANCE.md DFX-19: Bevy's still-pixel rule. A pixel whose closest motion is under
// TAA_STILL_MOTION_PIXELS on both axes keeps history up to TAA_STILL_HISTORY_FACTOR
// (1 - Bevy's MIN_HISTORY_BLEND_RATE) and is not rejected by depth disocclusion.
const TAA_STILL_MOTION_PIXELS = 0.01;
const TAA_STILL_HISTORY_FACTOR = 0.985;

// WGSL: one uniform holds the constant buffer's two cameras, so the upstream
// g_CurrCamera and g_PrevCamera read as cbCameraAttribs.g_CurrCamera and
// cbCameraAttribs.g_PrevCamera.
struct CameraAttribsPair
{
    g_CurrCamera: CameraAttribs,
    g_PrevCamera: CameraAttribs,
}
@group(0) @binding(0) var<uniform> cbCameraAttribs: CameraAttribsPair;

// cbuffer cbTemporalAntiAliasingAttribs
@group(0) @binding(1) var<uniform> g_TAAAttribs: TemporalAntiAliasingAttribs;

@group(0) @binding(2) var g_TextureCurrColor: texture_2d<f32>;
@group(0) @binding(3) var g_TexturePrevColor: texture_2d<f32>;
// PROVENANCE.md DFX-13: the motion vectors, of which the resolve finds the closest.
@group(0) @binding(4) var g_TextureMotion: texture_2d<f32>;
@group(0) @binding(5) var g_TextureCurrDepth: texture_2d<f32>;
@group(0) @binding(6) var g_TexturePrevDepth: texture_2d<f32>;

@group(0) @binding(7) var g_TexturePrevColor_sampler: sampler;

// PROVENANCE.md DFX-14: the previous frame's closest motion vectors.
@group(0) @binding(8) var g_TexturePrevMotion: texture_2d<f32>;

// PROVENANCE.md DFX-13: the depth buffer, in which the closest motion is found.
// WGSL: the depth buffer binds as a depth texture.
@group(0) @binding(9) var g_TextureDepth: texture_depth_2d;

struct PixelStatistic
{
    Mean:     vec3<f32>,
    Variance: vec3<f32>,
    StdDev:   vec3<f32>,
}

fn RGBToYCoCg(RGB: vec3<f32>) -> vec3<f32>
{
    // float Y  = dot(float3(+0.25, +0.50, +0.25), RGB);
    // float Co = dot(float3(+0.50, +0.00, -0.50), RGB);
    // float Cg = dot(float3(-0.25, +0.50, -0.25), RGB);

#if TAA_OPTION_YCOCG_COLOR_SPACE
    let Co   = RGB.x - RGB.z;
    let Temp = RGB.z + 0.5 * Co;
    let Cg   = RGB.y - Temp;
    let Y    = Temp + 0.5 * Cg;
    return vec3<f32>(Y, Co, Cg);
#else
    return RGB;
#endif
}

fn YCoCgToRGB(YCoCg: vec3<f32>) -> vec3<f32>
{
    // float R = dot(float3(+1.0, +1.0, -1.0), YCoCg);
    // float G = dot(float3(+1.0, +0.0, +1.0), YCoCg);
    // float B = dot(float3(+1.0, -1.0, -1.0), YCoCg);

#if TAA_OPTION_YCOCG_COLOR_SPACE
    let Tmp = YCoCg.x - 0.5 * YCoCg.z;
    let G   = YCoCg.z + Tmp;
    let B   = Tmp - 0.5 * YCoCg.y;
    let R   = B + YCoCg.y;
    return vec3<f32>(R, G, B);
#else
    return YCoCg;
#endif
}

// WGSL: rcp(x) is 1.0 / x.
fn HDRToSDR(Color: vec3<f32>) -> vec3<f32>
{
    return Color * (1.0 / (1.0 + Color));
}

fn SDRToHDR(Color: vec3<f32>) -> vec3<f32>
{
    return Color * (1.0 / (1.0 - Color + FLT_EPS));
}

fn SampleCurrColor(PixelCoord: vec2<i32>) -> vec3<f32>
{
    return max(HlslLoad(g_TextureCurrColor, PixelCoord, 0).rgb, vec3<f32>(0.0));
}

fn SampleCurrDepth(PixelCoord: vec2<i32>) -> f32
{
    return HlslLoad(g_TextureCurrDepth, PixelCoord, 0).x;
}

fn SamplePrevDepth(PixelCoord: vec2<i32>) -> f32
{
    return HlslLoad(g_TexturePrevDepth, PixelCoord, 0).x;
}

// PROVENANCE.md DFX-13: ComputeClosestMotion.fx's SampleDepth, SampleMotion and SampleClosestMotion.
fn SampleDepth(PixelCoord: vec2<i32>) -> f32
{
    return HlslLoadDepth(g_TextureDepth, PixelCoord, 0);
}

fn SampleMotion(PixelCoord: vec2<i32>) -> vec2<f32>
{
    return HlslLoad(g_TextureMotion, PixelCoord, 0).xy;
}

fn SampleClosestMotion(PixelCoord: vec2<i32>) -> vec2<f32>
{
    var ClosestDepth: f32 = DepthFarPlane;
    var ClosestOffset = vec2<i32>(0, 0);

    const SearchRadius = 1;
    for (var x = -SearchRadius; x <= SearchRadius; x++)
    {
        for (var y = -SearchRadius; y <= SearchRadius; y++)
        {
            let Coord = PixelCoord + vec2<i32>(x, y);
            let NeighborDepth = SampleDepth(Coord);
#if POSTFX_OPTION_INVERTED_DEPTH
            if (NeighborDepth > ClosestDepth)
#else
            if (NeighborDepth < ClosestDepth)
#endif
            {
                ClosestOffset = vec2<i32>(x, y);
                ClosestDepth = NeighborDepth;
            }
        }
    }

    return SampleMotion(PixelCoord + ClosestOffset);
}

// PROVENANCE.md DFX-14.
fn SamplePrevMotion(PixelCoord: vec2<i32>) -> vec2<f32>
{
    return HlslLoad(g_TexturePrevMotion, PixelCoord, 0).xy * F3NDC_XYZ_TO_UVD_SCALE.xy;
}

// PROVENANCE.md DFX-14: Godot's clip_aabb (taa_resolve.glsl, after Playdead's
// temporal reprojection): ColorPrev moves towards the box's centre, the
// neighbourhood mean, until it lies in the box. Godot clips towards the mean
// clamped to the box, which is the mean itself. Godot's FLT_MIN is the margin.
fn ClipToAABB(ColorPrev: vec3<f32>, AABBCentre: vec3<f32>, AABBExtents: vec3<f32>) -> vec3<f32>
{
    const Margin = 0.00000001;
    var R = ColorPrev - AABBCentre;
    let RMax = AABBExtents;
    let RMin = -AABBExtents;
    if (R.x > RMax.x + Margin) { R *= RMax.x / R.x; }
    if (R.y > RMax.y + Margin) { R *= RMax.y / R.y; }
    if (R.z > RMax.z + Margin) { R *= RMax.z / R.z; }
    if (R.x < RMin.x - Margin) { R *= RMin.x / R.x; }
    if (R.y < RMin.y - Margin) { R *= RMin.y / R.y; }
    if (R.z < RMin.z - Margin) { R *= RMin.z / R.z; }
    return AABBCentre + R;
}

fn ComputeDepthDisocclusionWeight(CurrDepth: f32, PrevDepth: f32) -> f32
{
    let LinearDepthCurr  = abs(DepthToCameraZ(CurrDepth, cbCameraAttribs.g_CurrCamera.mProj));
    let LinearDepthPrev  = abs(DepthToCameraZ(PrevDepth, cbCameraAttribs.g_PrevCamera.mProj));
    let MaxLinearDepth   = max(LinearDepthCurr, LinearDepthPrev);
    let LinearDepthDelta = abs(LinearDepthCurr - LinearDepthPrev);
    return exp(-LinearDepthDelta / max(MaxLinearDepth, 1e-6));
}

fn ComputeDepthDisocclusion(Position: vec2<f32>, PrevPosition: vec2<f32>) -> f32
{
    let PrevPositioni = vec2<i32>(PrevPosition);
    let CurrDepth = SampleCurrDepth(vec2<i32>(Position));
    // PROVENANCE.md DFX-32: a surface the previous camera could not see (its
    // reprojected depth at or nearer than that camera's near plane) is
    // disoccluded.
    if (IsAtOrNearerThanNearPlane(CurrDepth, cbCameraAttribs.g_PrevCamera.fNearPlaneDepth, cbCameraAttribs.g_PrevCamera.fFarPlaneDepth)) {
        return 0.0;
    }
    var Disocclusion = 0.0;

    const SearchRadius = 1;
    for (var y = -SearchRadius; y <= SearchRadius; y++)
    {
        for (var x = -SearchRadius; x <= SearchRadius; x++)
        {
            let Location = PrevPositioni + vec2<i32>(x, y);
            let PrevDepth = SamplePrevDepth(Location);
            let Weight = ComputeDepthDisocclusionWeight(CurrDepth, PrevDepth);
            Disocclusion = max(Disocclusion, Weight);
        }
    }

    return select(0.0, 1.0, Disocclusion > TAA_DEPTH_DISOCCLUSION_THRESHOLD);
}

fn SamplePrevColorCatmullRom(Position: vec2<f32>) -> vec4<f32>
{
    // Source: https://advances.realtimerendering.com/s2016/Filmic%20SMAA%20v7.pptx Slide 77

    let TexelSize = cbCameraAttribs.g_CurrCamera.f4ViewportSize.zw;
    let CenterPosition = floor(Position - 0.5) + 0.5;

    let F = Position - CenterPosition;
    let F2 = F * F;
    let F3 = F2 * F;

    let W0 = -0.5 * F3 + F2 - 0.5 * F;
    let W1 = 1.5 * F3 - 2.5 * F2 + 1.0;
    let W2 = -1.5 * F3 + 2.0 * F2 + 0.5 * F;
    let W3 = 0.5 * F3 - 0.5 * F2;
    let W12 = W1 + W2;

    let TexPos0  = (CenterPosition - 1.0) * TexelSize;
    let TexPos3  = (CenterPosition + 2.0) * TexelSize;
    let TexPos12 = (CenterPosition + W2 / W12) * TexelSize;

    let P0 = W12.x * W0.y;
    let P1 = W0.x * W12.y;
    let P2 = W12.x * W12.y;
    let P3 = W3.x * W12.y;
    let P4 = W12.x * W3.y;

    var Result = vec4<f32>(0.0, 0.0, 0.0, 0.0);
    Result += textureSampleLevel(g_TexturePrevColor, g_TexturePrevColor_sampler, vec2<f32>(TexPos12.x, TexPos0.y),  0.0) * P0;
    Result += textureSampleLevel(g_TexturePrevColor, g_TexturePrevColor_sampler, vec2<f32>(TexPos0.x,  TexPos12.y), 0.0) * P1;
    Result += textureSampleLevel(g_TexturePrevColor, g_TexturePrevColor_sampler, vec2<f32>(TexPos12.x, TexPos12.y), 0.0) * P2;
    Result += textureSampleLevel(g_TexturePrevColor, g_TexturePrevColor_sampler, vec2<f32>(TexPos3.x,  TexPos12.y), 0.0) * P3;
    Result += textureSampleLevel(g_TexturePrevColor, g_TexturePrevColor_sampler, vec2<f32>(TexPos12.x, TexPos3.y),  0.0) * P4;

    return max(Result * (1.0 / (P0 + P1 + P2 + P3 + P4)), vec4<f32>(0.0));
}

fn SamplePrevColorBilinear(Position: vec2<f32>) -> vec4<f32>
{
    return max(textureSampleLevel(g_TexturePrevColor, g_TexturePrevColor_sampler, Position * cbCameraAttribs.g_CurrCamera.f4ViewportSize.zw, 0.0), vec4<f32>(0.0));
}

fn SamplePrevColor(Position: vec2<f32>) -> vec4<f32>
{
#if TAA_OPTION_BICUBIC_FILTER
    return SamplePrevColorCatmullRom(Position);
#else
    return SamplePrevColorBilinear(Position);
#endif
}

// Welford's online algorithm:
//  https://en.wikipedia.org/wiki/Algorithms_for_calculating_variance
fn ComputePixelStatisticYCoCgSDR(PixelCoord: vec2<i32>) -> PixelStatistic
{
    var Desc: PixelStatistic;
    var WeightSum = 0.0;
    var M1 = vec3<f32>(0.0, 0.0, 0.0);
    var M2 = vec3<f32>(0.0, 0.0, 0.0);

    const StatisticRadius = 1;
    for (var x = -StatisticRadius; x <= StatisticRadius; x++)
    {
        for (var y = -StatisticRadius; y <= StatisticRadius; y++)
        {
            let Location = ClampScreenCoord(PixelCoord + vec2<i32>(x, y), vec2<i32>(cbCameraAttribs.g_CurrCamera.f4ViewportSize.xy));
            let HDRColor = SampleCurrColor(Location);
            let SDRColor = RGBToYCoCg(HDRToSDR(HDRColor));
#if TAA_OPTION_GAUSSIAN_WEIGHTING
            let Weight = exp(-3.0 * f32(x * x + y * y) / ((f32(StatisticRadius) + 1.0) * (f32(StatisticRadius) + 1.0)));
#else
            let Weight = 1.0;
#endif

            M1 += SDRColor * Weight;
            M2 += SDRColor * SDRColor * Weight;
            WeightSum += Weight;
        }
    }

    Desc.Mean = M1 / WeightSum;
    Desc.Variance = M2 / WeightSum - (Desc.Mean * Desc.Mean);
    Desc.StdDev = sqrt(max(Desc.Variance, vec3<f32>(0.0)));
    return Desc;
}

// PROVENANCE.md DFX-14: Godot's speed-scaled variance box (taa_resolve.glsl
// clip_history_3x3, taa.cpp variance_dynamic) for a pixel whose closest motion
// is Speed screen fractions per frame. Godot's smoothstep(0.02, 0.0, speed) is
// 1 - smoothstep(0.0, 0.02, speed); WGSL requires low < high.
fn ComputeVarianceGamma(Speed: f32) -> f32
{
    let VarianceDynamic = clamp(TAA_VARIANCE_DYNAMIC_BASE * TAA_VARIANCE_DYNAMIC_BASE_HEIGHT / cbCameraAttribs.g_CurrCamera.f4ViewportSize.y,
                                TAA_VARIANCE_DYNAMIC_MIN, TAA_VARIANCE_DYNAMIC_MAX);
    return VarianceDynamic * (1.0 - smoothstep(0.0, TAA_VARIANCE_BOX_ZERO_SPEED, Speed));
}

fn ComputeMaxAlpha(IsStill: bool) -> f32
{
    // PROVENANCE.md DFX-19: still pixels accumulate up to TAA_STILL_HISTORY_FACTOR.
    return select(g_TAAAttribs.TemporalStabilityFactor, TAA_STILL_HISTORY_FACTOR, IsStill);
}

fn ComputeCorrectedAlpha(Alpha: f32, IsStill: bool) -> f32
{
    return min(ComputeMaxAlpha(IsStill), saturate(1.0 / (2.0 - Alpha)));
}

// PROVENANCE.md DFX-13: the accumulated frame, and the closest motion vector the next frame
// reads (DFX-14).
struct PSOutput
{
    @location(0) Color:         vec4<f32>,
    @location(1) ClosestMotion: vec2<f32>,
}

@fragment
fn ComputeTemporalAccumulationPS(VSOut: FullScreenTriangleVSOutput) -> PSOutput
{
    let Position = VSOut.f4PixelPos.xy;
    let ClosestMotion = SampleClosestMotion(vec2<i32>(Position.xy));
    let Motion = ClosestMotion * F3NDC_XYZ_TO_UVD_SCALE.xy;
    let PrevPosition = Position.xy - Motion * cbCameraAttribs.g_CurrCamera.f4ViewportSize.xy;

    if (!IsInsideScreen_f2(PrevPosition, cbCameraAttribs.g_CurrCamera.f4ViewportSize.xy) || g_TAAAttribs.ResetAccumulation != 0u) {
        return PSOutput(vec4<f32>(SampleCurrColor(vec2<i32>(Position)), 0.5), ClosestMotion);
    }

    // PROVENANCE.md DFX-14: Godot's velocity disocclusion (taa_resolve.glsl
    // get_factor_disocclusion), the share of history rejected by the
    // difference in pixels between this pixel's motion and the previous
    // frame's motion where it was.
    let MotionDiffPixels = (Motion - SamplePrevMotion(vec2<i32>(PrevPosition))) * cbCameraAttribs.g_CurrCamera.f4ViewportSize.xy;
    let MotionDiffRejection = saturate((length(MotionDiffPixels) - TAA_MOTION_DIFF_THRESHOLD_PIXELS) * TAA_MOTION_DIFF_REJECTION_PER_PIXEL);
    // PROVENANCE.md DFX-19: nothing moved at a still pixel, and the depth buffers differ only by
    // their jitter, which on sub-pixel geometry would reject its history every few frames.
    let MotionPixels = abs(Motion * cbCameraAttribs.g_CurrCamera.f4ViewportSize.xy);
    let IsStill = MotionPixels.x < TAA_STILL_MOTION_PIXELS && MotionPixels.y < TAA_STILL_MOTION_PIXELS;
    let DepthFactor = select(ComputeDepthDisocclusion(Position, PrevPosition), 1.0, IsStill);

    let RGBHDRCurrColor = SampleCurrColor(vec2<i32>(Position));
    let RGBHDRPrevColor = SamplePrevColor(PrevPosition);

    let YCoCgSDRCurrColor = RGBToYCoCg(HDRToSDR(RGBHDRCurrColor.xyz));
    let YCoCgSDRPrevColor = RGBToYCoCg(HDRToSDR(RGBHDRPrevColor.xyz));

    if (g_TAAAttribs.SkipRejection != 0u)
    {
        let RGBHDROutput = SDRToHDR(YCoCgToRGB(mix(YCoCgSDRCurrColor, YCoCgSDRPrevColor, RGBHDRPrevColor.a)));
        return PSOutput(vec4<f32>(RGBHDROutput, ComputeCorrectedAlpha(RGBHDRPrevColor.a, false)), ClosestMotion);
    }

    let VarianceGamma = ComputeVarianceGamma(length(Motion));
    let PixelStat = ComputePixelStatisticYCoCgSDR(vec2<i32>(Position.xy));
    let YCoCgSDRClampedColor = ClipToAABB(YCoCgSDRPrevColor, PixelStat.Mean, VarianceGamma * PixelStat.StdDev);

    // PROVENANCE.md DFX-14: Godot adds the rejection to its current weight
    // (blend_factor = RPC_16 + factor_disocclusion), so history keeps at most
    // the cap less the rejection. DiligentFX's confidence still ramps the
    // weight up after a reset, and the rejection does not compound with it.
    let Alpha = min(RGBHDRPrevColor.a * DepthFactor, max(ComputeMaxAlpha(IsStill) - MotionDiffRejection, 0.0));
    let RGBHDROutput = SDRToHDR(YCoCgToRGB(mix(YCoCgSDRCurrColor, YCoCgSDRClampedColor, Alpha)));
    return PSOutput(vec4<f32>(RGBHDROutput, ComputeCorrectedAlpha(Alpha, IsStill)), ClosestMotion);
}
