//! Host halves of the shared shader structures: `CameraAttribs`
//! (`Shaders/Common/public/BasicStructures.fxh`),
//! `ScreenSpaceReflectionAttribs`
//! (`Shaders/PostProcess/ScreenSpaceReflection/public/ScreenSpaceReflectionStructures.fxh`)
//! and `TemporalAntiAliasingAttribs`
//! (`Shaders/PostProcess/TemporalAntiAliasing/public/TemporalAntiAliasingStructures.fxh`),
//! with the headers' `DEFAULT_VALUE` defaults and TAA's former defines'
//! values. All are constant-buffer
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

    /// Caps the maximum number of lookups that are performed from the depth buffer hierarchy. Most rays should terminate after approximately 20 lookups
    pub max_traversal_intersections: u32,

    /// This parameter is aimed at reducing noise by modify sampling in the ray tracing stage. Increasing the value increases the deviation from the ground truth but reduces the noise
    pub ggx_importance_sample_bias: f32,

    /// The value controls the kernel size in the spatial reconstruction step. Increasing the value increases the deviation from the ground truth but reduces the noise
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

/// `TemporalAntiAliasingStructures.fxh` `TemporalAntiAliasingAttribs`, with
/// the header's `TAA_*` defines and DFX-19's still-pixel constants as fields
/// after upstream's (PROVENANCE.md DFX-25).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct TemporalAntiAliasingAttribs {
    /// The value is responsible for interpolating between the current and previous frame. Increasing the value increases temporal stability but may introduce ghosting
    pub temporal_stability_factor: f32,

    /// If this parameter is set to true, the current frame will be written to the current history buffer without interpolation with the previous history buffer
    pub reset_accumulation: u32,

    pub skip_rejection: u32,

    pub padding0: f32,

    /// `TAA_MIN_VARIANCE_GAMMA`: the minimum value for the variance gamma.
    /// The variance gamma is used to adjust the influence of historical data in the anti-aliasing process.
    /// A lower value means that the algorithm is less influenced by past frames, making it more responsive to changes but potentially less smooth
    pub min_variance_gamma: f32,

    /// `TAA_MAX_VARIANCE_GAMMA`: the maximum value for the variance gamma.
    /// A higher maximum value allows the algorithm to rely more heavily on historical data,
    /// which can produce smoother results but may also introduce more motion blur or ghosting effects in fast-moving scenes.
    pub max_variance_gamma: f32,

    /// `TAA_MOTION_VECTOR_DIFF_FACTOR`: the threshold for pixel velocity difference that determines whether a pixel is considered to have "no history."
    /// If the difference in motion vectors between the current frame and the previous frame exceeds this value, the pixel is treated as if it has no historical data.
    /// This helps to prevent ghosting effects by not blending pixels with significantly different motion vectors.
    pub motion_vector_diff_factor: f32,

    /// `TAA_DEPTH_DISOCCLUSION_THRESHOLD`: the threshold for depth disocclusion. It is used to determine how much a change in depth between frames should be considered as disocclusion,
    /// which occurs when previously occluded objects become visible. A small threshold value means that only significant depth changes will be treated as disocclusion,
    /// which can help in maintaining the stability of the image but may ignore some smaller, yet visually important changes.
    pub depth_disocclusion_threshold: f32,

    /// `TAA_VARIANCE_INTERSECTION_MAX_T`: the max "distance" between source colour and target colour.
    /// Setting this to a larger value allows more bright pixels from the history buffer to be leaved unchanged.
    pub variance_intersection_max_t: f32,

    /// DFX-19: a pixel whose closest motion is under this many pixels on both axes is still.
    pub still_motion_pixels: f32,

    /// DFX-19: the most history a still pixel keeps, in place of `temporal_stability_factor`.
    pub still_history_factor: f32,

    pub padding1: f32,
}

impl Default for TemporalAntiAliasingAttribs {
    fn default() -> Self {
        Self {
            temporal_stability_factor: 0.9375,
            reset_accumulation: 0,
            skip_rejection: 0,
            padding0: 0.0,
            min_variance_gamma: 0.75,
            max_variance_gamma: 2.5,
            motion_vector_diff_factor: 256.0,
            depth_disocclusion_threshold: 0.9,
            variance_intersection_max_t: 10.0,
            still_motion_pixels: 0.01,
            // 1 - Bevy's MIN_HISTORY_BLEND_RATE (DFX-19).
            still_history_factor: 0.985,
            padding1: 0.0,
        }
    }
}

// CHECK_STRUCT_ALIGNMENT
const _: () = assert!(size_of::<CameraAttribs>().is_multiple_of(16));
const _: () = assert!(size_of::<ScreenSpaceReflectionAttribs>().is_multiple_of(16));
const _: () = assert!(size_of::<TemporalAntiAliasingAttribs>().is_multiple_of(16));
