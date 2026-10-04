//! What each slot of the atlas holds, as of the last finished frame, and
//! what this frame changes. A slot's static layer (in the static atlas) is
//! its face's static casters; its frame face (in the frame atlas) is that
//! layer with the face's moving casters drawn over it. Either is reusable
//! while what it shows is unchanged: its `FaceKey`, the static edits that
//! reach it and, for the frame face, its moving casters. Unreal's cached
//! shadow maps for movable lights and HDRP's mixed cached shadows document
//! the technique (their documentation only).
//!
//! What a frame draws commits at `finish_frame` (S3D-4): a slot drawn in a
//! frame that is abandoned, or submitted but not finished, holds nothing
//! reusable.
//!
//! What a slot holds is kept in the render frame of the scene's origin it
//! was drawn at. A frame whose scene moved its origin since translates the
//! lights and moving casters it records into its own (the architecture's
//! History contract), and its layers stay valid: a layer is depth from its
//! light, which the scene translated alike.
use super::shape::LightView;
use crate::content::identity::{InstanceId, LightId, ModelId};
use crate::scene::origin::translated;
use glam::{DVec3, Mat4};
use std::collections::HashMap;

/// What a face's static layer shows, apart from static edits: when it
/// changes, the layer is stale.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct FaceKey {
    pub light: LightId,
    pub view: LightView,
    pub face: usize,
    /// The frame's visibility mask.
    pub mask: u32,
    /// The scene's material caster revision (`Materials::casters`).
    pub casters: u64,
}

/// A moving instance a face draws, as it drew it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct MovingCaster {
    pub instance: InstanceId,
    pub pose: Mat4,
    pub model: ModelId,
    /// Its model's geometry (`Model::geometry`).
    pub geometry: u64,
    /// Its deformation's revision (`InstanceDeformation::revision`); zero
    /// when it does not deform.
    pub deformation: u64,
}

/// What a slot holds.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Slot {
    /// Its static layer, in the static atlas.
    pub layer: Option<FaceKey>,
    /// Its frame face, in the frame atlas: the static casters of a key and
    /// these moving ones.
    pub face: Option<(FaceKey, Vec<MovingCaster>)>,
}

/// The atlas's slots and shadowed lights as of the last finished frame, and
/// what the frame being rendered changes.
#[derive(Default)]
pub(super) struct Cache {
    slots: Vec<Slot>,
    /// Each shadowed light's view in the last finished frame.
    lights: HashMap<LightId, LightView>,
    /// The scene's finished frames when the last finished frame examined its
    /// static edits.
    examined: Option<u64>,
    /// The scene the slots show.
    scene: Option<u64>,
    /// The scene's render origin what the slots and lights record is in.
    origin: DVec3,
    pending: Pending,
}

#[derive(Default)]
struct Pending {
    slots: Vec<(usize, Slot)>,
    lights: HashMap<LightId, LightView>,
    examined: Option<u64>,
}

impl Cache {
    /// Starts a frame of scene `scene` with `slots` slots, whose static
    /// edits have `finished` frames behind them, at render origin `origin`:
    /// the previous frame's changes are dropped unless it finished,
    /// everything is stale for another scene or when a finished frame's
    /// edits went unexamined, and what is kept is translated into the
    /// origin's render frame.
    pub fn begin(&mut self, scene: u64, slots: usize, finished: u64, origin: DVec3) {
        self.pending = Pending {
            examined: Some(finished),
            ..Pending::default()
        };
        let current = self.scene == Some(scene)
            && self
                .examined
                .is_some_and(|examined| examined + 1 >= finished);
        if !current || self.slots.len() != slots {
            self.slots = vec![Slot::default(); slots];
            self.lights.clear();
        } else if origin != self.origin {
            self.move_origin((origin - self.origin).as_vec3());
        }
        self.scene = Some(scene);
        self.origin = origin;
    }

    /// Translates what the slots and lights record by `by`, as the scene
    /// translated its lights and instances.
    fn move_origin(&mut self, by: glam::Vec3) {
        let moved = |key: &mut FaceKey| key.view.position -= by;
        for slot in &mut self.slots {
            if let Some(key) = &mut slot.layer {
                moved(key);
            }
            if let Some((key, casters)) = &mut slot.face {
                moved(key);
                for caster in casters {
                    caster.pose = translated(caster.pose, by);
                }
            }
        }
        for view in self.lights.values_mut() {
            view.position -= by;
        }
    }

    pub fn slot(&self, slot: usize) -> &Slot {
        &self.slots[slot]
    }

    /// Every slot with content, for static edits to mark.
    pub fn held(&mut self) -> impl Iterator<Item = (usize, &mut Slot)> {
        self.slots
            .iter_mut()
            .enumerate()
            .filter(|(_, slot)| slot.layer.is_some() || slot.face.is_some())
    }

    /// `light`'s view in the last finished frame, while it had a shadow.
    pub fn light(&self, light: LightId) -> Option<&LightView> {
        self.lights.get(&light)
    }

    /// `light` has a shadow this frame, seen as `view`.
    pub fn shadowed(&mut self, light: LightId, view: LightView) {
        self.pending.lights.insert(light, view);
    }

    /// This frame draws `slot` to hold `content`. Until the frame finishes,
    /// the slot holds nothing reusable.
    pub fn draw(&mut self, slot: usize, content: Slot) {
        self.slots[slot] = Slot::default();
        self.pending.slots.push((slot, content));
    }

    /// Commits the frame last begun, once submitted.
    pub fn finish(&mut self) {
        let pending = std::mem::take(&mut self.pending);
        for (slot, content) in pending.slots {
            self.slots[slot] = content;
        }
        self.lights = pending.lights;
        self.examined = pending.examined.or(self.examined);
    }
}
