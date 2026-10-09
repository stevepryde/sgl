//! The shader modules the geometry pipelines are created from: one program
//! set for materials without a shader, and one for each shader a material
//! of the scene names, each holding the geometry program of each form
//! (`shading::programs::GeometryForm`) and the caster program, made when a
//! pipeline first needs it and dropped with the set.
use crate::Scene;
use crate::content::identity::ShaderId;
use crate::shading::bind::BindingTier;
use crate::shading::programs::{GeometryForm, ProgramShader, caster_program, geometry_program};

/// Which program of a set a pipeline is created from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Program {
    Geometry(GeometryForm),
    Caster,
}

/// One material shader's programs: SGL3D's default provider's, or a game's
/// module's.
pub(super) struct ProgramSet {
    /// The game's module, none for the default provider.
    source: Option<String>,
    label: String,
    geometry: Option<wgpu::ShaderModule>,
    shadow_masked: Option<wgpu::ShaderModule>,
    blended: Option<wgpu::ShaderModule>,
    caster: Option<wgpu::ShaderModule>,
}

impl ProgramSet {
    /// The default provider's programs.
    pub fn default_set() -> Self {
        Self {
            source: None,
            label: "SGL material".into(),
            geometry: None,
            shadow_masked: None,
            blended: None,
            caster: None,
        }
    }

    /// `shader`'s programs, of `scene`'s live shader.
    pub fn of(scene: &Scene, shader: ShaderId) -> Self {
        let shader = scene
            .shaders
            .get(shader)
            .expect("a shader a material names lives");
        Self {
            source: Some(shader.source.clone()),
            label: format!("SGL material shader {}", shader.label),
            ..Self::default_set()
        }
    }

    fn shader(&self) -> ProgramShader<'_> {
        self.source
            .as_deref()
            .map_or(ProgramShader::Default, ProgramShader::Game)
    }

    /// The module of `program` on a device of `tier`, made where it is
    /// first needed: a form that does not differ from the plain geometry
    /// program (`GeometryForm::distinct`) takes that one.
    pub fn module(
        &mut self,
        device: &wgpu::Device,
        tier: BindingTier,
        program: Program,
    ) -> &wgpu::ShaderModule {
        let program = match program {
            Program::Geometry(form) if !form.distinct(tier, self.shader()) => {
                Program::Geometry(GeometryForm::Plain)
            }
            program => program,
        };
        let Self {
            source,
            label,
            geometry,
            shadow_masked,
            blended,
            caster,
        } = self;
        let shader = source
            .as_deref()
            .map_or(ProgramShader::Default, ProgramShader::Game);
        let (slot, name) = match program {
            Program::Geometry(GeometryForm::Plain) => (geometry, "geometry"),
            Program::Geometry(GeometryForm::ShadowMask) => {
                (shadow_masked, "geometry with the shadow mask")
            }
            Program::Geometry(GeometryForm::Blended) => (blended, "blended geometry"),
            Program::Caster => (caster, "casters"),
        };
        slot.get_or_insert_with(|| {
            let source = match program {
                Program::Geometry(form) => geometry_program(form, tier, shader),
                Program::Caster => caster_program(shader),
            };
            device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(&format!("{label} {name}")),
                source: wgpu::ShaderSource::Wgsl(source.into()),
            })
        })
    }

    /// How many modules it holds.
    #[cfg(test)]
    pub fn modules(&self) -> usize {
        [
            &self.geometry,
            &self.shadow_masked,
            &self.blended,
            &self.caster,
        ]
        .into_iter()
        .filter(|module| module.is_some())
        .count()
    }
}
