use super::super::inputs::Inputs;
use super::*;
use crate::test_support::{half, hdr_texture, read, to_half};

/// Rec. 709 luminance summed over an RGBA16F readback.
fn total_luminance(texels: &[u8]) -> f64 {
    texels
        .chunks_exact(8)
        .map(|texel| {
            0.2126 * f64::from(half(texel))
                + 0.7152 * f64::from(half(&texel[2..]))
                + 0.0722 * f64::from(half(&texel[4..]))
        })
        .sum()
}

// Defects: an additive composite or upsample where the blend must replace a
// share of the finer level, filter weights that do not sum to one, a mip
// chain that loses or gains light at its edges, or a composite that changes
// the scene at intensity 0. Energy-conserving bloom only moves light: the
// image's total luminance is the input's, judged against the input itself.
// The input is dim, where the Karis average leaves energy within 0.1%. Both
// chain formats run: the device's own and RGBA16F without it.
#[test]
fn energy_conserving_bloom_keeps_the_scenes_total_luminance() {
    for without in [
        wgpu::Features::empty(),
        wgpu::Features::RG11B10UFLOAT_RENDERABLE,
    ] {
        let Some((device, queue)) = crate::test_support::device_without(without) else {
            return;
        };
        conserves(&device, &queue);
    }
}

fn conserves(device: &wgpu::Device, queue: &wgpu::Queue) {
    // Mip 0 at 512 texels high is then the scene's size.
    let size = [1024, 512];
    let mut state = 0x2545_f491_u32;
    let mut random = || {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        f32::from(state as u16) / f32::from(u16::MAX) * 0.05
    };
    let texels: Vec<_> = (0..size[0] * size[1])
        .map(|_| [random(), random(), random(), 1.])
        .collect();
    let scene = hdr_texture(device, queue, size, &texels);
    let inputs = Inputs::new(device);
    let mut bloom = Bloom::new(device, size, true);
    // The scene as the texture holds it.
    let input: Vec<u8> = texels
        .iter()
        .flatten()
        .flat_map(|&value| to_half(value).to_le_bytes())
        .collect();
    let expected = total_luminance(&input);
    let natural = BloomParameters::default();
    let scattered = BloomParameters { intensity: 1. };
    let none = BloomParameters { intensity: 0. };
    for parameters in [natural, scattered, none] {
        queue.write_buffer(
            &inputs.settings,
            0,
            bytemuck::bytes_of(&settings(&parameters, 0.)),
        );
        let mut encoder = device.create_command_encoder(&Default::default());
        let combined = bloom.encode(
            (device, &crate::view::compile::Compiler::new(device)),
            &mut encoder,
            &inputs,
            &scene,
            &parameters,
            None,
        );
        queue.submit([encoder.finish()]);
        let actual = read(device, queue, combined.texture(), 8);
        if parameters.intensity == 0. {
            assert_eq!(actual, input, "intensity 0 must leave the scene unchanged");
            continue;
        }
        let ratio = total_luminance(&actual) / expected;
        eprintln!(
            "bloom {:?} {parameters:?}: total luminance ratio {ratio}",
            bloom.format
        );
        // RGBA16F's 10-bit mantissas lose under 1% over the chain's 15
        // writes; Rg11b10Ufloat's 6- and 5-bit ones, as Bevy chose for
        // bandwidth, lose up to 5% when all light scatters (measured 4.5%
        // on the M5).
        let bound = if bloom.format == HDR { 0.01 } else { 0.05 };
        assert!(
            (ratio - 1.).abs() < bound,
            "{parameters:?} changed the total luminance by a factor {ratio}"
        );
    }
}

/// The count of non-finite channels in a readback of the chain's `format`:
/// a binary16 or packed float channel whose exponent bits are all set.
fn non_finite_channels(bytes: &[u8], format: wgpu::TextureFormat) -> usize {
    if format == HDR {
        return crate::stages::post::tests::non_finite_halves(bytes);
    }
    // Rg11b10Ufloat: red and green 6-bit mantissas then 5-bit exponents,
    // blue a 5-bit mantissa then a 5-bit exponent.
    bytes
        .chunks_exact(4)
        .map(|texel| {
            let bits = u32::from_le_bytes(texel.try_into().unwrap());
            [6, 17, 27]
                .into_iter()
                .filter(|shift| (bits >> shift) & 31 == 31)
                .count()
        })
        .sum()
}

