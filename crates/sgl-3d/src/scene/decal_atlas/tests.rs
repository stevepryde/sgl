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

// Plausible defects: an atlas whose build is more than the target can
// allocate passes `place`, so `fill` panics allocating its first level: no
// bound on the build, a byte count that wraps (a 32768-square level's bytes
// wrap `u32` to 0), or a bound that leaves out bytes of a texel. The oracle
// is the atlas each image needs (a side and its border in whole cells,
// doubled to a power of two, as Godot sizes it) against what the target can
// allocate: a wasm32 allocation is under 2 GiB, so an atlas whose first
// level is 2 GiB (16384 square at 8 bytes a texel) cannot be built there;
// natively one is, and a 4096-square image's 8192-square atlas is
// everywhere. `limit` admits every side here, so only the build refuses.
#[wasm_bindgen_test(unsupported = test)]
fn an_atlas_too_large_to_build_is_refused_before_it_is_built() {
    let key = (DecalImageId::issue(0, 1), Sampling::Color);
    let layout = |side: u32| AtlasLayout::place(&[(key, [side, side])], 32768);
    assert!(matches!(layout(16384), Err(SceneError::DeviceLimit)));
    let large = layout(8192);
    if cfg!(target_arch = "wasm32") {
        assert!(matches!(large, Err(SceneError::DeviceLimit)));
    } else {
        assert_eq!(large.unwrap().size, [16384, 16384]);
    }
    assert_eq!(layout(4096).unwrap().size, [8192, 8192]);
}

// Plausible defects: a wrong exponent bias or subnormal scale in the decode
// each mip is filtered through, darkening or brightening every mip. The
// oracle is IEEE 754 binary16's encodings.
#[wasm_bindgen_test(unsupported = test)]
fn half_value_decodes_binary16() {
    for (bits, value) in [
        (0x0000, 0.),
        (0x0001, 1. / 16_777_216.),
        (0x03ff, 1023. / 16_777_216.),
        (0x0400, 1. / 16384.),
        (0x3555, 1365. / 4096.),
        (0x3800, 0.5),
        (0x3bff, 2047. / 2048.),
        (0x3c00, 1.),
    ] {
        assert_eq!(half_value(bits), value, "{bits:#06x}");
    }
}
