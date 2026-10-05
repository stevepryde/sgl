// WGSL port of DiligentFX Shaders/PostProcess/ScreenSpaceReflection/private/SSR_ComputeSpatialReconstruction.fx
// (https://github.com/DiligentGraphics/DiligentFX, revision
// f26cfe5b901bf180c4a3c9bbd4d5df0b96536d4b). Copyright Diligent Graphics LLC,
// licensed under the Apache License, Version 2.0 (vendor/DiligentFX/License.txt).
// Modified: translated from HLSL to WGSL; see crates/sgl-post-fx/README.md.
// DFX-25 places the reflection's virtual point as AMD's reflection denoiser
// does, ffx-reflection-dnsr/ffx_denoiser_reflections_reproject.h
// (https://github.com/GPUOpen-Effects/FidelityFX-Denoiser, revision
// d7dfecbabe7b9523b14e7b067216e06b86e8d189), MIT licensed
// (LICENSE-amd-fidelityfx-denoiser.txt).

#include "ScreenSpaceReflectionStructures.fxh"
#include "BasicStructures.fxh"
#include "PBR_Common.fxh"
#include "SSR_Common.fxh"
#include "SSR_DenoiserTiles.fxh"
#include "FullScreenTriangleVSOutput.fxh"
#include "PostFX_Common.fxh"

// cbuffer cbCameraAttribs
@group(0) @binding(0) var<uniform> g_Camera: CameraAttribs;

// cbuffer cbScreenSpaceReflectionAttribs
@group(0) @binding(1) var<uniform> g_SSRAttribs: ScreenSpaceReflectionAttribs;

struct PSOutput
{
    @location(0) ResolvedRadiance: vec4<f32>,
    @location(1) ResolvedVariance: f32,
    @location(2) ResolvedDepth:    f32,
}

@group(0) @binding(2) var g_TextureRoughness: texture_2d<f32>;
@group(0) @binding(3) var g_TextureNormal: texture_2d<f32>;
// WGSL: the depth buffer binds as a depth texture.
@group(0) @binding(4) var g_TextureDepth: texture_depth_2d;
@group(0) @binding(5) var g_TextureRayDirectionPDF: texture_2d<f32>;
@group(0) @binding(6) var g_TextureIntersectSpecular: texture_2d<f32>;
// PROVENANCE.md DFX-29: the tiles the denoiser passes work on.
@group(0) @binding(7) var g_TextureDenoiserTiles: texture_2d<f32>;

struct PixelAreaStatistic
{
    Mean:      f32,
    Variance:  f32,
    WeightSum: f32,
    ColorSum:  vec4<f32>,
}

fn LoadRoughness(PixelCoord: vec2<i32>) -> f32
{
    return HlslLoad(g_TextureRoughness, PixelCoord, 0).x;
}

fn LoadNormalWS(PixelCoord: vec2<i32>) -> vec3<f32>
{
    return HlslLoad(g_TextureNormal, PixelCoord, 0).xyz;
}

fn LoadDepth(PixelCoord: vec2<i32>) -> f32
{
    return HlslLoadDepth(g_TextureDepth, PixelCoord, 0);
}

fn ComputeBlurKernelRotation(PixelCoord: vec2<u32>, FrameIndex: u32) -> vec4<f32>
{
    let Angle = Bayer4x4(PixelCoord, FrameIndex);
    return GetRotator(2.0 * M_PI * Angle);
}

