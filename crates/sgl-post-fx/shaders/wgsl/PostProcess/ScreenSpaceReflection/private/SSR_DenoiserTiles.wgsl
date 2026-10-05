// SGL's (PROVENANCE.md DFX-29): the tiles SSR's denoiser passes work on, as
// AMD FidelityFX SSSR's ClassifyTiles lists tiles for its denoiser.
// SSR_DENOISER_TILE_SIZE, the side of a tile in pixels, is a shader macro
// whose owner is the host's DENOISER_TILE_SIZE (screen_space_reflection.rs).

#ifndef _SSR_DENOISER_TILES_FXH_
#define _SSR_DENOISER_TILES_FXH_

// The classification marks blocks of half a tile's side, so one gather reads
// a tile's 2×2 blocks.
#define SSR_DENOISER_BLOCK_SIZE (SSR_DENOISER_TILE_SIZE / 2)

fn IsActiveDenoiserTile(DenoiserTiles: texture_2d<f32>, PixelCoord: vec2<i32>) -> bool
{
    return HlslLoad(DenoiserTiles, PixelCoord / SSR_DENOISER_TILE_SIZE, 0).x > 0.0;
}

#endif // _SSR_DENOISER_TILES_FXH_
