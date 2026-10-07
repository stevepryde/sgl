//! A device at S3D-1's floor runs every pipeline a frame builds on the Basic
//! binding tier, and a device at the Extended tier's threshold on that tier.
use crate::asset::{Asset, Image};
use crate::graphics_device::BindingTier;
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
/// The sampled textures per shader stage from which S3D-1 gives a device
/// the Extended binding tier.
const EXTENDED_SAMPLED_TEXTURES: u32 = 48;
/// S3D-1's floor of storage buffers per shader stage, and wgpu's default of
/// storage textures, which the stages that write the most bind (the
/// world-space reflection classification, the ray-traced shadow trace,
/// ambient occlusion's depth prefilter).
const STORAGE_BUFFERS: u32 = 8;
const STORAGE_TEXTURES: u32 = 4;

// Plausible defects: a change binds another sampled texture, storage buffer
// or storage texture to a stage (lit group 0 and group 1 hold the storage
// buffers' floor already; a material; a stage's own group, such as a ray
// list passed as a storage buffer beside them) past the floor S3D-1 states,
// so a device that offers exactly that floor fails to create a pipeline
// though the spec, the README and the docs promise it runs SGL3D; or the
// device takes the wrong binding tier, or composes an Extended binding
// below 48. The oracle is S3D-1's floor and wgpu's validation of every
// pipeline layout against the device's limits: frames that build the
// heaviest pipelines (blended receivers of screen-space reflections that
// between them carry every material map, world-space reflections, dynamic GI's rays over
// the irradiance volume, ambient occlusion, fog and TAA) on a device with
// the adapter's limits but that floor raise no validation error, and the
// device reports the Basic tier.
#[test]
fn a_device_at_the_binding_floor_runs_every_pipeline() {
    if let Some(tier) = heaviest_frame(SAMPLED_TEXTURES) {
        assert_eq!(tier, BindingTier::Basic);
    }
}

// Plausible defects: the tier is chosen from the wrong limit, with `>` for
// `>=`, or from a threshold above 48, so a device S3D-1 gives every binding
// takes the Basic tier; or the tier does not reach one of the scene and the
// renderer, which then build group 2 to different layouts, so the draw of a
// material binds a group its pipeline's layout does not take. The oracle is
// S3D-1's threshold and wgpu's validation: on a device at exactly 48
// sampled textures a stage the same frame raises no validation error and
// the device reports the Extended tier.
#[test]
fn a_device_at_the_extended_threshold_binds_the_extended_tier() {
    if let Some(tier) = heaviest_frame(EXTENDED_SAMPLED_TEXTURES) {
        assert_eq!(tier, BindingTier::Extended);
    }
}

/// The binding tier of a device with the adapter's limits but
/// `sampled_textures` sampled textures and the floor's storage buffers and
/// textures a stage, after it draws two frames of the heaviest pipelines
/// with no validation error; none without a GPU, or where the adapter
/// offers fewer sampled textures.
fn heaviest_frame(sampled_textures: u32) -> Option<BindingTier> {
    let adapter = test_support::adapter()?;
    let mut limits = crate::graphics_device::limits(&adapter);
    if limits.max_sampled_textures_per_shader_stage < sampled_textures {
        eprintln!("skipping: the adapter binds fewer than {sampled_textures} sampled textures");
        return None;
    }
    limits.max_sampled_textures_per_shader_stage = sampled_textures;
    limits.max_storage_buffers_per_shader_stage = STORAGE_BUFFERS;
    limits.max_storage_textures_per_shader_stage = STORAGE_TEXTURES;
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
    let normal_glass = scene.add_asset(&device, &queue, glass(true)).unwrap();
    let bump_glass = scene.add_asset(&device, &queue, glass(false)).unwrap();
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
    Some(renderer.binding_tier())
}

/// A blended receiver of screen-space reflections carrying the maps a
/// material takes, each its own image: base, metallic-roughness with its
/// occlusion packed, emission, anisotropy, and a normal map where `normal`,
/// else a bump map, which a material takes only without a normal map; on a
/// cube with authored tangents, which anisotropy needs.
fn glass(normal: bool) -> Asset {
    let mut glass = test_support::cube();
    for vertex in &mut glass.meshes[0].vertices {
        let tangent = Vec3::from_array(vertex.normal).any_orthonormal_vector();
        vertex.tangent = tangent.extend(1.).to_array();
    }
    glass.images = (0..6)
        .map(|_| Image::Rgba8(image::RgbaImage::from_pixel(4, 4, image::Rgba([200; 4]))))
        .collect();
    let material = &mut glass.materials[0];
    material.alpha = AlphaMode::Blend {
        receives_screen_space_reflections: true,
    };
    material.base[3] = 0.5;
    material.base_texture = Some(0);
    material.mr_texture = Some(1);
    material.occlusion_texture = Some(1);
    material.emissive_texture = Some(2);
    if normal {
        material.normal_texture = Some(3);
    } else {
        material.bump_texture = Some(4);
        material.bump_scale = 1.;
    }
    material.anisotropy_texture = Some(5);
    material.anisotropy_strength = 0.5;
    glass
}
