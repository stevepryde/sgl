//! The mip chain stage at the GPU boundary: the levels it draws into a
//! decoded image's textures, read back, and what it leaves alone.
use super::MipChains;
use crate::asset::{Image, MaterialTexture, Rgba8MipChain};
use crate::content::images::{area_means, noise};
use crate::view::compile::Compiler;
use crate::{Scene, test_support};

/// A scene with one material whose base map (colour) and
/// metallic-roughness map (data) are `image`: its colour and data
/// textures.
fn colour_and_data(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    image: Image,
) -> (Scene, [wgpu::Texture; 2]) {
    let mut material = test_support::cube().materials[0].clone();
    material.base_texture = Some(MaterialTexture::new(0));
    material.mr_texture = Some(MaterialTexture::new(0));
    let mut scene = Scene::new(device, queue);
    scene
        .add_materials(device, queue, &[material], &[image])
        .unwrap();
    let texture = scene.materials.texture(0);
    let textures =
        [&texture.color, &texture.data].map(|view| view.as_ref().unwrap().texture().clone());
    (scene, textures)
}

/// Every level of `texture`, read back.
fn levels(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
) -> Vec<image::RgbaImage> {
    (0..texture.mip_level_count())
        .map(|level| {
            let size = texture
                .size()
                .mip_level_size(level, wgpu::TextureDimension::D2);
            let texels = test_support::read_mip(device, queue, texture, level);
            image::RgbaImage::from_raw(size.width, size.height, texels).unwrap()
        })
        .collect()
}

// Plausible defects: a level drawn from the wrong level or not at all (a
// texture left with unwritten levels, which read as zero); a footprint
// that leaves out the odd row or column of a size that is not a power of
// two, or weighs its texels wrongly; colour averaged as stored, or data in
// linear light; a chain that stops before one texel. The oracle is the
// brute-force area model (`area_means`) of each level from the level above
// as the device holds it, within one 8-bit step, the device's rounding of
// a mean and of sRGB's encoding, and the full chain's level count and
// sizes. Noise makes a wrong footprint or transfer function differ by tens
// of steps.
#[test]
fn a_built_chain_is_the_area_mean_of_each_level_above() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    for (seed, (width, height)) in [(8, 8), (7, 5), (1, 6), (5, 1), (9, 9), (16, 2)]
        .into_iter()
        .enumerate()
    {
        let source = noise(width, height, seed as u64 + 11);
        let (mut scene, textures) = colour_and_data(&device, &queue, Image::Rgba8(source.clone()));
        MipChains::default().build(
            &Compiler::new(&device),
            (&device, &queue),
            scene.materials.chains(),
            None,
        );
        for (texture, srgb) in textures.iter().zip([true, false]) {
            let levels = levels(&device, &queue, texture);
            assert_eq!(levels.len() as u32, 32 - width.max(height).leading_zeros());
            assert_eq!(levels[0], source, "the first level is the image");
            for (above, level) in levels.iter().zip(&levels[1..]) {
                assert_eq!(
                    level.dimensions(),
                    ((above.width() / 2).max(1), (above.height() / 2).max(1))
                );
                for (texel, mean) in level.pixels().zip(area_means(above, srgb)) {
                    for channel in 0..4 {
                        let off = (f64::from(texel[channel]) - mean[channel]).abs();
                        assert!(
                            off <= 1.,
                            "{width}x{height} srgb {srgb} level {:?} channel {channel}: {} for a mean of {}",
                            level.dimensions(),
                            texel[channel],
                            mean[channel]
                        );
                    }
                }
            }
        }
    }
}

