//! Which light each slot of the shadow mask holds (the architecture's
//! Ray-traced shadows). Slot 0 is the directional light with the frame's
//! cascades; slots 1 to 15 go to the casting local lights the local-light
//! atlas places, in its ranking, largest screen coverage first. A light
//! keeps its slot while the atlas places it, as it keeps its atlas slots,
//! so a slot's history is one light's; a freed slot goes to the
//! highest-ranked light without one, and its history restarts. Wicked
//! Engine's slot is a light's index among the first sixteen of its sorted
//! entity array (2ff1d9e `screenspaceshadowCS.hlsl` 76–84); SGL3D's lights
//! have no such order. Each held slot's bit in the table's `baked` says
//! whether its light is baked, as the scene holds it this frame; a slot
//! whose light turns baked or live restarts too, since its history then
//! records another set of receivers' rays.
use crate::content::identity::{Identity, LightId};
use crate::shading::lights::SHADOW_OPACITY_CUTOFF;
use crate::shading::shadow_mask::{
    RT_SHADOW_LIGHTS, SHADOW_MASK_DIRECTIONAL, SHADOW_MASK_EMPTY, ShadowMaskSlots,
};
use crate::shading::uniforms::FrameUniform;

/// The lights a frame's slots may hold: the directional light with the
/// frame's cascades, by its index in `FrameInput::directional_lights`, and
/// the casting local lights the atlas placed, best first, each with whether
/// it is baked (`Light::baked`); each only with a shadow above the opacity
/// cutoff, which lighting looks up.
#[derive(Clone, Debug, Default)]
pub(crate) struct SlotLights {
    pub directional: Option<usize>,
    pub local: Vec<(LightId, bool)>,
}

impl SlotLights {
    /// The lights of a frame of `scene` seen as `input`, whose frame data
    /// is `frame`, with the local lights the atlas placed `ranked`.
    pub fn of(
        input: &crate::FrameInput,
        frame: &FrameUniform,
        scene: &crate::Scene,
        ranked: &[LightId],
    ) -> Self {
        let directional = crate::view::directional_shadow(input)
            .map(|(index, _)| index)
            .filter(|&index| {
                frame.directional_lights[index].shadow_opacity > SHADOW_OPACITY_CUTOFF
            });
        let local = ranked
            .iter()
            .filter_map(|&id| {
                let light = scene.light(id).ok()?;
                (light.shadow_opacity > SHADOW_OPACITY_CUTOFF).then_some((id, light.baked))
            })
            .collect();
        Self { directional, local }
    }

    /// Whether no slot holds a light.
    pub fn is_empty(&self) -> bool {
        self.directional.is_none() && self.local.is_empty()
    }
}

/// The slots' lights, kept from frame to frame.
#[derive(Clone, Debug, Default)]
pub(crate) struct Slots {
    /// Slot 0's light: the shadowed directional light's index in
    /// `FrameInput::directional_lights`.
    directional: Option<usize>,
    /// Slots 1 to 15's lights, at their slot less one.
    local: [Option<LightId>; RT_SHADOW_LIGHTS - 1],
    /// The last table's `baked` bits.
    baked: u32,
}

