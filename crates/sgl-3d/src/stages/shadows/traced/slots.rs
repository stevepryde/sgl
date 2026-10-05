//! Which light each slot of the shadow mask holds (the architecture's
//! Ray-traced shadows). Slot 0 is the directional light with the frame's
//! cascades; slots 1 to 15 go to the casting local lights the local-light
//! atlas places, in its ranking, largest screen coverage first. A light
//! keeps its slot while the atlas places it, as it keeps its atlas slots,
//! so a slot's history is one light's; a freed slot goes to the
//! highest-ranked light without one, and its history restarts. Wicked
//! Engine's slot is a light's index among the first sixteen of its sorted
//! entity array (2ff1d9e `screenspaceshadowCS.hlsl` 76–84); SGL3D's lights
//! have no such order.
use crate::content::identity::{Identity, LightId};
use crate::shading::shadow_mask::{
    RT_SHADOW_LIGHTS, SHADOW_MASK_DIRECTIONAL, SHADOW_MASK_EMPTY, ShadowMaskSlots,
};

/// The slots' lights, kept from frame to frame.
#[derive(Clone, Debug, Default)]
pub(crate) struct Slots {
    /// Slot 0's light: the shadowed directional light's index in
    /// `FrameInput::directional_lights`.
    directional: Option<usize>,
    /// Slots 1 to 15's lights, at their slot less one.
    local: [Option<LightId>; RT_SHADOW_LIGHTS - 1],
}

impl Slots {
    /// This frame's slot table, for the shadowed directional light
    /// `directional` (by its index in `FrameInput::directional_lights`) and
    /// the local lights the atlas placed, `ranked` best first. A slot whose
    /// light changed restarts its history.
    pub fn assign(&mut self, directional: Option<usize>, ranked: &[LightId]) -> ShadowMaskSlots {
        let mut restart = 0;
        if directional.is_some() && directional != self.directional {
            restart |= 1;
        }
        self.directional = directional;
        // A light keeps its slot while the atlas places it.
        for held in &mut self.local {
            if held.is_some_and(|light| !ranked.contains(&light)) {
                *held = None;
            }
        }
        // The free slots, lowest first, go to the highest-ranked lights
        // without one.
        let held = self.local;
        let mut newcomers = ranked
            .iter()
            .filter(|light| !held.contains(&Some(**light)));
        for (slot, held) in self.local.iter_mut().enumerate() {
            if held.is_some() {
                continue;
            }
            let Some(&light) = newcomers.next() else {
                break;
            };
            *held = Some(light);
            restart |= 1 << (slot + 1);
        }
        let mut keys = [SHADOW_MASK_EMPTY; RT_SHADOW_LIGHTS];
        if directional.is_some() {
            keys[0] = SHADOW_MASK_DIRECTIONAL;
        }
        for (key, held) in keys[1..].iter_mut().zip(&self.local) {
            if let Some(light) = held {
                *key = u32::try_from(light.index()).expect("light indices fit in u32");
            }
        }
        ShadowMaskSlots::new(keys, restart)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn light(index: usize) -> LightId {
        LightId::issue(index, index as u64 + 1)
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
        let first = slots.assign(Some(1), &[a, b, c]);
        assert_eq!(keys(&first)[0], SHADOW_MASK_DIRECTIONAL);
        assert_eq!(
            [a, b, c].map(|light| slot_of(&first, light)),
            [Some(1), Some(2), Some(3)]
        );
        assert_eq!(first.restart, 0b1111, "every new light restarts");
        assert!(keys(&first)[4..].iter().all(|&key| key == SHADOW_MASK_EMPTY));

        // Reranked, each keeps its slot and its history.
        let reranked = slots.assign(Some(1), &[c, a, b]);
        assert_eq!(keys(&reranked), keys(&first));
        assert_eq!(reranked.restart, 0);

        // b leaves; of the newcomers e outranks d, so e takes b's slot, and
        // d the next free one.
        let swapped = slots.assign(Some(1), &[e, c, a, d]);
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
        assert_eq!(slots.assign(Some(0), &[e, c, a, d]).restart, 1);
        let none = slots.assign(None, &[e, c, a, d]);
        assert_eq!(keys(&none)[0], SHADOW_MASK_EMPTY);
        assert_eq!(none.restart, 0);
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn more_lights_than_slots_fill_them_in_rank_order() {
        let mut slots = Slots::default();
        let ranked: Vec<_> = (0..20).map(light).collect();
        let table = slots.assign(None, &ranked);
        for (rank, &light) in ranked.iter().enumerate() {
            let expected = (rank < RT_SHADOW_LIGHTS - 1).then_some(rank + 1);
            assert_eq!(slot_of(&table, light), expected, "rank {rank}");
        }
        // The seventh leaves: the sixteenth, the best without a slot, takes
        // its slot.
        let mut next = ranked.clone();
        next.remove(6);
        let table = slots.assign(None, &next);
        assert_eq!(slot_of(&table, ranked[15]), Some(7));
        assert_eq!(table.restart, 1 << 7);
    }
}
