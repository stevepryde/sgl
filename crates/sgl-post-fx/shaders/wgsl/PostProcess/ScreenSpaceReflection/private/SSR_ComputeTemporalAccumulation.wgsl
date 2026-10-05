// WGSL port of DiligentFX Shaders/PostProcess/ScreenSpaceReflection/private/SSR_ComputeTemporalAccumulation.fx
// (https://github.com/DiligentGraphics/DiligentFX, revision
// f26cfe5b901bf180c4a3c9bbd4d5df0b96536d4b). Copyright Diligent Graphics LLC,
// licensed under the Apache License, Version 2.0 (vendor/DiligentFX/License.txt).
// Modified: translated from HLSL to WGSL; see crates/sgl-post-fx/README.md.
// DFX-25 adds the surface-reprojection discard of AMD's reflection denoiser,
// ffx-reflection-dnsr/ffx_denoiser_reflections_reproject.h
// (https://github.com/GPUOpen-Effects/FidelityFX-Denoiser, revision
// d7dfecbabe7b9523b14e7b067216e06b86e8d189), MIT licensed
// (LICENSE-amd-fidelityfx-denoiser.txt).

#include "ScreenSpaceReflectionStructures.fxh"
#include "SSR_Common.fxh"
#include "SSR_DenoiserTiles.fxh"
#include "BasicStructures.fxh"
#include "FullScreenTriangleVSOutput.fxh"

// WGSL: one uniform holds the constant buffer's two cameras, so the upstream
// g_CurrCamera and g_PrevCamera read as cbCameraAttribs.g_CurrCamera and
// cbCameraAttribs.g_PrevCamera.
struct CameraAttribsPair
{
    g_CurrCamera: CameraAttribs,
    g_PrevCamera: CameraAttribs,
}
@group(0) @binding(0) var<uniform> cbCameraAttribs: CameraAttribsPair;

// cbuffer cbScreenSpaceReflectionAttribs
@group(0) @binding(1) var<uniform> g_SSRAttribs: ScreenSpaceReflectionAttribs;

@group(0) @binding(2) var g_TextureMotion: texture_2d<f32>;
@group(0) @binding(3) var g_TextureHitDepth: texture_2d<f32>;

@group(0) @binding(4) var g_TextureCurrDepth: texture_2d<f32>;
@group(0) @binding(5) var g_TextureCurrRadiance: texture_2d<f32>;
@group(0) @binding(6) var g_TextureCurrVariance: texture_2d<f32>;

@group(0) @binding(7) var g_TexturePrevDepth: texture_2d<f32>;
@group(0) @binding(8) var g_TexturePrevRadiance: texture_2d<f32>;
@group(0) @binding(9) var g_TexturePrevVariance: texture_2d<f32>;

// WGSL: g_TexturePrevDepth_sampler is only declared upstream (depth is loaded).
@group(0) @binding(10) var g_TexturePrevRadiance_sampler: sampler;
@group(0) @binding(11) var g_TexturePrevVariance_sampler: sampler;
// PROVENANCE.md DFX-29.
@group(0) @binding(12) var g_TextureDenoiserTiles: texture_2d<f32>;

struct ProjectionDesc
{
    Color:     vec4<f32>,
    PrevCoord: vec2<f32>,
    IsSuccess: bool,
}

struct PixelStatistic
{
    Mean:     vec4<f32>,
    Variance: vec4<f32>,
    StdDev:   vec4<f32>,
}

struct PSOutput
{
    @location(0) Radiance: vec4<f32>,
    @location(1) Variance: f32,
}

fn LoadMotion(PixelCoord: vec2<i32>) -> vec2<f32>
{
    return HlslLoad(g_TextureMotion, PixelCoord, 0).xy * F3NDC_XYZ_TO_UVD_SCALE.xy;
}

fn LoadCurrDepth(PixelCoord: vec2<i32>) -> f32
{
    return HlslLoad(g_TextureCurrDepth, PixelCoord, 0).x;
}

fn LoadPrevDepth(PixelCoord: vec2<i32>) -> f32
{
    return HlslLoad(g_TexturePrevDepth, PixelCoord, 0).x;
}

fn LoadHitDepth(PixelCoord: vec2<i32>) -> f32
{
    return HlslLoad(g_TextureHitDepth, PixelCoord, 0).x;
}

