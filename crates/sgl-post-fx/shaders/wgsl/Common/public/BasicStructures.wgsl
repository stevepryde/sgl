// WGSL port of DiligentFX Shaders/Common/public/BasicStructures.fxh
// (https://github.com/DiligentGraphics/DiligentFX, revision
// f26cfe5b901bf180c4a3c9bbd4d5df0b96536d4b). Copyright Diligent Graphics LLC,
// licensed under the Apache License, Version 2.0 (vendor/DiligentFX/License.txt).
// Modified: translated from HLSL to WGSL; see crates/sgl-post-fx/README.md.
// CascadeAttribs, ShadowMapAttribs and LightAttribs are unused by the ported
// passes and omitted. The layout equals the HLSL constant buffer layout (576
// bytes), src/structures.rs mirrors it.

#ifndef _BASIC_STRUCTURES_FXH_
#define _BASIC_STRUCTURES_FXH_

#include "ShaderDefinitions.fxh"

struct CameraAttribs
{
    f4Position:     vec4<f32>, // Camera world position
    f4ViewportSize: vec4<f32>, // (width, height, 1/width, 1/height)

    fNearPlaneZ:     f32,
    fFarPlaneZ:      f32, // fNearPlaneZ < fFarPlaneZ
    fNearPlaneDepth: f32,
    fFarPlaneDepth:  f32,

    // Tight scene bounds
    fSceneNearZ:     f32,
    fSceneFarZ:      f32, // fSceneNearZ < fSceneFarZ
    fSceneNearDepth: f32,
    fSceneFarDepth:  f32,

    fHandness:    f32, // +1.0 for right-handed coordinate system, -1.0 for left-handed
    uiFrameIndex: u32,
    Padding0:     f32,
    Padding1:     f32,

    // Distance to the point of focus
    fFocusDistance: f32,
    // Ratio of the aperture (known as f-stop or f-number)
    fFStop:         f32,
    // Distance between the lens and the film in mm
    fFocalLength:   f32,
    // Sensor width in mm
    fSensorWidth:   f32,

    // Sensor height in mm
    fSensorHeight: f32,
    // Exposure adjustment as a log base-2 value.
    fExposure:     f32,
    // TAA jitter
    f2Jitter:      vec2<f32>,

    mView:        mat4x4<f32>,
    mProj:        mat4x4<f32>,
    mViewProj:    mat4x4<f32>,
    mViewInv:     mat4x4<f32>,
    mProjInv:     mat4x4<f32>,
    mViewProjInv: mat4x4<f32>,

    f4ExtraData: array<vec4<f32>, 5>, // Any appliation-specific data
}

#endif //_BASIC_STRUCTURES_FXH_
