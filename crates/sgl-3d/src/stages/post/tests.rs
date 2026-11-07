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
        tone_mapping: crate::settings::ToneMapping::default(),
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
                Output::texture(format),
                sizes([80, 64]),
                true,
                SmaaQuality::default(),
            )
            .unwrap();
            for size in [[80, 64], [37, 29]] {
                post.resize(&device, sizes(size), true);
                let bytes = &capture[..(size[0] * size[1] * 8) as usize];
                post.tone_map.hold_capture(&device);
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
            Output::texture(HDR),
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
                    spatial: false,
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
                assert!(
                    renderer
                        .diagnostic_target(crate::diagnostics::DiagnosticTarget::ToneMapped)
                        .is_none(),
                    "resizing must not allocate a diagnostic-only output target"
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
    let mut post = Post::new(
        &device,
        &queue,
        Output::texture(HDR),
        sizes,
        false,
        SmaaQuality::Medium,
    )
    .unwrap();
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
                spatial: false,
            },
            look(&exposure),
            &output,
            None,
        );
        queue.submit([encoder.finish()]);
        let targets = post.smaa_targets.as_ref().unwrap();
        let changed = read(&device, &queue, targets.tone_mapped.texture(), 8)
            != read(
                &device,
                &queue,
                targets.antialiased.as_ref().unwrap().texture(),
                8,
            );
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

#[test]
fn spatial_upscaling_is_opt_in_and_does_not_replace_temporal_reconstruction() {
    use crate::settings::Upscaling;
    assert_eq!(
        crate::settings::Settings::default().upscaling,
        Upscaling::Off
    );
    let scaled = Sizes {
        render: [8, 6],
        scene: [8, 6],
        output: [17, 13],
    };
    assert!(!spatial_resolves(
        Upscaling::Off,
        scaled,
        Completed::Composite
    ));
    assert!(spatial_resolves(
        Upscaling::On,
        scaled,
        Completed::Composite
    ));
    assert!(spatial_resolves(Upscaling::On, scaled, Completed::Taa));
    assert!(!spatial_resolves(
        Upscaling::On,
        sizes([8, 6]),
        Completed::Composite
    ));
    let temporal = Sizes {
        render: [4, 3],
        ..scaled
    };
    assert!(spatial_resolves(Upscaling::On, temporal, Completed::Fsr2));
    assert!(!spatial_resolves(
        Upscaling::On,
        temporal,
        Completed::Composite
    ));
    assert!(!spatial_resolves(
        Upscaling::On,
        Sizes {
            output: [7, 5],
            ..scaled
        },
        Completed::Taa
    ));
}

