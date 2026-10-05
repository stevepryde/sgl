// SGL's (PROVENANCE.md DFX-29): the passes that find the tiles SSR's denoiser
// works on, run between the intersection and spatial reconstruction, as AMD
// FidelityFX SSSR keeps ClassifyTiles in a pass of its own
// (sdk/include/FidelityFX/gpu/sssr/ffx_sssr_classify_tiles.h,
// https://github.com/GPUOpen-LibrariesAndSDKs/FidelityFX-SDK, revision
// c6efa6bf7f2027b3ec94f28578bb5965eabb9e55). No AMD code is copied.

#include "ScreenSpaceReflectionStructures.fxh"
#include "SSR_Common.fxh"
#include "SSR_DenoiserTiles.fxh"
#include "FullScreenTriangleVSOutput.fxh"

@group(0) @binding(0) var<uniform> g_SSRAttribs: ScreenSpaceReflectionAttribs;
@group(0) @binding(1) var g_TextureIntersectSpecular: texture_2d<f32>;
// Per block, whether a ray in it found a confident hit.
@group(0) @binding(2) var g_TextureDenoiserHits: texture_2d<f32>;
@group(0) @binding(3) var g_LinearClamp: sampler;

// How far a confident hit reaches through the denoiser passes, in pixels
// along each axis, conservatively:
// - spatial reconstruction reads rays within SpatialReconstructionRadius
//   times a Poisson offset shorter than 1, rounded, a pixel; at half
//   resolution a ray covers two pixels and the rounding goes towards +x/+y,
//   a pixel more;
// - temporal accumulation's 3×3 neighbourhood statistic, a pixel;
// - bilateral cleanup's kernel, i32(min(2 sigma, Radius)) pixels, its Radius
//   at most 2.
fn DenoiserReach() -> i32
{
#if SSR_OPTION_HALF_RESOLUTION
    let Rounding = 2;
#else
    let Rounding = 1;
#endif
    let Spatial = i32(ceil(g_SSRAttribs.SpatialReconstructionRadius)) + Rounding;
    let Bilateral = min(i32(2.0 * g_SSRAttribs.BilateralCleanupSpatialSigmaFactor), 2);
    return Spatial + 1 + Bilateral;
}

// Whether any ray of a block found a confident hit. One fragment per block;
// each gather reads 2×2 rays.
@fragment
fn ClassifyDenoiserTilesPS(VSOut: FullScreenTriangleVSOutput) -> @location(0) f32
{
#if SSR_OPTION_HALF_RESOLUTION
    const Rays = SSR_DENOISER_BLOCK_SIZE / 2;
#else
    const Rays = SSR_DENOISER_BLOCK_SIZE;
#endif
    let Size = vec2<f32>(textureDimensions(g_TextureIntersectSpecular));
    let Origin = floor(VSOut.f4PixelPos.xy) * f32(Rays);
    var Confidence = vec4<f32>(0.0);
    for (var y = 1; y < Rays; y += 2)
    {
        for (var x = 1; x < Rays; x += 2)
        {
            let Corner = Origin + vec2<f32>(f32(x), f32(y));
            Confidence = max(Confidence, textureGather(3, g_TextureIntersectSpecular, g_LinearClamp, Corner / Size));
        }
    }
    return select(0.0, 1.0, any(Confidence > vec4<f32>(0.0)));
}

// Whether a hit lies within DenoiserReach of a tile. One fragment per tile;
// one gather reads a tile's 2×2 blocks. Tiles more than TileReach apart are
// at least 8 TileReach + 1 pixels apart, beyond the reach.
@fragment
fn DilateDenoiserTilesPS(VSOut: FullScreenTriangleVSOutput) -> @location(0) f32
{
    let TileReach = (DenoiserReach() + SSR_DENOISER_TILE_SIZE - 1) / SSR_DENOISER_TILE_SIZE;
    let Size = vec2<f32>(textureDimensions(g_TextureDenoiserHits));
    let Corner = floor(VSOut.f4PixelPos.xy) * 2.0 + 1.0;
    var Hits = vec4<f32>(0.0);
    for (var y = -TileReach; y <= TileReach; y++)
    {
        for (var x = -TileReach; x <= TileReach; x++)
        {
            Hits = max(Hits, textureGather(0, g_TextureDenoiserHits, g_LinearClamp, (Corner + 2.0 * vec2<f32>(f32(x), f32(y))) / Size));
        }
    }
    return select(0.0, 1.0, any(Hits > vec4<f32>(0.0)));
}
