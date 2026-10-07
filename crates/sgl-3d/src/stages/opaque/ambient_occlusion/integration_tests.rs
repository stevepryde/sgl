//! The real frame path must attenuate only missing ambient visibility. These
//! checks catch whole-image AO, baked-visibility duplication, stale AO after
//! disabling it, and completion skipping or misapplying the occlusion. The
//! independent hemisphere oracle lives in the algorithm tests.
use crate::renderer::Renderer;
use crate::settings::Settings;
use crate::static_lighting::IrradianceAtlas;
use crate::{
    test_support::{self, half, read},
    *,
};
use glam::Vec3;
use glam::camera;
use settings::AmbientOcclusionQuality as Quality;

const SIZE: [u32; 2] = [65, 49];

struct Snapshot {
    color: Vec<u8>,
    ambient: Vec<u8>,
    composite: Vec<u8>,
    geometry: Vec<Vec<u8>>,
}

#[allow(clippy::too_many_arguments)]
fn render(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    scene: &mut Scene,
    renderer: &mut Renderer,
    settings: &Settings,
    input: &FrameInput,
    output: &wgpu::TextureView,
    quality: Quality,
) -> Snapshot {
    let settings = Settings {
        ambient_occlusion: quality,
        ..*settings
    };
    let mut encoder = device.create_command_encoder(&Default::default());
    renderer.render(
        device,
        queue,
        &mut encoder,
        scene,
        input,
        &settings,
        output,
        None,
    );
    queue.submit([encoder.finish()]);
    renderer.finish_frame(scene);
    let t = renderer.targets();
    Snapshot {
        color: read(device, queue, t.color.texture(), 8),
        ambient: read(device, queue, t.ambient.texture(), 8),
        composite: read(device, queue, t.composite.texture(), 8),
        geometry: [(&t.depth, 4), (&t.normal, 8), (&t.material, 8), (&t.f0, 4)]
            .map(|(view, bpp)| read(device, queue, view.texture(), bpp))
            .into(),
    }
}

fn brightness(bytes: &[u8]) -> f32 {
    bytes
        .chunks_exact(8)
        .map(|p| (0..3).map(|c| half(&p[c * 2..])).sum::<f32>())
        .sum()
}

/// Makes `material` Lambertian (KHR_materials_specular's specular 0): under
/// the hemisphere fill alone its lit colour is then all ambient diffuse,
/// which ambient occlusion weights linearly, with none of the specular's
/// multiple scattering, which it weights as specular.
fn lambertian(queue: &wgpu::Queue, scene: &mut Scene, material: MaterialId) {
    let mut values = scene.material(material).unwrap();
    values.specular = 0.;
    scene.set_material(queue, material, values).unwrap();
}

/// A grey box standing on a grey floor, seen from above at an angle: the
/// scene, the box's material and a frame with an environment of constant
/// linear radiance 0.25, a black backdrop and no light on. With `occlusion`,
/// the material packs an occlusion map in its metallic-roughness image
/// (ORM), red `occlusion` throughout, with full green and blue, so its
/// roughness and metallic factors hold.
fn box_on_floor(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    occlusion: Option<u8>,
) -> (Scene, MaterialId, FrameInput) {
    let mut world = test_support::cube();
    let mut floor = world.meshes[0].clone();
    for v in &mut floor.vertices {
        let p = Vec3::from_array(v.position) * Vec3::new(6., 0.1, 6.) - Vec3::Y * 0.05;
        v.position = p.to_array();
    }
    for v in &mut world.meshes[0].vertices {
        v.position[1] += 0.5;
    }
    world.meshes.push(floor);
    for mesh in &mut world.meshes {
        for v in &mut mesh.vertices {
            v.lightmap_uv = [0.5; 2];
        }
    }
    world.materials[0].base = [0.5, 0.5, 0.5, 1.];
    world.materials[0].metallic = 0.;
    world.materials[0].roughness = 0.7;
    if let Some(red) = occlusion {
        world.images = vec![asset::Image::Rgba8(image::RgbaImage::from_pixel(
            4,
            4,
            image::Rgba([red, 255, 255, 255]),
        ))];
        world.materials[0].mr_texture = Some(0);
        world.materials[0].occlusion_texture = Some(0);
    }
    let mut scene = Scene::new(device, queue);
    let (world, _) = test_support::add_static(device, queue, &mut scene, world);
    let environment = scene
        .add_environment(
            device,
            queue,
            &test_support::environment([0, 0, 0, 255], &[0, 0x34]),
        )
        .unwrap();
    let eye = Vec3::new(2.4, 2., 3.3);
    let mut input = FrameInput::new(Camera {
        eye,
        view: camera::rh::view::look_at_mat4(eye, Vec3::new(0., 0.3, 0.), Vec3::Y),
        projection: perspective(55f32.to_radians(), SIZE[0] as f32 / SIZE[1] as f32, 0.1),
    });
    input.backdrop = Backdrop::Color([0.; 3]);
    input.reflection_environment = EnvironmentLight {
        yaw: 0.,
        intensity: 0.,
    };
    input.diffuse_environment.intensity = 0.;
    input.camera_cut = true;
    input.environment = Some(environment);
    input.ambient_occlusion_radius = 0.5;
    (scene, world.materials[0], input)
}

