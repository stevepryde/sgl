//! Selections: the instances each holds, with the `Scene` operations that
//! add, replace and remove them, which a view's frame outlines
//! (`FrameInput::outline`, `stages::outline`).
use super::slots::Slots;
use super::{Scene, SceneError};
use crate::content::identity::{Identity, InstanceId, SelectionId};
use crate::content::outline::Outline;

#[derive(Default)]
pub(crate) struct Selections {
    /// Each selection's instances, in index order, each once.
    pub slots: Slots<SelectionId, Vec<InstanceId>>,
}

impl Scene {
    /// Adds a selection of `instances`, which a view outlines while its
    /// frame names it (`FrameInput::outline`). Each must be an instance of
    /// this scene (`SceneError::UnknownInstance`, adding nothing); one named
    /// twice counts once, and an empty selection outlines nothing. An
    /// instance removed later is no longer outlined.
    pub fn add_selection(&mut self, instances: &[InstanceId]) -> Result<SelectionId, SceneError> {
        let instances = self.selected(instances)?;
        Ok(self.selections.slots.insert(instances))
    }

    /// Replaces a selection's instances, as `add_selection` takes them; a
    /// refused list leaves it as it was.
    pub fn set_selection(
        &mut self,
        id: SelectionId,
        instances: &[InstanceId],
    ) -> Result<(), SceneError> {
        self.selections
            .slots
            .get(id)
            .ok_or(SceneError::UnknownSelection)?;
        let instances = self.selected(instances)?;
        *self.selections.slots.get_mut(id).expect("checked above") = instances;
        Ok(())
    }

    /// Removes a selection. A frame whose input still names it outlines
    /// nothing.
    pub fn remove_selection(&mut self, id: SelectionId) -> Result<(), SceneError> {
        self.selections
            .slots
            .remove(id)
            .map(drop)
            .ok_or(SceneError::UnknownSelection)
    }

    /// `instances` as a selection holds them, each of this scene.
    fn selected(&self, instances: &[InstanceId]) -> Result<Vec<InstanceId>, SceneError> {
        let mut selected = instances.to_vec();
        if selected
            .iter()
            .any(|&id| self.instances.slots.get(id).is_none())
        {
            return Err(SceneError::UnknownInstance);
        }
        selected.sort_unstable_by_key(|id| id.index());
        selected.dedup();
        Ok(selected)
    }

    /// The outline `input` asks for and the instances it outlines, while it
    /// draws something: the scene holds its selection, the selection holds
    /// an instance and the width is above 0.
    pub(crate) fn drawn_outline(&self, input: Option<Outline>) -> Option<(Outline, &[InstanceId])> {
        let outline = input.filter(|outline| outline.drawn_width() > 0.)?;
        let instances = self.selections.slots.get(outline.selection)?;
        (!instances.is_empty()).then_some((outline, instances.as_slice()))
    }
}
