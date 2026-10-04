// WGSL port of DiligentFX Shaders/Common/private/FullScreenTriangleVS.fx
// (https://github.com/DiligentGraphics/DiligentFX, revision
// f26cfe5b901bf180c4a3c9bbd4d5df0b96536d4b). Copyright Diligent Graphics LLC,
// licensed under the Apache License, Version 2.0 (vendor/DiligentFX/License.txt).
// Modified: translated from HLSL to WGSL; see crates/sgl-post-fx/README.md.

#include "FullScreenTriangleVSOutput.fxh"

@vertex
fn FullScreenTriangleVS(@builtin(vertex_index)   VertexId: u32,
                        @builtin(instance_index) InstID: u32) -> FullScreenTriangleVSOutput
{
    var VSOut: FullScreenTriangleVSOutput;

    // WGSL: no unary plus on literals.
    var PosXY: array<vec2<f32>, 3>;
    PosXY[0] = vec2<f32>(-1.0, -1.0);
    PosXY[1] = vec2<f32>(-1.0, 3.0);
    PosXY[2] = vec2<f32>(3.0, -1.0);

    let f2XY = PosXY[VertexId % 3u];

    VSOut.f2NormalizedXY = f2XY;
    // We use VertexId trick on old hardware that does not support BaseInstance.
    VSOut.uInstID = select(VertexId / 3u, InstID, InstID != 0u);

#ifdef TRIANGLE_DEPTH
    let z = DepthToNormalizedDeviceZ(TRIANGLE_DEPTH);
#else
    // Write 0 to the depth buffer
    // NDC_MIN_Z ==  0 in DX
    // NDC_MIN_Z == -1 in GL
    let z = NDC_MIN_Z;
#endif

    VSOut.f4PixelPos = vec4<f32>(f2XY, z, 1.0);
    return VSOut;
}
