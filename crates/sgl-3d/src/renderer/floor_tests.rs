//! A device at S3D-1's floor runs every pipeline a frame builds.
use crate::renderer::Renderer;
use crate::settings::{self, DynamicGiQuality, Settings};
use crate::shading::gbuffer;
use crate::{
    AlphaMode, Camera, DynamicGiVolume, FrameInput, InstanceState, IrradianceVolume, Mobility,
    Scene, test_support,
};
use glam::{Mat4, Vec3};

/// S3D-1's floor of sampled textures per shader stage.
const SAMPLED_TEXTURES: u32 = 21;

// Plausible defect: a change binds another sampled texture to a stage (lit
// group 0, a material, a stage's own group) past the floor S3D-1 states, so
// a device that offers exactly that floor fails to create a pipeline though
// the spec, the README and the docs promise it runs SGL3D. The oracle is
// S3D-1's floor and wgpu's validation of every pipeline layout against the
// device's limits: frames that build the heaviest pipelines (blended
// receivers of screen-space reflections, world-space reflections, dynamic
// GI's rays over the irradiance volume, ambient occlusion, fog and TAA) on a
// device with the adapter's limits but that floor raise no validation
// error.
#[test]
fn a_device_at_the_sampled_texture_floor_runs_every_pipeline() {
    let Some(adapter) = test_support::adapter() else {
        return;
    };
    let mut limits = crate::graphics_device::limits(&adapter);
    limits.max_sampled_textures_per_shader_stage = SAMPLED_TEXTURES;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_features: crate::graphics_device::features(&adapter),
        required_limits: limits,
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
        atmosphere: true,
        ..Settings::default()
    };
    let size = [32, 32];
    let mut renderer = Renderer::for_test(&device, &queue, size, &settings);
    let mut scene = Scene::new(&device, &queue);
    let wall = scene
        .add_asset(&device, &queue, test_support::cube())
        .unwrap();
    let mut glass = test_support::cube();
    glass.materials[0].alpha = AlphaMode::Blend {
        receives_screen_space_reflections: true,
    };
    glass.materials[0].base[3] = 0.5;
    let glass = scene.add_asset(&device, &queue, glass).unwrap();
    for (model, z, mobility) in [
        (wall.model, -4., Mobility::Static),
        (glass.model, -2., Mobility::Moving),
    ] {
        scene
            .add_instance(
                &device,
                &queue,
                InstanceState {
                    pose: Mat4::from_translation(Vec3::new(0.3, 0., z)),
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
    let mut input = FrameInput::new(Camera {
        view: Mat4::IDENTITY,
        projection: crate::perspective(1., 1., 0.1),
        eye: Vec3::ZERO,
    });
    input.atmosphere = true;
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
}
