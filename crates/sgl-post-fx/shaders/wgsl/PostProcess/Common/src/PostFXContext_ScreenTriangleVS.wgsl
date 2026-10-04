// WGSL port of the HLSL ScreenTriangleVS string in DiligentFX PostProcess/Common/src/PostFXContext.cpp
// (https://github.com/DiligentGraphics/DiligentFX, revision
// f26cfe5b901bf180c4a3c9bbd4d5df0b96536d4b). Copyright Diligent Graphics LLC,
// licensed under the Apache License, Version 2.0 (vendor/DiligentFX/License.txt).
// Modified: translated from HLSL to WGSL; see crates/sgl-post-fx/README.md.

struct VSOutput
{
    @builtin(position) Position: vec4<f32>,
    @location(0) Texcoord: vec2<f32>,
}

@vertex
fn main(@builtin(vertex_index) VertexId: u32) -> VSOutput
{
    var VSOut: VSOutput;

    // WGSL: no unary plus on literals.
    var PosXY: array<vec2<f32>, 3>;
    PosXY[0] = vec2<f32>(-1.0, -1.0);
    PosXY[1] = vec2<f32>(-1.0, 3.0);
    PosXY[2] = vec2<f32>(3.0, -1.0);

    let f2XY = PosXY[VertexId % 3u];

    VSOut.Texcoord = vec2<f32>(0.5, 0.5) + vec2<f32>(0.5, -0.5) * f2XY;
    VSOut.Position = vec4<f32>(f2XY, 0.0, 1.0);
    return VSOut;
}
