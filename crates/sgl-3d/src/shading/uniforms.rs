//! Rust mirrors of uniforms.wgsl: the view and frame data of group 0 and the
//! per-instance object record of group 1.

/// `View::flags`: a static specular probe capture face.
pub(crate) const VIEW_PROBE_CAPTURE: u32 = 1;
/// `DirectionalLight::flags`: the light has the frame's shadow cascades.
pub(crate) const DIRECTIONAL_LIGHT_SHADOW: u32 = 1;
/// `Frame::flags`: the frame's volumetric fog ran; draws fog themselves
/// from its volume.
pub(crate) const FRAME_FOG: u32 = 1;
/// `Frame::flags`: baked (fixed) lighting is on.
pub(crate) const FRAME_BAKED_LIGHTING: u32 = 2;
/// `Frame::flags`: an irradiance atlas is installed.
pub(crate) const FRAME_IRRADIANCE_ATLAS: u32 = 4;
/// `Frame::flags`: the backdrop is `backdrop_color`, not the panorama.
pub(crate) const FRAME_BACKDROP_COLOR: u32 = 8;
/// `Frame::flags`: the camera's shadows take the temporal filter.
pub(crate) const FRAME_TEMPORAL_SHADOW_FILTER: u32 = 16;
/// `Frame::flags`: the camera's shadows take one hardware 2×2 tap.
pub(crate) const FRAME_HARDWARE_SHADOW_FILTER: u32 = 32;
/// `Frame::flags`: the dynamic GI volume lights the frame.
pub(crate) const FRAME_DYNAMIC_GI: u32 = 64;
/// `Frame::flags`: the irradiance volume lights the frame.
pub(crate) const FRAME_IRRADIANCE_VOLUME: u32 = 128;
/// `Object::flags`: a static instance; a moving one has the bit clear.
pub(crate) const OBJECT_STATIC: u32 = 1;

/// One rendered view (`View` in uniforms.wgsl).
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct ViewUniform {
    pub view: [[f32; 4]; 4],
    /// Unjittered.
    pub projection: [[f32; 4]; 4],
    /// The jittered projection times the view.
    pub view_projection: [[f32; 4]; 4],
    pub inverse_view_projection: [[f32; 4]; 4],
    /// Unjittered, for motion.
    pub stable_view_projection: [[f32; 4]; 4],
    pub previous_view_projection: [[f32; 4]; 4],
    pub eye: [f32; 3],
    pub mip_bias: f32,
    /// Half the NDC jitter.
    pub jitter: [f32; 2],
    pub viewport: [f32; 2],
    pub flags: u32,
    /// WGSL rounds `View` up to its 16-byte alignment.
    pub padding: [u32; 3],
}

/// One directional light (`DirectionalLight` in uniforms.wgsl).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct DirectionalLightUniform {
    /// From a receiver toward the light; any nonzero length.
    pub direction_to_light: [f32; 3],
    /// `DIRECTIONAL_LIGHT_*` bits.
    pub flags: u32,
    pub color: [f32; 3],
    /// Zero for no light.
    pub illuminance: f32,
    /// The scale of its light in the volumetric fog.
    pub fog_energy: f32,
    /// How dark its shadow is, 0..=1.
    pub shadow_opacity: f32,
    /// WGSL rounds `DirectionalLight` up to its 16-byte alignment.
    pub padding: [f32; 2],
}

/// One directional shadow cascade (`ShadowCascade` in uniforms.wgsl).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct ShadowCascadeUniform {
    pub clip_from_world: [[f32; 4]; 4],
    pub texel_size: f32,
    pub far_bound: f32,
    /// WGSL rounds `ShadowCascade` up to its 16-byte alignment.
    pub padding: [f32; 2],
}

/// What every view of one frame shares (`Frame` in uniforms.wgsl).
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct FrameUniform {
    pub directional_lights: [DirectionalLightUniform; 2],
    pub shadow_cascades: [ShadowCascadeUniform; crate::view::cascades::MAX_SHADOW_CASCADES],
    pub hemisphere_sky_color: [f32; 3],
    pub hemisphere_intensity: f32,
    pub hemisphere_ground_color: [f32; 3],
    pub diffuse_environment_yaw: f32,
    pub backdrop_color: [f32; 3],
    pub diffuse_environment_intensity: f32,
    pub mist_thin_color: [f32; 3],
    pub mist_opacity: f32,
    pub mist_dense_color: [f32; 3],
    pub backdrop_yaw: f32,
    pub mist_size: [f32; 2],
    pub mist_drift: [f32; 2],
    pub backdrop_brightness: f32,
    /// One over the fog volume's length and over its detail spread.
    pub fog_inverse_length: f32,
    pub fog_inverse_detail_spread: f32,
    pub reflection_yaw: f32,
    pub reflection_intensity: f32,
    pub elapsed_seconds: f32,
    pub fixed_irradiance_scale: f32,
    pub visibility_mask: u32,
    /// `FRAME_*` bits.
    pub flags: u32,
    pub shadow_cascade_count: u32,
    /// Frames since history restarted.
    pub frame_count: u32,
    /// `shading::material::animation_phase` of the frame's time.
    pub animation_phase: f32,
    pub lightmap_chart: [f32; 4],
    /// With `FRAME_DYNAMIC_GI`, the scene's dynamic GI volume: its first
    /// probe's position, its probes' spacing and their count on each axis,
    /// and its scroll, where they are stored.
    pub dynamic_gi_origin: [f32; 3],
    /// WGSL aligns each `vec3` to 16 bytes.
    pub padding_origin: f32,
    pub dynamic_gi_spacing: [f32; 3],
    pub padding_spacing: f32,
    pub dynamic_gi_probes: [u32; 3],
    pub padding_probes: u32,
    pub dynamic_gi_scroll: [u32; 3],
    pub padding_scroll: u32,
    /// With `FRAME_IRRADIANCE_VOLUME`, the scene's irradiance volume: its
    /// first cell's least corner, its cells' size and their count on each
    /// axis.
    pub irradiance_volume_origin: [f32; 3],
    pub padding_volume_origin: f32,
    pub irradiance_volume_cell_size: [f32; 3],
    pub padding_cell_size: f32,
    pub irradiance_volume_cells: [u32; 3],
    pub padding_cells: u32,
}

/// One instance's record (`Object` in uniforms.wgsl), at its index in the
/// scene's object buffer (`scene::objects`).
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct ObjectUniform {
    pub model: [[f32; 4]; 4],
    pub previous_model: [[f32; 4]; 4],
    pub baked_irradiance: [[f32; 4]; 6],
    /// `OBJECT_*` bits.
    pub flags: u32,
    /// A deforming instance's vertices in the scene source: its positions
    /// this frame and in the last submitted frame, and its normals and
    /// tangents (`shading::deformation`). Zero for one that does not deform.
    pub deformed_positions: u32,
    pub previous_positions: u32,
    pub deformed_normals: u32,
}

/// The camera's view and frame data as last uploaded.
#[derive(Clone, Copy, Debug)]
pub(crate) struct FrameValues {
    pub view: ViewUniform,
    pub frame: FrameUniform,
}