#[test]
fn gtao_primary_lighting_ownership_and_disable_restore() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let (mut scene, material, mut input) = box_on_floor(&device, &queue, None);
    let settings = Settings {
        scene_resolution: settings::SceneResolution::Full,
        atmosphere: false,
        bloom: settings::Bloom::Off,
        ..Settings::default()
    };
    let mut renderer = Renderer::new(
        &device,
        &queue,
        wgpu::TextureFormat::Rgba8Unorm,
        SIZE,
        1.,
        &settings,
    )
    .unwrap();
    let output = view::targets::target(
        &device,
        "AO ownership output",
        SIZE,
        wgpu::TextureFormat::Rgba8Unorm,
    );
    let irradiance = |color| IrradianceAtlas {
        size: [1, 1],
        irradiance: vec![color],
        back_irradiance: vec![color],
        directionality: vec![],
        back_directionality: vec![],
    };
    for layer in [
        "hemisphere",
        "sky diffuse",
        "direct",
        "emissive",
        "fixed",
        "metal multi",
    ] {
        input.diffuse_environment.intensity =
            f32::from(matches!(layer, "sky diffuse" | "metal multi"));
        input.hemisphere_light = HemisphereLight {
            sky_color: [1.; 3],
            ground_color: [1.; 3],
            intensity: f32::from(layer == "hemisphere"),
        };
        input.directional_lights[0] = Some(DirectionalLight {
            direction: Vec3::new(-0.3, -1., -0.4),
            color: [1., 0.5, 0.25],
            illuminance: f32::from(layer == "direct"),
            shadow: None,
            ..Default::default()
        });
        let mut values = scene.material(material).unwrap();
        values.metallic = f32::from(layer == "metal multi");
        values.emission = if layer == "emissive" {
            [0.25, 0.5, 1.]
        } else {
            [0.; 3]
        };
        scene.set_material(&queue, material, values).unwrap();
        scene
            .set_static_irradiance_atlas(
                &device,
                &queue,
                &irradiance(if layer == "fixed" {
                    [1., 0., 0.]
                } else {
                    [0.; 3]
                }),
            )
            .unwrap();
        let mut frame = |quality| {
            render(
                &device,
                &queue,
                &mut scene,
                &mut renderer,
                &settings,
                &input,
                &output,
                quality,
            )
        };
        let off = frame(Quality::Off);
        let on = frame(Quality::Ultra);
        let restored = frame(Quality::Off);
        assert!(
            brightness(&off.color) > 1.,
            "{layer} fixture must carry measurable radiance"
        );
        assert_eq!(
            off.geometry, on.geometry,
            "AO must not change material or depth inputs ({layer})"
        );
        // Opaque colour and its ambient diffuse stay unoccluded: source
        // completion occludes the ambient diffuse into the composite.
        assert_eq!(off.color, on.color, "AO changed opaque HDR ({layer})");
        assert_eq!(
            off.ambient, on.ambient,
            "AO changed the recorded ambient diffuse ({layer})"
        );
        assert_eq!(
            off.color, restored.color,
            "Off failed to restore opaque HDR ({layer})"
        );
        assert_eq!(
            off.composite, restored.composite,
            "Off failed to restore complete HDR ({layer})"
        );
        if layer == "metal multi" {
            // The metal's specular multiple scattering of the sky's diffuse
            // light takes the frame's ambient occlusion too, as specular: it
            // darkens, never brightens.
            assert_ne!(
                off.composite, on.composite,
                "AO left the metal's multiple scattering whole"
            );
            for (before, after) in off
                .composite
                .chunks_exact(8)
                .zip(on.composite.chunks_exact(8))
            {
                assert!(
                    half(after) <= half(before) + 0.001,
                    "AO created energy in {layer}"
                );
            }
        } else if matches!(layer, "hemisphere" | "sky diffuse") {
            let affected = off
                .composite
                .chunks_exact(8)
                .zip(on.composite.chunks_exact(8))
                .filter(|(before, after)| half(before) - half(after) > 0.01)
                .count();
            assert!(
                affected >= 8,
                "AO had no measurable effect near the box/floor contact: {layer}, {affected}"
            );
            for (before, after) in off
                .composite
                .chunks_exact(8)
                .zip(on.composite.chunks_exact(8))
            {
                assert!(
                    half(after) <= half(before) + 0.001,
                    "AO created diffuse energy in {layer}"
                );
            }
        } else {
            assert_eq!(
                off.composite, on.composite,
                "AO altered non-diffuse completed HDR ({layer})"
            );
        }
    }
}

