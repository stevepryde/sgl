// A baked specular probe's mip chain (content/baked_specular_probe.rs
// `LEVELS`): mip i holds perceptual roughness i / (SPECULAR_PROBE_LEVELS - 1),
// as Filament's cmgen and Bevy store specular probes (lod = perceptual
// roughness * max lod).
const SPECULAR_PROBE_LEVELS:u32=7u;
// The mip holding perceptual roughness `roughness` (0 to 1).
fn specular_probe_lod(roughness:f32)->f32 {
 return roughness*f32(SPECULAR_PROBE_LEVELS-1u);
}
// The perceptual roughness mip `level` holds.
fn specular_probe_roughness(level:u32)->f32 {
 return f32(level)/f32(SPECULAR_PROBE_LEVELS-1u);
}
