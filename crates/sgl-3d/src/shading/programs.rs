//! The programs that rasterise scene geometry: the geometry program, whose
//! entry points every camera and probe-capture pass is created with, and the
//! caster program of every shadow view, with the entry points their
//! pipelines name. `view::pipelines` creates its pipelines from them; they
//! are the shading layer's so that the scene can compose and validate the
//! same programs (specs/sgl3d-architecture.md, Shader contract).
use super::bind::BindingTier;
use super::shader::{
    SHADER_DEFAULT, SHADER_INPUTS_BOUND, SHADER_INPUTS_NONE, SHADER_SCENE_DEPTH,
    SHADER_SCENE_DEPTH_NONE,
};
use super::{Module, compose};

/// The material shader a program composes: SGL3D's default provider, whose
/// functions return their argument, or a game's module
/// (`Scene::add_shader`), composed last, after the parameter blocks it reads
/// (`shading::shader`).
#[derive(Clone, Copy, Debug)]
pub(crate) enum ProgramShader<'a> {
    Default,
    Game(&'a str),
}

/// What marks where a game's module starts in a program it is composed
/// into.
pub(crate) const GAME_MODULE_HEADER: &str = "\n// ---- module game shader\n";

/// `roots` composed with `shader`: the default provider and no binding of
/// its inputs, or the bindings of its parameter blocks and instance data
/// and the game's module after them.
fn shaded(roots: &[&'static Module], shader: ProgramShader<'_>) -> String {
    match shader {
        ProgramShader::Default => {
            let mut all = roots.to_vec();
            all.extend([&SHADER_DEFAULT, &SHADER_INPUTS_NONE]);
            compose(&all)
        }
        ProgramShader::Game(source) => {
            let mut all = roots.to_vec();
            all.push(&SHADER_INPUTS_BOUND);
            let mut program = compose(&all);
            program.push_str(GAME_MODULE_HEADER);
            program.push_str(source);
            program.push('\n');
            program
        }
    }
}

/// Where the camera lighting's surfaces take their shadows from beside the
/// maps: one mask at most, bound at the pass's group 3.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum CameraShadows {
    /// The maps alone.
    Maps,
    /// The lights the ray-traced shadow mask holds, from it
    /// (`shading::bind::shadow_mask`).
    ShadowMask,
    /// The directional light with the frame's cascades, the lesser of its
    /// cascades and the contact mask (`shading::bind::contact_shadow`).
    ContactMask,
}

/// Which geometry program a pass draws with: the one every pass but the
/// opaque stage's camera lighting and the camera's blended draws takes;
/// the camera lighting's (the fused pass and the lighting pass) with what
/// it binds at its group 3: the shadow mask's provider while ray-traced
/// shadows run, the contact provider while contact shadows run (the
/// lighting pass's alone), and screen-space indirect light's while that
/// pass has a kept result; and the blended draws', which on the Extended
/// binding tier give a game's shader the opaque depth
/// (shader_scene_depth.wgsl).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum GeometryForm {
    Plain,
    Camera {
        shadows: CameraShadows,
        screen_indirect: bool,
    },
    Blended,
}

impl GeometryForm {
    /// Whether this form's program on a device of `tier` with `shader`
    /// differs from the plain one: the camera lighting's with a mask or
    /// screen-space indirect light, the blended draws' only for a game's
    /// shader on the
    /// Extended tier.
    pub fn distinct(self, tier: BindingTier, shader: ProgramShader<'_>) -> bool {
        match self {
            Self::Plain => false,
            Self::Camera {
                shadows,
                screen_indirect,
            } => shadows != CameraShadows::Maps || screen_indirect,
            Self::Blended => {
                tier == BindingTier::Extended && matches!(shader, ProgramShader::Game(_))
            }
        }
    }
}

