// WGSL port of DiligentFX Shaders/PostProcess/ScreenSpaceReflection/private/SSR_ComputeTemporalAccumulation.fx
// (https://github.com/DiligentGraphics/DiligentFX, revision
// f26cfe5b901bf180c4a3c9bbd4d5df0b96536d4b). Copyright Diligent Graphics LLC,
// licensed under the Apache License, Version 2.0 (vendor/DiligentFX/License.txt).
// Modified: translated from HLSL to WGSL; see crates/sgl-post-fx/README.md.
// DFX-63 takes the surface's previous depth from its depth motion and orders
// the fallback search from the reprojected position outwards, as AMD's
// FFX_DNSR_Reflections_PickReprojection (FidelityFX-Denoiser d7dfecb, MIT) and Wicked Engine's
// ssr_temporalCS do.

#include "ScreenSpaceReflectionStructures.fxh"
#include "SSR_Common.fxh"
#include "SSR_DenoiserTiles.fxh"
#include "BasicStructures.fxh"
#include "FullScreenTriangleVSOutput.fxh"

#include "SSR_Reprojection.fxh"

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
// PROVENANCE.md DFX-49: 1 on a frame whose histories were reset (DFX-12).
@group(0) @binding(13) var<uniform> g_HistoryReset: u32;
// PROVENANCE.md DFX-63: this frame's depth, which the depth motion starts from.
@group(0) @binding(14) var g_TextureDepth: texture_depth_2d;
@group(0) @binding(15) var g_TextureNormal: texture_2d<f32>;

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

fn LoadCurrDepth(PixelCoord: vec2<i32>) -> f32
{
    return HlslLoad(g_TextureCurrDepth, PixelCoord, 0).x;
}

// PROVENANCE.md DFX-63: the surface's own depth in the previous camera's
// device depth. Where the motion's depth motion is valid (W > 0) it is the
// surface's view Z now plus that motion (previous minus current view Z,
// NRD's viewZprev = viewZ + motion.z), which holds the surface's own
// movement as well as the camera's, so a surface the camera follows keeps
// its depth; on or behind the previous camera's plane it is that camera's
// near-plane depth, which ComputeReprojection rejects (DFX-32). Elsewhere it
// is the context's reprojected depth, which moves the surface with the
// camera alone, as TAA's adaptive test falls back to it.
fn LoadExpectedPrevDepth(PixelCoord: vec2<i32>) -> f32
{
    let DepthMotion = HlslLoad(g_TextureMotion, PixelCoord, 0).zw;
    if (!(DepthMotion.y > 0.0)) {
        return LoadCurrDepth(PixelCoord);
    }
    let PrevProj = cbCameraAttribs.g_PrevCamera.mProj;
    let PrevCamZ = DepthToCameraZ(HlslLoadDepth(g_TextureDepth, PixelCoord, 0), cbCameraAttribs.g_CurrCamera.mProj) + DepthMotion.x;
    if (!(PrevProj[2][3] * PrevCamZ + PrevProj[3][3] > 0.0)) {
        return cbCameraAttribs.g_PrevCamera.fNearPlaneDepth;
    }
    return CameraZToDepth(PrevCamZ, PrevProj);
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

// TODO: Use normals to compute disocclusion
// Welford's online algorithm:
// https://en.wikipedia.org/wiki/Algorithms_for_calculating_variance
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
    // DFX-43, DFX-63: CurrDepth is the surface's expected depth in the
    // previous camera (LoadExpectedPrevDepth).
    let CurrCamZ = DepthToCameraZ(CurrDepth, cbCameraAttribs.g_PrevCamera.mProj);

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

        // PROVENANCE.md DFX-63: the reprojected position first, then its
        // four edge neighbours, then its corners, and only a strictly better
        // weight replaces the best so far, so a tie keeps the nearer
        // position, as AMD's and Wicked's searches keep their reprojected
        // one. Upstream's rows from -1 took the upper-left neighbour on a
        // uniform surface. Nine positions: a fixed list, no data extends it.
        const SearchOffsets = array<vec2<f32>, 9>(
            vec2<f32>(0.0, 0.0),
            vec2<f32>(0.0, -1.0), vec2<f32>(-1.0, 0.0), vec2<f32>(1.0, 0.0), vec2<f32>(0.0, 1.0),
            vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, -1.0), vec2<f32>(-1.0, 1.0), vec2<f32>(1.0, 1.0),
        );
        const BestTotalWeightEarlyExitThreshold = 0.9;
        for (var i = 0; i < 9; i++)
        {
            let Location = PrevPos + SearchOffsets[i];

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
    // PROVENANCE.md DFX-32: a surface the previous camera could not see (its
    // reprojected depth at or nearer than that camera's near plane) has no
    // history.
    Desc.IsSuccess = Desc.IsSuccess && !IsAtOrNearerThanNearPlane(CurrDepth, cbCameraAttribs.g_PrevCamera.fNearPlaneDepth, cbCameraAttribs.g_PrevCamera.fFarPlaneDepth);
    return Desc;
}

