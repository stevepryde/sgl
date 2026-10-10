//! Host halves of the shared shader structures: `CameraAttribs`
//! (`Shaders/Common/public/BasicStructures.fxh`),
//! `ScreenSpaceReflectionAttribs`
//! (`Shaders/PostProcess/ScreenSpaceReflection/public/ScreenSpaceReflectionStructures.fxh`)
//! and `TemporalAntiAliasingAttribs`
//! (`Shaders/PostProcess/TemporalAntiAliasing/public/TemporalAntiAliasingStructures.fxh`),
//! with the headers' `DEFAULT_VALUE` defaults. All are constant-buffer
//! layouts (`CHECK_STRUCT_ALIGNMENT`: a multiple of 16 bytes).
//!
//! Matrices are stored column by column as `[f32; 16]`. An HLSL matrix row is
//! a WGSL matrix column in this port, so these arrays are the rows of the
//! upstream row-vector matrices: a glam `Mat4::to_cols_array` of the
//! column-vector matrix (see README.md).

/// `BasicStructures.fxh` `CameraAttribs`.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct CameraAttribs {
    /// Camera world position
    pub f4_position: [f32; 4],
    /// (width, height, 1/width, 1/height)
    pub f4_viewport_size: [f32; 4],

    pub f_near_plane_z: f32,
    /// fNearPlaneZ < fFarPlaneZ
    pub f_far_plane_z: f32,
    pub f_near_plane_depth: f32,
    pub f_far_plane_depth: f32,

    // Tight scene bounds
    pub f_scene_near_z: f32,
    /// fSceneNearZ < fSceneFarZ
    pub f_scene_far_z: f32,
    pub f_scene_near_depth: f32,
    pub f_scene_far_depth: f32,

    /// +1.0 for right-handed coordinate system, -1.0 for left-handed
    pub f_handness: f32,
    pub ui_frame_index: u32,
    pub padding0: f32,
    pub padding1: f32,

    /// Distance to the point of focus
    pub f_focus_distance: f32,
    /// Ratio of the aperture (known as f-stop or f-number)
    pub f_f_stop: f32,
    /// Distance between the lens and the film in mm
    pub f_focal_length: f32,
    /// Sensor width in mm
    pub f_sensor_width: f32,

    /// Sensor height in mm
    pub f_sensor_height: f32,
    /// Exposure adjustment as a log base-2 value.
    pub f_exposure: f32,
    /// TAA jitter
    pub f2_jitter: [f32; 2],

    pub m_view: [f32; 16],
    pub m_proj: [f32; 16],
    pub m_view_proj: [f32; 16],
    pub m_view_inv: [f32; 16],
    pub m_proj_inv: [f32; 16],
    pub m_view_proj_inv: [f32; 16],

    /// Any appliation-specific data
    pub f4_extra_data: [[f32; 4]; 5],
}

impl Default for CameraAttribs {
    fn default() -> Self {
        Self {
            f_focus_distance: 10.0,
            f_f_stop: 5.6,
            f_focal_length: 50.0,
            f_sensor_width: 36.0,
            f_sensor_height: 24.0,
            f_exposure: 0.0,
            ..bytemuck::Zeroable::zeroed()
        }
    }
}

impl CameraAttribs {
    /// Set the near and far clip planes z and depth values.
    ///
    /// fNearZ > fFarZ means that the depth buffer is reversed.
    pub fn set_clip_planes(&mut self, f_near_z: f32, f_far_z: f32) {
        let use_reverse_depth = f_near_z > f_far_z;
        self.f_near_plane_z = if use_reverse_depth { f_far_z } else { f_near_z };
        self.f_far_plane_z = if use_reverse_depth { f_near_z } else { f_far_z };
        self.f_near_plane_depth = if use_reverse_depth { 1.0 } else { 0.0 };
        self.f_far_plane_depth = if use_reverse_depth { 0.0 } else { 1.0 };
        self.f_scene_near_z = self.f_near_plane_z;
        self.f_scene_far_z = self.f_far_plane_z;
        self.f_scene_near_depth = self.f_near_plane_depth;
        self.f_scene_far_depth = self.f_far_plane_depth;
    }
}

