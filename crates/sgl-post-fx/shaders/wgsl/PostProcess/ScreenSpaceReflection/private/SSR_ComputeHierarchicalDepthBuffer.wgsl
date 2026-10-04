// WGSL port of DiligentFX Shaders/PostProcess/ScreenSpaceReflection/private/SSR_ComputeHierarchicalDepthBuffer.fx
// (https://github.com/DiligentGraphics/DiligentFX, revision
// f26cfe5b901bf180c4a3c9bbd4d5df0b96536d4b). Copyright Diligent Graphics LLC,
// licensed under the Apache License, Version 2.0 (vendor/DiligentFX/License.txt).
// Modified: translated from HLSL to WGSL; see crates/sgl-post-fx/README.md.
// WGSL: only the SUPPORTED_SHADER_SRV path; wgpu always has texture
// subresource views (the other path is WebGL's).

#include "SSR_Common.fxh"
#include "FullScreenTriangleVSOutput.fxh"

@group(0) @binding(0) var g_TextureLastMip: texture_2d<f32>;

fn LoadDepth(Location: vec2<i32>, Dimension: vec3<i32>) -> f32
{
    let Position = ClampScreenCoord(Location, Dimension.xy);
    return HlslLoad(g_TextureLastMip, Position, 0).x;
}

fn UpdateClosestDepth(Location: vec2<i32>, LastMipDimension: vec3<i32>, MinDepth: ptr<function, f32>)
{
    let Depth = LoadDepth(Location, LastMipDimension);
    *MinDepth = ClosestDepth(*MinDepth, Depth);
}

@fragment
fn ComputeHierarchicalDepthBufferPS(VSOut: FullScreenTriangleVSOutput) -> @location(0) f32
{
    var LastMipDimension: vec3<i32>;
    LastMipDimension = vec3<i32>(vec2<i32>(textureDimensions(g_TextureLastMip)), 0); // z unused

    let RemappedPosition = vec2<i32>(2.0 * floor(VSOut.f4PixelPos.xy));

    var MinDepth: f32 = DepthFarPlane;
    UpdateClosestDepth(RemappedPosition + vec2<i32>(0, 0), LastMipDimension, &MinDepth);
    UpdateClosestDepth(RemappedPosition + vec2<i32>(0, 1), LastMipDimension, &MinDepth);
    UpdateClosestDepth(RemappedPosition + vec2<i32>(1, 0), LastMipDimension, &MinDepth);
    UpdateClosestDepth(RemappedPosition + vec2<i32>(1, 1), LastMipDimension, &MinDepth);

    let IsWidthOdd  = (LastMipDimension.x & 1) != 0;
    let IsHeightOdd = (LastMipDimension.y & 1) != 0;

    if (IsWidthOdd)
    {
        UpdateClosestDepth(RemappedPosition + vec2<i32>(2, 0), LastMipDimension, &MinDepth);
        UpdateClosestDepth(RemappedPosition + vec2<i32>(2, 1), LastMipDimension, &MinDepth);
    }

    if (IsHeightOdd)
    {
        UpdateClosestDepth(RemappedPosition + vec2<i32>(0, 2), LastMipDimension, &MinDepth);
        UpdateClosestDepth(RemappedPosition + vec2<i32>(1, 2), LastMipDimension, &MinDepth);
    }

    if (IsWidthOdd && IsHeightOdd)
    {
        UpdateClosestDepth(RemappedPosition + vec2<i32>(2, 2), LastMipDimension, &MinDepth);
    }

    return MinDepth;
}