// The previous plane is measured at the history sample that passed depth
// testing. A mirror image translates twice the plane's offset along N.
// A plane sliding tangentially remains the same plane.
fn VirtualPlaneSupport(Position: vec2<f32>, Depth: f32, HitDepth: f32, RasterHit: vec2<f32>, SampleOffset: vec2<f32>) -> f32
{
    let Curr = cbCameraAttribs.g_CurrCamera;
    let Prev = cbCameraAttribs.g_PrevCamera;
    let SamplePixel = vec2<i32>(RasterHit);
    let OldDepth = LoadPrevDepth(SamplePixel);
    let N = HlslLoad(g_TextureNormal, vec2<i32>(Position), 0).xyz;
    if (IsBackground(OldDepth) || !(abs(dot(N, N) - 1.0) <= SSR_NORMAL_LENGTH_SQ_TOLERANCE)) { return 0.0; }
    let UV = Position * Curr.f4ViewportSize.zw;
    let CurrentWS = InvProjectPosition(vec3<f32>(UV, Depth), Curr.mViewProjInv);
    let OldUV = (vec2<f32>(SamplePixel) + 0.5) * Prev.f4ViewportSize.zw;
    let OldWS = InvProjectPosition(vec3<f32>(OldUV, OldDepth), Prev.mViewProjInv);
    let Normal = normalize(N);
    let Translation = 2.0 * dot(OldWS - CurrentWS, Normal) * Normal;
    let VirtualWS = InvProjectPosition(vec3<f32>(UV, HitDepth), Curr.mViewProjInv);
    return VirtualImageOverlap(VirtualWS, Translation, SampleOffset);
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
    let HitDepth = LoadHitDepth(vec2<i32>(Position.xy));
    let Pixel = vec2<i32>(Position.xy);
    let Current = LoadCurrRadiance(Pixel);
    let CurrentLuminance = Luminance(Current.rgb);
    let Motion = HlslLoad(g_TextureMotion, Pixel, 0);
    let Curr = cbCameraAttribs.g_CurrCamera;
    let Prev = cbCameraAttribs.g_PrevCamera;
    let NdcToPixels = F3NDC_XYZ_TO_UVD_SCALE.xy * Curr.f4ViewportSize.xy;
    // Both bilinear history reads use the stable Diligent history grid. For
    // projection, add the current jitter before unprojecting and remove the
    // previous jitter when returning to that grid.
    let PrevIncidentPoint = Position.xy - Motion.xy * NdcToPixels;
    let PrevReflectionHit = PreviousCameraPixel(Position.xy + Curr.f2Jitter * NdcToPixels, HitDepth)
                            - Prev.f2Jitter * NdcToPixels;

    let PrevColorIncidentPoint = SamplePrevRadianceLinear(PrevIncidentPoint);
    let PrevColorReflectionHit = SamplePrevRadianceLinear(PrevReflectionHit);

    let PrevDistanceIncidentPoint = abs(Luminance(PrevColorIncidentPoint.rgb) - CurrentLuminance);
    let PrevDistanceReflectionHit = abs(Luminance(PrevColorReflectionHit.rgb) - CurrentLuminance);

    // PROVENANCE.md DFX-57: a clamp-to-edge sample outside the previous
    // viewport is not a candidate, even when its luminance fits better.
    // This also excludes DFX-31's behind-camera sentinel.
    let IncidentInside = IsInsideScreen_f2(PrevIncidentPoint, cbCameraAttribs.g_CurrCamera.f4ViewportSize.xy);
    let HitInside = IsInsideScreen_f2(PrevReflectionHit, cbCameraAttribs.g_CurrCamera.f4ViewportSize.xy);
    let SurfaceDepth = HlslLoadDepth(g_TextureDepth, Pixel, 0);
    // A zero or unresolvable secondary distance puts the virtual image at
    // the receiver's stored depth. Prefer its measured motion when on-screen.
    // Compare in the format used for the virtual depth (DFX-59).
#if SSR_OPTION_INVERTED_DEPTH
    let HasSeparateHit = quantizeToF16(HitDepth) != quantizeToF16(SurfaceDepth);
#else
    let HasSeparateHit = HitDepth != SurfaceDepth;
#endif
    let UseIncidentPoint = !HitInside || (IncidentInside && (!HasSeparateHit || PrevDistanceIncidentPoint < PrevDistanceReflectionHit));
    let PrevCoord = select(PrevReflectionHit, PrevIncidentPoint, UseIncidentPoint);
    let ExpectedPrevDepth = LoadExpectedPrevDepth(Pixel);
    let Reprojection = ComputeReprojection(PrevCoord, ExpectedPrevDepth);

    var Output: PSOutput;
    // PROVENANCE.md DFX-49: a reset history is no history, as where reprojection fails: the
    // frame takes the current radiance, as TAA's resolve does on a reset.
    if (g_HistoryReset == 0u && Reprojection.IsSuccess)
    {
        let ColorMin = PixelStat.Mean - SSR_TEMPORAL_VARIANCE_GAMMA * PixelStat.StdDev;
        let ColorMax = PixelStat.Mean + SSR_TEMPORAL_VARIANCE_GAMMA * PixelStat.StdDev;
        let PrevRadiance = clamp(Reprojection.Color, ColorMin, ColorMax);
        let PrevVariance = SamplePrevVarianceLinear(Reprojection.PrevCoord);
        // A moving receiver can still carry valid history when its reflected
        // image follows it. Otherwise measure how much of the previous pixel the
        // camera alone would have covered. An offscreen alternative supplies no
        // competing correspondence; keep the candidate that passed depth testing.
        var ReceiverWeight = 1.0;
        if (HasSeparateHit && IncidentInside && HitInside) {
            // The measured depth belongs to the raster pixel. Both sides of
            // this comparison must include the same jitter displacement.
            let StaticSurface = PreviousCameraPixel(Position.xy, SurfaceDepth);
            let MeasuredSurface = PreviousMotionPixel(Position.xy, Motion.xy);
            ReceiverWeight = max(PixelFootprintOverlap(MeasuredSurface, StaticSurface),
                                 PixelFootprintOverlap(PrevIncidentPoint, PrevReflectionHit));
            if (!UseIncidentPoint && Motion.w > 0.0 && ReceiverWeight < 1.0) {
                let RasterHit = Reprojection.PrevCoord + Prev.f2Jitter * NdcToPixels;
                if (IsInsideScreen_f2(RasterHit, Prev.f4ViewportSize.xy)) {
                    let PlaneSupport = VirtualPlaneSupport(Position.xy, SurfaceDepth, HitDepth, RasterHit,
                                                          Reprojection.PrevCoord - PrevReflectionHit);
                    ReceiverWeight = max(ReceiverWeight, PlaneSupport);
                }
            }
        }
        let SourceWeight = 1.0 - HlslLoad(g_TextureCurrVariance, Pixel, 0).g;
        Output.Radiance = mix(Current, PrevRadiance,
                              g_SSRAttribs.TemporalRadianceStabilityFactor * ReceiverWeight * SourceWeight);
        Output.Variance = mix(LoadCurrVariance(vec2<i32>(Position.xy)), PrevVariance, g_SSRAttribs.TemporalVarianceStabilityFactor);
    }
    else
    {
        Output.Radiance = Current;
        Output.Variance = 1.0f;
    }
    return Output;
}
