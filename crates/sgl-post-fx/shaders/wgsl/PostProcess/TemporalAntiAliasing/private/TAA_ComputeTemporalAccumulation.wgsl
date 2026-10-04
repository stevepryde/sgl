// WGSL port of DiligentFX Shaders/PostProcess/TemporalAntiAliasing/private/TAA_ComputeTemporalAccumulation.fx
// (https://github.com/DiligentGraphics/DiligentFX, revision
// f26cfe5b901bf180c4a3c9bbd4d5df0b96536d4b). Copyright Diligent Graphics LLC,
// licensed under the Apache License, Version 2.0 (vendor/DiligentFX/License.txt).
// Modified: translated from HLSL to WGSL; see crates/sgl-post-fx/README.md.
// DFX-19 adds the still-pixel history of Bevy's TAA,
// crates/bevy_anti_alias/src/taa/taa.wesl (https://github.com/bevyengine/bevy,
// revision 92a29e701a6b0bf8846484c3999c2ba97d90dd06), MIT licensed
// (LICENSE-bevy.txt).

#include "BasicStructures.fxh"
#include "FullScreenTriangleVSOutput.fxh"
#include "PostFX_Common.fxh"
#include "TemporalAntiAliasingStructures.fxh"

#define FLT_EPS 5.960464478e-8

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
@group(0) @binding(4) var g_TextureMotion: texture_2d<f32>;
@group(0) @binding(5) var g_TextureCurrDepth: texture_2d<f32>;
@group(0) @binding(6) var g_TexturePrevDepth: texture_2d<f32>;

@group(0) @binding(7) var g_TexturePrevColor_sampler: sampler;

// PROVENANCE.md DFX-14: the previous frame's closest motion vectors.
@group(0) @binding(8) var g_TexturePrevMotion: texture_2d<f32>;

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

fn SampleMotion(PixelCoord: vec2<i32>) -> vec2<f32>
{
    return HlslLoad(g_TextureMotion, PixelCoord, 0).xy * F3NDC_XYZ_TO_UVD_SCALE.xy;
}

// PROVENANCE.md DFX-14.
fn SamplePrevMotion(PixelCoord: vec2<i32>) -> vec2<f32>
{
    return HlslLoad(g_TexturePrevMotion, PixelCoord, 0).xy * F3NDC_XYZ_TO_UVD_SCALE.xy;
}

fn ClipToAABB(ColorPrev: vec3<f32>, ColorCurr: vec3<f32>, AABBCentre: vec3<f32>, AABBExtents: vec3<f32>) -> vec3<f32>
{
    let MaxT = TAA_VARIANCE_INTERSECTION_MAX_T;
    let Direction = ColorCurr - ColorPrev;
    let Intersection = ((AABBCentre - sign(Direction) * AABBExtents) - ColorPrev) / Direction;
    let PossibleT = mix(vec3<f32>(MaxT + 1.0, MaxT + 1.0, MaxT + 1.0), Intersection, vec3<f32>(Intersection >= vec3<f32>(0.0, 0.0, 0.0)));
    let T = min(MaxT, min(PossibleT.x, min(PossibleT.y, PossibleT.z)));
    return mix(ColorPrev, ColorPrev + Direction * T, vec3<f32>(vec3<f32>(T, T, T) < vec3<f32>(MaxT, MaxT, MaxT)));
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

fn ComputeCorrectedAlpha(Alpha: f32, IsStill: bool) -> f32
{
    // PROVENANCE.md DFX-19: still pixels accumulate up to TAA_STILL_HISTORY_FACTOR.
    let MaxAlpha = select(g_TAAAttribs.TemporalStabilityFactor, TAA_STILL_HISTORY_FACTOR, IsStill);
    return min(MaxAlpha, saturate(1.0 / (2.0 - Alpha)));
}

@fragment
fn ComputeTemporalAccumulationPS(VSOut: FullScreenTriangleVSOutput) -> @location(0) vec4<f32>
{
    let Position = VSOut.f4PixelPos.xy;
    let Motion = SampleMotion(vec2<i32>(Position.xy));
    let PrevPosition = Position.xy - Motion * cbCameraAttribs.g_CurrCamera.f4ViewportSize.xy;

    if (!IsInsideScreen_f2(PrevPosition, cbCameraAttribs.g_CurrCamera.f4ViewportSize.xy) || g_TAAAttribs.ResetAccumulation != 0u) {
        return vec4<f32>(SampleCurrColor(vec2<i32>(Position)), 0.5);
    }

    let AspectRatio = cbCameraAttribs.g_CurrCamera.f4ViewportSize.x * cbCameraAttribs.g_CurrCamera.f4ViewportSize.w;
    let MotionFactor = saturate(1.0 - length(vec2<f32>(Motion.x * AspectRatio, Motion.y)) * TAA_MOTION_VECTOR_DIFF_FACTOR);
    // PROVENANCE.md DFX-14: history is rejected by the difference between
    // this pixel's motion and the previous frame's motion where it was, as
    // TAA_MOTION_VECTOR_DIFF_FACTOR documents. The speed alone (MotionFactor)
    // still sets the variance gamma.
    let MotionDiff = Motion - SamplePrevMotion(vec2<i32>(PrevPosition));
    let MotionDiffFactor = saturate(1.0 - length(vec2<f32>(MotionDiff.x * AspectRatio, MotionDiff.y)) * TAA_MOTION_VECTOR_DIFF_FACTOR);
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
        return vec4<f32>(RGBHDROutput, ComputeCorrectedAlpha(RGBHDRPrevColor.a, false));
    }

    let VarianceGamma = mix(TAA_MIN_VARIANCE_GAMMA, TAA_MAX_VARIANCE_GAMMA, MotionFactor * MotionFactor);
    let PixelStat = ComputePixelStatisticYCoCgSDR(vec2<i32>(Position.xy));
    let YCoCgSDRClampedColor = ClipToAABB(YCoCgSDRPrevColor, YCoCgSDRCurrColor, PixelStat.Mean, VarianceGamma * PixelStat.StdDev);

    let Alpha = RGBHDRPrevColor.a * MotionDiffFactor * DepthFactor;
    let RGBHDROutput = SDRToHDR(YCoCgToRGB(mix(YCoCgSDRCurrColor, YCoCgSDRClampedColor, Alpha)));
    return vec4<f32>(RGBHDROutput, ComputeCorrectedAlpha(Alpha, IsStill));
}
