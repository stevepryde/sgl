//! What a view's frame applies to its resolved HDR frame
//! (`FrameInput::hdr_effect`): one of the scene's HDR effects
//! (`Scene::add_hdr_effect`) with its parameter block for this frame.
use super::identity::HdrEffectId;
use super::shader::HDR_EFFECT_PARAMS_MAX_BYTES;

/// An HDR effect and its `EffectParams` for one view's frame, made by
/// `Scene::hdr_effect`, which checks the parameters' size against the
/// effect's block. `Copy`, so a `FrameInput` carries it by value.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct HdrEffect {
    effect: HdrEffectId,
    len: u16,
    parameters: [u8; HDR_EFFECT_PARAMS_MAX_BYTES as usize],
}

impl HdrEffect {
    /// `effect` with `parameters`, which the scene checked are its
    /// `EffectParams` exactly.
    pub(crate) fn new(effect: HdrEffectId, parameters: &[u8]) -> Self {
        let mut block = [0; HDR_EFFECT_PARAMS_MAX_BYTES as usize];
        block[..parameters.len()].copy_from_slice(parameters);
        Self {
            effect,
            len: u16::try_from(parameters.len()).expect("checked against the limit"),
            parameters: block,
        }
    }

    /// The effect.
    pub fn effect(&self) -> HdrEffectId {
        self.effect
    }

    /// Its `EffectParams`, as `Scene::hdr_effect_parameters_layout` lays
    /// them out.
    pub fn parameters(&self) -> &[u8] {
        &self.parameters[..usize::from(self.len)]
    }
}

impl std::fmt::Debug for HdrEffect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HdrEffect")
            .field("effect", &self.effect)
            .field("parameters", &self.parameters())
            .finish()
    }
}