/// Scene geometry's camera and probe-capture passes. A program composes it
/// with one shadow-mask provider, one contact provider, one screen-space
/// indirect light provider, one lit provider, one material-map provider and
/// one transmission provider (`geometry_program`).
pub(crate) static GEOMETRY: Module = Module {
    name: "geometry",
    source: sgl_post_fx::embed_wgsl!("geometry.wgsl"),
    deps: &[
        &super::BIND_LIT,
        &super::BIND_SCENE,
        &super::BIND_MATERIAL,
        &super::BIND_BLENDED,
        &super::GBUFFER,
        &super::VERTEX,
        &super::VERTEX_PULL,
        &super::SURFACE,
        &super::SURFACE_RASTER,
        &super::FRAME_FOG,
        &super::TEMPORAL_MASKS,
        &super::TRANSMISSION,
        &super::MATERIAL_SHADER,
    ],
};
/// The entry points the geometry passes' pipelines are created with, from
/// either geometry program: the vertex all share and each pass's fragment.
pub(crate) const SOURCE_VS_ENTRY: &str = "source_vs";
pub(crate) const FS_ENTRY: &str = "fs";
pub(crate) const STABLE_FS_ENTRY: &str = "stable_fs";
pub(crate) const STABLE_LEGACY_FS_ENTRY: &str = "stable_legacy_fs";
pub(crate) const ANISOTROPY_FS_ENTRY: &str = "anisotropy_fs";
pub(crate) const SOURCE_FS_ENTRY: &str = "source_fs";
pub(crate) const FUSED_OPAQUE_FS_ENTRY: &str = "fused_opaque_fs";
pub(crate) const BLENDED_FS_ENTRY: &str = "blended_fs";
pub(crate) const BLENDED_MASKED_FS_ENTRY: &str = "blended_masked_fs";
pub(crate) const RECEIVER_FS_ENTRY: &str = "receiver_fs";
pub(crate) const TEMPORAL_COMPOSITION_FS_ENTRY: &str = "temporal_composition_fs";
/// The volume layers' fragments, each keeping one side
/// (`view::pipelines::GeometryPass::VolumeEntry` and its siblings).
pub(crate) const VOLUME_ENTRY_FS_ENTRY: &str = "volume_entry_fs";
pub(crate) const VOLUME_EXIT_FS_ENTRY: &str = "volume_exit_fs";
pub(crate) const VOLUME_SECOND_EXIT_FS_ENTRY: &str = "volume_second_exit_fs";

/// The geometry program of `form` on a device of `tier` with `shader`:
/// `GEOMETRY` with the shadow mask's provider for a camera form with the
/// shadow mask, else with the provider that holds no slot; the contact
/// provider for a camera form with the contact mask, else the one that
/// leaves the maps alone; screen-space indirect light's provider for a
/// camera form with it, else the one that gives none; the tier's lit,
/// material-map and transmission providers; and the scene depth provider of
/// a game's shader's blended draws on the Extended tier, else none.
pub(crate) fn geometry_program(
    form: GeometryForm,
    tier: BindingTier,
    shader: ProgramShader<'_>,
) -> String {
    let (shadows, screen_indirect) = match form {
        GeometryForm::Camera {
            shadows,
            screen_indirect,
        } => (shadows, screen_indirect),
        _ => (CameraShadows::Maps, false),
    };
    let mask = if shadows == CameraShadows::ShadowMask {
        &super::SHADOW_MASK
    } else {
        &super::SHADOW_MASK_NONE
    };
    let contact = if shadows == CameraShadows::ContactMask {
        &super::CONTACT_SHADOW
    } else {
        &super::CONTACT_SHADOW_NONE
    };
    let bounce = if screen_indirect {
        &super::CAMERA_SCREEN_INDIRECT
    } else {
        &super::CAMERA_SCREEN_INDIRECT_NONE
    };
    let depth = if form == GeometryForm::Blended && form.distinct(tier, shader) {
        &SHADER_SCENE_DEPTH
    } else {
        &SHADER_SCENE_DEPTH_NONE
    };
    shaded(
        &[
            &GEOMETRY,
            mask,
            contact,
            bounce,
            super::lit_provider(tier),
            super::material_provider(tier),
            super::transmission_provider(tier),
            depth,
        ],
        shader,
    )
}