// Independent invariants: constant colour and alpha survive at borders;
// spatial reconstruction differs from linear sampling on a diagonal; no
// optional feature is requested. Check target lifetime through On/Off and
// output-only resize transitions, not source-code assertions.
#[test]
fn spatial_upscaling_runs_on_baseline_device_and_releases_optional_targets() {
    let Some(adapter) = crate::test_support::adapter() else {
        return;
    };
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_features: wgpu::Features::empty(),
        required_limits: wgpu::Limits::default(),
        ..Default::default()
    }))
    .unwrap();
    assert!(device.features().is_empty());
    let scene_size = [8, 6];
    let mut sizes = Sizes {
        render: scene_size,
        scene: scene_size,
        output: [17, 13],
    };
    let mut post = Post::new(
        &device,
        &queue,
        Output::texture(HDR),
        sizes,
        false,
        SmaaQuality::default(),
    )
    .unwrap();
    assert!(!post.tone_map.holds_capture());
    assert!(!post.fsr1.target_is_held());
    let exposure = exposure_texture(&device, &queue, 1.);
    for output_size in [[17, 13], [19, 15], [17, 13]] {
        sizes.output = output_size;
        post.resize(&device, sizes, false);
        assert!(!post.fsr1.target_is_held());
        assert!(!post.tone_map.holds_capture());
        let output = target(&device, "spatial presentation", output_size, HDR);
        // Both flat extremes catch undefined zero reciprocals in AMD's
        // contrast limiters; coloured grey also catches double gamma.
        for color in [[0., 0., 0., 1.], [1., 1., 1., 1.], [0.18, 0.5, 0.75, 1.]] {
            let scene =
                crate::test_support::hdr_texture(&device, &queue, scene_size, &vec![color; 48]);
            let mut reference = device.create_command_encoder(&Default::default());
            post.present(
                &device,
                &queue,
                &mut reference,
                &scene,
                Presentation {
                    bloom: false,
                    smaa: None,
                    capture: true,
                    spatial: false,
                },
                look(&exposure),
                &output,
                None,
            );
            queue.submit([reference.finish()]);
            let expected = read(&device, &queue, post.tone_map.tone_mapped().texture(), 8);
            let mut encoder = device.create_command_encoder(&Default::default());
            post.present(
                &device,
                &queue,
                &mut encoder,
                &scene,
                Presentation {
                    bloom: false,
                    smaa: None,
                    capture: true,
                    spatial: true,
                },
                look(&exposure),
                &output,
                None,
            );
            queue.submit([encoder.finish()]);
            let captured = post.tone_map.tone_mapped();
            assert_eq!(
                [captured.texture().width(), captured.texture().height()],
                output_size
            );
            // Capture is before dither, independent of the output transfer.
            let data = read(&device, &queue, captured.texture(), 8);
            let expected = &expected[..8];
            let flat = &data[..8];
            for pixel in data.chunks_exact(8) {
                for channel in 0..4 {
                    let actual = half(&pixel[channel * 2..]);
                    assert!(actual.is_finite());
                    assert!(
                        (actual - half(&flat[channel * 2..])).abs() < 0.003,
                        "flat colour changed at border"
                    );
                    // AMD's medium reciprocal has up to 0.316% relative
                    // error; converting gamma-2 back to linear doubles it.
                    // Allow that 0.632% plus half-float roundoff, without
                    // hiding a missing or doubled colour conversion.
                    assert!(
                        (actual - half(&expected[channel * 2..])).abs() < 0.008,
                        "flat colour/gamma differs from linear presentation"
                    );
                }
                assert_eq!(half(&pixel[6..]), 1.);
            }
        }
        let texels: Vec<_> = (0..6)
            .flat_map(|y| {
                (0..8).map(move |x| {
                    let c = if x + y > 6 { 0.8 } else { 0.1 };
                    [c, c * 0.7, c * 0.3, 1.]
                })
            })
            .collect();
        let scene = crate::test_support::hdr_texture(&device, &queue, scene_size, &texels);
        let mut capture_identity = None;
        for smaa in [None, Some(SmaaQuality::Medium)] {
            for _ in 0..2 {
                let mut encoder = device.create_command_encoder(&Default::default());
                post.present(
                    &device,
                    &queue,
                    &mut encoder,
                    &scene,
                    Presentation {
                        bloom: false,
                        smaa,
                        capture: true,
                        spatial: true,
                    },
                    look(&exposure),
                    &output,
                    None,
                );
                queue.submit([encoder.finish()]);
                let view = post.captured_tone_mapped().unwrap().clone();
                if let Some(previous) = capture_identity.as_ref() {
                    assert_eq!(
                        &view, previous,
                        "consecutive captures must reuse the output target"
                    );
                }
                capture_identity = Some(view);
            }
        }
        let mut spatial_output = None;
        for spatial in [true, false, true] {
            let mut encoder = device.create_command_encoder(&Default::default());
            post.present(
                &device,
                &queue,
                &mut encoder,
                &scene,
                Presentation {
                    bloom: false,
                    smaa: None,
                    capture: false,
                    spatial,
                },
                look(&exposure),
                &output,
                None,
            );
            queue.submit([encoder.finish()]);
            assert!(post.captured_tone_mapped().is_none());
            assert!(!post.tone_map.holds_capture());
            assert_eq!(post.fsr1.target_is_held(), spatial);
            if spatial {
                let data = read(&device, &queue, output.texture(), 8);
                if let Some(before) = spatial_output.as_ref() {
                    assert_eq!(&data, before);
                }
                spatial_output = Some(data);
                assert!(post.smaa_targets.as_ref().unwrap().antialiased.is_none());
            } else {
                assert!(post.smaa_targets.is_none());
                assert_ne!(
                    spatial_output.as_ref().unwrap(),
                    &read(&device, &queue, output.texture(), 8)
                );
            }
        }
    }
}