// Defects: source completion skips the occlusion of ambient diffuse; applies
// another texel's, channel's or frame's visibility; or occludes in a frame
// without ambient occlusion (with the visibility an earlier frame left).
// Under the hemisphere fill alone a receiver's lit colour is all ambient
// diffuse, so its occluded radiance is its unoccluded radiance times its
// ambient visibility. The oracle is XeGTAO's own visibility read back from
// the frame, applied to the frame's unoccluded colour; a frame without
// ambient occlusion after it must complete its colour unchanged.
#[test]
fn completion_occludes_ambient_diffuse_by_the_frames_visibility() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let (mut scene, material, mut input) = box_on_floor(&device, &queue, None);
    lambertian(&queue, &mut scene, material);
    input.hemisphere_light = HemisphereLight {
        sky_color: [1., 0.8, 0.6],
        ground_color: [0.6, 0.8, 1.],
        intensity: 1.,
    };
    input.baked_lighting = false;
    let settings = Settings {
        scene_resolution: settings::SceneResolution::Full,
        antialiasing: settings::Antialiasing::Off,
        screen_space_reflections: settings::ScreenSpaceReflections::Off,
        atmosphere: false,
        bloom: settings::Bloom::Off,
        ..Settings::default()
    };
    let mut renderer = Renderer::for_test(&device, &queue, SIZE, &settings);
    let output = view::targets::target(&device, "AO output", SIZE, shading::gbuffer::COLOR);
    let frame = |scene: &mut Scene, renderer: &mut Renderer, quality| {
        render(
            &device, &queue, scene, renderer, &settings, &input, &output, quality,
        )
    };
    let on = frame(&mut scene, &mut renderer, Quality::Medium);
    let visibility: Vec<f32> = read(
        &device,
        &queue,
        renderer
            .diagnostic_target(diagnostics::DiagnosticTarget::AmbientOcclusion)
            .expect("ambient occlusion ran")
            .texture(),
        4,
    )
    .chunks_exact(4)
    .map(|texel| u32::from_le_bytes(texel.try_into().unwrap()) as f32 / 255.)
    .collect();
    let depth: Vec<f32> = on.geometry[0]
        .chunks_exact(4)
        .map(|texel| f32::from_le_bytes(texel.try_into().unwrap()))
        .collect();
    let rgb = |bytes: &[u8], pixel: usize| -> [f32; 3] {
        std::array::from_fn(|c| half(&bytes[pixel * 8 + c * 2..]))
    };
    let mut occluded = 0;
    for (pixel, &visibility) in visibility.iter().enumerate() {
        let (color, composite) = (rgb(&on.color, pixel), rgb(&on.composite, pixel));
        // Where nothing was drawn, completion keeps the backdrop.
        let visibility = if depth[pixel] > 0. { visibility } else { 1. };
        if visibility < 0.9 && color.iter().any(|&c| c > 0.01) {
            occluded += 1;
        }
        for c in 0..3 {
            let expected = color[c] * visibility;
            assert!(
                (composite[c] - expected).abs() <= expected / 512. + 1e-5,
                "pixel {pixel} channel {c}: composite {} for colour {} at visibility {visibility}",
                composite[c],
                color[c]
            );
        }
    }
    assert!(
        occluded >= 8,
        "the box must occlude the floor at its contact: {occluded} pixels"
    );
    let off = frame(&mut scene, &mut renderer, Quality::Off);
    assert_eq!(off.color, on.color, "AO changed opaque colour");
    for pixel in 0..visibility.len() {
        assert_eq!(
            rgb(&off.composite, pixel),
            rgb(&off.color, pixel),
            "pixel {pixel}: completion occluded a frame without ambient occlusion"
        );
    }
}

