// Adapted from DiligentFX SSR_ComputeTemporalAccumulation.fx at
// f26cfe5b901bf180c4a3c9bbd4d5df0b96536d4b. Copyright Diligent Graphics LLC,
// Apache License 2.0 (vendor/DiligentFX/License.txt).
// Camera and motion transport both end in the previous jittered pixel grid.
#include "BasicStructures.fxh"
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

// Matrices include their frame's jitter. A current depth texel already lies
// on the jittered grid: unproject and project its centre without another shift.
fn PreviousCameraPixel(PixelCenter: vec2<f32>, Depth: f32) -> vec2<f32>
{
    let Texcoord = PixelCenter * cbCameraAttribs.g_CurrCamera.f4ViewportSize.zw;
    let PositionWS = InvProjectPosition(vec3<f32>(Texcoord, Depth), cbCameraAttribs.g_CurrCamera.mViewProjInv);
    // PROVENANCE.md DFX-31: a point on or behind the previous camera's plane
    // was not on its screen. It lies a screen off, where ComputeReprojection
    // rejects it, rather than mirrored onto the screen by ProjectPosition's
    // division by its negative w.
    if (IsOnOrBehindCameraPlane(PositionWS, cbCameraAttribs.g_PrevCamera.mViewProj)) {
        return -cbCameraAttribs.g_CurrCamera.f4ViewportSize.xy;
    }
    let PrevCoordUV = ProjectPosition(PositionWS, cbCameraAttribs.g_PrevCamera.mViewProj);
    return PrevCoordUV.xy * cbCameraAttribs.g_CurrCamera.f4ViewportSize.xy;
}

// G-buffer motion is unjittered current minus previous NDC. The difference
// between the sampling grids is added exactly once.
fn PreviousMotionPixel(Pixel: vec2<f32>, MotionNDC: vec2<f32>) -> vec2<f32>
{
    let Curr = cbCameraAttribs.g_CurrCamera;
    let Prev = cbCameraAttribs.g_PrevCamera;
    return Pixel - (MotionNDC + Curr.f2Jitter - Prev.f2Jitter) * F3NDC_XYZ_TO_UVD_SCALE.xy * Curr.f4ViewportSize.xy;
}

fn PixelFootprintOverlap(A: vec2<f32>, B: vec2<f32>) -> f32
{
    let Extent = max(vec2<f32>(1.0) - abs(A - B), vec2<f32>(0.0));
    return Extent.x * Extent.y;
}

// Project the same reflected image with and without its estimated physical
// translation; a point behind the camera supplies no visible correspondence.
fn VirtualImageOverlap(ImageWS: vec3<f32>, Translation: vec3<f32>, SampleOffset: vec2<f32>) -> f32
{
    let Prev = cbCameraAttribs.g_PrevCamera;
    let Before = Prev.mViewProj * vec4<f32>(ImageWS, 1.0);
    let After = Before + Prev.mViewProj * vec4<f32>(Translation, 0.0);
    if (!(Before.w > 0.0 && After.w > 0.0)) { return 0.0; }
    let Offset = (After.xy / After.w - Before.xy / Before.w) * F3NDC_XYZ_TO_UVD_SCALE.xy * Prev.f4ViewportSize.xy;
    return PixelFootprintOverlap(SampleOffset, Offset);
}

fn ComputeDisocclusion(CurrCameraZ_: f32, PrevCameraZ_: f32) -> f32
{
    // WGSL: parameters are immutable; the upstream in-parameters are copied.
    let CurrCameraZ = abs(CurrCameraZ_);
    let PrevCameraZ = abs(PrevCameraZ_);
    return exp(-abs(CurrCameraZ - PrevCameraZ) / max(max(CurrCameraZ, PrevCameraZ), 1e-6));
}
