// WGSL port of DiligentFX Shaders/Common/public/PostFX_Common.fxh
// (https://github.com/DiligentGraphics/DiligentFX, revision
// f26cfe5b901bf180c4a3c9bbd4d5df0b96536d4b). Copyright Diligent Graphics LLC,
// licensed under the Apache License, Version 2.0 (vendor/DiligentFX/License.txt).
// Modified: translated from HLSL to WGSL; see crates/sgl-post-fx/README.md.
// Only the declarations the ported passes use are present, in upstream order
// (CRNG, PCGHash, InitCRND, Rand, CombineRotators, the int2 IsInsideScreen
// overload and SampleCameraZFromDepthUC are omitted).

#ifndef _POST_FX_COMMON_FXH_
#define _POST_FX_COMMON_FXH_

#include "ShaderUtilities.fxh"

#define M_PI                      3.14159265358979
#define M_HALF_PI                 1.57079632679490
#define M_EPSILON                 1e-3
#define M_GOLDEN_RATIO            1.61803398875
#define FLT_EPS                   5.960464478e-8
#define FLT_MAX                   3.402823466e+38
#define FLT_MIN                   1.175494351e-38
// PROVENANCE.md DFX-46: binary16's largest finite value, the bound of the
// colour the effects read and write.
#define HALF_MAX                  65504.0

fn Luminance(Color: vec3<f32>) -> f32
{
    return dot(Color, vec3<f32>(0.299f, 0.587f, 0.114f));
}

fn ComputeHalfResolutionOffset(PixelCoord: vec2<u32>) -> u32
{
    // This is the packed matrix:
    //  0 1 2 3
    //  3 2 1 0
    //  1 0 3 2
    //  2 3 0 1
    let PackedOffsets = 1320229860u;
    let Idx = ((PixelCoord.x & 0x3u) << 3u) + ((PixelCoord.y & 0x3u) << 1u);
    return (PackedOffsets >> Idx) & 0x3u;
}

fn Bayer4x4(SamplePos: vec2<u32>, FrameIndex: u32) -> f32
{
    let SamplePosWrap = SamplePos & vec2<u32>(3u);
    let A = 2068378560u * (1u - (SamplePosWrap.x >> 1u)) + 1500172770u * (SamplePosWrap.x >> 1u);
    let B = (SamplePosWrap.y + ((SamplePosWrap.x & 1u) << 2u)) << 2u;
    let SampleOffset = FrameIndex;
    let Bayer = ((A >> B) + SampleOffset) & 0xFu;
    return f32(Bayer) / 16.0;
}

fn GetRotator(Angle: f32) -> vec4<f32>
{
    // WGSL: sincos is sin and cos.
    let Sin = sin(Angle);
    let Cos = cos(Angle);
    return vec4<f32>(Cos, Sin, -Sin, Cos);
}

fn RotateVector(Rotator: vec4<f32>, Vec: vec2<f32>) -> vec2<f32>
{
    return Vec.x * Rotator.xz + Vec.y * Rotator.yw;
}

fn ProjectPosition(Origin: vec3<f32>, Transform: mat4x4<f32>) -> vec3<f32>
{
    var Projected = Transform * vec4<f32>(Origin, 1.0);
    Projected = vec4<f32>(Projected.xyz / Projected.w, Projected.w);
    Projected = vec4<f32>(NormalizedDeviceXYToTexUV(Projected.xy), Projected.zw);
    Projected.z = NormalizedDeviceZToDepth(Projected.z);
    return Projected.xyz;
}

// PROVENANCE.md DFX-39: the end of the ray is clipped to the near plane (view Z `NearPlaneZ`)
// before it is projected. A ray heading towards the camera from nearer than its length otherwise
// ends behind the camera, where the projection mirrors its direction on the screen and in depth.
// A near plane left at 0 clips just in front of the camera plane, as Godot's bias does.
fn ProjectDirection(Origin: vec3<f32>, Direction: vec3<f32>, OriginSS: vec3<f32>, Mat: mat4x4<f32>, NearPlaneZ: f32) -> vec3<f32>
{
    let ClipZ = max(NearPlaneZ, 1e-5);
    var End = Origin + Direction;
    if (Direction.z < 0.0 && End.z < ClipZ) {
        // An origin on (or by rounding nearer than) the clip plane keeps no length.
        End = Origin + Direction * max((ClipZ - Origin.z) / Direction.z, 0.0);
    }
    return ProjectPosition(End, Mat) - OriginSS;
}

fn InvProjectPosition(Coord_: vec3<f32>, Transform: mat4x4<f32>) -> vec3<f32>
{
    // WGSL: parameters are immutable; the upstream in-parameter is copied.
    var Coord = Coord_;
    Coord = vec3<f32>(TexUVToNormalizedDeviceXY(Coord.xy), Coord.z);
    Coord.z = DepthToNormalizedDeviceZ(Coord.z);
    let Projected = Transform * vec4<f32>(Coord, 1.0);
    return Projected.xyz / Projected.w;
}

fn ScreenXYDepthToViewSpace(Coord: vec3<f32>, Transform: mat4x4<f32>) -> vec3<f32>
{
    let NDC = vec3<f32>(TexUVToNormalizedDeviceXY(Coord.xy), DepthToCameraZ(Coord.z, Transform));
    // DFX-15: remove the projection's off-centre terms (TAA jitter), which ProjectPosition applies.
    return vec3<f32>(NDC.z * (NDC.x - Transform[2][0]) / Transform[0][0], NDC.z * (NDC.y - Transform[2][1]) / Transform[1][1], NDC.z);
}

// WGSL: the float2 overload of IsInsideScreen.
fn IsInsideScreen_f2(PixelCoord: vec2<f32>, Dimension: vec2<f32>) -> bool
{
    return PixelCoord.x >= 0.0 &&
           PixelCoord.y >= 0.0 &&
           PixelCoord.x < Dimension.x &&
           PixelCoord.y < Dimension.y;
}

fn ClampScreenCoord(PixelCoord: vec2<i32>, Dimension: vec2<i32>) -> vec2<i32>
{
    return clamp(PixelCoord, vec2<i32>(0, 0), Dimension - vec2<i32>(1, 1));
}

fn ComputeSpatialWeight(Distance: f32, Sigma: f32) -> f32
{
    return exp(-(Distance) / (2.0 * Sigma * Sigma));
}

// PROVENANCE.md DFX-31 and DFX-32: whether `Position` lies on or behind the
// plane of the camera whose view-projection is `ViewProj` (clip w <= 0),
// where it has no place on that camera's screen or in its depth range.
fn IsOnOrBehindCameraPlane(Position: vec3<f32>, ViewProj: mat4x4<f32>) -> bool
{
    return (ViewProj * vec4<f32>(Position, 1.0)).w <= 0.0;
}

// PROVENANCE.md DFX-32: whether `Depth` lies on or nearer than the near plane
// of a camera whose near and far planes have depths `NearPlaneDepth` and
// `FarPlaneDepth`, where the camera drew nothing. ComputeReprojectedDepth
// writes the previous camera's near-plane depth for a surface on or behind
// that camera.
fn IsAtOrNearerThanNearPlane(Depth: f32, NearPlaneDepth: f32, FarPlaneDepth: f32) -> bool
{
    return (Depth - NearPlaneDepth) * (FarPlaneDepth - NearPlaneDepth) <= 0.0;
}

#endif // _POST_FX_COMMON_FXH_