fn LoadCurrRadiance(PixelCoord: vec2<i32>) -> vec4<f32>
{
    return HlslLoad(g_TextureCurrRadiance, PixelCoord, 0);
}

fn LoadCurrVariance(PixelCoord: vec2<i32>) -> f32
{
    return HlslLoad(g_TextureCurrVariance, PixelCoord, 0).x;
}

fn LoadPrevRadiance(PixelCoord: vec2<i32>) -> vec4<f32>
{
    return HlslLoad(g_TexturePrevRadiance, PixelCoord, 0);
}

fn LoadPrevVariance(PixelCoord: vec2<i32>) -> f32
{
    return HlslLoad(g_TexturePrevVariance, PixelCoord, 0).x;
}

fn SamplePrevRadianceLinear(PixelCoord: vec2<f32>) -> vec4<f32>
{
    let Texcoord = PixelCoord * cbCameraAttribs.g_CurrCamera.f4ViewportSize.zw;
    return textureSampleLevel(g_TexturePrevRadiance, g_TexturePrevRadiance_sampler, Texcoord, 0.0);
}

fn SamplePrevVarianceLinear(PixelCoord: vec2<f32>) -> f32
{
    let Texcoord = PixelCoord * cbCameraAttribs.g_CurrCamera.f4ViewportSize.zw;
    return textureSampleLevel(g_TexturePrevVariance, g_TexturePrevVariance_sampler, Texcoord, 0.0).x;
}

fn ComputeReflectionHitPosition(PixelCoord: vec2<i32>, Depth: f32) -> vec2<f32>
{
    let Texcoord = (vec2<f32>(PixelCoord) + 0.5) * cbCameraAttribs.g_CurrCamera.f4ViewportSize.zw + F3NDC_XYZ_TO_UVD_SCALE.xy * cbCameraAttribs.g_CurrCamera.f2Jitter;
    let PositionWS = InvProjectPosition(vec3<f32>(Texcoord, Depth), cbCameraAttribs.g_CurrCamera.mViewProjInv);
    // PROVENANCE.md DFX-31: a point on or behind the previous camera's plane
    // was not on its screen. It lies a screen off, where ComputeReprojection
    // rejects it, rather than mirrored onto the screen by ProjectPosition's
    // division by its negative w.
    if ((cbCameraAttribs.g_PrevCamera.mViewProj * vec4<f32>(PositionWS, 1.0)).w <= 0.0) {
        return -cbCameraAttribs.g_CurrCamera.f4ViewportSize.xy;
    }
    let PrevCoordUV = ProjectPosition(PositionWS, cbCameraAttribs.g_PrevCamera.mViewProj);
    return (PrevCoordUV.xy - F3NDC_XYZ_TO_UVD_SCALE.xy * cbCameraAttribs.g_PrevCamera.f2Jitter) * cbCameraAttribs.g_CurrCamera.f4ViewportSize.xy;
}

// TODO: Use normals to compute disocclusion
fn ComputeDisocclusion(CurrCameraZ_: f32, PrevCameraZ_: f32) -> f32
{
    // WGSL: parameters are immutable; the upstream in-parameters are copied.
    let CurrCameraZ = abs(CurrCameraZ_);
    let PrevCameraZ = abs(PrevCameraZ_);
    return exp(-abs(CurrCameraZ - PrevCameraZ) / max(max(CurrCameraZ, PrevCameraZ), 1e-6));
}

// Welford's online algorithm:
//  https://en.wikipedia.org/wiki/Algorithms_for_calculating_variance
fn ComputePixelStatistic(PixelCoord: vec2<i32>) -> PixelStatistic
{
    var Desc: PixelStatistic;
    var M1 = vec4<f32>(0.0, 0.0, 0.0, 0.0);
    var M2 = vec4<f32>(0.0, 0.0, 0.0, 0.0);

    const StatisticRadius = 1;
    for (var x = -StatisticRadius; x <= StatisticRadius; x++)
    {
        for (var y = -StatisticRadius; y <= StatisticRadius; y++)
        {
            let Location = ClampScreenCoord(PixelCoord + vec2<i32>(x, y), vec2<i32>(cbCameraAttribs.g_CurrCamera.f4ViewportSize.xy));
            let SampleColor = HlslLoad(g_TextureCurrRadiance, Location, 0);

            M1 += SampleColor;
            M2 += SampleColor * SampleColor;
        }
    }

    Desc.Mean = M1 / 9.0;
    Desc.Variance = (M2 / 9.0) - (Desc.Mean * Desc.Mean);
    Desc.StdDev = sqrt(max(Desc.Variance, vec4<f32>(0.0f)));
    return Desc;
}