/// The directional and local-light shadow casters.
pub(crate) static CASTER: Module = Module {
    name: "caster",
    source: sgl_post_fx::embed_wgsl!("caster.wgsl"),
    deps: &[
        &super::BIND_SHADOW,
        &super::BIND_SCENE,
        &super::SCENE_RAYS,
        &super::VERTEX_PULL,
        &super::MATERIAL_SURFACE_RASTER,
        &super::BIND_CASTER_POSITIONS,
        &super::MATERIAL_SHADER,
    ],
};
/// The entry points the casters' pipelines are created with: a vertex for
/// each way a caster's vertices arrive and its depth is clipped, and the
/// fragments that clamp depth or cut out masked texels.
pub(crate) const SHADOW_VS_ENTRY: &str = "shadow_vs";
pub(crate) const SHADOW_UNCLIPPED_VS_ENTRY: &str = "shadow_unclipped_vs";
pub(crate) const SHADOW_MASKED_VS_ENTRY: &str = "shadow_masked_vs";
pub(crate) const SHADOW_MASKED_UNCLIPPED_VS_ENTRY: &str = "shadow_masked_unclipped_vs";
pub(crate) const SHADOW_PULLED_VS_ENTRY: &str = "shadow_pulled_vs";
pub(crate) const SHADOW_PULLED_UNCLIPPED_VS_ENTRY: &str = "shadow_pulled_unclipped_vs";
pub(crate) const SHADOW_PULLED_MASKED_VS_ENTRY: &str = "shadow_pulled_masked_vs";
pub(crate) const SHADOW_PULLED_MASKED_UNCLIPPED_VS_ENTRY: &str =
    "shadow_pulled_masked_unclipped_vs";
pub(crate) const SHADOW_PAIRED_VS_ENTRY: &str = "shadow_paired_vs";
pub(crate) const SHADOW_PAIRED_UNCLIPPED_VS_ENTRY: &str = "shadow_paired_unclipped_vs";
pub(crate) const SHADOW_UNCLIPPED_FS_ENTRY: &str = "shadow_unclipped_fs";
pub(crate) const SHADOW_MASKED_FS_ENTRY: &str = "shadow_masked_fs";
pub(crate) const SHADOW_MASKED_UNCLIPPED_FS_ENTRY: &str = "shadow_masked_unclipped_fs";
/// A masked material's casters where it has a shader, whose surface
/// function gives their coverage.
pub(crate) const SHADOW_SHADED_MASKED_VS_ENTRY: &str = "shadow_shaded_masked_vs";
pub(crate) const SHADOW_SHADED_MASKED_UNCLIPPED_VS_ENTRY: &str =
    "shadow_shaded_masked_unclipped_vs";
pub(crate) const SHADOW_PULLED_SHADED_MASKED_VS_ENTRY: &str = "shadow_pulled_shaded_masked_vs";
pub(crate) const SHADOW_PULLED_SHADED_MASKED_UNCLIPPED_VS_ENTRY: &str =
    "shadow_pulled_shaded_masked_unclipped_vs";
pub(crate) const SHADOW_SHADED_MASKED_FS_ENTRY: &str = "shadow_shaded_masked_fs";
pub(crate) const SHADOW_SHADED_MASKED_UNCLIPPED_FS_ENTRY: &str =
    "shadow_shaded_masked_unclipped_fs";

/// The caster program with `shader`.
pub(crate) fn caster_program(tier: BindingTier, shader: ProgramShader<'_>) -> String {
    shaded(
        &[
            &CASTER,
            super::material_provider(tier),
            &SHADER_SCENE_DEPTH_NONE,
        ],
        shader,
    )
}
