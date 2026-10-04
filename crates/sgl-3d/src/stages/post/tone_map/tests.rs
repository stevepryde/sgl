use super::*;
use crate::test_support::{half, hdr_texture, read};

/// The looks `FILAMENT_AGX` gives outputs for, in its order.
const LOOKS: [AgxLook; 3] = [AgxLook::None, AgxLook::Punchy, AgxLook::Golden];

/// Linear sRGB inputs and Filament ef1a133's AgX of them with each of
/// `LOOKS` (`AgxLook::NONE`, `PUNCHY`, `GOLDEN`), as its FColorGrading
/// applies a custom `ToneMapper` with no adjustments on its non-FILMIC path
/// (`details/ColorGrading.cpp` 743-755 and 1068-1088: sRGB to Rec. 2020,
/// `AgxToneMapper`, Rec. 2020 to sRGB, saturate). Printed by a driver
/// compiled against Filament's own `filament/src/ToneMapper.cpp` and
/// `ColorSpaceUtils.h`, not by SGL3D's port. Inputs are exact in binary16.
const FILAMENT_AGX: [([f32; 3], [[f32; 3]; 3]); 14] = [
    (
        [0., 0., 0.],
        [
            [0.000003, 0.000003, 0.000003],
            [0., 0., 0.],
            [0.000062, 0.000039, 0.000004],
        ],
    ),
    (
        [0.1875, 0.1875, 0.1875],
        [
            [0.221852, 0.221866, 0.221870],
            [0.101806, 0.101794, 0.101794],
            [0.413908, 0.244072, 0.003208],
        ],
    ),
    (
        [1., 1., 1.],
        [
            [0.582851, 0.582942, 0.582951],
            [0.445297, 0.445338, 0.445340],
            [0.854991, 0.498339, 0.],
        ],
    ),
    (
        [0.0078125, 0.0078125, 0.0078125],
        [
            [0.003786, 0.003785, 0.003785],
            [0.000083, 0.000083, 0.000083],
            [0.018560, 0.011343, 0.000644],
        ],
    ),
    (
        [4., 4., 4.],
        [
            [0.848942, 0.849088, 0.849087],
            [0.783230, 0.783329, 0.783308],
            [1., 0.656721, 0.],
        ],
    ),
    (
        [16., 16., 16.],
        [
            [0.960219, 0.960433, 0.960422],
            [0.941367, 0.941577, 0.941533],
            [1., 0.718696, 0.],
        ],
    ),
    (
        [1., 0., 0.],
        [
            [0.716378, 0.107100, 0.067093],
            [0.729062, 0., 0.],
            [1., 0.098244, 0.],
        ],
    ),
    (
        [0., 1., 0.],
        [
            [0.236793, 0.577799, 0.131953],
            [0.023117, 0.476337, 0.],
            [0.419260, 0.539332, 0.],
        ],
    ),
    (
        [0., 0., 1.],
        [
            [0.034411, 0.135925, 0.721746],
            [0., 0.041249, 0.879733],
            [0.165240, 0.175172, 0.184265],
        ],
    ),
    (
        [0.5, 0.25, 0.0625],
        [
            [0.436684, 0.269598, 0.119957],
            [0.324699, 0.129908, 0.008126],
            [0.694917, 0.269229, 0.],
        ],
    ),
    (
        [0.03125, 0.3125, 2.5],
        [
            [0.218974, 0.443889, 0.877349],
            [0.018123, 0.298271, 1.],
            [0.440500, 0.424452, 0.104610],
        ],
    ),
    (
        [8., 3., 0.5],
        [
            [0.964025, 0.812540, 0.626392],
            [0.994882, 0.726177, 0.427413],
            [1., 0.626293, 0.],
        ],
    ),
    (
        [0.0625, 0.0625, 0.375],
        [
            [0.083936, 0.121252, 0.397486],
            [0.003853, 0.036489, 0.349701],
            [0.221408, 0.156491, 0.086830],
        ],
    ),
    (
        [96., 64., 16.],
        [
            [0.960785, 0.961000, 0.961025],
            [0.942193, 0.942405, 0.942430],
            [1., 0.719006, 0.],
        ],
    ),
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
// applied after the curve instead of before it, a neutral grading that is
// not neutral, a look's wrong slope, power or saturation, a look applied
// outside the curve's output or a look sent as another. The oracle is
// Filament's own C++ evaluated on the same inputs with each look; the second
// pass scales the inputs by 4 and exposes them by 1/4, which must tone map
// identically.
#[test]
fn tone_maps_as_filaments_agx_with_each_look_after_the_exposure() {
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    let size = [FILAMENT_AGX.len() as u32, 1];
    let inputs = Inputs::new(&device);
    let mut tone_map = ToneMap::new(&device, &inputs, HDR, size);
    let output = target(&device, "tone map fixture output", size, HDR);
    let unread_bloom = target(&device, "tone map fixture bloom", [1, 1], HDR);
    for (look_index, look) in LOOKS.into_iter().enumerate() {
        let grading = ColorGrading {
            agx_look: look,
            ..Default::default()
        };
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
                &grading,
                true,
                &output,
                None,
            );
            queue.submit([encoder.finish()]);
            let actual = read(&device, &queue, tone_map.tone_mapped().texture(), 8);
            for (index, (input, expected)) in FILAMENT_AGX.iter().enumerate() {
                let expected = expected[look_index];
                for channel in 0..3 {
                    let value = half(&actual[index * 8 + channel * 2..]);
                    // Binary16 output rounding plus single-precision evaluation.
                    assert!(
                        (value - expected[channel]).abs() <= 1.5e-3,
                        "{look:?} {input:?} x{scale} at exposure {multiplier}: channel {channel} is {value}, Filament {}",
                        expected[channel]
                    );
                }
            }
        }
    }
}