fn ComputeReprojection(PrevPos: vec2<f32>, CurrDepth: f32) -> ProjectionDesc
{
    let CurrCamZ = DepthToCameraZ(CurrDepth, cbCameraAttribs.g_CurrCamera.mProj);

    var Desc: ProjectionDesc;

    {
        let PrevCamZ = DepthToCameraZ(LoadPrevDepth(vec2<i32>(PrevPos)), cbCameraAttribs.g_PrevCamera.mProj);
        Desc.PrevCoord = PrevPos;
        Desc.Color     = SamplePrevRadianceLinear(Desc.PrevCoord);
        Desc.IsSuccess = ComputeDisocclusion(CurrCamZ, PrevCamZ) > SSR_DISOCCLUSION_THRESHOLD;
    }

    let DepthDim = vec2<i32>(textureDimensions(g_TextureCurrDepth));
    if (!Desc.IsSuccess)
    {
        var BestWeights     = vec4<f32>(0.0, 0.0, 0.0, 0.0);
        var BestFetchCoords = vec4<i32>(0, 0, 0, 0);
        var BestTotalWeight = 0.0;

        const SearchRadius = 1;
        const BestTotalWeightEarlyExitThreshold = 0.9;
        for (var y = -SearchRadius; y <= SearchRadius; y++)
        {
            for (var x = -SearchRadius; x <= SearchRadius; x++)
            {
                let Location = PrevPos + vec2<f32>(f32(x), f32(y));

                var FetchCoords: vec4<i32>;
                var Weights: vec4<f32>;
                GetBilinearSamplingInfoUC(Location, DepthDim, &FetchCoords, &Weights);

                let PrevZ00 = DepthToCameraZ(LoadPrevDepth(FetchCoords.xy), cbCameraAttribs.g_PrevCamera.mProj);
                let PrevZ10 = DepthToCameraZ(LoadPrevDepth(FetchCoords.zy), cbCameraAttribs.g_PrevCamera.mProj);
                let PrevZ01 = DepthToCameraZ(LoadPrevDepth(FetchCoords.xw), cbCameraAttribs.g_PrevCamera.mProj);
                let PrevZ11 = DepthToCameraZ(LoadPrevDepth(FetchCoords.zw), cbCameraAttribs.g_PrevCamera.mProj);

                Weights.x *= select(0.0, 1.0, ComputeDisocclusion(CurrCamZ, PrevZ00) > (SSR_DISOCCLUSION_THRESHOLD / 2.0));
                Weights.y *= select(0.0, 1.0, ComputeDisocclusion(CurrCamZ, PrevZ10) > (SSR_DISOCCLUSION_THRESHOLD / 2.0));
                Weights.z *= select(0.0, 1.0, ComputeDisocclusion(CurrCamZ, PrevZ01) > (SSR_DISOCCLUSION_THRESHOLD / 2.0));
                Weights.w *= select(0.0, 1.0, ComputeDisocclusion(CurrCamZ, PrevZ11) > (SSR_DISOCCLUSION_THRESHOLD / 2.0));

                let TotalWeight = dot(Weights, vec4<f32>(1.0, 1.0, 1.0, 1.0));
                if (TotalWeight > BestTotalWeight)
                {
                    BestTotalWeight = TotalWeight;
                    BestWeights     = Weights;
                    BestFetchCoords = FetchCoords;
                    Desc.PrevCoord  = Location;

                    if (BestTotalWeight > BestTotalWeightEarlyExitThreshold) {
                        break;
                    }
                }
            }

            if (BestTotalWeight > BestTotalWeightEarlyExitThreshold) {
                break;
            }
        }

        Desc.IsSuccess = BestTotalWeight > 0.1;
        if (Desc.IsSuccess)
        {
            Desc.Color = (
                LoadPrevRadiance(BestFetchCoords.xy) * BestWeights.x +
                LoadPrevRadiance(BestFetchCoords.zy) * BestWeights.y +
                LoadPrevRadiance(BestFetchCoords.xw) * BestWeights.z +
                LoadPrevRadiance(BestFetchCoords.zw) * BestWeights.w
            ) / BestTotalWeight;
        }
    }

    Desc.IsSuccess = Desc.IsSuccess && IsInsideScreen_f2(Desc.PrevCoord, cbCameraAttribs.g_CurrCamera.f4ViewportSize.xy);
    return Desc;
}

