//! Lit group 0's lookup tables, one texture (shading/lookup_tables.wgsl): a
//! 64×64 RGBA16F array whose first two layers hold the linearly transformed
//! cosines that fit GGX, which rectangle lights are shaded with, and whose
//! third holds the DFG table in red and green over its first 16×16 texels.
//! One texture holds both so lit group 0 and a material stay within the
//! sampled textures a fragment stage may bind (S3D-1).

/// Texels on each side of a layer, and of the DFG table.
const LAYER: u32 = 64;
const DFG: u32 = 16;
/// The fit's layers, then the DFG table's.
const LTC_LAYERS: u32 = 2;
const DFG_LAYER: u32 = LTC_LAYERS;

/// The tables, uploaded.
///
/// The fit is as Bevy 9d12036 ships it (`crates/bevy_pbr/src/ltc/ltc.ktx2`,
/// its zstd-compressed level decompressed, unchanged): two 64×64 RGBA16F
/// layers, the inverse matrix's four free elements, then the BRDF's
/// magnitude and Fresnel weights, over perceptual roughness and
/// sqrt(1 − N·V). The fit is Heitz, Dupuy, Hill and Neubelt's
/// (selfshadow/ltc_code `fit/results`, `src/LICENSE-ltc-code.txt`).
///
/// The DFG table is Three.js r185's 16×16 RG16F reference material response
/// (`three_dfg.rg16`), over perceptual roughness and N·V.
pub(crate) fn lookup_tables(device: &wgpu::Device, queue: &wgpu::Queue) -> wgpu::TextureView {
    let size = wgpu::Extent3d {
        width: LAYER,
        height: LAYER,
        depth_or_array_layers: DFG_LAYER + 1,
    };
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("lit lookup tables"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba16Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let mut texels = include_bytes!("ltc_ggx.rgba16").to_vec();
    let mut dfg = vec![0u8; (LAYER * LAYER * 8) as usize];
    // Each RG16F texel's two halves, then zero blue and alpha.
    for (index, texel) in include_bytes!("three_dfg.rg16").chunks_exact(4).enumerate() {
        let (x, y) = (index as u32 % DFG, index as u32 / DFG);
        let at = ((y * LAYER + x) * 8) as usize;
        dfg[at..at + 4].copy_from_slice(texel);
    }
    texels.extend(dfg);
    queue.write_texture(
        texture.as_image_copy(),
        &texels,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(LAYER * 8),
            rows_per_image: Some(LAYER),
        },
        size,
    );
    texture.create_view(&wgpu::TextureViewDescriptor {
        dimension: Some(wgpu::TextureViewDimension::D2Array),
        ..Default::default()
    })
}

#[cfg(test)]
pub(crate) fn constants() -> [crate::shading::layout_tests::Constant; 3] {
    use crate::shading::layout_tests::Constant;
    use naga::Literal::{F32, I32};
    [
        Constant::new("geometry", "LOOKUP_LAYER_SIZE", F32(LAYER as f32)),
        Constant::new("geometry", "LOOKUP_DFG_SIZE", F32(DFG as f32)),
        Constant::new("geometry", "LOOKUP_DFG_LAYER", I32(DFG_LAYER as i32)),
    ]
}
