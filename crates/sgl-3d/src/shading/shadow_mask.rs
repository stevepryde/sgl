//! Rust mirror of shadow_mask_slots.wgsl: the ray-traced shadow stage's
//! slot table and the layout of its mask (the architecture's Ray-traced
//! shadows), which the stage writes and the opaque stage's lighting pass
//! reads at its group 3 (`shading::bind::shadow_mask`).

/// The slots the mask holds, Wicked Engine's `MAX_RTSHADOWS`
/// (`RT_SHADOW_LIGHTS` in WGSL): slot 0 the shadowed directional light,
/// slots 1 to 15 casting scene lights.
pub(crate) const RT_SHADOW_LIGHTS: usize = 16;
/// A slot no light holds.
pub(crate) const SHADOW_MASK_EMPTY: u32 = u32::MAX;
/// Slot 0's key while the frame's shadowed directional light holds it.
pub(crate) const SHADOW_MASK_DIRECTIONAL: u32 = u32::MAX - 1;
/// The mask's format: four slots a texel, one a channel.
pub(crate) const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
/// The mask's layers: four slots a layer.
pub(crate) const LAYERS: u32 = (RT_SHADOW_LIGHTS / 4) as u32;

/// The slot table (`ShadowMaskSlots`): each slot's key, four to a vector
/// (`SHADOW_MASK_EMPTY`, `SHADOW_MASK_DIRECTIONAL` or a scene light's
/// index), a bit for each slot whose history restarts this frame, and a bit
/// for each slot whose light is baked (`Light::baked`).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct ShadowMaskSlots {
    pub lights: [[u32; 4]; RT_SHADOW_LIGHTS / 4],
    pub restart: u32,
    pub baked: u32,
    /// WGSL rounds the struct up to its 16-byte alignment.
    pub padding: [u32; 2],
}

impl ShadowMaskSlots {
    /// The table holding `keys`, slot by slot, restarting the slots whose
    /// bit `restart` sets, the lights of the slots whose bit `baked` sets
    /// baked.
    pub fn new(keys: [u32; RT_SHADOW_LIGHTS], restart: u32, baked: u32) -> Self {
        let mut lights = [[SHADOW_MASK_EMPTY; 4]; RT_SHADOW_LIGHTS / 4];
        for (slot, key) in keys.into_iter().enumerate() {
            lights[slot / 4][slot % 4] = key;
        }
        Self {
            lights,
            restart,
            baked,
            padding: [0; 2],
        }
    }
}

#[cfg(test)]
pub(crate) fn mirrors() -> [crate::shading::layout_tests::Mirror; 1] {
    [crate::shading::layout_tests::mirror!(
        "geometry_shadow_mask",
        "ShadowMaskSlots",
        ShadowMaskSlots,
        [lights, restart, baked]
    )]
}

#[cfg(test)]
pub(crate) fn constants() -> [crate::shading::layout_tests::Constant; 4] {
    use crate::shading::layout_tests::Constant;
    [
        Constant::new(
            "geometry_shadow_mask",
            "RT_SHADOW_LIGHTS",
            naga::Literal::U32(RT_SHADOW_LIGHTS as u32),
        ),
        Constant::new(
            "geometry_shadow_mask",
            "SHADOW_MASK_LAYERS",
            naga::Literal::U32(LAYERS),
        ),
        Constant::new(
            "geometry_shadow_mask",
            "SHADOW_MASK_EMPTY",
            naga::Literal::U32(SHADOW_MASK_EMPTY),
        ),
        Constant::new(
            "geometry_shadow_mask",
            "SHADOW_MASK_DIRECTIONAL",
            naga::Literal::U32(SHADOW_MASK_DIRECTIONAL),
        ),
    ]
}
