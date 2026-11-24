//! A device at S3D-1's floor, WebGPU's default limits, runs every pipeline
//! a frame builds on the Basic binding tier, and a device at the Extended
//! tier's threshold on that tier.
use crate::asset::{Asset, Image};
use crate::graphics_device::BindingTier;
use crate::renderer::test_frame::TestRenderer;
use crate::settings::{self, DynamicGiQuality, Settings};
use crate::shading::gbuffer;
use crate::{
    AlphaMode, Camera, DynamicGiVolume, FrameInput, InstanceState, IrradianceVolume, Mobility,
    Scene, test_support,
};
use glam::{Mat4, Vec3};

/// The sampled textures per shader stage from which S3D-1 gives a device
/// the Extended binding tier.
const EXTENDED_SAMPLED_TEXTURES: u32 = 48;

// Plausible defects: a change binds another sampled texture, storage buffer
// or storage texture to a stage, or writes more colour-attachment bytes in a
// pass without a fallback, past WebGPU's default limits, the floor S3D-1
// states, so a device that offers just those (a browser's default WebGPU
// adapter, Chromium before 149) fails to create a pipeline though the spec,
// the README and the docs promise it runs SGL3D; or the device takes the
// wrong binding tier, composes an Extended binding below 48, or runs the
// dynamic GI stage there. The oracle is wgpu's `Limits::default()`, which
// wgpu sets to WebGPU's, and wgpu's validation of every pipeline layout,
// pipeline, bind group and pass against the device's limits, which wgpu
// runs before any backend: frames that build the heaviest pipelines
// (transmissive, dispersive blended receivers of screen-space reflections
// that between them carry every material map, one through a game's shader
// that reads the scene depth, a masked wall through the same shader,
// world-space reflections, ambient occlusion, screen-space indirect light,
// whose second frame binds its kept result, fog under a procedural sky with
// aerial perspective, contact shadows from a shadowed sun, whose lighting
// pass binds the contact mask beside that kept result, and TAA, over an
// irradiance volume and a dynamic GI volume) on a device of
// those limits and no optional feature raise no validation error, the
// device reports the Basic tier, and dynamic GI is reported off.
#[test]
fn a_device_at_webgpus_default_limits_runs_every_pipeline() {
    let (tier, dynamic_gi) = heaviest_frame(wgpu::Limits::default(), wgpu::Trace::Off);
    assert_eq!(tier, BindingTier::Basic);
    assert_eq!(dynamic_gi, DynamicGiQuality::Off);
}

// Plausible defects: the tier is chosen from the wrong limit, with `>` for
// `>=`, or from a threshold above 48, so a device S3D-1 gives every binding
// takes the Basic tier; or the tier does not reach one of the scene and the
// renderer, which then build group 2 to different layouts, so the draw of a
// material binds a group its pipeline's layout does not take; or dynamic GI
// is reported off where it runs. The oracle is S3D-1's threshold and wgpu's
// validation: on a device of WebGPU's default limits but exactly 48 sampled
// textures a stage, as Chrome's upper tier offers, the same frame raises no
// validation error, the device reports the Extended tier, and dynamic GI
// runs at the setting.
#[test]
fn a_device_at_the_extended_threshold_binds_the_extended_tier() {
    let limits = wgpu::Limits {
        max_sampled_textures_per_shader_stage: EXTENDED_SAMPLED_TEXTURES,
        ..wgpu::Limits::default()
    };
    let (tier, dynamic_gi) = heaviest_frame(limits, wgpu::Trace::Off);
    assert_eq!(tier, BindingTier::Extended);
    assert_eq!(dynamic_gi, DynamicGiQuality::High);
}

