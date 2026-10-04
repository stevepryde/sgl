use super::*;
use crate::test_support::to_half;

// The sharp level must return, through the GPU's own cube lookup, exactly the
// captured texel each output texel faces. A face or in-face orientation that
// is not the inverse of hardware cube sampling reads another texel instead.
#[test]
fn sharp_level_reproduces_each_captured_texel_through_cube_sampling() {
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    let size = 1 << (LEVELS - 1);
    let capture = ProbePrefilter::new(&device, size);
    // Every texel of every face distinct: face, column and row in R, G, B.
    let value = |face: u32, x: u32, y: u32| {
        [
            1. + x as f32 / size as f32,
            1. + y as f32 / size as f32,
            1. + face as f32 / 8.,
        ]
    };
    let texels: Vec<u16> = (0..6)
        .flat_map(|face| (0..size).flat_map(move |y| (0..size).map(move |x| (face, x, y))))
        .flat_map(|(face, x, y)| {
            let [r, g, b] = value(face, x, y);
            [to_half(r), to_half(g), to_half(b), to_half(1.)]
        })
        .collect();
    queue.write_texture(
        capture.cube.texture().as_image_copy(),
        bytemuck::cast_slice(&texels),
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(size * 8),
            rows_per_image: Some(size),
        },
        wgpu::Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: 6,
        },
    );
    let mut encoder = device.create_command_encoder(&Default::default());
    capture.encode(&device, &mut encoder);
    let levels = capture.read(&device, &queue, encoder).unwrap();
    let half = |bits: u16| crate::test_support::half(&bits.to_le_bytes());
    for (index, texel) in levels[..texels.len()].chunks_exact(4).enumerate() {
        let (face, y, x) = (
            index as u32 / (size * size),
            index as u32 / size % size,
            index as u32 % size,
        );
        let expected = value(face, x, y);
        let actual = [half(texel[0]), half(texel[1]), half(texel[2])];
        assert!(
            expected
                .iter()
                .zip(actual)
                .all(|(e, a)| (e - a).abs() < 2e-3),
            "face {face} texel ({x}, {y}): {actual:?}, captured {expected:?}"
        );
    }
}
