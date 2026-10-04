//! The atlas's placement judged geometrically.
use super::*;
use wasm_bindgen_test::wasm_bindgen_test;

// Plausible defects: a skyline that takes an item's height at one column
// short of its width, or raises the wrong columns after placing it, so two
// images (with their borders) overlap; an image's cells too few for its
// texels and border (Godot's sizing, for a side more than half a cell past
// whole cells); an atlas cut short of an item; a size that is not a power
// of two, so a mip does not halve it. The oracle is rectangle overlap of
// every pair, in grid cells, and each image's extent in texels.
#[wasm_bindgen_test(unsupported = test)]
fn placed_images_and_their_borders_never_overlap() {
    let mut state = 0x9e37_79b9u32;
    let mut next = |below: u32| {
        state = state.wrapping_mul(1664525).wrapping_add(1013904223);
        (state >> 8) % below
    };
    let mut items: Vec<Item> = (0..60)
        .map(|index| {
            Item::new(
                (DecalImageId::issue(index, 1), Sampling::Color),
                [1 + next(700), 1 + next(300)],
            )
        })
        .collect();
    let size = place(&mut items);
    assert!(size[0].is_power_of_two() && size[1].is_power_of_two());
    assert!(size[0] >= 8 * BORDER && size[1] >= 2 * BORDER);
    let cells = size.map(|side| side / BORDER);
    for (index, item) in items.iter().enumerate() {
        let [x, y] = item.position;
        assert!(
            x + item.size[0] <= cells[0] && y + item.size[1] <= cells[1],
            "image {index} leaves the atlas"
        );
        // Its texels and half a cell of border on each side, a texel at the
        // last mip, fit its cells.
        assert!(item.pixel_size[0] + BORDER <= item.size[0] * BORDER);
        assert!(item.pixel_size[1] + BORDER <= item.size[1] * BORDER);
        for other in &items[index + 1..] {
            let [ox, oy] = other.position;
            let apart = x + item.size[0] <= ox
                || ox + other.size[0] <= x
                || y + item.size[1] <= oy
                || oy + other.size[1] <= y;
            assert!(apart, "two images overlap");
        }
    }
}