// Defects: a radius outside `FrameInput::ambient_occlusion_radius`'s
// documented 0.01..=10000 m turns ambient occlusion off, which only
// `Settings::ambient_occlusion` does (AR-5), or reaches XeGTAO, whose
// falloff and sample spacing divide by it (a radius of -1 there darkens
// this frame's mean visibility by half). The oracle is the frame at the
// nearer end of that range.
#[test]
fn a_radius_outside_its_range_occludes_as_its_nearer_end() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let (mut scene, _, mut input) = box_on_floor(&device, &queue, None);
    let settings = Settings {
        scene_resolution: settings::SceneResolution::Full,
        antialiasing: settings::Antialiasing::Off,
        screen_space_reflections: settings::ScreenSpaceReflections::Off,
        atmosphere: false,
        bloom: settings::Bloom::Off,
        ..Settings::default()
    };
    let mut renderer = Renderer::for_test(&device, &queue, SIZE, &settings);
    let output = view::targets::target(&device, "AO output", SIZE, shading::gbuffer::COLOR);
    let mut visibility = |radius: f32| {
        input.ambient_occlusion_radius = radius;
        render(
            &device,
            &queue,
            &mut scene,
            &mut renderer,
            &settings,
            &input,
            &output,
            Quality::Medium,
        );
        let target = renderer
            .diagnostic_target(diagnostics::DiagnosticTarget::AmbientOcclusion)
            .unwrap_or_else(|| panic!("radius {radius} turned ambient occlusion off"));
        read(&device, &queue, target.texture(), 4)
    };
    let least = visibility(0.01);
    for radius in [0., -1., f32::NAN] {
        assert!(
            visibility(radius) == least,
            "radius {radius} occluded otherwise than 0.01 m"
        );
    }
    assert!(
        visibility(f32::INFINITY) == visibility(10000.),
        "an infinite radius occluded otherwise than 10000 m"
    );
}