/// The 8-bit sRGB code value, unrounded, of linear `value`, by the sRGB
/// transfer function.
fn srgb_code(value: f64) -> f64 {
    let encoded = if value <= 0.0031308 {
        12.92 * value
    } else {
        1.055 * value.powf(1. / 2.4) - 0.055
    };
    255. * encoded.clamp(0., 1.)
}

// Defects: no dither, so a gradient bands, every pixel of a column rounding
// alike; a dither too strong, that strays more than a code value; and a
// dither only the direct or only the captured path applies. The oracle is the undithered tone-mapped scene the capture writes
// and the sRGB transfer function, at the GPU's 8-bit sRGB encoding: on both
// paths each output code stays within a code value and a half of its exact
// code, and down each column of a horizontal gradient the outputs average to
// that exact code, not to its rounding as undithered output does.
#[test]
fn dithering_breaks_8_bit_banding_into_bounded_noise() {
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let size = [256, 64];
    let inputs = Inputs::new(&device);
    let mut tone_map = ToneMap::new(&device, &inputs, format, size);
    let unread_bloom = target(&device, "dither fixture bloom", [1, 1], HDR);
    let exposure = exposure_texture(&device, &queue, 1.);
    // Mid greys spanning about eight codes, so most columns fall between two.
    let texels: Vec<_> = (0..size[1])
        .flat_map(|_| {
            (0..size[0]).map(|x| {
                let grey = 0.18 + 0.04 * x as f32 / size[0] as f32;
                [grey, grey, grey, 1.]
            })
        })
        .collect();
    let hdr = hdr_texture(&device, &queue, size, &texels);
    let mut present = |capture: bool| {
        let output = target(&device, "dither fixture output", size, format);
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
            capture,
            &output,
            None,
        );
        queue.submit([encoder.finish()]);
        read(&device, &queue, output.texture(), 4)
    };
    let outputs = [("captured", present(true)), ("direct", present(false))];
    // The direct path leaves the capture in place.
    let undithered = read(&device, &queue, tone_map.tone_mapped().texture(), 8);
    let exact: Vec<f64> = undithered
        .chunks_exact(8)
        .flat_map(|texel| (0..3).map(|channel| srgb_code(f64::from(half(&texel[channel * 2..])))))
        .collect();
    let width = size[0] as usize;
    let rows = f64::from(size[1]);
    // The mean over the columns and channels of each column's mean error.
    let banding = |error: &dyn Fn(usize) -> f64| {
        let mut columns = vec![0.; width * 3];
        for index in 0..exact.len() {
            let (pixel, channel) = (index / 3, index % 3);
            columns[pixel % width * 3 + channel] += error(index) / rows;
        }
        columns.iter().map(|mean| mean.abs()).sum::<f64>() / columns.len() as f64
    };
    let rounded = banding(&|index| exact[index].round() - exact[index]);
    for (path, output) in &outputs {
        let code = |index: usize| f64::from(output[index / 3 * 4 + index % 3]);
        for (index, exact) in exact.iter().enumerate() {
            assert!(
                (code(index) - exact).abs() < 1.5,
                "{path}: pixel {} channel {}: code {} for exact {exact}",
                index / 3,
                index % 3,
                code(index)
            );
        }
        let dithered = banding(&|index| code(index) - exact[index]);
        eprintln!("{path}: mean column error dithered {dithered}, rounded {rounded} code values");
        assert!(
            dithered < 0.25 * rounded,
            "{path}: columns average {dithered} code values from exact, rounding {rounded}"
        );
    }
}
