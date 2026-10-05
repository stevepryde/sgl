// WGSL port of DiligentFX Shaders/PostProcess/ScreenSpaceReflection/private/SSR_ComputeBilateralCleanup.fx
// (https://github.com/DiligentGraphics/DiligentFX, revision
// f26cfe5b901bf180c4a3c9bbd4d5df0b96536d4b). Copyright Diligent Graphics LLC,
// licensed under the Apache License, Version 2.0 (vendor/DiligentFX/License.txt).
// Modified: translated from HLSL to WGSL; see crates/sgl-post-fx/README.md.

#include "ScreenSpaceReflectionStructures.fxh"
#include "BasicStructures.fxh"
#include "SSR_Common.fxh"
#include "FullScreenTriangleVSOutput.fxh"

// cbuffer cbCameraAttribs
@group(0) @binding(0) var<uniform> g_Camera: CameraAttribs;

// cbuffer cbScreenSpaceReflectionAttribs
@group(0) @binding(1) var<uniform> g_SSRAttribs: ScreenSpaceReflectionAttribs;

// WGSL: the depth buffer binds as a depth texture.
@group(0) @binding(2) var g_TextureDepth: texture_depth_2d;
@group(0) @binding(3) var g_TextureNormal: texture_2d<f32>;
@group(0) @binding(4) var g_TextureRoughness: texture_2d<f32>;

@group(0) @binding(5) var g_TextureRadiance: texture_2d<f32>;
@group(0) @binding(6) var g_TextureVariance: texture_2d<f32>;
// PROVENANCE.md DFX-29.
@group(0) @binding(7) var g_TextureDenoiserTiles: texture_2d<f32>;

fn LoadDepth(PixelCoord: vec2<i32>) -> f32
{
    return HlslLoadDepth(g_TextureDepth, PixelCoord, 0);
}

fn LoadRoughness(PixelCoord: vec2<i32>) -> f32
{
    return HlslLoad(g_TextureRoughness, PixelCoord, 0).x;
}

fn LoadNormalWS(PixelCoord: vec2<i32>) -> vec3<f32>
{
    return HlslLoad(g_TextureNormal, PixelCoord, 0).xyz;
}

fn LoadRadiance(PixelCoord: vec2<i32>) -> vec4<f32>
{
    return HlslLoad(g_TextureRadiance, PixelCoord, 0);
}

fn LoadVariance(PixelCoord: vec2<i32>) -> f32
{
    return HlslLoad(g_TextureVariance, PixelCoord, 0).x;
}

@fragment
fn ComputeBilateralCleanupPS(VSOut: FullScreenTriangleVSOutput) -> @location(0) vec4<f32>
{
    let PixelCoord = vec2<i32>(VSOut.f4PixelPos.xy);

    let Roughness = LoadRoughness(PixelCoord);
    let Variance  = LoadVariance(PixelCoord);
    let NormalWS  = LoadNormalWS(PixelCoord);
    let CameraZ   = DepthToCameraZ(LoadDepth(PixelCoord), g_Camera.mProj);
    let GradCamZ  = vec2<f32>(dpdx(CameraZ), dpdy(CameraZ));

    // DFX-29: every radiance in reach is zero. After the derivatives, which
    // need uniform control flow.
    if (!IsActiveDenoiserTile(g_TextureDenoiserTiles, PixelCoord)) {
        return vec4<f32>(0.0);
    }

    let RoughnessTarget = saturate(f32(SSR_BILATERAL_ROUGHNESS_FACTOR) * Roughness);
    let Radius = mix(0.0, select(0.0, 2.0, Variance > SSS_BILATERAL_VARIANCE_ESTIMATE_THRESHOLD), RoughnessTarget);
    let Sigma = g_SSRAttribs.BilateralCleanupSpatialSigmaFactor;
    let EffectiveRadius = i32(min(2.0 * Sigma, Radius));
    var RadianceResult = LoadRadiance(PixelCoord);

    if (Variance > SSR_BILATERAL_VARIANCE_EXIT_THRESHOLD && EffectiveRadius > 0)
    {
        var ColorSum = vec4<f32>(0.0, 0.0, 0.0, 0.0);
        var WeightSum = 0.0f;
        for (var x = -EffectiveRadius; x <= EffectiveRadius; x++)
        {
            for (var y = -EffectiveRadius; y <= EffectiveRadius; y++)
            {
                let Location = ClampScreenCoord(PixelCoord + vec2<i32>(x, y), vec2<i32>(g_Camera.f4ViewportSize.xy));

                let SampledDepth     = LoadDepth(Location);
                let SampledRoughness = LoadRoughness(Location);
                let SampledRadiance  = LoadRadiance(Location);
                let SampledNormalWS  = LoadNormalWS(Location);

                if (IsReflectionSample(SampledRoughness, SampledDepth, g_SSRAttribs.RoughnessThreshold))
                {
                    let SampledCameraZ = DepthToCameraZ(SampledDepth, g_Camera.mProj);
                    let WeightS = exp(-0.5 * dot(vec2<f32>(f32(x), f32(y)), vec2<f32>(f32(x), f32(y))) / (Sigma * Sigma));
                    let WeightZ = exp(-abs(CameraZ - SampledCameraZ) / (SSR_BILATERAL_SIGMA_DEPTH * (abs(dot(vec2<f32>(f32(x), f32(y)), GradCamZ)) + 1e-6)));
                    let WeightN = pow(max(0.0, dot(NormalWS, SampledNormalWS)), SSR_BILATERAL_SIGMA_NORMAL);
                    let Weight = WeightS * WeightN * WeightZ;

                    WeightSum += Weight;
                    ColorSum  += Weight * SampledRadiance;
                }
            }
        }

        RadianceResult = ColorSum / max(WeightSum, 1.0e-6f);
    }

    // DFX-17: the transition fades premultiplied radiance with its confidence.
    return RadianceResult * g_SSRAttribs.AlphaInterpolation;
}