// Defects: a material's occlusion map, packed in the red channel of its
// metallic-roughness image, is not applied, is read from another channel,
// ignores its strength, applies only while ambient occlusion runs or only
// while it does not, multiplies the frame's ambient occlusion rather than
// taking the lesser of the two, or is left out of a blended surface's
// forward shading; or the G-buffer's F0 code misreads it in either half,
// receivers that take baked scene lights (128-254, without baked lighting)
// and those an atlas chart lights (1-127, with a black atlas). Under the
// hemisphere fill alone a receiver's lit colour is all ambient diffuse, so
// its completed radiance is its unoccluded colour times its visibility:
// glTF 2.0's lerp(1, red, strength), recorded to 1/126, taken with XeGTAO's
// visibility read back from the frame as Filament and Bevy take them, the
// lesser. A blended surface takes no ambient occlusion: its radiance is the
// unoccluded one times glTF's alone.
#[test]
fn a_packed_occlusion_map_occludes_ambient_diffuse() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    const RED: u8 = 64;
    let (mut scene, material, mut input) = box_on_floor(&device, &queue, Some(RED));
    lambertian(&queue, &mut scene, material);
    input.hemisphere_light = HemisphereLight {
        sky_color: [1., 0.8, 0.6],
        ground_color: [0.6, 0.8, 1.],
        intensity: 1.,
    };
    let settings = Settings {
        scene_resolution: settings::SceneResolution::Full,
        antialiasing: settings::Antialiasing::Off,
        screen_space_reflections: settings::ScreenSpaceReflections::Off,
        atmosphere: false,
        bloom: settings::Bloom::Off,
        ..Settings::default()
    };
    let mut renderer = Renderer::for_test(&device, &queue, SIZE, &settings);
    let output = view::targets::target(&device, "AO output", SIZE, shading::gbuffer::COLOR);
    let rgb = |bytes: &[u8], pixel: usize| -> [f32; 3] {
        std::array::from_fn(|c| half(&bytes[pixel * 8 + c * 2..]))
    };
    let occlusion = |strength: f32| 1. + strength * (f32::from(RED) / 255. - 1.);
    for charted in [false, true] {
        input.baked_lighting = charted;
        if charted {
            let black = IrradianceAtlas {
                size: [1, 1],
                irradiance: vec![[0.; 3]],
                back_irradiance: vec![[0.; 3]],
                directionality: vec![],
                back_directionality: vec![],
            };
            scene
                .set_static_irradiance_atlas(&device, &queue, &black)
                .unwrap();
        }
        for strength in [1., 0.5] {
            let mut values = scene.material(material).unwrap();
            values.occlusion_strength = strength;
            scene.set_material(&queue, material, values).unwrap();
            for quality in [Quality::Off, Quality::Medium] {
                let frame = render(
                    &device,
                    &queue,
                    &mut scene,
                    &mut renderer,
                    &settings,
                    &input,
                    &output,
                    quality,
                );
                let pixels = SIZE[0] as usize * SIZE[1] as usize;
                let ambient: Vec<f32> = match renderer
                    .diagnostic_target(diagnostics::DiagnosticTarget::AmbientOcclusion)
                {
                    Some(target) => read(&device, &queue, target.texture(), 4)
                        .chunks_exact(4)
                        .map(|texel| u32::from_le_bytes(texel.try_into().unwrap()) as f32 / 255.)
                        .collect(),
                    None => vec![1.; pixels],
                };
                let depth: Vec<f32> = frame.geometry[0]
                    .chunks_exact(4)
                    .map(|texel| f32::from_le_bytes(texel.try_into().unwrap()))
                    .collect();
                let mut lit = 0;
                let mut both = 0;
                for pixel in (0..pixels).filter(|&pixel| depth[pixel] > 0.) {
                    // The fixture covers the half of the F0 code it names.
                    let code = frame.geometry[3][pixel * 4 + 3];
                    assert_eq!(code >= 128, !charted, "pixel {pixel}: F0 code {code}");
                    let (color, composite) =
                        (rgb(&frame.color, pixel), rgb(&frame.composite, pixel));
                    let visibility = occlusion(strength).min(ambient[pixel]);
                    lit += 1;
                    if ambient[pixel] < 0.9 {
                        both += 1;
                    }
                    for c in 0..3 {
                        let expected = color[c] * visibility;
                        let tolerance = color[c] * 0.5 / 126. + expected / 512. + 1e-5;
                        assert!(
                            (composite[c] - expected).abs() <= tolerance,
                            "charted {charted}, {quality:?}, strength {strength}, pixel {pixel} channel {c}: composite {} for colour {} at visibility {visibility}",
                            composite[c],
                            color[c]
                        );
                    }
                }
                assert!(
                    lit > 100,
                    "the box and floor must cover the frame: {lit} pixels"
                );
                if quality == Quality::Medium {
                    assert!(
                        both >= 8,
                        "the box must occlude the floor at its contact: {both} pixels"
                    );
                }
            }
        }
    }
    // Forward shading: the surfaces blended, at strength 0 and then 1.
    let mut blended = |strength: f32| {
        let mut values = scene.material(material).unwrap();
        values.occlusion_strength = strength;
        values.alpha = AlphaMode::Blend {
            receives_screen_space_reflections: false,
        };
        scene.set_material(&queue, material, values).unwrap();
        render(
            &device,
            &queue,
            &mut scene,
            &mut renderer,
            &settings,
            &input,
            &output,
            Quality::Off,
        )
        .composite
    };
    let unoccluded = blended(0.);
    let occluded = blended(1.);
    assert!(
        brightness(&unoccluded) > 1.,
        "the blended surfaces must be lit"
    );
    for pixel in 0..SIZE[0] as usize * SIZE[1] as usize {
        let (before, after) = (rgb(&unoccluded, pixel), rgb(&occluded, pixel));
        for c in 0..3 {
            let expected = before[c] * occlusion(1.);
            assert!(
                (after[c] - expected).abs() <= before[c] / 256. + 1e-5,
                "blended pixel {pixel} channel {c}: {} for {} unoccluded",
                after[c],
                before[c]
            );
        }
    }
}

