//! The Rust side of dynamic_gi.wgsl: the probe texture's format and size,
//! which the dynamic GI stage allocates and lit group 0 binds, and the
//! constants both languages lay it out by.

/// Each probe's irradiance map: 6×6 texels and a one-texel border
/// (`DDGI_COLOR_RESOLUTION`, `DDGI_COLOR_TEXELS`).
pub(crate) const COLOR_RESOLUTION: u32 = 6;
pub(crate) const COLOR_TEXELS: u32 = COLOR_RESOLUTION + 2;
/// Each probe's depth map: 16×16 texels and a one-texel border
/// (`DDGI_DEPTH_RESOLUTION`, `DDGI_DEPTH_TEXELS`).
pub(crate) const DEPTH_RESOLUTION: u32 = 16;
pub(crate) const DEPTH_TEXELS: u32 = DEPTH_RESOLUTION + 2;
/// The data region's slabs of the lattice to a row (`DDGI_DATA_SLABS`).
pub(crate) const DATA_SLABS: u32 = DEPTH_TEXELS;
/// The probe texture's format: RGBA16F, which filters and stores from
/// compute on every device.
pub(crate) const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
/// The most rays a probe traces a frame at any quality: Wicked Engine's
/// default `DDGI_RAYCOUNT` (df44c3d wiRenderer.cpp 143), High's
/// (`DDGI_MOST_RAYS`).
pub(crate) const MOST_RAYS: u32 = 256;
/// Texels to a row of the ray list and ray results (`DDGI_RAY_ROW`): a
/// probe's rays, `max_rays` of them, follow one another from texel
/// `probe * max_rays`.
pub(crate) const RAY_ROW: u32 = 2048;
/// The bytes each probe's blend history takes in the dynamic GI stage's
/// buffers: its irradiance estimator (six words per irradiance texel), its
/// depth moments (a word per depth texel) and its offset and state (two
/// words).
pub(crate) const HISTORY_BYTES: u64 = 4
    * (6 * (COLOR_RESOLUTION * COLOR_RESOLUTION) + DEPTH_RESOLUTION * DEPTH_RESOLUTION + 2) as u64;

/// The probe texture's width and height for a lattice of `probes`: the
/// depth region's tiles, then the irradiance region's, two slabs to a band,
/// then the data region's rows (dynamic_gi.wgsl).
pub(crate) fn texture_size(probes: [u32; 3]) -> [u64; 2] {
    let [x, y, z] = probes.map(u64::from);
    [
        x * y * u64::from(DEPTH_TEXELS),
        z * u64::from(DEPTH_TEXELS)
            + z.div_ceil(2) * u64::from(COLOR_TEXELS)
            + z.div_ceil(u64::from(DATA_SLABS)),
    ]
}

/// The ray list's and ray results' width and height for `probe_count`
/// probes tracing at most `max_rays` each.
pub(crate) fn ray_texture_size(probe_count: u64, max_rays: u32) -> [u64; 2] {
    let rays = probe_count * u64::from(max_rays);
    [u64::from(RAY_ROW), rays.div_ceil(u64::from(RAY_ROW)).max(1)]
}

/// Whether a lattice of `probes` fits `limits`: its probe texture and its
/// rays at `MOST_RAYS` within the largest 2D texture, and its history
/// within one storage binding.
pub(crate) fn fits(probes: [u32; 3], limits: &wgpu::Limits) -> bool {
    let count = probes.iter().map(|&n| u64::from(n)).product::<u64>();
    let largest = u64::from(limits.max_texture_dimension_2d);
    let binding = limits
        .max_storage_buffer_binding_size
        .min(limits.max_buffer_size);
    let textures = [texture_size(probes), ray_texture_size(count, MOST_RAYS)];
    textures
        .iter()
        .all(|size| size.iter().all(|&side| side <= largest))
        && count * HISTORY_BYTES <= binding
        && count * u64::from(MOST_RAYS) <= u64::from(u32::MAX)
}

#[cfg(test)]
pub(crate) fn constants() -> Vec<super::layout_tests::Constant> {
    use super::layout_tests::Constant;
    [
        ("DDGI_COLOR_RESOLUTION", COLOR_RESOLUTION),
        ("DDGI_COLOR_TEXELS", COLOR_TEXELS),
        ("DDGI_DEPTH_RESOLUTION", DEPTH_RESOLUTION),
        ("DDGI_DEPTH_TEXELS", DEPTH_TEXELS),
        ("DDGI_DATA_SLABS", DATA_SLABS),
        ("DDGI_RAY_ROW", RAY_ROW),
        ("DDGI_MOST_RAYS", MOST_RAYS),
    ]
    .into_iter()
    .map(|(name, value)| Constant::new("geometry", name, naga::Literal::U32(value)))
    .collect()
}
