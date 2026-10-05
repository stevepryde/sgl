//! The instances an observed frame's camera drew without a pixel, which the
//! camera's opaque and masked list skips while the diagnostics oracle runs
//! (`InstanceVisibility::SkipHidden`).
use crate::content::identity::{Identity, InstanceId};

/// A set of instances by identity. Each index holds its hidden instance's
/// generation, so content that later reuses the index is not in the set.
#[derive(Default)]
#[cfg_attr(
    not(feature = "diagnostics"),
    allow(dead_code, reason = "only the diagnostics oracle fills it")
)]
pub(crate) struct HiddenInstances {
    generations: Vec<Option<u64>>,
}

impl HiddenInstances {
    #[cfg(feature = "diagnostics")]
    pub fn insert(&mut self, id: InstanceId) {
        let index = id.index();
        if self.generations.len() <= index {
            self.generations.resize(index + 1, None);
        }
        self.generations[index] = Some(id.generation());
    }

    pub fn holds(&self, id: InstanceId) -> bool {
        self.generations.get(id.index()).copied().flatten() == Some(id.generation())
    }
}
