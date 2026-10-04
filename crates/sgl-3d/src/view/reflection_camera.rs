//! The frame's camera used by reflections, with TAA's jitter when it runs.
#[derive(Clone, Copy)]
pub(crate) struct Camera {
    pub inverse_view_proj: [[f32; 4]; 4],
    pub view: [[f32; 4]; 4],
    pub proj: [[f32; 4]; 4],
    pub camera_position: [f32; 4],
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
        }
    }
}
