// WGSL port of DiligentCore Graphics/ShaderTools/include/HLSLDefinitions.fxh
// (https://github.com/DiligentGraphics/DiligentCore, revision
// 18bfa7b7563a0ef5b5fe074d37c2e8304100e965), which Diligent prepends to every
// HLSL shader. Copyright Diligent Graphics LLC, licensed under the Apache
// License, Version 2.0 (vendor/DiligentCore/License.txt).
// Modified: translated from HLSL to WGSL, with the WGSL resource-access
// section at the end; see crates/sgl-post-fx/README.md and PROVENANCE.md.
// The relational and logical operator macros, BoolToFloat, the float4x4 and
// float2x2 MatrixFromRows overloads and VK_IMAGE_FORMAT are unused by the
// ported passes and omitted.

#ifndef _HLSL_DEFINITIONS_
#define _HLSL_DEFINITIONS_

#define HLSL

#define NDC_MIN_Z 0.0 // Minimal z in the normalized device space

// WGSL: a constant instead of the float3(...) macro.
const F3NDC_XYZ_TO_UVD_SCALE = vec3<f32>(0.5, -0.5, 1.0);

fn NormalizedDeviceXYToTexUV(f2ProjSpaceXY: vec2<f32>) -> vec2<f32>
{
    return vec2<f32>(0.5,0.5) + vec2<f32>(0.5,-0.5) * f2ProjSpaceXY.xy;
}

fn TexUVToNormalizedDeviceXY(TexUV: vec2<f32>) -> vec2<f32>
{
    return (TexUV.xy - vec2<f32>(0.5, 0.5)) * vec2<f32>(2.0, -2.0);
}

fn NormalizedDeviceZToDepth(fNDC_Z: f32) -> f32
{
    return fNDC_Z;
}

fn DepthToNormalizedDeviceZ(fDepth: f32) -> f32
{
    return fDepth;
}

// WGSL: MATRIX_ELEMENT(mat, row, col) is written mat[row][col] where used.
// An HLSL matrix row is a WGSL matrix column throughout the port, so indexing
// and constructors read the same and HLSL mul(a, b) is WGSL b * a.

fn MatrixFromRows_f3(row0: vec3<f32>, row1: vec3<f32>, row2: vec3<f32>) -> mat3x3<f32>
{
    return mat3x3<f32>(row0, row1, row2);
}

// ---- WGSL: HLSL resource access semantics -----------------------------------
// HLSL Texture2D.Load outside the texture's coordinates or mips returns zero;
// WGSL textureLoad leaves it undefined. Every ported Load goes through these.

fn HlslOutside(Location: vec2<i32>, Mip: i32, Levels: u32, Size: vec2<u32>) -> bool
{
    return Mip < 0 || u32(Mip) >= Levels || any(Location < vec2<i32>(0)) || any(vec2<u32>(Location) >= Size);
}

fn HlslLoad(Texture: texture_2d<f32>, Location: vec2<i32>, Mip: i32) -> vec4<f32>
{
    if (Mip < 0 || u32(Mip) >= textureNumLevels(Texture)) {
        return vec4<f32>(0.0);
    }
    if (HlslOutside(Location, Mip, textureNumLevels(Texture), textureDimensions(Texture, Mip))) {
        return vec4<f32>(0.0);
    }
    return textureLoad(Texture, Location, Mip);
}

fn HlslLoadDepth(Texture: texture_depth_2d, Location: vec2<i32>, Mip: i32) -> f32
{
    if (Mip < 0 || u32(Mip) >= textureNumLevels(Texture)) {
        return 0.0;
    }
    if (HlslOutside(Location, Mip, textureNumLevels(Texture), textureDimensions(Texture, Mip))) {
        return 0.0;
    }
    return textureLoad(Texture, Location, Mip);
}

fn HlslLoadUint(Texture: texture_2d<u32>, Location: vec2<i32>, Mip: i32) -> vec4<u32>
{
    if (Mip < 0 || u32(Mip) >= textureNumLevels(Texture)) {
        return vec4<u32>(0u);
    }
    if (HlslOutside(Location, Mip, textureNumLevels(Texture), textureDimensions(Texture, Mip))) {
        return vec4<u32>(0u);
    }
    return textureLoad(Texture, Location, Mip);
}

#endif // _HLSL_DEFINITIONS_
