use super::*;
use crate::test_support::to_half;

// The sharp level must hold, through the GPU's own cube lookup, the uniform
// box average of the captured samples behind each output texel: their
// arithmetic mean, the definition of a texel's area average on the capture's
// equal grid. Defects: a reduction that keeps one sample (a point decimation)
// or weighs them wrongly, a face resolved into another face's layer, or a
// face or in-face orientation that is not the inverse of hardware cube
// sampling, each of which reads another value.
#[test]
fn sharp_level_holds_each_texels_box_average_through_cube_sampling() {
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    let size = 1 << (LEVELS - 1);
    let samples = 4;
    let capture_size = size * samples;
    let capture = ProbePrefilter::new(&device, size, capture_size);
    // Every texel of every face distinct, its face, column and row in R, G
    // and B, and every sample within it apart from the others.
    let value = |face: u32, x: u32, y: u32| {
        let within = ((x % samples) * samples + y % samples) as f32 / (samples * samples) as f32;
        [
            1. + (x / samples) as f32 / size as f32 + within * 0.5,
            1. + (y / samples) as f32 / size as f32,
            1. + face as f32 / 8. + within * 0.25,
        ]
    };
    let half = |bits: u16| crate::test_support::half(&bits.to_le_bytes());
    let mut expected = vec![[0f32; 3]; 6 * (size * size) as usize];
    for face in 0..6 {
        let texels: Vec<u16> = (0..capture_size)
            .flat_map(|y| (0..capture_size).map(move |x| (x, y)))
            .flat_map(|(x, y)| {
                let [r, g, b] = value(face, x, y);
                [to_half(r), to_half(g), to_half(b), to_half(1.)]
            })
            .collect();
        // The mean of the samples as stored, at half precision.
        for (index, texel) in texels.chunks_exact(4).enumerate() {
            let (x, y) = (index as u32 % capture_size, index as u32 / capture_size);
            let mean =
                &mut expected[(face * size * size + y / samples * size + x / samples) as usize];
            for c in 0..3 {
                mean[c] += half(texel[c]) / (samples * samples) as f32;
            }
        }
        queue.write_texture(
            capture.rendered.as_image_copy(),
            bytemuck::cast_slice(&texels),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(capture_size * 8),
                rows_per_image: Some(capture_size),
            },
            wgpu::Extent3d {
                width: capture_size,
                height: capture_size,
                depth_or_array_layers: 1,
            },
        );
        let mut encoder = device.create_command_encoder(&Default::default());
        capture.resolve_face(&device, &mut encoder, face);
        queue.submit([encoder.finish()]);
    }
    let mut encoder = device.create_command_encoder(&Default::default());
    capture.encode(&device, &mut encoder);
    let levels = capture.read(&device, &queue, encoder).unwrap();
    // One half-precision rounding per level of the 4x4 chain, each at most
    // half a step (2^-10 between 2 and 4); neighbouring texels differ by
    // 1/64.
    let tolerance = 3e-3;
    for (index, texel) in levels[..expected.len() * 4].chunks_exact(4).enumerate() {
        let (face, y, x) = (
            index as u32 / (size * size),
            index as u32 / size % size,
            index as u32 % size,
        );
        let actual = [half(texel[0]), half(texel[1]), half(texel[2])];
        assert!(
            expected[index]
                .iter()
                .zip(actual)
                .all(|(e, a)| (e - a).abs() < tolerance),
            "face {face} texel ({x}, {y}): {actual:?}, mean of its samples {:?}",
            expected[index]
        );
    }
}
