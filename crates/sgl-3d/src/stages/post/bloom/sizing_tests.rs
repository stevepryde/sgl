//! The mip chain's size for a scene, judged by Bevy's sizing and the
//! device's largest texture.
use super::*;
use wasm_bindgen_test::wasm_bindgen_test;

// Plausible defects: the fit to the device changes the chain of a scene
// Bevy's sizing already fits, which changes the bloom's look, or the chain of
// a scene far wider than high stays wider than the device allows, or loses
// the scene's aspect. Oracles: Bevy's `prepare_bloom_textures` puts a
// 1920×1080 scene's mip 0 at 1920 × 512 / 1080 rounded, 910 × 512; a 64×1
// scene scaled to the largest width, 16384 or WebGPU's default 8192, is
// 256 or 128 high at its aspect.
#[wasm_bindgen_test(unsupported = test)]
fn the_chain_is_bevys_unless_the_device_needs_it_narrower() {
    assert_eq!(chain_size([1920, 1080], 16384), [910, 512]);
    assert_eq!(chain_size([1, 64], 16384), [8, 512]);
    assert_eq!(chain_size([64, 1], 16384), [16384, 256]);
    assert_eq!(chain_size([64, 1], 8192), [8192, 128]);
}
