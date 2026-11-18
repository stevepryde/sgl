// The temporal masks' reactive value (view::targets::mask_targets), which
// every draw that writes the reactive mask outputs through
// `temporal_reactive`. AMD's FSR documentation recommends clamping the
// reactivity FSR reads to about 0.9, as a value near 1 does not give good
// results (FidelityFX SDK 1.1.4, MIT, see LICENSE-amd-fidelityfx.txt,
// docs/techniques/super-resolution-upscaler.md 180, "Reactive mask").
const TEMPORAL_REACTIVE_MAX:f32=.9;
// A draw's reactive output for its `share` of the pixel: the share capped
// at TEMPORAL_REACTIVE_MAX in red and as a fraction of that cap in alpha,
// which the reactive target's blend takes: r + s (1 - r / cap) over the
// mask's `s`. That is AMD's FSR sample's union of draws, m (1 - s) + s
// (samples/fsrapi/fsrapirendermodule.cpp, its reactive target's blend),
// taken in units of the cap, so overlapping draws approach the cap and a
// pixel's reactivity never exceeds it.
fn temporal_reactive(share:f32)->vec4<f32> {
 let r=clamp(share,0.,TEMPORAL_REACTIVE_MAX);
 return vec4(r,0.,0.,r/TEMPORAL_REACTIVE_MAX);
}
