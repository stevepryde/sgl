// WGSL port of DiligentFX Shaders/Common/private/FullScreenTriangleVSOutput.fxh
// (https://github.com/DiligentGraphics/DiligentFX, revision
// f26cfe5b901bf180c4a3c9bbd4d5df0b96536d4b). Copyright Diligent Graphics LLC,
// licensed under the Apache License, Version 2.0 (vendor/DiligentFX/License.txt).
// Modified: translated from HLSL to WGSL; see crates/sgl-post-fx/README.md.

struct FullScreenTriangleVSOutput
{
    @builtin(position) f4PixelPos: vec4<f32>, // Pixel position on the screen
    @location(0) f2NormalizedXY: vec2<f32>, // Normalized device XY coordinates [-1,1]x[-1,1]
    // WGSL: integer stage variables are flat-interpolated.
    @location(1) @interpolate(flat) uInstID: u32,
}
