// WGSL port of DiligentFX Shaders/Common/public/PBR_Common.fxh
// (https://github.com/DiligentGraphics/DiligentFX, revision
// f26cfe5b901bf180c4a3c9bbd4d5df0b96536d4b). Copyright Diligent Graphics LLC,
// licensed under the Apache License, Version 2.0 (vendor/DiligentFX/License.txt).
// Modified: translated from HLSL to WGSL; see crates/sgl-post-fx/README.md.
// Only the functions the ported passes use are present, in upstream order.

#ifndef _PBR_COMMON_FXH_
#define _PBR_COMMON_FXH_

#ifndef PI
#    define PI 3.141592653589793
#endif

// Visibility = G2(v,l,a) / (4 * (n,v) * (n,l))
// see https://google.github.io/filament/Filament.md.html#materialsystem/specularbrdf
fn SmithGGXVisibilityCorrelated(NdotL: f32, NdotV: f32, AlphaRoughness: f32) -> f32
{
    // G1 (masking) is % microfacets visible in 1 direction
    // G2 (shadow-masking) is % microfacets visible in 2 directions
    // If uncorrelated:
    //    G2(NdotL, NdotV) = G1(NdotL) * G1(NdotV)
    //    Less realistic as higher points are more likely visible to both L and V
    //
    // https://ubm-twvideo01.s3.amazonaws.com/o1/vault/gdc2017/Presentations/Hammon_Earl_PBR_Diffuse_Lighting.pdf

    let a2 = AlphaRoughness * AlphaRoughness;

    let GGXV = NdotL * sqrt(max(NdotV * NdotV * (1.0 - a2) + a2, 1e-7));
    let GGXL = NdotV * sqrt(max(NdotL * NdotL * (1.0 - a2) + a2, 1e-7));

    return 0.5 / (GGXV + GGXL);
}

// Smith GGX masking function G1
// [1] "Sampling the GGX Distribution of Visible Normals" (2018) by Eric Heitz - eq. (2)
// https://jcgt.org/published/0007/04/01/
fn SmithGGXMasking(NdotV: f32, AlphaRoughness: f32) -> f32
{
    let a2 = AlphaRoughness * AlphaRoughness;

    // See the upstream comment for the derivation from [1], eq. (2).
    let Denom = NdotV + sqrt(a2 + (1.0 - a2) * NdotV * NdotV);
    return 2.0 * max(NdotV, 0.0) / max(Denom, 1e-6);
}

// The following equation(s) model the distribution of microfacet normals across the area being drawn (aka D())
// Implementation from "Average Irregularity Representation of a Roughened Surface for Ray Reflection" by T. S. Trowbridge, and K. P. Reitz
// Follows the distribution function recommended in the SIGGRAPH 2013 course notes from EPIC Games, Equation 3.
fn NormalDistribution_GGX(NdotH: f32, AlphaRoughness_: f32) -> f32
{
    // "Sampling the GGX Distribution of Visible Normals" (2018) by Eric Heitz - eq. (1)
    // https://jcgt.org/published/0007/04/01/

    // Make sure we reasonably handle AlphaRoughness == 0
    // (which corresponds to delta function)
    // WGSL: parameters are immutable; the upstream in-parameter is copied.
    let AlphaRoughness = max(AlphaRoughness_, 1e-3);

    let a2  = AlphaRoughness * AlphaRoughness;
    let nh2 = NdotH * NdotH;
    let f   = nh2 * a2 + (1.0 - nh2);
    return a2 / max(PI * f * f, 1e-9);
}

// Samples a normal from Visible Normal Distribution as described in
// [1] "Sampling Visible GGX Normals with Spherical Caps" (2023) by Jonathan Dupuy, Anis Benyoub
//     https://arxiv.org/pdf/2306.05044.pdf
fn SmithGGXSampleVisibleNormalSC(View: vec3<f32>, // View direction in tangent space
                                 ax: f32,         // X roughness
                                 ay: f32,         // Y roughness
                                 u1: f32,         // Uniform random variable in [0, 1]
                                 u2: f32          // Uniform random variable in [0, 1]
) -> vec3<f32>
{
    // Stretch the view vector so we are sampling as if roughness==1
    let V = normalize(View * vec3<f32>(ax, ay, 1.0));

    let Phi = 2.0 * PI * u1;
    let Z = (1.0 - u2) * (1.0 + V.z) - V.z;
    let SinTheta = sqrt(clamp(1.0 - Z * Z, 0.0, 1.0));
    let H = vec3<f32>(SinTheta * cos(Phi), SinTheta * sin(Phi), Z) + V;

    // Transform the normal back to the ellipsoid configuration
    return normalize(vec3<f32>(ax * H.x, ay * H.y, H.z));
}

#endif // _PBR_COMMON_FXH_
