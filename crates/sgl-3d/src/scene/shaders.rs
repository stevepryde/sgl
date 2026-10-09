//! Shaders: each one's validated WGSL module and its parameter block's
//! layout, with the `Scene` operations that add, read and remove them, set a
//! material's parameter block and an instance's shader data (the
//! architecture's Scene content, Shader).
use super::slots::Slots;
use super::static_edits::posed_bounds;
use super::{Scene, SceneError};
use crate::content::identity::{InstanceId, MaterialId, ShaderId};
use crate::content::instance::Mobility;
use crate::content::shader::{ShaderParamsLayout, ShaderSource};

pub(crate) struct Shader {
    /// The game's module, which its programs compose
    /// (`shading::programs::ProgramShader::Game`).
    pub source: String,
    pub label: String,
    pub layout: ShaderParamsLayout,
    /// Its `material_surface` reaches `scene_volume_path`, so its blended
    /// materials' meshes are drawn into the volume layers.
    pub reads_volume_path: bool,
    /// The materials that name it.
    pub users: u32,
}

#[derive(Default)]
pub(crate) struct Shaders {
    pub slots: Slots<ShaderId, Shader>,
}

impl Shaders {
    pub fn get(&self, id: ShaderId) -> Result<&Shader, SceneError> {
        self.slots.get(id).ok_or(SceneError::UnknownShader)
    }

    /// Counts `by` more materials naming `id`, a live shader.
    pub fn count(&mut self, id: Option<ShaderId>, by: i32) {
        if let Some(id) = id {
            let shader = self.slots.get_mut(id).expect("a named shader lives");
            shader.users = shader.users.checked_add_signed(by).unwrap();
        }
    }
}

impl Scene {
    /// Adds a game's material shader: one WGSL module that defines
    /// `struct ShaderParams`, `material_vertex` and `material_surface` over
    /// the contract's structs (the package README's "Programmable
    /// surfaces"). It is validated here, composed into every program the
    /// device's binding tier creates for it, so a module this accepts never
    /// fails later; one it refuses adds nothing (`SceneError::Shader`).
    /// Materials name it (`SurfaceMaterial::shader`), and the first frame
    /// that draws one of them compiles its pipelines: add shaders at load.
    pub fn add_shader(&mut self, source: ShaderSource) -> Result<ShaderId, SceneError> {
        let validated = crate::shading::shader::validate(&source.wgsl, self.materials.tier())?;
        Ok(self.shaders.slots.insert(Shader {
            source: source.wgsl,
            label: source.label,
            layout: validated.layout,
            reads_volume_path: validated.reads_volume_path,
            users: 0,
        }))
    }

    /// Removes a shader no material names. A shader is replaced by removing
    /// it and adding another.
    pub fn remove_shader(&mut self, id: ShaderId) -> Result<(), SceneError> {
        if self.shaders.get(id)?.users > 0 {
            return Err(SceneError::ShaderInUse);
        }
        self.shaders.slots.remove(id);
        Ok(())
    }

    /// The uniform layout naga gives the shader's `ShaderParams`, which a
    /// game's Rust mirror of the block checks itself against.
    pub fn shader_parameters_layout(
        &self,
        id: ShaderId,
    ) -> Result<&ShaderParamsLayout, SceneError> {
        Ok(&self.shaders.get(id)?.layout)
    }

    /// Replaces the parameter block of `material`'s shader (its
    /// `ShaderParams`, as `shader_parameters_layout` lays it out), which
    /// starts as zeros. It is an edit: the queue uploads it, and the frame
    /// after the next submitted one evaluates its motion from it. Time
    /// comes from the frame, so most blocks are set once.
    pub fn set_shader_parameters(
        &mut self,
        queue: &wgpu::Queue,
        material: MaterialId,
        bytes: &[u8],
    ) -> Result<(), SceneError> {
        let expected = match self.materials.get(material)?.values.shader {
            Some(shader) => self.shaders.get(shader.shader)?.layout.size,
            None => 0,
        };
        if expected == 0 || bytes.len() != expected as usize {
            return Err(SceneError::ShaderParameters {
                expected,
                given: u32::try_from(bytes.len()).unwrap_or(u32::MAX),
            });
        }
        self.materials.set_parameters(queue, material, bytes);
        Ok(())
    }

    /// Replaces the data an instance's materials' shaders read
    /// (`VertexContext::instance`, `SurfaceContext::instance`), zero when it
    /// is added: a wind phase, a chunk's anchor. Moving the render origin
    /// leaves it as it is. The frame after the next submitted one evaluates
    /// its motion from it. For a static instance it is a static edit, as its
    /// pose would be: its local-light shadow caches hold its displacement.
    pub fn set_instance_shader_data(
        &mut self,
        queue: &wgpu::Queue,
        id: InstanceId,
        data: [f32; 4],
    ) -> Result<(), SceneError> {
        let instance = self.instances.get(id)?;
        if instance.shader_data == data {
            return Ok(());
        }
        if instance.mobility == Mobility::Static {
            let model = self.drawn_model(instance.state.model);
            let bounds = self.shaded_bounds(model);
            self.static_edits
                .record(posed_bounds(bounds, instance.state.pose));
        }
        self.instances.set_shader_data(queue, id, data);
        Ok(())
    }

    /// The data an instance's materials' shaders read.
    pub fn instance_shader_data(&self, id: InstanceId) -> Result<[f32; 4], SceneError> {
        Ok(self.instances.get(id)?.shader_data)
    }
}