fn ComputeWeightRayLength(PixelCoord: vec2<i32>, V: vec3<f32>, N: vec3<f32>, Roughness: f32, NdotV: f32, Weight: f32) -> vec2<f32>
{
    let RayDirectionPDF = HlslLoad(g_TextureRayDirectionPDF, PixelCoord, 0);
    let RayLength = length(RayDirectionPDF.xyz);
    if (RayLength < 1e-6)
    {
        return vec2<f32>(1e-6, 1e-6);
    }
    else
    {
        let RayDirection = RayDirectionPDF.xyz / RayLength;
        let PDF = RayDirectionPDF.w;
        let AlphaRoughness = Roughness * Roughness;

        let L = RayDirection;
        let H = normalize(L + V);

        let NdotH = saturate(dot(N, H));
        let NdotL = saturate(dot(N, L));

        let Vis = SmithGGXVisibilityCorrelated(NdotL, NdotV, AlphaRoughness);
        let D = NormalDistribution_GGX(NdotH, AlphaRoughness);
        var LocalBRDF = Vis * D * NdotL;
        LocalBRDF *= Weight;
        return vec2<f32>(max(LocalBRDF / max(PDF, 1e-5), 1e-6), RayLength);
    }
}

// Weighted incremental variance
// https://en.wikipedia.org/wiki/Algorithms_for_calculating_variance
fn ComputeWeightedVariance(Stat: ptr<function, PixelAreaStatistic>, SampleColor: vec4<f32>, Weight: f32)
{
    (*Stat).ColorSum += Weight * SampleColor;
    (*Stat).WeightSum += Weight;

    let Value = Luminance(SampleColor.rgb);
    let PrevMean = (*Stat).Mean;

    (*Stat).Mean += Weight * (1.0 / (*Stat).WeightSum) * (Value - PrevMean);
    (*Stat).Variance += Weight * (Value - PrevMean) * (Value - (*Stat).Mean);
}

// PROVENANCE.md DFX-25: the depth of the reflection's virtual point, the hit
// distance beyond the surface along the view ray, as AMD's reflection denoiser
// places it (FFX_DNSR_Reflections_GetHitPositionReprojection). Upstream took
// the whole distance for the camera-space Z, which places the point too far by
// the inverse cosine of the ray's angle from the view axis.
fn ComputeResolvedDepth(PositionWS: vec3<f32>, Depth: f32, SurfaceHitDistance: f32) -> f32
{
    let CameraSurfaceDistance = distance(g_Camera.f4Position.xyz, PositionWS);
    let SurfaceCameraZ = DepthToCameraZ(Depth, g_Camera.mProj);
    return CameraZToDepth(SurfaceCameraZ * (CameraSurfaceDistance + SurfaceHitDistance) / max(CameraSurfaceDistance, 1e-6), g_Camera.mProj);
}

fn ScreenSpaceToWorldSpace(ScreenCoordUV: vec3<f32>) -> vec3<f32>
{
    return InvProjectPosition(ScreenCoordUV, g_Camera.mViewProjInv);
}

