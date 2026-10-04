// WGSL port of DiligentFX Shaders/PostProcess/ScreenSpaceReflection/public/ScreenSpaceReflectionStructures.fxh
// (https://github.com/DiligentGraphics/DiligentFX, revision
// f26cfe5b901bf180c4a3c9bbd4d5df0b96536d4b). Copyright Diligent Graphics LLC,
// licensed under the Apache License, Version 2.0 (vendor/DiligentFX/License.txt).
// Modified: translated from HLSL to WGSL; see crates/sgl-post-fx/README.md.
// DFX-25 adds SSR_REPROJECT_SURFACE_DISCARD_VARIANCE_WEIGHT, AMD's
// FFX_DNSR_REFLECTIONS_REPROJECT_SURFACE_DISCARD_VARIANCE_WEIGHT from
// ffx-reflection-dnsr/ffx_denoiser_reflections_config.h
// (https://github.com/GPUOpen-Effects/FidelityFX-Denoiser, revision
// d7dfecbabe7b9523b14e7b067216e06b86e8d189), MIT licensed
// (LICENSE-amd-fidelityfx-denoiser.txt), and takes SSR_TEMPORAL_VARIANCE_GAMMA
// from Wicked Engine's ssr_temporalCS.hlsl (revision
// 2ff1d9e7b36091d6edf9f823af77e6bc9af20e3b, MIT).

#ifndef _SCREEN_SPACE_REFLECTION_STRUCTURES_FXH_
#define _SCREEN_SPACE_REFLECTION_STRUCTURES_FXH_

#include "ShaderDefinitions.fxh"

// Maximum mip level of depth buffer used in the Hi-Z tracing
#define SSR_DEPTH_HIERARCHY_MAX_MIP 6

// Number of samples on the Poisson disc used at the stage of spatial reconstruction
#define SSR_SPATIAL_RECONSTRUCTION_SAMPLES 8

// Parameter regulates from which level of roughness the maximum radius will be used at the stage of spatial reconstruction
#define SSR_SPATIAL_RECONSTRUCTION_ROUGHNESS_FACTOR 5

// Sets the sigma in Gaussian weighting for points on the Poisson disk at the stage of spatial reconstruction
#define SSR_SPATIAL_RECONSTRUCTION_SIGMA 0.9

// Determines the similarity threshold of depth between the current and previous frame to calculate disocclusion in the temporal accumulation step.
#define SSR_DISOCCLUSION_THRESHOLD 0.9

// Sets the value for the variance gamma in the temporal accumulation step
// PROVENANCE.md DFX-25: Wicked Engine's ssr_temporalCS temporalScale.
#define SSR_TEMPORAL_VARIANCE_GAMMA 2.0

// PROVENANCE.md DFX-25: AMD's FFX_DNSR_REFLECTIONS_REPROJECT_SURFACE_DISCARD_VARIANCE_WEIGHT. The surface
// reprojection's history is kept while its squared distance from the neighbourhood mean stays under this
// many times the length of the neighbourhood variance.
#define SSR_REPROJECT_SURFACE_DISCARD_VARIANCE_WEIGHT 1.5

// Defines the factor for edge-stopping function on world-space normals in the bilateral filtering step
#define SSR_BILATERAL_SIGMA_NORMAL 128.0

// Defines the factor for edge-stopping function on linear depth buffer in the bilateral filtering step
#define SSR_BILATERAL_SIGMA_DEPTH 1.0

// Defines the variance threshold at which the bilateral filtering should be launched
#define SSR_BILATERAL_VARIANCE_EXIT_THRESHOLD 0.00005

// Defines the variance threshold at which the maximum radius of the bilateral filter is used
#define SSS_BILATERAL_VARIANCE_ESTIMATE_THRESHOLD 0.001

// Parameter regulates from which level of roughness the maximum radius will be used at the stage of bilateral filtering
#define SSR_BILATERAL_ROUGHNESS_FACTOR 8

// Defaults are those of the host structure (src/structures.rs).
struct ScreenSpaceReflectionAttribs
{
    // A bias for accepting hits. Larger values may cause streaks, lower values may cause holes
    DepthBufferThickness: f32,

    // Regions with a roughness value greater than this threshold won't spawn rays"
    RoughnessThreshold: f32,

    // The most detailed MIP map level in the depth hierarchy. Perfect mirrors always use 0 as the most detailed level
    MostDetailedMip: u32,

    // A boolean to describe the space used to store roughness in the materialParameters texture.
    // WGSL: BOOL is u32 (ShaderDefinitions.wgsl).
    IsRoughnessPerceptual: u32,

    // The channel to read the roughness from the materialParameters texture
    RoughnessChannel: u32,

    // Caps the maximum number of lookups that are performed from the depth buffer hierarchy. Most rays should terminate after approximately 20 lookups
    MaxTraversalIntersections: u32,

    // This parameter is aimed at reducing noise by modify sampling in the ray tracing stage. Increasing the value increases the deviation from the ground truth but reduces the noise
    GGXImportanceSampleBias: f32,

    // The value controls the kernel size in the spatial reconstruction step. Increasing the value increases the deviation from the ground truth but reduces the noise
    SpatialReconstructionRadius: f32,

    // A factor to control the accmulation of history values of radiance buffer. Higher values reduce noise, but are more likely to exhibit ghosting artefacts
    TemporalRadianceStabilityFactor: f32,

    // A factor to control the accmulation of history values of variance buffer. Higher values reduce noise, but are more likely to exhibit ghosting artefacts
    TemporalVarianceStabilityFactor: f32,

    // This parameter represents the standard deviation in the Gaussian kernel, which forms the spatial component of the bilateral filter
    BilateralCleanupSpatialSigmaFactor: f32,

    // The parameter is responsible for adjusting the intensity of SSR with time
    AlphaInterpolation: f32,
}

#endif //_SCREEN_SPACE_REFLECTION_STRUCTURES_FXH_