// Plausible defects: textures that share a pass (four of one format and
// size) drawn each from another's level above, or one target of the pass
// left unwritten; a texture left over after the full passes skipped; or
// textures of different sizes put in one pass. Nine colour maps of one
// size, each its own noise, make two full passes and one left over, and one
// of another size a pass of its own. The oracle is the area model of each
// texture's own levels, as in `a_built_chain_is_the_area_mean_of_each_level_above`.
#[test]
fn textures_drawn_together_each_build_their_own_chain() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let sizes = [(7, 5); 9].into_iter().chain([(6, 6)]);
    let images: Vec<_> = sizes
        .enumerate()
        .map(|(seed, (width, height))| noise(width, height, seed as u64 + 31))
        .collect();
    let materials: Vec<_> = (0..images.len())
        .map(|image| {
            let mut material = test_support::cube().materials[0].clone();
            material.base_texture = Some(MaterialTexture::new(image));
            material
        })
        .collect();
    let mut scene = Scene::new(&device, &queue);
    let decoded: Vec<_> = images.iter().cloned().map(Image::Rgba8).collect();
    scene
        .add_materials(&device, &queue, &materials, &decoded)
        .unwrap();
    MipChains::default().build(
        &Compiler::new(&device),
        (&device, &queue),
        scene.materials.chains(),
        None,
    );
    for (index, source) in images.iter().enumerate() {
        let texture = scene.materials.texture(index);
        let texture = texture.color.as_ref().unwrap().texture();
        let levels = levels(&device, &queue, texture);
        assert_eq!(&levels[0], source);
        for (above, level) in levels.iter().zip(&levels[1..]) {
            for (texel, mean) in level.pixels().zip(area_means(above, true)) {
                for channel in 0..4 {
                    let off = (f64::from(texel[channel]) - mean[channel]).abs();
                    assert!(
                        off <= 1.,
                        "image {index} level {:?} channel {channel}: {} for a mean of {}",
                        level.dimensions(),
                        texel[channel],
                        mean[channel]
                    );
                }
            }
        }
    }
}

// Plausible defect: the colour texture averaged as stored (its sRGB view or
// target replaced by a linear one), the data texture averaged in linear
// light, or alpha taken through the transfer function. The oracle is
// computed by hand from sRGB's transfer function (IEC 61966-2-1): equal
// parts of codes 0 and 255 have the mean 127.5 as stored, and in linear
// light 0.5, which encodes to 1.055 * 0.5^(1/2.4) - 0.055 = 0.7354, code
// 187.5. The two answers are 59 steps apart; each is held within one.
#[test]
fn colour_is_averaged_in_linear_light_and_data_and_alpha_as_stored() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let checker =
        image::RgbaImage::from_fn(4, 4, |x, y| image::Rgba([((x + y) % 2 * 255) as u8; 4]));
    let (mut scene, [colour, data]) = colour_and_data(&device, &queue, Image::Rgba8(checker));
    MipChains::default().build(
        &Compiler::new(&device),
        (&device, &queue),
        scene.materials.chains(),
        None,
    );
    let within = |code: u8, expected: f64| (f64::from(code) - expected).abs() <= 1.;
    for level in 1..3 {
        for texel in levels(&device, &queue, &colour)[level].pixels() {
            assert!(
                texel.0[..3].iter().all(|&code| within(code, 187.5)) && within(texel[3], 127.5),
                "colour level {level}: {texel:?}"
            );
        }
        for texel in levels(&device, &queue, &data)[level].pixels() {
            assert!(
                texel.0.iter().all(|&code| within(code, 127.5)),
                "data level {level}: {texel:?}"
            );
        }
    }
}

// Plausible defect: a chain the caller prepared, or a block-compressed one,
// queued for the stage and drawn over (a prepared chain's levels replaced
// by the GPU's, a compressed texture used as a render target, which the
// device refuses), or the stage's programs created for a scene that has no
// chain to build. The oracles: the prepared textures' levels read back are
// the bytes the caller handed in; and wgpu's own count of render pipelines
// (`wgpu::Instance::generate_report`) is unchanged by the stage for such a
// scene, while a decoded image's chain raises it, which shows the count
// sees the stage's programs.
#[test]
fn prepared_and_compressed_chains_are_uploaded_as_stored_and_build_nothing() {
    let Some((instance, device, queue)) = test_support::counted_device() else {
        return;
    };
    let pipelines = || {
        let report = instance.generate_report().unwrap();
        report.hub_report().render_pipelines.num_allocated
    };
    let chain = Rgba8MipChain::new(noise(8, 4, 3)).unwrap();
    let (mut scene, textures) = colour_and_data(&device, &queue, Image::Rgba8Mips(chain.clone()));
    if device
        .features()
        .contains(wgpu::Features::TEXTURE_COMPRESSION_BC)
    {
        let block = test_support::bc7_mode_6([[100, 60, 20, 127]; 2], [1; 2], [0; 16]);
        let blocks = [block; 4].concat();
        let compressed = crate::asset::CompressedImage {
            format: crate::asset::CompressedFormat::Bc7,
            width: 8,
            height: 8,
            levels: vec![blocks[..64].to_vec(), blocks[..16].to_vec()],
            decoded_level_0: None,
        };
        let mut material = test_support::cube().materials[0].clone();
        material.base_texture = Some(MaterialTexture::new(0));
        scene
            .add_materials(
                &device,
                &queue,
                &[material],
                &[Image::Compressed(compressed)],
            )
            .unwrap();
    }
    let mut stage = MipChains::default();
    let compiler = Compiler::new(&device);
    let before = pipelines();
    stage.build(&compiler, (&device, &queue), scene.materials.chains(), None);
    assert_eq!(pipelines(), before, "nothing to build creates no program");
    for (texture, srgb) in textures.iter().zip([true, false]) {
        assert_eq!(levels(&device, &queue, texture), chain.levels(srgb));
    }
    let (mut decoded, _) = colour_and_data(&device, &queue, Image::Rgba8(noise(8, 4, 3)));
    stage.build(
        &compiler,
        (&device, &queue),
        decoded.materials.chains(),
        None,
    );
    assert!(
        pipelines() > before,
        "a decoded image's chain creates the stage's programs"
    );
}

