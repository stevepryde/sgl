use super::*;
use crate::settings::{Bloom, RenderPreset};
use crate::test_support::{half, read};

/// A 1×1 exposure multiplier of `value`, as the exposure stage writes it.
pub(super) fn exposure_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    value: f32,
) -> wgpu::TextureView {
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

/// The default look exposed by `exposure`.
pub(super) fn look(exposure: &wgpu::TextureView) -> Look<'_> {
    static BLOOM: std::sync::LazyLock<crate::BloomParameters> =
        std::sync::LazyLock::new(Default::default);
    static GRADING: std::sync::LazyLock<crate::ColorGrading> =
        std::sync::LazyLock::new(Default::default);
    Look {
        exposure,
        stops: 0.,
        bloom: &BLOOM,
        grading: &GRADING,
    }
}

/// Sizes for a scene presented at its own size.
fn sizes(size: [u32; 2]) -> Sizes {
    Sizes {
        render: size,
        scene: size,
        output: size,
    }
}

// Defects: AA Off still filters, flips rows, loses channels or skips/doubles the
// display transfer, on an sRGB output or one that stores what is written
// alike. The independent oracle is captured pre-AA scene data plus the sRGB
// transfer equation, within the output's dither of up to a code value.
// Compilation cannot check these pixel semantics.
#[test]
fn antialiasing_off_preserves_captured_pixels() {
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    pollster::block_on(async {
        let capture = include_bytes!("../../../tests/fixtures/tone-mapped-crop.rgba16");
        for format in [
            HDR,
            wgpu::TextureFormat::Rgba8UnormSrgb,
            wgpu::TextureFormat::Bgra8UnormSrgb,
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureFormat::Bgra8Unorm,
        ] {
            let mut post = Post::new(
                &device,
                &queue,
                format,
                sizes([80, 64]),
                true,
                SmaaQuality::default(),
            )
            .unwrap();
            for size in [[80, 64], [37, 29]] {
                post.resize(&device, sizes(size), true);
                let bytes = &capture[..(size[0] * size[1] * 8) as usize];
                queue.write_texture(
                    post.tone_map.tone_mapped().texture().as_image_copy(),
                    bytes,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(size[0] * 8),
                        rows_per_image: Some(size[1]),
                    },
                    post.tone_map.tone_mapped().texture().size(),
                );
                let output = target(&device, "AA switch result", size, format);
                {
                    let mut encoder = device.create_command_encoder(&Default::default());
                    post.tone_map.present_output(
                        &device,
                        &mut encoder,
                        &post.inputs,
                        post.bloom.halo(),
                        &output,
                        None,
                    );
                    queue.submit([encoder.finish()]);
                    let actual = read(
                        &device,
                        &queue,
                        output.texture(),
                        if format == HDR { 8 } else { 4 },
                    );
                    // Linear colour as an 8-bit sRGB code, unrounded.
                    let code = |linear: f64| {
                        let encoded = if linear <= 0.0031308 {
                            12.92 * linear
                        } else {
                            1.055 * linear.powf(1.0 / 2.4) - 0.055
                        };
                        encoded.clamp(0.0, 1.0) * 255.0
                    };
                    if format == HDR {
                        for (index, texel) in actual.chunks_exact(2).enumerate() {
                            let value = f64::from(half(texel));
                            let captured = f64::from(half(&bytes[index * 2..]));
                            let channel = index % 4;
                            if channel == 3 {
                                assert_eq!(value, captured, "AA Off must copy alpha exactly");
                            } else {
                                assert!(
                                    (code(value) - code(captured)).abs() <= 1.,
                                    "{size:?} pixel {} channel {channel}: {value} vs {captured}",
                                    index / 4
                                );
                            }
                        }
                    } else {
                        for (pixel, rgba) in actual.chunks_exact(4).enumerate() {
                            for (channel, &value) in rgba.iter().enumerate() {
                                let source_channel = if format.remove_srgb_suffix()
                                    == wgpu::TextureFormat::Bgra8Unorm
                                    && channel < 3
                                {
                                    2 - channel
                                } else {
                                    channel
                                };
                                let linear =
                                    f64::from(half(&bytes[pixel * 8 + source_channel * 2..]));
                                let expected = if channel == 3 {
                                    linear.clamp(0.0, 1.0) * 255.0
                                } else {
                                    code(linear)
                                }
                                .round() as i32;
                                assert!(
                                    (i32::from(value) - expected).abs() <= 1,
                                    "{format:?} {size:?} pixel {pixel} channel {channel}: {} vs {expected}",
                                    value
                                );
                            }
                        }
                    }
                }
                eprintln!("AA bypass: {format:?} {size:?}; <=1 sRGB code value");
            }
        }
    });
}

