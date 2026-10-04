use super::*;
use crate::test_support::{half, hdr_texture, read};

/// Linear sRGB inputs and Filament ef1a133's AgX of them, as its
/// FColorGrading applies a custom `ToneMapper` with no adjustments on its
/// non-FILMIC path (`details/ColorGrading.cpp` 743-755 and 1068-1088: sRGB
/// to Rec. 2020, `AgxToneMapper`, Rec. 2020 to sRGB, saturate). Printed by a
/// driver compiled against Filament's own `filament/src/ToneMapper.cpp` and
/// `ColorSpaceUtils.h`, not by SGL3D's port. Inputs are exact in binary16.
const FILAMENT_AGX: [([f32; 3], [f32; 3]); 14] = [
    ([0., 0., 0.], [0.000003, 0.000003, 0.000003]),
    ([0.1875, 0.1875, 0.1875], [0.221852, 0.221866, 0.221870]),
    ([1., 1., 1.], [0.582851, 0.582942, 0.582951]),
    (
        [0.0078125, 0.0078125, 0.0078125],
        [0.003786, 0.003785, 0.003785],
    ),
    ([4., 4., 4.], [0.848942, 0.849088, 0.849087]),
    ([16., 16., 16.], [0.960219, 0.960433, 0.960422]),
    ([1., 0., 0.], [0.716378, 0.107100, 0.067093]),
    ([0., 1., 0.], [0.236793, 0.577799, 0.131953]),
    ([0., 0., 1.], [0.034411, 0.135925, 0.721746]),
    ([0.5, 0.25, 0.0625], [0.436684, 0.269598, 0.119957]),
    ([0.03125, 0.3125, 2.5], [0.218974, 0.443889, 0.877349]),
    ([8., 3., 0.5], [0.964025, 0.812540, 0.626392]),
    ([0.0625, 0.0625, 0.375], [0.083936, 0.121252, 0.397486]),
    ([96., 64., 16.], [0.960785, 0.961000, 0.961025]),
];

fn exposure_texture(device: &wgpu::Device, queue: &wgpu::Queue, value: f32) -> wgpu::TextureView {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("fixture exposure"),
        size: wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R32Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        texture.as_image_copy(),
        bytemuck::bytes_of(&value),
        wgpu::TexelCopyBufferLayout::default(),
        texture.size(),
    );
    texture.create_view(&Default::default())
}

// Defects: a transposed or wrong AgX or colour-space matrix, a wrong log2
// range or sigmoid coefficient, a missing primaries conversion, an exposure
// applied after the curve instead of before it, or a neutral grading that is
// not neutral. The oracle is Filament's own C++ evaluated on the same inputs;
// the second pass scales the inputs by 4 and exposes them by 1/4, which must
// tone map identically.
#[test]
fn neutral_grading_tone_maps_as_filaments_agx_after_the_exposure() {
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    let size = [FILAMENT_AGX.len() as u32, 1];
    let inputs = Inputs::new(&device);
    let mut tone_map = ToneMap::new(&device, &inputs, HDR, size);
    let output = target(&device, "tone map fixture output", size, HDR);
    let unread_bloom = target(&device, "tone map fixture bloom", [1, 1], HDR);
    for (scale, multiplier) in [(1., 1.), (4., 0.25)] {
        let texels: Vec<_> = FILAMENT_AGX
            .iter()
            .map(|([r, g, b], _)| [r * scale, g * scale, b * scale, 1.])
            .collect();
        let hdr = hdr_texture(&device, &queue, size, &texels);
        let exposure = exposure_texture(&device, &queue, multiplier);
        let mut encoder = device.create_command_encoder(&Default::default());
        tone_map.encode(
            &device,
            &queue,
            &mut encoder,
            &inputs,
            &hdr,
            &unread_bloom,
            &exposure,
            &ColorGrading::default(),
            true,
            &output,
            None,
        );
        queue.submit([encoder.finish()]);
        let actual = read(&device, &queue, tone_map.tone_mapped().texture(), 8);
        for (index, (input, expected)) in FILAMENT_AGX.iter().enumerate() {
            for channel in 0..3 {
                let value = half(&actual[index * 8 + channel * 2..]);
                // Binary16 output rounding plus single-precision evaluation.
                assert!(
                    (value - expected[channel]).abs() <= 1.5e-3,
                    "{input:?} x{scale} at exposure {multiplier}: channel {channel} is {value}, Filament {}",
                    expected[channel]
                );
            }
        }
    }
}