@fragment
fn ComputeSpatialReconstructionPS(VSOut: FullScreenTriangleVSOutput) -> PSOutput
{
    // samples = 8, min distance = 0.5, average samples on radius = 2
    // WGSL: no unary plus on literals.
    var Poisson: array<vec3<f32>, SSR_SPATIAL_RECONSTRUCTION_SAMPLES>;
    Poisson[0] = vec3<f32>(-0.4706069, -0.4427112, 0.6461146);
    Poisson[1] = vec3<f32>(-0.9057375, 0.3003471, 0.9542373);
    Poisson[2] = vec3<f32>(-0.3487388, 0.4037880, 0.5335386);
    Poisson[3] = vec3<f32>(0.1023042, 0.6439373, 0.6520134);
    Poisson[4] = vec3<f32>(0.5699277, 0.3513750, 0.6695386);
    Poisson[5] = vec3<f32>(0.2939128, -0.1131226, 0.3149309);
    Poisson[6] = vec3<f32>(0.7836658, -0.4208784, 0.8895339);
    Poisson[7] = vec3<f32>(0.1564120, -0.8198990, 0.8346850);

    let Position = VSOut.f4PixelPos;
    let PixelCoord = vec2<i32>(Position.xy);
    // DFX-29: every sample here missed, so the reconstruction is zero.
    if (!IsActiveDenoiserTile(g_TextureDenoiserTiles, PixelCoord)) {
        return PSOutput(vec4<f32>(0.0), 0.0, 0.0);
    }

    let ScreenCoordUV = Position.xy * g_Camera.f4ViewportSize.zw;
    let Depth = LoadDepth(PixelCoord);
    let PositionWS = ScreenSpaceToWorldSpace(vec3<f32>(ScreenCoordUV, Depth));
    let NormalWS = LoadNormalWS(PixelCoord);
    let ViewWS = normalize(g_Camera.f4Position.xyz - PositionWS);
    let NdotV = saturate(dot(NormalWS, ViewWS));

    let Roughness = LoadRoughness(PixelCoord);
    let RoughnessFactor = saturate(f32(SSR_SPATIAL_RECONSTRUCTION_ROUGHNESS_FACTOR) * Roughness);
    let Radius = mix(0.0, min(g_SSRAttribs.SpatialReconstructionRadius, f32(SSR_SPATIAL_RECONSTRUCTION_MAX_RADIUS)), RoughnessFactor);
    let Rotator = ComputeBlurKernelRotation(vec2<u32>(PixelCoord), g_Camera.uiFrameIndex);

    var PixelAreaStat: PixelAreaStatistic;
    PixelAreaStat.ColorSum = vec4<f32>(0.0, 0.0, 0.0, 0.0);
    PixelAreaStat.WeightSum = 0.0;
    PixelAreaStat.Variance = 0.0;
    PixelAreaStat.Mean = 0.0;

    var NearestSurfaceHitDistance = 0.0;

    // TODO: Try to implement sampling from https://youtu.be/MyTOGHqyquU?t=1043
    for (var SampleIdx = 0; SampleIdx < SSR_SPATIAL_RECONSTRUCTION_SAMPLES; SampleIdx++)
    {
        let Xi = RotateVector(Rotator, Poisson[SampleIdx].xy);
#if SSR_OPTION_HALF_RESOLUTION
        let SampleCoord = ClampScreenCoord(vec2<i32>(0.5 * (floor(Position.xy) + Radius * Xi) + vec2<f32>(0.5, 0.5)), vec2<i32>(0.5 * g_Camera.f4ViewportSize.xy));
#else
        let SampleCoord = ClampScreenCoord(vec2<i32>(Position.xy + Radius * Xi), vec2<i32>(g_Camera.f4ViewportSize.xy));
#endif
        let WeightS = ComputeSpatialWeight(Poisson[SampleIdx].z * Poisson[SampleIdx].z, SSR_SPATIAL_RECONSTRUCTION_SIGMA);
        let WeightLength = ComputeWeightRayLength(SampleCoord, ViewWS, NormalWS, Roughness, NdotV, WeightS);
        var SampleColor = HlslLoad(g_TextureIntersectSpecular, SampleCoord, 0);
        // DFX-16: tone-map before averaging so a single bright hit cannot dominate;
        // confidence shares the weight (DFX-17).
        SampleColor = SampleColor / (1.0 + Luminance(SampleColor.rgb));
        ComputeWeightedVariance(&PixelAreaStat, SampleColor, WeightLength.x);

        if (WeightLength.x > 1.0e-6) {
            NearestSurfaceHitDistance = max(WeightLength.y, NearestSurfaceHitDistance);
        }
    }

    var Output: PSOutput;
    Output.ResolvedRadiance = PixelAreaStat.ColorSum / max(PixelAreaStat.WeightSum, 1e-6f);
    // DFX-16: undo the tone mapping.
    Output.ResolvedRadiance = Output.ResolvedRadiance / (1.0 - Luminance(Output.ResolvedRadiance.rgb));
    Output.ResolvedVariance = PixelAreaStat.Variance / max(PixelAreaStat.WeightSum, 1e-6f);
    Output.ResolvedDepth = ComputeResolvedDepth(PositionWS, Depth, NearestSurfaceHitDistance);
    return Output;
}
