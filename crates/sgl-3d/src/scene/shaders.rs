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
    /// One of its functions reads the frame's time from a context, so what
    /// its materials cast may change from frame to frame.
    pub reads_time: bool,
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
    /// Materials name it through `SurfaceMaterial::shader`. Each geometry
    /// pipeline using the shader is compiled on its first frame or probe draw.
    pub fn add_shader(&mut self, source: ShaderSource) -> Result<ShaderId, SceneError> {
        let validated = crate::shading::shader::validate(&source.wgsl, self.materials.tier())?;
        Ok(self.shaders.slots.insert(Shader {
            source: source.wgsl,
            label: source.label,
            layout: validated.layout,
            reads_volume_path: validated.reads_volume_path,
            reads_time: validated.reads_time,
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
    /// after the next submitted one evaluates its motion from it; until the
    /// block was in a finished frame, a frame evaluates its motion from its
    /// own block, so a material that gains or changes its shader writes no
    /// motion from its parameters in its first frame. Time comes from the
    /// frame, so most blocks are set once.
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

/// Whether an opaque or masked material's shading differs in this frame
/// from the last submitted frame's where its geometry stands still
/// (`Scene::shading_changes`), which the temporal composition marks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ShadingChange {
    /// It does not: no normal layer moves, and it has no shader or one that
    /// reads no time and whose parameters and instance data are the last
    /// submitted frame's.
    None,
    /// It changes from frame to frame: its normal layers move
    /// (`Material::record_moves`), its shader reads the time
    /// (`Shader::reads_time`) or a frame changed the picture of the render
    /// texture it names (`RenderTextures::changed`).
    Continuous,
    /// An edit since the last submitted frame changed what its shader
    /// reads: its parameter block (`Scene::set_shader_parameters`) or the
    /// shader data of an instance of a model drawn with it
    /// (`Scene::set_instance_shader_data`).
    Edited,
}

/// The frame's shading changes (`Scene::shading_changes`): the materials
/// whose instances' shader data was edited since the last submitted frame.
pub(crate) struct ShadingChanges {
    edited: Vec<MaterialId>,
}

impl ShadingChanges {
    /// How `id`'s shading changes in `scene`'s frame. An edit takes
    /// precedence over a continuous change.
    pub fn of(&self, scene: &Scene, id: MaterialId) -> ShadingChange {
        let material = scene.drawn_material(id);
        // The picture it emits is another than the last finished frame's.
        let picture = material
            .values
            .emission_render_texture
            .is_some_and(|texture| scene.render_textures.changed(texture));
        let Some(shader) = material.values.shader else {
            return if material.record_moves() || picture {
                ShadingChange::Continuous
            } else {
                ShadingChange::None
            };
        };
        if material.parameters_changed() || self.edited.contains(&id) {
            ShadingChange::Edited
        } else if material.record_moves()
            || picture
            || scene
                .shaders
                .get(shader.shader)
                .is_ok_and(|shader| shader.reads_time)
        {
            ShadingChange::Continuous
        } else {
            ShadingChange::None
        }
    }
}

impl Scene {
    /// The frame's shading changes: per material, whether its shading
    /// differs from the last submitted frame's where its geometry stands
    /// still (`ShadingChanges::of`). An instance's edited shader data counts
    /// for every material its model draws with, and so for every instance
    /// drawn with those materials.
    pub(crate) fn shading_changes(&self) -> ShadingChanges {
        let mut edited = Vec::new();
        for instance in self.instances.shader_data_edited() {
            let model = self.drawn_model(instance.state.model);
            for mesh in &model.meshes {
                if !edited.contains(&mesh.material) {
                    edited.push(mesh.material);
                }
            }
        }
        ShadingChanges { edited }
    }
}
