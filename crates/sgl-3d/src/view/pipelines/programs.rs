//! The shader modules the geometry pipelines are created from: one program
//! set for materials without a shader, and one for each shader a material
//! of the scene names, each holding the geometry program of each form
//! (`shading::programs::GeometryForm`) and the caster program, made when a
//! pipeline first needs it, on the thread that creates that pipeline, and
//! dropped with the set and the last pipeline job holding it.
use crate::Scene;
use crate::content::identity::ShaderId;
use crate::shading::bind::BindingTier;
use crate::shading::programs::{
    CameraShadows, GeometryForm, ProgramShader, caster_program, geometry_program,
};
use crate::view::compile::SharedModule;
use std::sync::Arc;

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

type Slot = Option<SharedModule>;

/// One material shader's programs: SGL3D's default provider's, or a game's
/// module's.
pub(super) struct ProgramSet {
    /// The game's module, none for the default provider.
    source: Option<Arc<str>>,
    label: String,
    geometry: Slot,
    /// The camera lighting's, one for each combination of what it binds
    /// at group 3, indexed by `camera_slot`.
    camera: [Slot; CAMERA_SLOTS],
    blended: Slot,
    caster: Slot,
}

impl ProgramSet {
    /// The default provider's programs.
    pub fn default_set() -> Self {
        Self {
            source: None,
            label: "SGL material".into(),
            geometry: Slot::default(),
            camera: std::array::from_fn(|_| Slot::default()),
            blended: Slot::default(),
            caster: Slot::default(),
        }
    }

    /// `shader`'s programs, of `scene`'s live shader.
    pub fn of(scene: &Scene, shader: ShaderId) -> Self {
        let shader = scene
            .shaders
            .get(shader)
            .expect("a shader a material names lives");
        Self {
            source: Some(shader.source.as_str().into()),
            label: format!("SGL material shader {}", shader.label),
            ..Self::default_set()
        }
    }

    fn shader(&self) -> ProgramShader<'_> {
        self.source
            .as_deref()
            .map_or(ProgramShader::Default, ProgramShader::Game)
    }

    /// The module of `program` on a device of `tier`: a form that does not
    /// differ from the plain geometry program (`GeometryForm::distinct`)
    /// takes that one.
    pub fn module(&mut self, tier: BindingTier, program: Program) -> SharedModule {
        let program = match program {
            Program::Geometry(form) if !form.distinct(tier, self.shader()) => {
                Program::Geometry(GeometryForm::Plain)
            }
            program => program,
        };
        let (slot, name) = match program {
            Program::Geometry(GeometryForm::Plain) => (&mut self.geometry, "geometry"),
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
                (
                    &mut self.camera[camera_slot(shadows, screen_indirect)],
                    name,
                )
            }
            Program::Geometry(GeometryForm::Blended) => (&mut self.blended, "blended geometry"),
            Program::Caster => (&mut self.caster, "casters"),
        };
        let (source, label) = (&self.source, &self.label);
        slot.get_or_insert_with(|| {
            let source = source.clone();
            SharedModule::new(format!("{label} {name}"), move || {
                let shader = source
                    .as_deref()
                    .map_or(ProgramShader::Default, ProgramShader::Game);
                match program {
                    Program::Geometry(form) => geometry_program(form, tier, shader),
                    Program::Caster => caster_program(tier, shader),
                }
            })
        })
        .clone()
    }

    /// How many modules it holds.
    #[cfg(all(test, not(target_arch = "wasm32")))]
    pub fn modules(&self) -> usize {
        [&self.geometry, &self.blended, &self.caster]
            .into_iter()
            .chain(&self.camera)
            .filter(|module| module.as_ref().is_some_and(SharedModule::created))
            .count()
    }
}