/// Rec. 709 luminance of texel `index` of a chain readback of `format`.
fn chain_luminance(bytes: &[u8], format: wgpu::TextureFormat, index: usize) -> f64 {
    let [r, g, b] = if format == HDR {
        [0, 2, 4].map(|offset| f64::from(half(&bytes[index * 8 + offset..])))
    } else {
        let bits = u32::from_le_bytes(bytes[index * 4..index * 4 + 4].try_into().unwrap());
        // Each channel's mantissa width and lowest bit.
        [(6, 0), (6, 11), (5, 22)].map(|(mantissa_bits, shift)| {
            let mantissa = f64::from((bits >> shift) & ((1 << mantissa_bits) - 1));
            let exponent = (bits >> (shift + mantissa_bits)) & 31;
            let scale = f64::from(1 << mantissa_bits);
            if exponent == 0 {
                mantissa / scale * 2f64.powi(-14)
            } else {
                (1. + mantissa / scale) * 2f64.powi(exponent as i32 - 15)
            }
        })
    };
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

// Plausible defect: one completed-scene texel of +infinity, which RGBA16F
// holds where radiance passed its range on a backend that rounds overflow
// to infinity (#464) and which reaches bloom unclamped when neither TAA nor
// FSR ran, becoming NaN in the first downsample's Karis average (infinity
// times 1 / (1 + infinity)) or spreading as infinity or NaN through the
// filtered taps, every mip, the upsamples and the composite; or the hot
// texel scattering nothing where NaN is clamped away. Oracles: RD-4's
// finite-output invariant at every mip and in the combined scene, and the
// Karis average growing with its input, so mip 0 is brighter at the
// infinite block than at an ordinary texel. Both chain formats run.
#[test]
fn infinite_texels_leave_every_mip_finite() {
    for without in [
        wgpu::Features::empty(),
        wgpu::Features::RG11B10UFLOAT_RENDERABLE,
    ] {
        let Some((device, queue)) = crate::test_support::device_without(without) else {
            return;
        };
        let size = crate::stages::post::tests::INFINITE_SCENE;
        let scene = hdr_texture(
            &device,
            &queue,
            size,
            &crate::stages::post::tests::infinite_scene(),
        );
        let inputs = Inputs::new(&device);
        let mut bloom = Bloom::new(&device, size, true);
        let parameters = BloomParameters::default();
        queue.write_buffer(
            &inputs.settings,
            0,
            bytemuck::bytes_of(&settings(&parameters, 0.)),
        );
        let mut encoder = device.create_command_encoder(&Default::default());
        let combined = bloom.encode(
            (&device, &crate::view::compile::Compiler::new(&device)),
            &mut encoder,
            &inputs,
            &scene,
            &parameters,
            None,
        );
        queue.submit([encoder.finish()]);
        let mut failures = Vec::new();
        for (mip, view) in bloom.targets.mips.iter().enumerate() {
            let size = view
                .texture()
                .size()
                .mip_level_size(mip as u32, wgpu::TextureDimension::D2);
            let bytes = crate::test_support::read_mip(&device, &queue, view.texture(), mip as u32);
            let count = non_finite_channels(&bytes, bloom.format);
            if count > 0 {
                failures.push(format!("mip {mip}: {count} non-finite channels"));
            }
            if mip == 0 {
                // Where mip 0 holds the all-infinite block's centre, and an
                // ordinary corner of the scene: the Karis average grows with
                // its input, so the hottest light is the brightest there.
                let luminance = |[x, y]: [f32; 2]| {
                    let texel = (y * size.height as f32) as u32 * size.width
                        + (x * size.width as f32) as u32;
                    chain_luminance(&bytes, bloom.format, texel as usize)
                };
                let scene = crate::stages::post::tests::INFINITE_SCENE.map(|v| v as f32);
                let hot = luminance([9. / scene[0], 9. / scene[1]]);
                let ordinary = luminance([60. / scene[0], 44. / scene[1]]);
                if hot.is_nan() || hot <= ordinary {
                    failures.push(format!(
                        "mip 0: the infinite block's luminance {hot} is not above an ordinary texel's {ordinary}"
                    ));
                }
            }
        }
        let count = non_finite_channels(&read(&device, &queue, combined.texture(), 8), HDR);
        if count > 0 {
            failures.push(format!("combined: {count} non-finite channels"));
        }
        assert!(failures.is_empty(), "{:?}: {failures:?}", bloom.format);
    }
}
