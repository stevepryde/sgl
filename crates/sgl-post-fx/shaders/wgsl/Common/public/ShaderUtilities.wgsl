// WGSL port of DiligentFX Shaders/Common/public/ShaderUtilities.fxh
// (https://github.com/DiligentGraphics/DiligentFX, revision
// f26cfe5b901bf180c4a3c9bbd4d5df0b96536d4b). Copyright Diligent Graphics LLC,
// licensed under the Apache License, Version 2.0 (vendor/DiligentFX/License.txt).
// Modified: translated from HLSL to WGSL; see crates/sgl-post-fx/README.md.
// Only the functions the ported passes use are present, in upstream order.

#ifndef _SHADER_UTILITIES_FXH_
#define _SHADER_UTILITIES_FXH_

// Transforms camera-space Z to normalized device z coordinate
fn CameraZToNormalizedDeviceZ(CameraZ: f32, mProj: mat4x4<f32>) -> f32
{
    // In Direct3D and Vulkan, normalized device z range is [0, +1]
    // In OpengGL, normalized device z range is [-1, +1] (unless GL_ARB_clip_control extension is used to correct this nonsense).
    let m22 = mProj[2][2];
    let m32 = mProj[3][2];
    let m23 = mProj[2][3];
    let m33 = mProj[3][3];
    return (m22 * CameraZ + m32) / (m23 * CameraZ + m33);
}

fn CameraZToDepth(CameraZ: f32, mProj: mat4x4<f32>) -> f32
{
    // Transformations to/from normalized device coordinates are the
    // same in both APIs.
    // However, in GL, depth must be transformed to NDC Z first
    return NormalizedDeviceZToDepth(CameraZToNormalizedDeviceZ(CameraZ, mProj));
}

fn NormalizedDeviceZToCameraZ(NdcZ: f32, mProj: mat4x4<f32>) -> f32
{
    let m22 = mProj[2][2];
    let m32 = mProj[3][2];
    let m23 = mProj[2][3];
    let m33 = mProj[3][3];
    return (m32 - NdcZ * m33) / (NdcZ * m23 - m22);
}

fn DepthToCameraZ(fDepth: f32, mProj: mat4x4<f32>) -> f32
{
    // Transformations to/from normalized device coordinates are the
    // same in both APIs.
    // However, in GL, depth must be transformed to NDC Z first
    return NormalizedDeviceZToCameraZ(DepthToNormalizedDeviceZ(fDepth), mProj);
}

/// Returns bilinear sampling information for unnormailzed coordinates.
///
/// \param [in]  Location    - Unnormalized location in the texture space.
/// \param [in]  Dimensions  - Texture dimensions.
/// \param [out] FetchCoords - Texture coordinates to fetch the data from:
///                            (x, y) - lower left corner
///                            (z, w) - upper right corner
/// \param [out] Weights     - Bilinear interpolation weights.
///
/// \remarks    The filtering should be done as follows:
///                 Tex.Load(FetchCoords.xy) * Weights.x +
///                 Tex.Load(FetchCoords.zy) * Weights.y +
///                 Tex.Load(FetchCoords.xw) * Weights.z +
///                 Tex.Load(FetchCoords.zw) * Weights.w
fn GetBilinearSamplingInfoUC(Location_: vec2<f32>,
                             Dimensions_: vec2<i32>,
                             FetchCoords: ptr<function, vec4<i32>>,
                             Weights: ptr<function, vec4<f32>>)
{
    // WGSL: parameters are immutable; the upstream in-parameters are copied.
    var Location = Location_;
    var Dimensions = Dimensions_;
    Location -= vec2<f32>(0.5, 0.5);
    let Location00 = floor(Location);
    (*FetchCoords) = vec4<i32>(vec2<i32>(Location00), vec2<i32>(Location00) + vec2<i32>(1, 1));
    Dimensions -= vec2<i32>(1, 1);
    (*FetchCoords) = clamp(*FetchCoords, vec4<i32>(0, 0, 0, 0), Dimensions.xyxy);

    let x = Location.x - Location00.x;
    let y = Location.y - Location00.y;
    (*Weights) = vec4<f32>(1.0 - x, x, 1.0 - x, x) * vec4<f32>(1.0 - y, 1.0 - y, y, y);
}

#endif //_SHADER_UTILITIES_FXH_
