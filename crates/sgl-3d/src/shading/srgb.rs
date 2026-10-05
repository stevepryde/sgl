//! sRGB's transfer function (IEC 61966-2-1), each way, for the CPU:
//! filtering colour mips in linear light, the decal atlas and packed vertex
//! colours. srgb.wgsl decodes for shaders.

/// The linear value of sRGB-encoded `value` in 0..=1.
pub(crate) fn to_linear(value: f32) -> f32 {
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

/// The sRGB encoding of linear `value` in 0..=1.
pub(crate) fn from_linear(value: f32) -> f32 {
    if value <= 0.0031308 {
        value * 12.92
    } else {
        1.055 * value.powf(1. / 2.4) - 0.055
    }
}
