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
use glam::{Mat4, Vec3};
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

/// A grey box standing on a grey floor, seen from above at an angle: the
/// scene, the box's material and a frame with an environment of constant
/// linear radiance 0.25, a black backdrop and no light on.
fn box_on_floor(device: &wgpu::Device, queue: &wgpu::Queue) -> (Scene, MaterialId, FrameInput) {
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
        view: Mat4::look_at_rh(eye, Vec3::new(0., 0.3, 0.), Vec3::Y),
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
    let (mut scene, material, mut input) = box_on_floor(&device, &queue);
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
        if matches!(layer, "hemisphere" | "sky diffuse") {
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
    let (mut scene, _, mut input) = box_on_floor(&device, &queue);
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
