// WGSL port of the HLSL CopyTexturePS string in DiligentFX PostProcess/Common/src/PostFXContext.cpp
// (https://github.com/DiligentGraphics/DiligentFX, revision
// f26cfe5b901bf180c4a3c9bbd4d5df0b96536d4b). Copyright Diligent Graphics LLC,
// licensed under the Apache License, Version 2.0 (vendor/DiligentFX/License.txt).
// Modified: translated from HLSL to WGSL; see crates/sgl-post-fx/README.md.
// WGSL: g_Texture's type is a permutation. CopyTextureDepth and
// ComputePreviousDepth read depth buffers, which bind as depth textures
// (COPY_TEXTURE_DEPTH); CopyTextureColor reads a float texture.

struct PSInput
{
    @builtin(position) Position: vec4<f32>,
    @location(0) Texcoord: vec2<f32>,
}

#if COPY_TEXTURE_DEPTH
@group(0) @binding(0) var g_Texture: texture_depth_2d;
#else
@group(0) @binding(0) var g_Texture: texture_2d<f32>;
#endif
@group(0) @binding(1) var g_Texture_sampler: sampler;

@fragment
fn main(PSIn: PSInput) -> @location(0) vec4<f32>
{
#if COPY_TEXTURE_DEPTH
    // WGSL: a depth texture samples to a scalar; HLSL returns it in .x.
    return vec4<f32>(textureSample(g_Texture, g_Texture_sampler, PSIn.Texcoord), 0.0, 0.0, 0.0);
#else
    return textureSample(g_Texture, g_Texture_sampler, PSIn.Texcoord);
#endif
}