// Defects: disabling bloom consumes stale halos, or a preset change discards an
// explicit On override. Without bloom, a single bright texel on black leaves
// its neighbours as black as the far corner; with bloom they are brighter,
// independently of the bloom kernel or tone mapper's particular equations.
// The tone-mapped capture is read, before the output's dither.
#[test]
fn bloom_switch_removes_halos_and_preserves_low_override() {
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    pollster::block_on(async {
        let mut post = Post::new(
            &device,
            &queue,
            HDR,
            sizes([64, 64]),
            true,
            SmaaQuality::default(),
        )
        .unwrap();
        let output = target(&device, "bloom switch result", [64, 64], HDR);
        let exposure = exposure_texture(&device, &queue, 1.);
        let scene = target(&device, "impulse", [64, 64], HDR);
        for preset in [RenderPreset::High, RenderPreset::Low, RenderPreset::High] {
            post.resize(&device, sizes([64, 64]), preset == RenderPreset::High);
            let size = [64, 64];
            let mut input = vec![0u16; (size[0] * size[1] * 4) as usize];
            // 0x4c00 is IEEE binary16 16.0, an HDR impulse.
            let center = ((size[1] / 2 * size[0] + size[0] / 2) * 4) as usize;
            input[center..center + 3].fill(0x4c00);
            queue.write_texture(
                scene.texture().as_image_copy(),
                bytemuck::cast_slice(&input),
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(size[0] * 8),
                    rows_per_image: Some(size[1]),
                },
                scene.texture().size(),
            );
            let mut off_before = None;
            for bloom in [Bloom::Off, Bloom::On, Bloom::Off] {
                let enabled = bloom.enabled(preset == RenderPreset::Low);
                let mut encoder = device.create_command_encoder(&Default::default());
                let presentation = Presentation {
                    bloom: enabled,
                    smaa: None,
                    capture: true,
                };
                post.present(
                    &device,
                    &queue,
                    &mut encoder,
                    &scene,
                    presentation,
                    look(&exposure),
                    &output,
                    None,
                );
                queue.submit([encoder.finish()]);
                let actual = read(&device, &queue, post.tone_mapped().texture(), 8);
                let neighbor = half(&actual[(32 * 64 + 26) * 8..]);
                // Tone-mapped black, far from the impulse.
                let black = half(&actual[..]);
                if bloom == Bloom::Off {
                    assert_eq!(neighbor, black, "Off must not spread the impulse");
                    if let Some(before) = &off_before {
                        assert_eq!(
                            &actual, before,
                            "disabling bloom must remove all previous halo data"
                        );
                    }
                    off_before = Some(actual);
                } else {
                    assert!(
                        neighbor > black,
                        "explicit On must render a halo in either preset"
                    );
                }
            }
        }
        eprintln!(
            "Bloom: Off/On/Off removes stale halos across High/Low/High; Low honors explicit On"
        );
    });
}