// Defects: source completion leaves a material's occlusion off the
// environment specular it adds, occluding only the ambient diffuse. A
// smooth white metal in a uniform environment, with no diffuse light, is
// lit by its environment specular alone; Lagarde's specular occlusion at
// visibility 0 is 0 for any lobe (Lagarde and de Rousiers 2014), so a
// packed occlusion map of red 0 at full strength leaves it black, without
// the frame's ambient occlusion, while at strength 0 it reflects.
#[test]
fn a_packed_occlusion_map_occludes_environment_specular() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let (mut scene, material, mut input) = box_on_floor(&device, &queue, Some(0));
    input.reflection_environment = EnvironmentLight {
        yaw: 0.,
        intensity: 1.,
    };
    let settings = Settings {
        scene_resolution: settings::SceneResolution::Full,
        antialiasing: settings::Antialiasing::Off,
        screen_space_reflections: settings::ScreenSpaceReflections::Off,
        atmosphere: false,
        bloom: settings::Bloom::Off,
        ..Settings::default()
    };
    let mut renderer = Renderer::for_test(&device, &queue, SIZE, &settings);
    let output = view::targets::target(&device, "AO output", SIZE, shading::gbuffer::COLOR);
    let mut composite = |strength: f32| -> Vec<f32> {
        let mut values = scene.material(material).unwrap();
        values.base = [1.; 4];
        values.metallic = 1.;
        values.roughness = 0.2;
        values.occlusion_strength = strength;
        scene.set_material(&queue, material, values).unwrap();
        let frame = render(
            &device,
            &queue,
            &mut scene,
            &mut renderer,
            &settings,
            &input,
            &output,
            Quality::Off,
        );
        frame
            .composite
            .chunks_exact(8)
            .flat_map(|texel| (0..3).map(|c| half(&texel[c * 2..])))
            .collect()
    };
    let reflected = composite(0.);
    assert!(
        reflected.iter().filter(|&&value| value > 0.01).count() > 300,
        "the metal must reflect its environment"
    );
    let occluded = composite(1.);
    let brightest = occluded.iter().copied().fold(0., f32::max);
    assert!(
        brightest == 0.,
        "a fully occluded metal reflected {brightest}"
    );
}

