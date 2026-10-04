// WGSL port of DiligentFX Shaders/Common/private/ComputeBlueNoiseTexture.fx
// (https://github.com/DiligentGraphics/DiligentFX, revision
// f26cfe5b901bf180c4a3c9bbd4d5df0b96536d4b). Copyright Diligent Graphics LLC,
// licensed under the Apache License, Version 2.0 (vendor/DiligentFX/License.txt).
// Modified: translated from HLSL to WGSL; see crates/sgl-post-fx/README.md.

#include "BasicStructures.fxh"
#include "FullScreenTriangleVSOutput.fxh"
#include "PostFX_Common.fxh"

#define HILBERT_LEVEL 7u
#define HILBERT_WIDTH (1u << HILBERT_LEVEL)

struct PSOutput
{
    @location(0) BlueNoiseXY: vec2<f32>,
    @location(1) BlueNoiseZW: vec2<f32>,
}

@group(0) @binding(0) var g_SobolBuffer: texture_2d<u32>;
@group(0) @binding(1) var g_ScramblingTileBuffer: texture_2d<u32>;

// Blue Noise Sampler by Eric Heitz. Returns a value in the range [0, 1].
fn SampleRandomNumber(PixelCoord_: vec2<u32>, SampleDimension_: u32) -> f32
{
    // Wrap arguments
    // WGSL: parameters are immutable; the upstream in-parameters are copied.
    let PixelCoord = PixelCoord_ & vec2<u32>(127u);
    let SampleDimension = SampleDimension_ & 255u;

    // Fetch value in sequence
    var Value = HlslLoadUint(g_SobolBuffer, vec2<i32>(vec2<u32>(SampleDimension, 0u)), 0).x;

    // If the dimension is optimized, xor sequence value based on optimized scrambling
    let OriginalIndex = (SampleDimension % 8u) + (PixelCoord.x + PixelCoord.y * 128u) * 8u;
    Value = Value ^ HlslLoadUint(g_ScramblingTileBuffer, vec2<i32>(vec2<u32>(OriginalIndex % 512u, OriginalIndex / 512u)), 0).x; // TODO: AMD doesn't support integer division

    return (f32(Value) + 0.5f) / 256.0f;
}

fn HilbertIndex(PixelCoord_: vec2<u32>) -> u32
{
    // WGSL: parameters are immutable; the upstream in-parameter is copied.
    var PixelCoord = PixelCoord_ & vec2<u32>(HILBERT_WIDTH - 1u);
    var Index = 0u;
    for (var CurLevel = HILBERT_WIDTH / 2u; CurLevel > 0u; CurLevel /= 2u)
    {
        let RegionX = u32((PixelCoord.x & CurLevel) > 0u);
        let RegionY = u32((PixelCoord.y & CurLevel) > 0u);
        Index += CurLevel * CurLevel * ((3u * RegionX) ^ RegionY);
        if (RegionY == 0u)
        {
            if (RegionX == 1u)
            {
                PixelCoord.x = u32((HILBERT_WIDTH - 1u)) - PixelCoord.x;
                PixelCoord.y = u32((HILBERT_WIDTH - 1u)) - PixelCoord.y;
            }

            let Temp = PixelCoord.x;
            PixelCoord.x = PixelCoord.y;
            PixelCoord.y = Temp;
        }
    }
    return Index;
}

// Roberts R1 sequence see - https://extremelearning.com.au/unreasonable-effectiveness-of-quasirandom-sequences/
fn SampleRandomVector2D(PixelCoord: vec2<u32>, FrameIndex: u32) -> vec2<f32>
{
    let G = 1.61803398875f;
    let Alpha = 0.5 + (1.0 / G) * f32(FrameIndex & 0xFFu);
    return vec2<f32>(
        fract(SampleRandomNumber(PixelCoord, 0u) + Alpha),
        fract(SampleRandomNumber(PixelCoord, 1u) + Alpha)
    );
}

 // R2 sequence - see http://extremelearning.com.au/unreasonable-effectiveness-of-quasirandom-sequences/
fn SampleRandomVector1D1D(PixelCoord: vec2<u32>, FrameIndex: u32) -> vec2<f32>
{
    var Index = (HilbertIndex(PixelCoord) + FrameIndex);
    Index += 288u * (FrameIndex & (HILBERT_WIDTH - 1u));

    let G = 1.32471795724474602596;
    let Alpha = vec2<f32>(1.0 / G, 1.0 / (G * G));
    return fract(vec2<f32>(0.5, 0.5) + f32(Index) * Alpha);
}

@fragment
fn ComputeBlueNoiseTexturePS(VSOut: FullScreenTriangleVSOutput) -> PSOutput
{
    let FrameIndex = VSOut.uInstID;

    var Output: PSOutput;
    Output.BlueNoiseXY = SampleRandomVector2D(vec2<u32>(VSOut.f4PixelPos.xy), FrameIndex);
    Output.BlueNoiseZW = SampleRandomVector1D1D(vec2<u32>(VSOut.f4PixelPos.xy), FrameIndex);
    return Output;
}
