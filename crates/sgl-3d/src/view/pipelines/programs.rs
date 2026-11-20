//! The shader modules the geometry pipelines are created from: one program
//! set for materials without a shader, and one for each shader a material
//! of the scene names, each holding the geometry program of each form
//! (`shading::programs::GeometryForm`) and the caster program, made when a
//! pipeline first needs it and dropped with the set.
use crate::Scene;
use crate::content::identity::ShaderId;
use crate::shading::bind::BindingTier;
use crate::shading::programs::{
    CameraShadows, GeometryForm, ProgramShader, caster_program, geometry_program,
};

/// Which program of a set a pipeline is created from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Program {
    Geometry(GeometryForm),
    Caster,
}

/// The camera lighting's program slots: one for each combination of the
/// providers `GeometryForm::Camera` composes, its shadows' three and
/// screen-space indirect light's two.
const CAMERA_SLOTS: usize = 6;

/// The camera lighting program's slot for its providers.
fn camera_slot(shadows: CameraShadows, screen_indirect: bool) -> usize {
    let shadows = match shadows {
        CameraShadows::Maps => 0,
        CameraShadows::ShadowMask => 1,
        CameraShadows::ContactMask => 2,
    };
    shadows * 2 + usize::from(screen_indirect)
}

/// One material shader's programs: SGL3D's default provider's, or a game's
/// module's.
pub(super) struct ProgramSet {
    /// The game's module, none for the default provider.
    source: Option<String>,
    label: String,
    geometry: Option<wgpu::ShaderModule>,
    /// The camera lighting's, one for each combination of what it binds
    /// at group 3, indexed by `camera_slot`.
    camera: [Option<wgpu::ShaderModule>; CAMERA_SLOTS],
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
            camera: std::array::from_fn(|_| None),
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
            camera,
            blended,
            caster,
        } = self;
        let shader = source
            .as_deref()
            .map_or(ProgramShader::Default, ProgramShader::Game);
        let (slot, name) = match program {
            Program::Geometry(GeometryForm::Plain) => (geometry, "geometry"),
            Program::Geometry(GeometryForm::Camera {
                shadows,
                screen_indirect,
            }) => {
                let name = match (shadows, screen_indirect) {
                    (CameraShadows::Maps, false) => "camera geometry",
                    (CameraShadows::ShadowMask, false) => "geometry with the shadow mask",
                    (CameraShadows::ContactMask, false) => "geometry with the contact mask",
                    (CameraShadows::Maps, true) => "geometry with screen-space indirect light",
                    (CameraShadows::ShadowMask, true) => {
                        "geometry with the shadow mask and screen-space indirect light"
                    }
                    (CameraShadows::ContactMask, true) => {
                        "geometry with the contact mask and screen-space indirect light"
                    }
                };
                (&mut camera[camera_slot(shadows, screen_indirect)], name)
            }
            Program::Geometry(GeometryForm::Blended) => (blended, "blended geometry"),
            Program::Caster => (caster, "casters"),
        };
        slot.get_or_insert_with(|| {
            let source = match program {
                Program::Geometry(form) => geometry_program(form, tier, shader),
                Program::Caster => caster_program(tier, shader),
            };
            device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(&format!("{label} {name}")),
                source: wgpu::ShaderSource::Wgsl(source.into()),
            })
        })
    }

    /// Drops its camera lighting programs that take screen-space indirect
    /// light.
    pub fn forget_screen_indirect(&mut self) {
        for shadows in [
            CameraShadows::Maps,
            CameraShadows::ShadowMask,
            CameraShadows::ContactMask,
        ] {
            self.camera[camera_slot(shadows, true)] = None;
        }
    }

    /// How many modules it holds.
    #[cfg(all(test, not(target_arch = "wasm32")))]
    pub fn modules(&self) -> usize {
        [&self.geometry, &self.blended, &self.caster]
            .into_iter()
            .chain(&self.camera)
            .filter(|module| module.is_some())
            .count()
    }
}