// Defects: a material's occlusion leaves a lightmapped receiver's baked
// diffuse whole, occludes it only in some views, or lets the frame's
// ambient occlusion occlude it too. The oracle is glTF 2.0's occlusion of
// indirect light, lerp(1, red, strength), which three.js and Godot apply to
// a light map: lit by a uniform lightmap alone, a box and floor packing an
// occlusion map of red 64 complete to 64/255 of their radiance at strength
// 0, with ambient occlusion off and on, for a bake holds its own occlusion.
#[test]
fn a_packed_occlusion_map_occludes_lightmapped_diffuse() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    const RED: u8 = 64;
    let (mut scene, material, mut input) = box_on_floor(&device, &queue, Some(RED));
    scene
        .set_lightmap(
            &device,
            &queue,
            &crate::static_lighting::Lightmap {
                size: [2, 2],
                uv_scale_offset: [1., 1., 0., 0.],
                irradiance: vec![[0.3, 0.2, 0.1]; 4],
                directionality: vec![],
            },
            &[material],
        )
        .unwrap();
    input.baked_lighting = true;
    let settings = Settings {
        scene_resolution: settings::SceneResolution::Full,
        antialiasing: settings::Antialiasing::Off,
        screen_space_reflections: settings::ScreenSpaceReflections::Off,
        atmosphere: false,
        bloom: settings::Bloom::Off,
        ..Settings::default()
    };
    let mut renderer = Renderer::for_test(&device, &queue, SIZE, &settings);
    let output = view::targets::target(&device, "AO output", SIZE, shading::gbuffer::COLOR);
    let mut composite = |strength: f32, quality| -> Vec<f32> {
        let mut values = scene.material(material).unwrap();
        values.occlusion_strength = strength;
        scene.set_material(&queue, material, values).unwrap();
        render(
            &device,
            &queue,
            &mut scene,
            &mut renderer,
            &settings,
            &input,
            &output,
            quality,
        )
        .composite
        .chunks_exact(8)
        .flat_map(|texel| (0..3).map(|c| half(&texel[c * 2..])))
        .collect()
    };
    for quality in [Quality::Off, Quality::Medium] {
        let unoccluded = composite(0., quality);
        assert!(
            unoccluded.iter().filter(|&&value| value > 0.01).count() > 300,
            "the lightmap must light the box and floor"
        );
        let occluded = composite(1., quality);
        for (channel, (&before, &after)) in unoccluded.iter().zip(&occluded).enumerate() {
            let expected = before * f32::from(RED) / 255.;
            assert!(
                (after - expected).abs() <= before / 256. + 1e-5,
                "{quality:?}, channel {channel}: {after} for {before} unoccluded"
            );
        }
    }
}

/// The visibilities `occluded_metal` renders a material of red 0 at: its
/// strengths 0, 0.5 and 1 give glTF's lerp(1, 0, strength) of 1, 0.5 and 0.
const METAL_STRENGTHS: [f32; 3] = [0., 0.5, 1.];

/// A rough white metal (roughness 0.8), whose indirect light is all
/// specular multiple scattering, packing an occlusion map of red 0, lit by
/// what `light` gives `input` and `scene` alone under a black sky: its
/// composite at each of METAL_STRENGTHS, at ambient occlusion `quality`.
fn occluded_metal(
    light: impl Fn(&wgpu::Device, &wgpu::Queue, &mut Scene, MaterialId, &mut FrameInput),
    quality: Quality,
) -> Option<[Vec<f32>; 3]> {
    let (device, queue) = test_support::device()?;
    let (mut scene, material, mut input) = box_on_floor(&device, &queue, Some(0));
    input.hemisphere_light = HemisphereLight {
        sky_color: [0.; 3],
        ground_color: [0.; 3],
        intensity: 0.,
    };
    input.baked_lighting = false;
    light(&device, &queue, &mut scene, material, &mut input);
    let settings = Settings {
        scene_resolution: settings::SceneResolution::Full,
        antialiasing: settings::Antialiasing::Off,
        screen_space_reflections: settings::ScreenSpaceReflections::Off,
        atmosphere: false,
        bloom: settings::Bloom::Off,
        ..Settings::default()
    };
    let mut renderer = Renderer::for_test(&device, &queue, SIZE, &settings);
    let output = view::targets::target(&device, "AO output", SIZE, shading::gbuffer::COLOR);
    Some(METAL_STRENGTHS.map(|strength| {
        let mut values = scene.material(material).unwrap();
        values.base = [1.; 4];
        values.metallic = 1.;
        values.roughness = 0.8;
        values.occlusion_strength = strength;
        scene.set_material(&queue, material, values).unwrap();
        render(
            &device,
            &queue,
            &mut scene,
            &mut renderer,
            &settings,
            &input,
            &output,
            quality,
        )
        .composite
        .chunks_exact(8)
        .flat_map(|texel| (0..3).map(|c| half(&texel[c * 2..])))
        .collect()
    }))
}

