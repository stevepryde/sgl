//! HDR effects: each one's validated WGSL module and its parameter block's
//! layout, with the `Scene` operations that add, read and remove them and
//! make a frame's effect with its parameters (the architecture's Scene
//! content, HDR effect).
use super::slots::Slots;
use super::{Scene, SceneError};
use crate::content::hdr_effect::HdrEffect;
use crate::content::identity::HdrEffectId;
use crate::content::shader::{ShaderParamsLayout, ShaderSource};

pub(crate) struct HdrEffectModule {
    /// The game's module, which its program composes
    /// (`shading::hdr_effect::program`).
    pub source: String,
    pub label: String,
    pub layout: ShaderParamsLayout,
}

#[derive(Default)]
pub(crate) struct HdrEffects {
    pub slots: Slots<HdrEffectId, HdrEffectModule>,
}

impl HdrEffects {
    pub fn get(&self, id: HdrEffectId) -> Result<&HdrEffectModule, SceneError> {
        self.slots.get(id).ok_or(SceneError::UnknownHdrEffect)
    }
}

impl Scene {
    /// Adds a game's HDR effect: one WGSL module that defines
    /// `struct EffectParams` and `hdr_effect` over the HDR effect contract
    /// (the package README's "HDR effects"), which a view's frame applies to
    /// its antialiased HDR frame through `FrameInput::hdr_effect`. It is
    /// validated here, composed into its program, with naga's validation
    /// that wgpu runs, so a module this accepts does not fail it later; one
    /// it refuses adds nothing
    /// (`SceneError::Shader`). Each view creates its pipeline on the first
    /// frame that applies it, or in the job `Renderer::compile` queues for
    /// that frame.
    pub fn add_hdr_effect(&mut self, source: ShaderSource) -> Result<HdrEffectId, SceneError> {
        let layout = crate::shading::shader::validate_hdr_effect(&source.wgsl)?;
        Ok(self.hdr_effects.slots.insert(HdrEffectModule {
            source: source.wgsl,
            label: source.label,
            layout,
        }))
    }

    /// Removes an HDR effect. A frame whose input still names it applies
    /// none, and each view drops its pipeline at its next frame.
    pub fn remove_hdr_effect(&mut self, id: HdrEffectId) -> Result<(), SceneError> {
        self.hdr_effects.get(id)?;
        self.hdr_effects.slots.remove(id);
        Ok(())
    }

    /// The uniform layout naga gives the effect's `EffectParams`, which a
    /// game's Rust mirror of the block checks itself against.
    pub fn hdr_effect_parameters_layout(
        &self,
        id: HdrEffectId,
    ) -> Result<&ShaderParamsLayout, SceneError> {
        Ok(&self.hdr_effects.get(id)?.layout)
    }

    /// The frame value that applies `effect` with `parameters`, its
    /// `EffectParams` as `hdr_effect_parameters_layout` lays them out
    /// (`FrameInput::hdr_effect`): exactly the block's size, else
    /// `SceneError::HdrEffectParameters`.
    pub fn hdr_effect(
        &self,
        effect: HdrEffectId,
        parameters: &[u8],
    ) -> Result<HdrEffect, SceneError> {
        let expected = self.hdr_effects.get(effect)?.layout.size;
        if parameters.len() != expected as usize {
            return Err(SceneError::HdrEffectParameters {
                expected,
                given: u32::try_from(parameters.len()).unwrap_or(u32::MAX),
            });
        }
        Ok(HdrEffect::new(effect, parameters))
    }

    /// The module of the effect `input` names, while the scene holds it.
    pub(crate) fn applied_hdr_effect(
        &self,
        input: Option<HdrEffect>,
    ) -> Option<(HdrEffect, &HdrEffectModule)> {
        let effect = input?;
        Some((effect, self.hdr_effects.get(effect.effect()).ok()?))
    }
}
