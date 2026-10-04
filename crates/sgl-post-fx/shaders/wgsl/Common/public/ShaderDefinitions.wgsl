// WGSL port of DiligentFX Shaders/Common/public/ShaderDefinitions.fxh
// (https://github.com/DiligentGraphics/DiligentFX, revision
// f26cfe5b901bf180c4a3c9bbd4d5df0b96536d4b). Copyright Diligent Graphics LLC,
// licensed under the Apache License, Version 2.0 (vendor/DiligentFX/License.txt).
// Modified: translated from HLSL to WGSL; see crates/sgl-post-fx/README.md.
//
// The header's shader half defines BOOL as bool and DEFAULT_VALUE(x) as
// nothing. WGSL: a BOOL member of a uniform structure is u32 (HLSL bool is four
// bytes in a constant buffer; WGSL uniforms cannot hold bool). Defaults belong
// to the host structures in src/structures.rs.

#ifndef _SHADER_DEFINITIONS_FXH_
#define _SHADER_DEFINITIONS_FXH_

#endif //_SHADER_DEFINITIONS_FXH_