// Defects: DPI scaling is applied twice, a preset stretches a non-16:9 scene,
// small windows upscale, or switching back to Full leaves stale-sized targets.
// Physical texture extents expose these integration failures independently of
// the resolution helper's calculated dimensions.
#[test]
fn scene_resolution_budgets_resize_targets_without_stretching_or_upscaling() {
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    pollster::block_on(async {
        use crate::settings::SceneResolution;
        let mut settings = crate::settings::Settings::default();
        let mut renderer =
            crate::renderer::Renderer::new(&device, &queue, HDR, [64, 64], 2., &settings).unwrap();
        for display in [[2560, 1440], [2560, 1080], [1200, 1600], [800, 600]] {
            for (resolution, bounds) in [
                (SceneResolution::Hd, [1280, 720]),
                (SceneResolution::FullHd, [1920, 1080]),
                (SceneResolution::Full, display),
            ] {
                settings.scene_resolution = resolution;
                renderer.resize(&device, display, 2., &settings);
                let extent = renderer.targets().color.texture().size();
                let scene = [extent.width, extent.height];
                for axis in 0..2 {
                    assert!(scene[axis] <= bounds[axis] && scene[axis] <= display[axis]);
                }
                // One dimension must reach its budget (unless the window is smaller).
                assert!(
                    (0..2).any(|axis| scene[axis].abs_diff(bounds[axis].min(display[axis])) <= 1)
                );
                let aspect_error = (scene[0] as i64 * display[1] as i64
                    - scene[1] as i64 * display[0] as i64)
                    .abs();
                assert!(
                    aspect_error <= i64::from(display[0].max(display[1])),
                    "aspect distortion"
                );
                if display[0] <= bounds[0] && display[1] <= bounds[1] {
                    assert_eq!(
                        scene, display,
                        "small windows and Full must retain every pixel"
                    );
                }
                let output = renderer.tone_mapped().texture().size();
                assert_eq!(
                    [output.width, output.height],
                    display,
                    "display/UI remains full resolution"
                );
                eprintln!("{resolution:?}: {display:?} -> {scene:?}");
            }
        }
    });
}

// Defect: SMAA detects edges on the unexposed HDR scene, so its absolute
// thresholds scale with the exposure: a scene exposed up by 4 stops loses
// its edges and one exposed down by 4 stops finds edges in every gradient.
// The oracle is the displayed image: a scene 16 times brighter under an
// exposure 16 times lower (and the reverse) is the same image, so its
// antialiased presentation is identical, texel for texel, to the scene's
// own; and SMAA's output differs from its input, so its edges are found at
// all. The scene is presented above its size, through the resample.
#[test]
fn smaa_antialiases_the_displayed_image_whatever_the_exposure() {
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    let scene_size = [48, 40];
    let sizes = Sizes {
        render: scene_size,
        scene: scene_size,
        output: [72, 60],
    };
    let mut post = Post::new(&device, &queue, HDR, sizes, false, SmaaQuality::Medium).unwrap();
    let output = target(&device, "SMAA exposure result", sizes.output, HDR);
    // A dark field and a bright one across a shallow diagonal: 0.05 and 0.8
    // differ by under SMAA's 0.1 threshold once divided by 16.
    let texels = |scale: f32| -> Vec<[f32; 4]> {
        (0..scene_size[1])
            .flat_map(|y| {
                (0..scene_size[0]).map(move |x| {
                    let value = if 3 * x > 5 * y + 10 { 0.8 } else { 0.05 };
                    [value * scale, value * scale, value * scale, 1.]
                })
            })
            .collect()
    };
    // The captured presentation, and whether SMAA changed the tone-mapped
    // scene.
    let mut present = |scale: f32| {
        let scene = crate::test_support::hdr_texture(&device, &queue, scene_size, &texels(scale));
        let exposure = exposure_texture(&device, &queue, 1. / scale);
        let mut encoder = device.create_command_encoder(&Default::default());
        post.present(
            &device,
            &queue,
            &mut encoder,
            &scene,
            Presentation {
                bloom: false,
                smaa: Some(SmaaQuality::Medium),
                capture: true,
            },
            look(&exposure),
            &output,
            None,
        );
        queue.submit([encoder.finish()]);
        let targets = post.smaa_targets.as_ref().unwrap();
        let changed = read(&device, &queue, targets.tone_mapped.texture(), 8)
            != read(&device, &queue, targets.antialiased.texture(), 8);
        (
            read(&device, &queue, post.tone_mapped().texture(), 8),
            changed,
        )
    };
    let (displayed, changed) = present(1.);
    assert!(changed, "SMAA must change the diagonal");
    for scale in [16., 1. / 16.] {
        assert!(
            present(scale).0 == displayed,
            "the scene times {scale} under an exposure of 1/{scale} must antialias alike"
        );
    }
}