// Plausible defect: a texture whose material was removed before the next
// frame, or a sprite image removed, still waits for its chain, so the
// stage draws a chain nothing samples and the scene holds its memory until
// a frame or capture runs, which for a scene a tool never draws is never.
// The oracle is the stage's queue: a material's texture and a sprite image
// wait once added, and nothing waits once both are removed. (A failed
// `add_materials` frees its textures through the same path as a removal.)
#[test]
fn removed_textures_no_longer_wait_for_their_chains() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut material = test_support::cube().materials[0].clone();
    material.base_texture = Some(MaterialTexture::new(0));
    let mut scene = Scene::new(&device, &queue);
    let ids = scene
        .add_materials(
            &device,
            &queue,
            &[material],
            &[Image::Rgba8(noise(8, 8, 5))],
        )
        .unwrap();
    let sprite = scene
        .add_sprite_image(&device, &queue, &Image::Rgba8(noise(8, 8, 6)))
        .unwrap();
    assert!(!scene.materials.chains().is_empty());
    scene.remove_material(ids[0]).unwrap();
    scene.remove_sprite_image(sprite).unwrap();
    assert!(scene.materials.chains().is_empty());
}

// Plausible defect: the chains' passes untimed in the frame's timing, a
// timestamp written by a pass other than the first and last (an end before
// its start, or a write past the span), or a pair of writes a pass, which
// would take one of a frame's 128 timed passes for each of a scene's
// hundreds of chain passes. The oracle is the frame's timing report
// (`GpuTiming`): the chains of five textures of one size, a pass of four
// and a pass of one for each of their three levels below the first, six
// passes, report one group, "mip chains", of one timed pass and a positive
// time.
#[test]
fn a_frames_chains_are_one_timed_span() {
    let Some((device, queue)) = test_support::device_choosing(|adapter| {
        crate::graphics_device::features(adapter)
            | (adapter.features() & wgpu::Features::TIMESTAMP_QUERY)
    }) else {
        return;
    };
    let Some(mut timing) = crate::timing::GpuTiming::new(&device, &queue) else {
        eprintln!("skipping: the device has no timestamp queries");
        return;
    };
    let mut chains = crate::scene::textures::Chains::default();
    for seed in 0..5 {
        chains.upload(&device, &queue, &noise(8, 8, seed), true);
    }
    let mut stage = MipChains::default();
    let _ = timing.begin_frame(&device, &queue);
    stage.build(
        &Compiler::new(&device),
        (&device, &queue),
        &mut chains,
        Some(&timing),
    );
    timing.submitted(&queue);
    let mut reported = None;
    for _ in 0..8 {
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        if let Some(frame) = timing.begin_frame(&device, &queue).next() {
            reported = Some(frame);
            break;
        }
    }
    let frame = reported.expect("the frame's timing completes");
    assert_eq!(frame.passes.len(), 1, "{frame:?}");
    let group = &frame.passes[0];
    assert_eq!((group.name, group.passes), ("mip chains", 1), "{frame:?}");
    assert!(group.ms > 0. && group.ms.is_finite(), "{frame:?}");
}
