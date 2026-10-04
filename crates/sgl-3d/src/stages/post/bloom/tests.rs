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
    let bloom = Bloom::new(device, &inputs, size, true);
    // The scene as the texture holds it.
    let input: Vec<u8> = texels
        .iter()
        .flatten()
        .flat_map(|&value| to_half(value).to_le_bytes())
        .collect();
    let expected = total_luminance(&input);
    let natural = BloomParameters::default();
    let scattered = BloomParameters {
        intensity: 1.,
        low_frequency_boost: 0.,
        ..natural
    };
    let none = BloomParameters {
        intensity: 0.,
        ..natural
    };
    for parameters in [natural, scattered, none] {
        queue.write_buffer(
            &inputs.settings,
            0,
            bytemuck::bytes_of(&settings(&parameters, 0.)),
        );
        let mut encoder = device.create_command_encoder(&Default::default());
        let combined = bloom.encode(device, &mut encoder, &inputs, &scene, &parameters, None);
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