// #895: a build without debug assertions embeds SGL's WGSL with its
// comments, indentation and trailing whitespace removed
// (`sgl_post_fx::wgsl`), and this one embeds it whole. Plausible defects:
// stripping joins two tokens a comment separated, ends a comment early or
// late (nested, `/*/`, a line comment at a carriage return), drops or
// merges a line so errors move, or removes a line `compose` or
// `sgl-post-fx`'s preprocessor reads. Every shader module the heaviest
// frames create on each binding tier, as wgpu's API trace records its
// source, must keep its line count and parse through naga to the same
// module stripped: the oracle is naga's parse of the whole source. The
// modules are composed whole and stripped after; a release build composes
// stripped files, which line for line is the same text but for the
// composition's own comments.
#[test]
fn every_program_of_the_heaviest_frames_parses_the_same_stripped() {
    let extended = wgpu::Limits {
        max_sampled_textures_per_shader_stage: EXTENDED_SAMPLED_TEXTURES,
        ..wgpu::Limits::default()
    };
    // Each tier's programs, once each: most are the same on both. The
    // tiers' devices, and then the comparisons, run on threads of their
    // own, to keep the test to seconds.
    let tiers = [("basic", wgpu::Limits::default()), ("extended", extended)];
    let traced: Vec<Vec<String>> = std::thread::scope(|scope| {
        let threads: Vec<_> = tiers
            .into_iter()
            .map(|(tier, limits)| scope.spawn(move || traced_programs(tier, limits)))
            .collect();
        threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect()
    });
    let programs: Vec<String> = traced
        .into_iter()
        .flatten()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    let workers = std::thread::available_parallelism().map_or(1, |n| n.get().min(4));
    let stripped_programs: usize = std::thread::scope(|scope| {
        let threads: Vec<_> = programs
            .chunks(programs.len().div_ceil(workers))
            .map(|chunk| {
                scope.spawn(move || {
                    chunk
                        .iter()
                        .filter(|source| strips_alike(source.as_str()))
                        .count()
                })
            })
            .collect();
        threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .sum()
    });
    // Stripping removed something.
    assert!(stripped_programs > 0);
}

