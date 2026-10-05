// Ports AMD FidelityFX Denoiser d7dfecbabe7b9523b14e7b067216e06b86e8d189,
// ffx-shadows-dnsr/ffx_denoiser_shadows_util.h, to WGSL, with upstream's
// names and order (src/LICENSE-amd-fidelityfx-denoiser.txt):
/**********************************************************************
Copyright (c) 2021 Advanced Micro Devices, Inc. All rights reserved.

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in
all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT.  IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
THE SOFTWARE.
********************************************************************/
// Changed: translated to WGSL.

fn FFX_DNSR_Shadows_RoundedDivide(value:u32,divisor:u32)->u32 {
 return (value+divisor-1u)/divisor;
}

fn FFX_DNSR_Shadows_GetTileIndexFromPixelPosition(pixel_pos:vec2<u32>)->vec2<u32> {
 return vec2(pixel_pos.x/8u,pixel_pos.y/4u);
}

fn FFX_DNSR_Shadows_LinearTileIndex(tile_index:vec2<u32>,screen_width:u32)->u32 {
 return tile_index.y*FFX_DNSR_Shadows_RoundedDivide(screen_width,8u)+tile_index.x;
}

fn FFX_DNSR_Shadows_GetBitMaskFromPixelPosition(pixel_pos:vec2<u32>)->u32 {
 let lane_index=(pixel_pos.y%4u)*8u+(pixel_pos.x%8u);
 return 1u<<lane_index;
}

const TILE_META_DATA_CLEAR_MASK:u32=1u;
const TILE_META_DATA_LIGHT_MASK:u32=2u;

// From ffx_a.h

fn FFX_DNSR_Shadows_BitfieldExtract(src:u32,off:u32,bits:u32)->u32 {
 let mask=(1u<<bits)-1u;
 return (src>>off)&mask;
}
fn FFX_DNSR_Shadows_BitfieldInsert(src:u32,ins:u32,bits:u32)->u32 {
 let mask=(1u<<bits)-1u;
 return (ins&mask)|(src&(~mask));
}

//  LANE TO 8x8 MAPPING
//  ===================
//  00 01 08 09 10 11 18 19
//  02 03 0a 0b 12 13 1a 1b
//  04 05 0c 0d 14 15 1c 1d
//  06 07 0e 0f 16 17 1e 1f
//  20 21 28 29 30 31 38 39
//  22 23 2a 2b 32 33 3a 3b
//  24 25 2c 2d 34 35 3c 3d
//  26 27 2e 2f 36 37 3e 3f
fn FFX_DNSR_Shadows_RemapLane8x8(lane:u32)->vec2<u32> {
 return vec2(FFX_DNSR_Shadows_BitfieldInsert(FFX_DNSR_Shadows_BitfieldExtract(lane,2u,3u),lane,1u),FFX_DNSR_Shadows_BitfieldInsert(FFX_DNSR_Shadows_BitfieldExtract(lane,3u,3u),FFX_DNSR_Shadows_BitfieldExtract(lane,1u,2u),2u));
}
