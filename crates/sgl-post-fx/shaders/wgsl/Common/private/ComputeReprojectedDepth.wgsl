// WGSL port of DiligentFX Shaders/Common/private/ComputeReprojectedDepth.fx
// (https://github.com/DiligentGraphics/DiligentFX, revision
// f26cfe5b901bf180c4a3c9bbd4d5df0b96536d4b). Copyright Diligent Graphics LLC,
// licensed under the Apache License, Version 2.0 (vendor/DiligentFX/License.txt).
// Modified: translated from HLSL to WGSL; see crates/sgl-post-fx/README.md.

#include "BasicStructures.fxh"
#include "FullScreenTriangleVSOutput.fxh"
#include "PostFX_Common.fxh"

// WGSL: one uniform holds the constant buffer's two cameras, so the upstream
// g_CurrCamera and g_PrevCamera read as cbCameraAttribs.g_CurrCamera and
// cbCameraAttribs.g_PrevCamera.
struct CameraAttribsPair
{
    g_CurrCamera: CameraAttribs,
    g_PrevCamera: CameraAttribs,
}
@group(0) @binding(0) var<uniform> cbCameraAttribs: CameraAttribsPair;

// WGSL: the depth buffer binds as a depth texture.
@group(0) @binding(1) var g_TextureDepth: texture_depth_2d;

fn SampleDepth(PixelCoord: vec2<i32>) -> f32
{
    return HlslLoadDepth(g_TextureDepth, PixelCoord, 0);
}

@fragment
fn ComputeReprojectedDepthPS(VSOut: FullScreenTriangleVSOutput) -> @location(0) f32
{
    let Position = VSOut.f4PixelPos;
    let Depth = SampleDepth(vec2<i32>(Position.xy));
    var CurrScreenCoord = vec3<f32>(Position.xy * cbCameraAttribs.g_CurrCamera.f4ViewportSize.zw, Depth);
    CurrScreenCoord = vec3<f32>(CurrScreenCoord.xy + F3NDC_XYZ_TO_UVD_SCALE.xy * cbCameraAttribs.g_CurrCamera.f2Jitter, CurrScreenCoord.z);
    let WorldPosition = InvProjectPosition(CurrScreenCoord, cbCameraAttribs.g_CurrCamera.mViewProjInv);
    // PROVENANCE.md DFX-32: a surface on or behind the previous camera's plane
    // (clip w <= 0) had no depth in its frame; dividing by a negative w would
    // mirror it to a depth as far in front. It takes the previous camera's
    // near-plane depth, which the temporal passes read as disoccluded
    // (IsAtOrNearerThanNearPlane).
    if (IsOnOrBehindCameraPlane(WorldPosition, cbCameraAttribs.g_PrevCamera.mViewProj)) {
        return cbCameraAttribs.g_PrevCamera.fNearPlaneDepth;
    }
    let PrevScreenCoord = ProjectPosition(WorldPosition, cbCameraAttribs.g_PrevCamera.mViewProj);
    return PrevScreenCoord.z;
}