/// The source of every shader module the heaviest frames create on a
/// device of `limits`, from wgpu's API trace of it.
fn traced_programs(tier: &str, limits: wgpu::Limits) -> Vec<String> {
    // Under the workspace's `target/`, where test output goes.
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/tmp")
        .join(format!("sgl-wgsl-strip-{tier}-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    heaviest_frame(limits, wgpu::Trace::Directory(directory.clone()));
    let mut programs = Vec::new();
    for entry in std::fs::read_dir(&directory).unwrap() {
        let path = entry.unwrap().path();
        if path
            .extension()
            .is_some_and(|extension| extension == "wgsl")
        {
            programs.push(std::fs::read_to_string(&path).unwrap());
        }
    }
    std::fs::remove_dir_all(&directory).unwrap();
    assert!(!programs.is_empty(), "{tier} created no program");
    programs
}

/// Panics unless `source` stripped keeps its lines and parses to the same
/// module; whether stripping removed anything.
fn strips_alike(source: &str) -> bool {
    let parse = |text: &str| {
        naga::front::wgsl::parse_str(text)
            .unwrap_or_else(|error| panic!("{}", error.emit_to_string(text)))
    };
    let stripped = sgl_post_fx::wgsl::stripped(source);
    let first = source.lines().find(|line| !line.trim().is_empty());
    assert_eq!(
        stripped.lines().count(),
        source.lines().count(),
        "{first:?}"
    );
    assert!(
        without_spans(&parse(source)) == without_spans(&parse(&stripped)),
        "the program starting {first:?} parses to another module stripped"
    );
    stripped.len() < source.len()
}

/// `module` as its `Debug` text without the source spans naga keeps for
/// errors (`Span { start, end }`), which stripping moves.
fn without_spans(module: &naga::Module) -> String {
    let debug = format!("{module:?}");
    let mut text = String::with_capacity(debug.len());
    let mut rest = debug.as_str();
    while let Some(at) = rest.find("Span { start: ") {
        text.push_str(&rest[..at]);
        rest = &rest[at + rest[at..].find('}').unwrap() + 1..];
    }
    text.push_str(rest);
    text
}

/// The binding tier and the dynamic GI in effect of a device of `limits`
/// and no optional feature, after it draws two frames of the heaviest
/// pipelines with no validation error, `trace` recording what it was asked
/// to do. The device is wgpu's noop backend,
/// whose adapter offers just `limits` and no feature: wgpu validates every
/// call as it does on a GPU and skips only the backend's shader translation
/// and compile, which is most of a GPU's time and no part of the oracle.
fn heaviest_frame(limits: wgpu::Limits, trace: wgpu::Trace) -> (BindingTier, DynamicGiQuality) {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::NOOP,
        backend_options: wgpu::BackendOptions {
            noop: wgpu::NoopBackendOptions {
                enable: true,
                limits: Some(limits.clone()),
                features: Some(wgpu::Features::empty()),
                ..Default::default()
            },
            ..Default::default()
        },
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    let adapter = pollster::block_on(instance.request_adapter(&Default::default())).unwrap();
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_limits: limits,
        trace,
        ..Default::default()
    }))
    .unwrap();
    let validation = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let settings = Settings {
        antialiasing: settings::Antialiasing::Taa,
        screen_space_reflections: settings::ScreenSpaceReflections::Full,
        world_space_reflections: settings::WorldSpaceReflections::Moving,
        dynamic_gi: DynamicGiQuality::High,
        ambient_occlusion: settings::AmbientOcclusionQuality::High,
        screen_space_indirect: settings::ScreenSpaceIndirect::High,
        atmosphere: true,
        contact_shadows: settings::ContactShadows::Long,
        ..Settings::default()
    };
    let size = [32, 32];
    let mut renderer = TestRenderer::for_test(&device, &queue, size, &settings);
    let mut scene = Scene::new(&device, &queue);
    // A game's shader, whose programs the tier's validation composed, on
    // the masked wall's casters and G-buffer and the bump glass's blended
    // draws, which take the scene depth on Extended.
    let shader = test_support::add_test_shader(&mut scene);
    let mut wall = test_support::cube();
    wall.materials[0].alpha = AlphaMode::Mask { cutoff: 0.5 };
    let wall = scene.add_asset(&device, &queue, wall).unwrap();
    let normal_glass = scene.add_asset(&device, &queue, glass(true)).unwrap();
    let bump_glass = scene.add_asset(&device, &queue, glass(false)).unwrap();
    for material in [wall.materials[0], bump_glass.materials[0]] {
        test_support::set_shader(&mut scene, &queue, material, (shader, 0.));
    }
    for (model, at, mobility) in [
        (wall.model, Vec3::new(0.3, 0., -4.), Mobility::Static),
        (
            normal_glass.model,
            Vec3::new(0.3, 0., -2.),
            Mobility::Moving,
        ),
        (
            bump_glass.model,
            Vec3::new(-0.5, 0., -2.5),
            Mobility::Moving,
        ),
    ] {
        scene
            .add_instance(
                &device,
                &queue,
                InstanceState {
                    pose: Mat4::from_translation(at),
                    ..InstanceState::new(model)
                },
                mobility,
            )
            .unwrap();
    }
    scene
        .set_dynamic_gi_volume(
            &device,
            Some(DynamicGiVolume {
                origin: Vec3::splat(-3.),
                spacing: Vec3::splat(2.),
                probes: [4, 4, 4],
            }),
        )
        .unwrap();
    scene
        .set_irradiance_volume(
            &device,
            &queue,
            Some(IrradianceVolume {
                origin: Vec3::splat(-4.),
                cell_size: Vec3::ONE,
                cells: [8, 8, 8],
            }),
        )
        .unwrap();
    // A masked and a blended sprite batch in view, whose pipelines bind the
    // unlit group 0, their batch's group and a blended batch's order.
    let sprite_image = scene
        .add_sprite_image(
            &device,
            &queue,
            &Image::Rgba8(image::RgbaImage::from_pixel(2, 2, image::Rgba([255; 4]))),
        )
        .unwrap();
    for (blend, x) in [
        (crate::sprite::SpriteBlend::Masked { cutoff: 0.5 }, -0.4),
        (crate::sprite::SpriteBlend::Blended, 0.4),
    ] {
        let batch = scene
            .add_sprite_batch(&crate::sprite::SpriteBatch {
                blend,
                ..crate::sprite::SpriteBatch::new(sprite_image)
            })
            .unwrap();
        let sprite = crate::sprite::Sprite::new(Vec3::new(x, 0.2, -1.5), glam::Vec2::splat(0.3));
        scene
            .set_sprites(&device, &queue, batch, &[sprite])
            .unwrap();
    }
    let mut input = FrameInput::new(Camera {
        view: Mat4::IDENTITY,
        projection: crate::perspective(1., 1., 0.1),
        eye: Vec3::ZERO,
    });
    input.atmosphere = true;
    input.fog.density = 0.05;
    input.environment = crate::FrameEnvironment::Sky(crate::Sky::default());
    input.directional_lights[0] = Some(crate::DirectionalLight {
        direction: Vec3::new(0.3, -1., -0.4),
        shadow: Some(crate::DirectionalShadow::DEFAULT),
        ..crate::DirectionalLight::default()
    });
    let output = crate::view::targets::target(&device, "floor", size, gbuffer::COLOR);
    for _ in 0..2 {
        let mut encoder = device.create_command_encoder(&Default::default());
        renderer.render(
            &device,
            &queue,
            &mut encoder,
            &mut scene,
            &input,
            &settings,
            &output,
            None,
        );
        queue.submit([encoder.finish()]);
        renderer.finish_frame(&mut scene);
    }
    if let Some(error) = pollster::block_on(validation.pop()) {
        panic!("{error}");
    }
    (
        renderer.binding_tier(),
        renderer.dynamic_gi_in_effect(&settings, &scene),
    )
}

