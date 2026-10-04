//! The allocator's placements, judged by the atlas's geometry and the
//! eviction and hysteresis rules it ports.
use super::*;
use crate::content::identity::Identity;
use wasm_bindgen_test::wasm_bindgen_test;

/// The atlas's size: its slots are 512, 256, 128 and 64 texels.
const SIZE: u32 = 4096;

fn light(index: usize) -> LightId {
    LightId::issue(index, index as u64 + 1)
}

/// Each held face's texel rectangle, by its light.
fn rects(atlas: &Atlas) -> Vec<(LightId, [u32; 4])> {
    let mut rects = Vec::new();
    for (&light, &allocation) in &atlas.owners {
        let placement = atlas.placement(allocation);
        for face in 0..allocation.count {
            let [x, y] = placement.origin(face);
            rects.push((light, [x, y, x + placement.size, y + placement.size]));
        }
    }
    rects
}

// Plausible defects: a slot given to two lights, a stolen light keeping its
// placement, faces crossing a quadrant's edge or the atlas's, or a slot
// origin computed with the wrong quadrant or subdivision. The oracle is the
// rectangles' geometry: every held face lies inside the atlas and overlaps no
// other.
#[wasm_bindgen_test(unsupported = test)]
fn held_faces_never_overlap() {
    let mut atlas = Atlas::new(SIZE);
    let mut state = 0x2545_f491_4f6c_dd1du64;
    let mut random = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for frame in 1..400 {
        let mut lights = Vec::new();
        for index in 0..120 {
            if random() % 3 != 0 {
                let coverage = (random() % 1000) as f32 / 500.;
                let faces = if random() % 2 == 0 { 1 } else { 6 };
                lights.push((light(index), coverage, faces));
            }
        }
        lights.sort_by(|a, b| b.1.total_cmp(&a.1));
        for (id, coverage, faces) in lights {
            atlas.update(id, coverage, faces, frame);
        }
        let rects = rects(&atlas);
        for (i, (light, a)) in rects.iter().enumerate() {
            assert!(a[2] <= SIZE && a[3] <= SIZE, "{light:?} {a:?}");
            for (other, b) in &rects[i + 1..] {
                let disjoint = a[2] <= b[0] || b[2] <= a[0] || a[3] <= b[1] || b[3] <= a[1];
                assert!(
                    disjoint,
                    "frame {frame}: {light:?} {a:?} and {other:?} {b:?}"
                );
            }
        }
    }
}

// Plausible defects: no hysteresis (a light whose wanted size changes moves,
// and redraws, every frame), or one that never ends. The oracle is Godot's
// rule: a light holds its slots for REALLOCATION_FRAMES before it moves to
// the size it wants.
#[wasm_bindgen_test(unsupported = test)]
fn a_light_moves_to_its_wanted_size_only_after_holding_its_slots() {
    let mut atlas = Atlas::new(SIZE);
    let small = atlas.update(light(0), 0., 1, 1).unwrap();
    assert_eq!(small.size, 64);
    for frame in 2..=1 + REALLOCATION_FRAMES {
        assert_eq!(atlas.update(light(0), 1., 1, frame), Some(small));
    }
    let large = atlas
        .update(light(0), 1., 1, 2 + REALLOCATION_FRAMES)
        .unwrap();
    assert_eq!(large.size, 512);
}

/// Fills the 64-texel quadrant with one-slot lights 0..1024 seen at frame 1,
/// except `stale`, last seen at frame `stale_frame`.
fn full(stale: usize, stale_frame: u64) -> Atlas {
    let mut atlas = Atlas::new(SIZE);
    for index in 0..1024 {
        atlas.update(light(index), 0., 1, 1).unwrap();
    }
    for frame in 2..=stale_frame {
        atlas.update(light(stale), 0., 1, frame).unwrap();
    }
    atlas
}

// Plausible defects: evicting a light seen more recently than another, a
// light seen this frame, or a light within its first REALLOCATION_FRAMES.
// The oracle is Godot's eviction rule: a light without room takes the slots
// of the least recently seen light not seen this frame that has held them
// long enough, and none when there is none.
#[wasm_bindgen_test(unsupported = test)]
fn a_full_quadrant_gives_up_the_least_recently_seen_light() {
    // Light 7 was seen more recently than every other: light 0, the first
    // of the least recently seen, loses its slot to light 2000.
    let mut atlas = full(7, 5);
    let frame = 2 + REALLOCATION_FRAMES;
    let taken = atlas.update(light(2000), 0., 1, frame).unwrap();
    assert_eq!(taken.origin(0), [2048, 2048]);
    assert!(!atlas.owners.contains_key(&light(0)));
    assert!(atlas.owners.contains_key(&light(7)));
    // Lights within their first REALLOCATION_FRAMES keep their slots.
    let mut atlas = full(7, 5);
    assert_eq!(atlas.update(light(2000), 0., 1, REALLOCATION_FRAMES), None);
    // So do lights seen this frame.
    let mut atlas = full(7, 5);
    for index in 0..1024 {
        atlas.update(light(index), 0., 1, frame).unwrap();
    }
    assert_eq!(atlas.update(light(2000), 0., 1, frame), None);
}

// Plausible defects: a cube's faces spread over two quadrants, or placed
// past the end of a quadrant's slots. The oracle is the layout: six faces of
// one light lie in one quadrant, wrapping its rows.
#[wasm_bindgen_test(unsupported = test)]
fn a_cube_takes_six_consecutive_slots_of_one_quadrant() {
    let mut atlas = Atlas::new(SIZE);
    let cube = atlas.update(light(0), 10., 6, 1).unwrap();
    assert_eq!(cube.size, 512);
    let quadrants: Vec<_> = (0..6)
        .map(|face| cube.origin(face).map(|texel| texel / (SIZE / 2)))
        .collect();
    assert!(quadrants.iter().all(|&quadrant| quadrant == quadrants[0]));
    let rows: Vec<_> = (0..6).map(|face| cube.origin(face)[1]).collect();
    assert!(rows.iter().any(|&row| row != rows[0]), "{rows:?}");
}

// Plausible defect: the atlas remembering when it last saw every light a
// scene ever had, so a game that adds and removes lights grows it without
// bound; lights that found no room hold no slots to release. The oracle is
// the count of lights the scene still has.
#[wasm_bindgen_test(unsupported = test)]
fn removed_lights_are_forgotten() {
    let mut atlas = Atlas::new(SIZE);
    // Cubes fill the 64-texel quadrant, all seen every frame.
    let resident: Vec<_> = (0..170).map(light).collect();
    let mut churned = Vec::new();
    for frame in 1..200 {
        for &id in &resident {
            atlas.update(id, 0., 6, frame);
        }
        // Each frame a new light, which finds no room, replaces the last.
        let id = LightId::issue(1000, 1000 + frame);
        assert_eq!(atlas.update(id, 0., 6, frame), None);
        churned.push(id);
        atlas.retain(|light| resident.contains(&light) || light == id);
    }
    assert!(
        atlas.seen.len() <= resident.len() + 1,
        "{}",
        atlas.seen.len()
    );
    assert!(
        churned[..churned.len() - 1]
            .iter()
            .all(|id| !atlas.seen.contains_key(id))
    );
}