/// `ScreenSpaceReflectionStructures.fxh` `ScreenSpaceReflectionAttribs`.
/// `BOOL` is four bytes, as in the C++ half of `ShaderDefinitions.fxh`.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ScreenSpaceReflectionAttribs {
    /// A bias for accepting hits. Larger values may cause streaks, lower values may cause holes
    pub depth_buffer_thickness: f32,

    /// Regions with a roughness value greater than this threshold won't spawn rays"
    pub roughness_threshold: f32,

    /// The most detailed MIP map level in the depth hierarchy. Perfect mirrors always use 0 as the most detailed level
    pub most_detailed_mip: u32,

    /// A boolean to describe the space used to store roughness in the materialParameters texture.
    pub is_roughness_perceptual: i32,

    /// The channel to read the roughness from the materialParameters texture
    pub roughness_channel: u32,

    /// Caps the maximum number of lookups that are performed from the depth buffer hierarchy. Most rays should terminate after approximately 20 lookups.
    /// At most 256, the top of DiligentFX's range (`SSR_MAX_TRAVERSAL_INTERSECTIONS`, PROVENANCE.md DFX-30); more count as 256.
    /// A ray that runs out of lookups before confirming a hit is a miss (PROVENANCE.md DFX-38).
    pub max_traversal_intersections: u32,

    /// This parameter is aimed at reducing noise by modify sampling in the ray tracing stage. Increasing the value increases the deviation from the ground truth but reduces the noise
    pub ggx_importance_sample_bias: f32,

    /// The value controls the kernel size in the spatial reconstruction step. Increasing the value increases the deviation from the ground truth but reduces the noise.
    /// At most 8, the top of DiligentFX's range (`SSR_SPATIAL_RECONSTRUCTION_MAX_RADIUS`, PROVENANCE.md DFX-30); more counts as 8
    pub spatial_reconstruction_radius: f32,

    /// A factor to control the accmulation of history values of radiance buffer. Higher values reduce noise, but are more likely to exhibit ghosting artefacts
    pub temporal_radiance_stability_factor: f32,

    /// A factor to control the accmulation of history values of variance buffer. Higher values reduce noise, but are more likely to exhibit ghosting artefacts
    pub temporal_variance_stability_factor: f32,

    /// This parameter represents the standard deviation in the Gaussian kernel, which forms the spatial component of the bilateral filter
    pub bilateral_cleanup_spatial_sigma_factor: f32,

    /// The parameter is responsible for adjusting the intensity of SSR with time
    pub alpha_interpolation: f32,
}

impl Default for ScreenSpaceReflectionAttribs {
    fn default() -> Self {
        Self {
            depth_buffer_thickness: 0.025,
            roughness_threshold: 0.2,
            most_detailed_mip: 0,
            is_roughness_perceptual: 1,
            roughness_channel: 0,
            max_traversal_intersections: 128,
            ggx_importance_sample_bias: 0.3,
            spatial_reconstruction_radius: 4.0,
            temporal_radiance_stability_factor: 1.0,
            temporal_variance_stability_factor: 0.9,
            bilateral_cleanup_spatial_sigma_factor: 0.9,
            alpha_interpolation: 1.0,
        }
    }
}

/// `TemporalAntiAliasingStructures.fxh` `TemporalAntiAliasingAttribs`.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct TemporalAntiAliasingAttribs {
    /// The value is responsible for interpolating between the current and previous frame. Increasing the value increases temporal stability but may introduce ghosting
    pub temporal_stability_factor: f32,

    /// If this parameter is set to true, the current frame will be written to the current history buffer without interpolation with the previous history buffer
    pub reset_accumulation: u32,

    pub skip_rejection: u32,

    pub padding0: f32,
}

impl Default for TemporalAntiAliasingAttribs {
    fn default() -> Self {
        Self {
            temporal_stability_factor: 0.9375,
            reset_accumulation: 0,
            skip_rejection: 0,
            padding0: 0.0,
        }
    }
}

// CHECK_STRUCT_ALIGNMENT
const _: () = assert!(size_of::<CameraAttribs>().is_multiple_of(16));
const _: () = assert!(size_of::<ScreenSpaceReflectionAttribs>().is_multiple_of(16));
const _: () = assert!(size_of::<TemporalAntiAliasingAttribs>().is_multiple_of(16));