/// A transmissive, dispersive blended receiver of screen-space reflections
/// carrying the maps a material takes, each its own image: base,
/// metallic-roughness with its occlusion packed, emission, anisotropy,
/// clearcoat, clearcoat roughness and normal, iridescence and its
/// thickness, transmission, thickness, sheen colour and roughness, diffuse
/// transmission and its colour, and a normal map where `normal`, else a
/// bump map, which a material takes only without a normal map; on a cube
/// with authored tangents, which anisotropy needs. On the Extended tier the
/// transparent stage copies the frame for it, binding its copy.
fn glass(normal: bool) -> Asset {
    let mut glass = test_support::cube();
    for vertex in &mut glass.meshes[0].vertices {
        let tangent = Vec3::from_array(vertex.normal).any_orthonormal_vector();
        vertex.tangent = tangent.extend(1.).to_array();
    }
    glass.images = (0..17)
        .map(|_| Image::Rgba8(image::RgbaImage::from_pixel(4, 4, image::Rgba([200; 4]))))
        .collect();
    let material = &mut glass.materials[0];
    material.alpha = AlphaMode::Blend {
        receives_screen_space_reflections: true,
        keeps_specular: false,
    };
    material.base[3] = 0.5;
    material.base_texture = Some(crate::asset::MaterialTexture::new(0));
    material.mr_texture = Some(crate::asset::MaterialTexture::new(1));
    material.occlusion_texture = Some(crate::asset::MaterialTexture::new(1));
    material.emissive_texture = Some(crate::asset::MaterialTexture::new(2));
    if normal {
        material.normal_texture = Some(crate::asset::MaterialTexture::new(3));
    } else {
        material.bump_texture = Some(crate::asset::MaterialTexture::new(4));
        material.bump_scale = 1.;
    }
    material.anisotropy_texture = Some(crate::asset::MaterialTexture::new(5));
    material.anisotropy_strength = 0.5;
    material.clearcoat = 0.5;
    material.clearcoat_texture = Some(crate::asset::MaterialTexture::new(6));
    material.coat_roughness_texture = Some(crate::asset::MaterialTexture::new(7));
    material.coat_normal_texture = Some(crate::asset::MaterialTexture::new(8));
    material.iridescence = 0.5;
    material.iridescence_texture = Some(crate::asset::MaterialTexture::new(9));
    material.iridescence_thickness_texture = Some(crate::asset::MaterialTexture::new(10));
    material.metallic = 0.;
    material.transmission = 1.;
    material.transmission_texture = Some(crate::asset::MaterialTexture::new(11));
    material.thickness = 0.2;
    material.thickness_texture = Some(crate::asset::MaterialTexture::new(12));
    material.attenuation_distance = 1.;
    material.attenuation_color = [0.8, 0.9, 1.];
    material.dispersion = 0.5;
    material.sheen_color = [0.5; 3];
    material.sheen_color_texture = Some(crate::asset::MaterialTexture::new(13));
    material.sheen_roughness_texture = Some(crate::asset::MaterialTexture::new(14));
    material.diffuse_transmission = 0.5;
    material.diffuse_transmission_texture = Some(crate::asset::MaterialTexture::new(15));
    material.diffuse_transmission_color_texture = Some(crate::asset::MaterialTexture::new(14));
    glass
}
