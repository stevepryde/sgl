//! The frame's camera used by reflections, with TAA's jitter when it runs.
#[derive(Clone, Copy)]
pub(crate) struct Camera {
    pub inverse_view_proj: [[f32; 4]; 4],
    pub view: [[f32; 4]; 4],
    pub proj: [[f32; 4]; 4],
    pub camera_position: [f32; 4],
    /// The projection without its near plane's clip, where `proj` is
    /// clipped at an oblique plane (a planar reflector's reflected view,
    /// `reflected_camera::oblique`): what gives the frustum's shape and a
    /// view depth its device depth independently of x and y, which `proj`
    /// then does not.
    pub unclipped: Option<[[f32; 4]; 4]>,
}
impl Camera {
    /// `projection` carries the frame's jitter, as the depth buffer does.
    pub fn new(view: glam::Mat4, projection: glam::Mat4) -> Self {
        let matrix = projection * view;
        Self {
            inverse_view_proj: matrix.inverse().to_cols_array_2d(),
            view: view.to_cols_array_2d(),
            proj: projection.to_cols_array_2d(),
            camera_position: view.inverse().w_axis.to_array(),
            unclipped: None,
        }
    }

    /// As `new`, of a view whose `projection` is `unclipped` clipped at an
    /// oblique plane.
    pub fn clipped(view: glam::Mat4, projection: glam::Mat4, unclipped: glam::Mat4) -> Self {
        Self {
            unclipped: Some(unclipped.to_cols_array_2d()),
            ..Self::new(view, projection)
        }
    }
}
