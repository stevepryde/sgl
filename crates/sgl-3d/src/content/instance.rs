//! A placed model's presentation state and whether it moves.
use super::identity::ModelId;
use glam::Mat4;

/// What a scene shows of an instance. `Scene::add_instance` and
/// `Scene::set_instance` take it; `Scene::instance` returns it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InstanceState {
    pub model: ModelId,
    /// Model to world. Finite and invertible.
    pub pose: Mat4,
    /// The main camera draws it.
    pub visible: bool,
    /// The other views show it: every light's shadow views, rays and, for a
    /// static instance, probe captures.
    pub capture_visible: bool,
}

impl InstanceState {
    /// `model` at the world origin, unturned and unscaled, shown to the main
    /// camera and every other view, as Godot's `MeshInstance3D` is visible
    /// and casts shadows by default. Set what differs and take the rest with
    /// `..InstanceState::new(model)`.
    pub fn new(model: ModelId) -> Self {
        Self {
            model,
            pose: Mat4::IDENTITY,
            visible: true,
            capture_visible: true,
        }
    }
}

/// Whether an instance moves, chosen when it is added.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Mobility {
    /// Expected to stay as it was added: bakes and probe captures contain it,
    /// it takes baked diffuse light from lightmap charts and the irradiance
    /// atlas, and it writes no motion.
    Static,
    /// Expected to be posed every frame: it takes baked diffuse light from
    /// its ambient cube and writes motion from its pose in the last
    /// submitted frame.
    Moving,
}