impl Slots {
    /// This frame's slot table, for the shadowed directional light
    /// `directional` (by its index in `FrameInput::directional_lights`) and
    /// the local lights the atlas placed, `ranked` best first, each with
    /// whether it is baked. A slot whose light changed, or turned baked or
    /// live, restarts its history.
    pub fn assign(
        &mut self,
        directional: Option<usize>,
        ranked: &[(LightId, bool)],
    ) -> ShadowMaskSlots {
        let mut restart = 0;
        if directional.is_some() && directional != self.directional {
            restart |= 1;
        }
        self.directional = directional;
        let placed = |light: LightId| ranked.iter().any(|&(id, _)| id == light);
        // A light keeps its slot while the atlas places it.
        for held in &mut self.local {
            if held.is_some_and(|light| !placed(light)) {
                *held = None;
            }
        }
        // The free slots, lowest first, go to the highest-ranked lights
        // without one.
        let held = self.local;
        let mut newcomers = ranked
            .iter()
            .filter(|&&(light, _)| !held.contains(&Some(light)));
        for (slot, held) in self.local.iter_mut().enumerate() {
            if held.is_some() {
                continue;
            }
            let Some(&(light, _)) = newcomers.next() else {
                break;
            };
            *held = Some(light);
            restart |= 1 << (slot + 1);
        }
        let mut keys = [SHADOW_MASK_EMPTY; RT_SHADOW_LIGHTS];
        if directional.is_some() {
            keys[0] = SHADOW_MASK_DIRECTIONAL;
        }
        let (mut held, mut baked) = (0, 0);
        for (slot, light) in self.local.iter().enumerate() {
            if let Some(light) = light {
                keys[slot + 1] = u32::try_from(light.index()).expect("light indices fit in u32");
                held |= 1 << (slot + 1);
                if ranked
                    .iter()
                    .any(|&(id, is_baked)| id == *light && is_baked)
                {
                    baked |= 1 << (slot + 1);
                }
            }
        }
        // A held slot whose light turned baked or live.
        restart |= (baked ^ self.baked) & held;
        self.baked = baked;
        ShadowMaskSlots::new(keys, restart, baked)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn light(index: usize) -> LightId {
        LightId::issue(index, index as u64 + 1)
    }

    /// `lights` ranked, none of them baked.
    fn live(lights: &[LightId]) -> Vec<(LightId, bool)> {
        lights.iter().map(|&light| (light, false)).collect()
    }

    /// Each slot's key, slot by slot.
    fn keys(table: &ShadowMaskSlots) -> Vec<u32> {
        table.lights.iter().flatten().copied().collect()
    }

    /// The slot holding `light`.
    fn slot_of(table: &ShadowMaskSlots, light: LightId) -> Option<usize> {
        keys(table)
            .iter()
            .position(|&key| key == light.index() as u32)
    }

    // Plausible defects: slots reassigned by rank each frame (a light's
    // history then mixes lights'), a slot kept for a light the atlas no
    // longer places, a freed slot given to a lower-ranked light, more
    // lights than slots overflowing, the directional light outside slot 0,
    // or restarts missing for a slot whose light changed, or set for one
    // whose light stayed. The oracle is the architecture's rule.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_light_keeps_its_slot_while_placed_and_a_freed_slot_goes_to_the_best_ranked() {
        let mut slots = Slots::default();
        let [a, b, c, d, e] = [10, 11, 12, 13, 14].map(light);
        let first = slots.assign(Some(1), &live(&[a, b, c]));
        assert_eq!(keys(&first)[0], SHADOW_MASK_DIRECTIONAL);
        assert_eq!(
            [a, b, c].map(|light| slot_of(&first, light)),
            [Some(1), Some(2), Some(3)]
        );
        assert_eq!(first.restart, 0b1111, "every new light restarts");
        assert!(
            keys(&first)[4..]
                .iter()
                .all(|&key| key == SHADOW_MASK_EMPTY)
        );

        // Reranked, each keeps its slot and its history.
        let reranked = slots.assign(Some(1), &live(&[c, a, b]));
        assert_eq!(keys(&reranked), keys(&first));
        assert_eq!(reranked.restart, 0);

        // b leaves; of the newcomers e outranks d, so e takes b's slot, and
        // d the next free one.
        let swapped = slots.assign(Some(1), &live(&[e, c, a, d]));
        assert_eq!(slot_of(&swapped, e), Some(2));
        assert_eq!(slot_of(&swapped, d), Some(4));
        assert_eq!(slot_of(&swapped, b), None);
        assert_eq!(
            [a, c].map(|light| slot_of(&swapped, light)),
            [Some(1), Some(3)]
        );
        assert_eq!(swapped.restart, 1 << 2 | 1 << 4);

        // The directional light changes from the second to the first, then
        // casts no shadow.
        assert_eq!(slots.assign(Some(0), &live(&[e, c, a, d])).restart, 1);
        let none = slots.assign(None, &live(&[e, c, a, d]));
        assert_eq!(keys(&none)[0], SHADOW_MASK_EMPTY);
        assert_eq!(none.restart, 0);
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn more_lights_than_slots_fill_them_in_rank_order() {
        let mut slots = Slots::default();
        let ranked: Vec<_> = (0..20).map(light).collect();
        let table = slots.assign(None, &live(&ranked));
        for (rank, &light) in ranked.iter().enumerate() {
            let expected = (rank < RT_SHADOW_LIGHTS - 1).then_some(rank + 1);
            assert_eq!(slot_of(&table, light), expected, "rank {rank}");
        }
        // The seventh leaves: the sixteenth, the best without a slot, takes
        // its slot.
        let mut next = ranked.clone();
        next.remove(6);
        let table = slots.assign(None, &live(&next));
        assert_eq!(slot_of(&table, ranked[15]), Some(7));
        assert_eq!(table.restart, 1 << 7);
    }

    // Plausible defects: a slot's baked bit taken from another slot's light,
    // set only when a light takes its slot (so a light whose flag changes
    // keeps its old bit and the trace keeps skipping or tracing the wrong
    // receivers), left set on a freed slot, and a flip that does not
    // restart the slot's history. The oracle is the architecture's rule:
    // each held slot's bit is its light's flag this frame, and a slot whose
    // light kept its place but turned baked or live restarts.
    #[wasm_bindgen_test(unsupported = test)]
    fn each_held_slot_says_whether_its_light_is_baked_this_frame() {
        let mut slots = Slots::default();
        let [a, b, c] = [10, 11, 12].map(light);
        let first = slots.assign(None, &[(a, false), (b, true), (c, false)]);
        assert_eq!(first.baked, 1 << 2);
        let flipped = slots.assign(None, &[(a, false), (b, false), (c, true)]);
        assert_eq!(flipped.baked, 1 << 3);
        assert_eq!(flipped.restart, 1 << 2 | 1 << 3);
        let reranked = slots.assign(None, &[(c, true), (a, false), (b, false)]);
        assert_eq!(reranked.baked, 1 << 3);
        assert_eq!(reranked.restart, 0);
        let left = slots.assign(None, &[(a, false), (b, false)]);
        assert_eq!(left.baked, 0);
        assert_eq!(left.restart, 0);
    }
}