@fragment
fn ComputeTemporalAccumulationPS(VSOut: FullScreenTriangleVSOutput) -> PSOutput
{
    let Position = VSOut.f4PixelPos;

    // DFX-29: the current neighbourhood is zero, which clamps any history to
    // zero. The radiance history holds that zero, so no older reflection
    // returns with the hits, and the variance holds 1, as where reprojection
    // finds no history.
    if (!IsActiveDenoiserTile(g_TextureDenoiserTiles, vec2<i32>(Position.xy))) {
        return PSOutput(vec4<f32>(0.0), 1.0);
    }

    // Secondary reprojection based on ray lengths:
    // https://www.ea.com/seed/news/seed-dd18-presentation-slides-raytracing (Slide 45)
    let PixelStat = ComputePixelStatistic(vec2<i32>(Position.xy));
    let Depth = LoadCurrDepth(vec2<i32>(Position.xy));
    let HitDepth = LoadHitDepth(vec2<i32>(Position.xy));
    let Motion = LoadMotion(vec2<i32>(Position.xy));

    let PrevIncidentPoint = Position.xy - Motion * vec2<f32>(cbCameraAttribs.g_CurrCamera.f4ViewportSize.xy);
    let PrevReflectionHit = ComputeReflectionHitPosition(vec2<i32>(Position.xy), HitDepth);

    let PrevColorIncidentPoint = SamplePrevRadianceLinear(PrevIncidentPoint);
    let PrevColorReflectionHit = SamplePrevRadianceLinear(PrevReflectionHit);

    let PrevDistanceIncidentPoint = abs(Luminance(PrevColorIncidentPoint.rgb) - Luminance(PixelStat.Mean.rgb));
    let PrevDistanceReflectionHit = abs(Luminance(PrevColorReflectionHit.rgb) - Luminance(PixelStat.Mean.rgb));

    let UseIncidentPoint = PrevDistanceIncidentPoint < PrevDistanceReflectionHit;
    let PrevCoord = select(PrevReflectionHit, PrevIncidentPoint, UseIncidentPoint);
    let Reprojection = ComputeReprojection(PrevCoord, Depth);

    // PROVENANCE.md DFX-25: AMD's reflection denoiser keeps the surface
    // (incident point) reprojection only while it lies near the current
    // neighbourhood; otherwise the reflection moved by its own parallax and the
    // history is rejected (FFX_DNSR_Reflections_PickReprojection).
    let IncidentOffset = PrevColorIncidentPoint.rgb - PixelStat.Mean.rgb;
    let IncidentAgrees = dot(IncidentOffset, IncidentOffset) < SSR_REPROJECT_SURFACE_DISCARD_VARIANCE_WEIGHT * length(PixelStat.Variance.rgb);

    var Output: PSOutput;
    if (Reprojection.IsSuccess && (!UseIncidentPoint || IncidentAgrees))
    {
        let ColorMin = PixelStat.Mean - SSR_TEMPORAL_VARIANCE_GAMMA * PixelStat.StdDev;
        let ColorMax = PixelStat.Mean + SSR_TEMPORAL_VARIANCE_GAMMA * PixelStat.StdDev;
        let PrevRadiance = clamp(Reprojection.Color, ColorMin, ColorMax);
        let PrevVariance = SamplePrevVarianceLinear(Reprojection.PrevCoord);
        Output.Radiance = mix(LoadCurrRadiance(vec2<i32>(Position.xy)), PrevRadiance, g_SSRAttribs.TemporalRadianceStabilityFactor);
        Output.Variance = mix(LoadCurrVariance(vec2<i32>(Position.xy)), PrevVariance, g_SSRAttribs.TemporalVarianceStabilityFactor);
    }
    else
    {
        Output.Radiance = LoadCurrRadiance(vec2<i32>(Position.xy));
        Output.Variance = 1.0f;
    }
    return Output;
}
