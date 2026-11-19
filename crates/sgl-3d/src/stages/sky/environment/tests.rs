use super::{ATLAS, AtlasCycle, STEPS, steps};
use std::collections::BTreeSet;
use wasm_bindgen_test::wasm_bindgen_test;

// Plausible defects: a face or level left out of the cycle or written twice,
// a level filtered before the level it reads, two steps writing over each
// other, or a step past the per-frame budget. The oracle is the contract
// (X-5, #833): the 16 steps write every face of every one of the atlas's ten
// levels exactly once, each level after every face of the level before, in
// disjoint rectangles inside the 384x512 atlas, and none filters more than a
// level-1 face's 4,096 texels.
#[wasm_bindgen_test(unsupported = test)]
fn the_cycle_writes_every_face_of_every_level_once_after_the_level_before() {
    assert_eq!(ATLAS.size(), [384, 512]);
    assert_eq!(ATLAS.levels(), 10);
    let mut written = BTreeSet::new();
    let mut regions: Vec<([u32; 2], [u32; 2])> = Vec::new();
    for (index, step) in steps().iter().enumerate() {
        for face in step.first_face..step.first_face + step.faces {
            assert!(face < 6, "step {index}: face {face}");
            assert!(
                written.insert((step.level, face)),
                "step {index}: level {} face {face} written twice",
                step.level
            );
            if step.level > 0 {
                for before in 0..6 {
                    assert!(
                        written.contains(&(step.level - 1, before)),
                        "step {index}: level {} before level {} face {before}",
                        step.level,
                        step.level - 1
                    );
                }
            }
        }
        let size = ATLAS.level(step.level).size;
        if step.level > 0 {
            assert!(size * size * step.faces <= 4096, "step {index}");
        }
        let (corner, extent) = step.region();
        assert_eq!(extent[0] * extent[1], size * size * step.faces);
        assert!(corner[0] + extent[0] <= 384 && corner[1] + extent[1] <= 512);
        for &(other, other_extent) in &regions {
            let apart = (0..2).any(|axis| {
                corner[axis] >= other[axis] + other_extent[axis]
                    || other[axis] >= corner[axis] + extent[axis]
            });
            assert!(apart, "step {index} overlaps an earlier step");
        }
        regions.push((corner, extent));
    }
    assert_eq!(written.len(), 60);
}

// Plausible defects: the atlas refreshed every frame (the cost the cycle
// exists to avoid), a changed sky never captured, a change during a cycle
// lost when it ends, or a revision that does not follow the atlas's writes.
// The oracle is the contract (X-5): a new atlas runs the whole cycle; then
// a cycle starts when the sky-view table has been computed since the last
// capture, runs one step a finished frame, and ends idle; a change during a
// cycle starts another when it ends; the revision changes exactly when a
// frame writes the atlas.
#[wasm_bindgen_test(unsupported = test)]
fn a_cycle_starts_when_the_sky_view_changes_and_idles_after() {
    let mut cycle = AtlasCycle::default();
    let frame = |cycle: &mut AtlasCycle, view_computed: bool| {
        let before = cycle.revision();
        let steps = cycle.plan(view_computed, false);
        assert_eq!(cycle.revision() != before, !steps.is_empty());
        cycle.settle(true);
        steps
    };
    assert_eq!(frame(&mut cycle, true), 0..STEPS, "a new atlas");
    assert_eq!(frame(&mut cycle, false), 0..0, "a still sky");
    assert_eq!(frame(&mut cycle, true), 0..1, "the sky changed");
    for step in 1..STEPS {
        // The sky changes once during the cycle.
        assert_eq!(frame(&mut cycle, step == 5), step..step + 1);
    }
    assert_eq!(
        frame(&mut cycle, false),
        0..1,
        "a change during the last cycle starts another"
    );
    for step in 1..STEPS {
        assert_eq!(frame(&mut cycle, false), step..step + 1);
    }
    assert_eq!(frame(&mut cycle, false), 0..0, "idle once it is captured");
}

// Plausible defects: a cut showing a stale sky for 16 frames, or an
// unfinished frame's step advancing the cycle (so a frame rendered again
// skips a step and leaves part of the atlas stale) or its revision. The
// oracle is the contract (X-5, D-58): a frame that restarts the view's
// history runs every step; a frame rendered again runs the same step, and
// what it leaves is committed only when it is finished.
#[wasm_bindgen_test(unsupported = test)]
fn a_restart_runs_every_step_and_an_unfinished_frame_runs_its_step_again() {
    let mut cycle = AtlasCycle::default();
    cycle.plan(false, false);
    cycle.settle(true);
    cycle.plan(true, false);
    cycle.settle(true);
    assert_eq!(cycle.plan(false, false), 1..2);
    let revision = cycle.revision();
    cycle.settle(false);
    assert_eq!(cycle.plan(false, false), 1..2, "rendered again");
    assert_eq!(cycle.revision(), revision);
    cycle.settle(true);
    assert_eq!(
        cycle.plan(false, true),
        0..STEPS,
        "the view's history restarts"
    );
    cycle.settle(true);
    assert_eq!(cycle.plan(false, false), 0..0, "nothing left after the cut");
    cycle.settle(true);
    cycle.forget();
    assert_eq!(cycle.plan(false, false), 0..STEPS, "a new atlas");
    cycle.settle(false);
    assert_eq!(cycle.plan(false, false), 0..STEPS, "until it is finished");
}