/// Checks `occluded_metal`'s composites against specular occlusion: at
/// visibility 0.5 the metal keeps more than 0.7 of its light, where its
/// diffuse visibility would keep 0.5 and Filament's specular occlusion with
/// GTAO's multi-bounce on F0 1 keeps about 0.9, and at visibility 0 it is
/// black, as Lagarde's specular occlusion and its multi-bounce are 0 there.
fn assert_specular_occlusion(name: &str, [full, half_visible, none]: &[Vec<f32>; 3]) {
    let lit = full.iter().filter(|&&value| value > 0.01).count();
    assert!(
        lit > 300,
        "{name}: the metal must reflect, {lit} values lit"
    );
    for (index, ((&full, &half_visible), &none)) in
        full.iter().zip(half_visible).zip(none).enumerate()
    {
        if full > 0.01 {
            let kept = half_visible / full;
            assert!(
                kept > 0.7 && kept <= 1. + 1. / 256.,
                "{name}, value {index}: keeps {kept} of {full} at visibility 0.5"
            );
        }
        assert!(
            none <= full / 512. + 1e-5,
            "{name}, value {index}: {none} of {full} at visibility 0"
        );
    }
}

// Defects: source completion occludes the camera's specular multiple
// scattering by the diffuse visibility, or leaves it whole (it holds no
// ambient diffuse share on a metal), or the lit pass records it nowhere.
// The oracle is Filament's occlusion of energy-compensated specular by its
// specular occlusion and multi-bounce (Lagarde and de Rousiers 2014;
// Jimenez et al. 2016): a rough white metal under the hemisphere fill alone
// keeps more than 0.7 of its light at visibility 0.5 (0.5 by the diffuse
// visibility) and none at 0, with and without the frame's ambient
// occlusion, which takes the lesser of the two.
#[test]
fn a_packed_occlusion_map_occludes_multiple_scattered_specular() {
    let fill = |_: &wgpu::Device,
                _: &wgpu::Queue,
                _: &mut Scene,
                _: MaterialId,
                input: &mut FrameInput| {
        input.hemisphere_light = HemisphereLight {
            sky_color: [1.; 3],
            ground_color: [1.; 3],
            intensity: 1.,
        };
    };
    let Some(off) = occluded_metal(fill, Quality::Off) else {
        return;
    };
    assert_specular_occlusion("ambient occlusion off", &off);
    // With the frame's ambient occlusion the metal is still black where its
    // material hides all of its ambient light.
    let medium = occluded_metal(fill, Quality::Medium).unwrap();
    for (index, (&full, &none)) in medium[0].iter().zip(&medium[2]).enumerate() {
        assert!(
            none <= full / 512. + 1e-5,
            "ambient occlusion Medium, value {index}: {none} of {full} at visibility 0"
        );
    }
}

// Defects: a lightmapped metal's specular multiple scattering takes its
// material's occlusion linearly, as its diffuse share does, or not at all.
// The oracle is as for the frame's ambient light (assert_specular_occlusion):
// a rough white metal lit by a uniform lightmap alone.
#[test]
fn a_packed_occlusion_map_occludes_lightmapped_multiple_scattering() {
    let lightmap = |device: &wgpu::Device,
                    queue: &wgpu::Queue,
                    scene: &mut Scene,
                    material: MaterialId,
                    input: &mut FrameInput| {
        scene
            .set_lightmap(
                device,
                queue,
                &crate::static_lighting::Lightmap {
                    size: [2, 2],
                    uv_scale_offset: [1., 1., 0., 0.],
                    irradiance: vec![[0.3; 3]; 4],
                    directionality: vec![],
                },
                &[material],
            )
            .unwrap();
        input.baked_lighting = true;
    };
    let Some(composites) = occluded_metal(lightmap, Quality::Off) else {
        return;
    };
    assert_specular_occlusion("lightmap", &composites);
}
