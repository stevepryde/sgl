//! The retained ray-data capability at resource and raster GPU boundaries.
use crate::asset::{Asset, CompressedFormat, CompressedImage, CpuMesh, Image, Vertex};
use crate::deformation::{Influence, MeshDeformation};
use crate::settings::{Antialiasing, DynamicGiQuality, Settings};
use crate::{
    Camera, DynamicGiVolume, FrameInput, InstanceState, Mobility, PreparedModel,
    PreparedModelOptions, RayData, Renderer, Scene, SceneError, SceneOptions, test_support,
};
use glam::{Mat4, Vec3};

fn scene(device: &wgpu::Device, queue: &wgpu::Queue, ray_data: RayData) -> Scene {
    Scene::with_options(device, queue, SceneOptions { ray_data })
}

fn empty_model(ray_data: RayData) -> PreparedModel {
    PreparedModel::with_options(Vec::new(), PreparedModelOptions { ray_data }).unwrap()
}

// Both mismatch directions must fail before edits, GPU allocations, uploads
// or content replacement, including when the target already has instances.
#[test]
fn prepared_mode_mismatch_preserves_content_and_resources() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    for (mode, other) in [
        (RayData::Enabled, RayData::Disabled),
        (RayData::Disabled, RayData::Enabled),
    ] {
        let mut scene = scene(&device, &queue, mode);
        let (ids, instance) =
            test_support::add_static(&device, &queue, &mut scene, test_support::cube());
        let before = scene.diagnostic_resources();
        let edits = scene.edits;
        let model_geometry = scene.drawn_model(ids.model).geometry;
        let counters = crate::counters::snapshot();
        assert!(matches!(
            scene.add_model(&device, &queue, empty_model(other)),
            Err(SceneError::RayDataMismatch)
        ));
        assert!(matches!(
            scene.set_model(&device, &queue, ids.model, empty_model(other)),
            Err(SceneError::RayDataMismatch)
        ));
        assert_eq!(scene.diagnostic_resources(), before);
        assert_eq!(scene.edits, edits);
        assert_eq!(scene.drawn_model(ids.model).geometry, model_geometry);
        assert_eq!(scene.instance(instance).unwrap().model, ids.model);
        assert_eq!(scene.models.slots.iter().count(), 1);
        let delta = crate::counters::snapshot().since(&counters);
        assert_eq!(delta.buffers_created, 0);
        assert_eq!(delta.uploaded_bytes(), 0);
    }
}

fn textured_grid() -> Asset {
    let mut asset = test_support::cube();
    let side = 32u32;
    let vertices = (0..=side)
        .flat_map(|y| {
            (0..=side).map(move |x| Vertex {
                position: [x as f32, y as f32, -2.],
                normal: [0., 0., 1.],
                uv: [x as f32 / side as f32, y as f32 / side as f32],
                color: [1.; 4],
                lightmap_bounds: [0., 0., 1., 1.],
                ..bytemuck::Zeroable::zeroed()
            })
        })
        .collect();
    let indices = (0..side)
        .flat_map(|y| {
            (0..side).flat_map(move |x| {
                let i = y * (side + 1) + x;
                [i, i + 1, i + side + 2, i, i + side + 2, i + side + 1]
            })
        })
        .collect();
    asset.meshes = vec![CpuMesh {
        vertices,
        indices,
        material: 0,
        deformation: Default::default(),
    }];
    asset.materials[0].base_texture = Some(0);
    asset.images = vec![Image::Rgba8(image::RgbaImage::from_pixel(
        256,
        256,
        image::Rgba([180, 90, 30, 255]),
    ))];
    asset
}

// A triangle-rich textured fixture must save real source/instance allocation,
// not only skip dispatch. Raster geometry and candidate records still exist.
#[test]
fn disabled_scene_omits_image_bvh_and_instance_source_allocation() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut enabled = scene(&device, &queue, RayData::Enabled);
    let mut disabled = scene(&device, &queue, RayData::Disabled);
    let asset = textured_grid();
    let (enabled_ids, _) = test_support::add_static(&device, &queue, &mut enabled, asset.clone());
    let counters = crate::counters::snapshot();
    let (disabled_ids, _) = test_support::add_static(&device, &queue, &mut disabled, asset);
    for (scene, ids) in [(&mut enabled, &enabled_ids), (&mut disabled, &disabled_ids)] {
        for _ in 0..64 {
            scene
                .add_instance(
                    &device,
                    &queue,
                    InstanceState {
                        model: ids.model,
                        pose: Mat4::IDENTITY,
                        visible: true,
                        capture_visible: true,
                    },
                    Mobility::Moving,
                )
                .unwrap();
        }
    }
    let on = enabled.diagnostic_resources();
    let off = disabled.diagnostic_resources();
    assert!(
        off.ray_source < on.ray_source / 2,
        "source allocation {off:?} vs {on:?}"
    );
    assert!(off.ray_source_live < on.ray_source_live / 2);
    assert!(off.instance_entries < on.instance_entries / 32);
    assert_eq!(off.geometry_live, on.geometry_live);
    assert_eq!(off.draw_candidates, on.draw_candidates);
    assert_eq!(disabled.drawn_model(disabled_ids.model).ray.bvh_root, 0);
    assert!(enabled.drawn_model(enabled_ids.model).ray.bvh_root > 0);
    assert!(disabled.materials.texture(0).ray.is_empty());
    assert!(!enabled.materials.texture(0).ray.is_empty());
    let delta = crate::counters::snapshot().since(&counters);
    assert_eq!(delta.step(crate::counters::BuildStep::RayBvh).calls, 0);
    eprintln!(
        "ray data source allocated/live: enabled {}/{}, disabled {}/{} bytes",
        on.ray_source, on.ray_source_live, off.ray_source, off.ray_source_live
    );
}

// Companion requirements are consumer-specific and fail before resource
// growth, even if a batch/asset has a valid image before its missing one.
#[test]
fn missing_compressed_companions_are_refused_before_retained_mutation() {
    let Some((device, queue)) = test_support::device_choosing(|adapter| {
        crate::graphics_device::features(adapter)
            | crate::graphics_device::texture_compression_features(adapter)
    }) else {
        return;
    };
    for format in [CompressedFormat::Etc2Rgba8, CompressedFormat::Astc4x4] {
        let image = Image::Compressed(CompressedImage {
            format,
            width: 4,
            height: 4,
            levels: vec![vec![0; 16]],
            decoded_level_0: None,
        });
        let mut enabled = scene(&device, &queue, RayData::Enabled);
        let before = enabled.diagnostic_resources();
        let edits = enabled.edits;
        assert!(matches!(
            enabled.add_decal_image(image.clone()),
            Err(SceneError::MissingDecodedImage)
        ));
        assert_eq!(enabled.edits, edits);
        if !device
            .features()
            .contains(crate::graphics_device::compressed_image_feature(format))
        {
            eprintln!("unsupported material fixture: {format:?}");
            continue;
        }
        let mut asset = test_support::cube();
        asset.images = vec![Image::Rgba8(image::RgbaImage::new(4, 4)), image.clone()];
        asset.materials[0].base_texture = Some(0);
        let mut second = asset.materials[0].clone();
        second.base_texture = Some(1);
        asset.materials.push(second);
        let counters = crate::counters::snapshot();
        assert!(matches!(
            enabled.add_materials(&device, &queue, &asset.materials, &asset.images),
            Err(SceneError::MissingDecodedImage)
        ));
        assert!(matches!(
            enabled.add_asset(&device, &queue, asset.clone()),
            Err(SceneError::MissingDecodedImage)
        ));
        assert_eq!(enabled.diagnostic_resources(), before);
        assert_eq!(enabled.edits, edits);
        assert_eq!(enabled.materials.slots.iter().count(), 0);
        let delta = crate::counters::snapshot().since(&counters);
        assert_eq!(delta.buffers_created, 0);
        assert_eq!(delta.uploaded_bytes(), 0);
        let mut disabled = scene(&device, &queue, RayData::Disabled);
        disabled.add_asset(&device, &queue, asset).unwrap();
        assert!(disabled.materials.texture(1).ray.is_empty());
        assert!(matches!(
            disabled.add_decal_image(image),
            Err(SceneError::MissingDecodedImage)
        ));
    }
}

fn masked_skin() -> Asset {
    let mut asset = test_support::masked(test_support::cube(), 0.5);
    let corners = [(-1., -1.), (1., -1.), (1., 1.), (-1., 1.)];
    asset.meshes = vec![CpuMesh {
        vertices: corners
            .into_iter()
            .map(|(x, y)| Vertex {
                position: [x, y, 0.6],
                normal: [0., 0., 1.],
                uv: [(x + 1.) * 0.5, (1. - y) * 0.5],
                color: [1.; 4],
                lightmap_bounds: [0., 0., 1., 1.],
                tangent: [1., 0., 0., 1.],
                ..bytemuck::Zeroable::zeroed()
            })
            .collect(),
        indices: vec![0, 1, 2, 0, 2, 3],
        material: 0,
        deformation: MeshDeformation {
            influences: vec![
                Influence {
                    joints: [0; 4],
                    weights: [1., 0., 0., 0.]
                };
                4
            ],
            morph_targets: Vec::new(),
        },
    }];
    asset
}

// One renderer switches enabled -> disabled -> enabled, including histories.
// The independent oracle is a masked skinned plane at clip depth 0.4: the
// left half is cut out, and the right half has that depth in raster/shadows.
#[test]
fn both_modes_preserve_masked_deformed_raster_and_shadow_depth() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let validation = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let settings = Settings {
        antialiasing: Antialiasing::Off,
        hardware_ray_tracing: true,
        ray_traced_shadows: true,
        dynamic_gi: DynamicGiQuality::High,
        ..Settings::default()
    };
    let requested = settings;
    let size = [16; 2];
    let mut renderer = Renderer::for_test(&device, &queue, size, &settings);
    let output = crate::view::targets::target(
        &device,
        "ray data frames",
        size,
        crate::shading::gbuffer::COLOR,
    );
    let shadow = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("ray data shadow oracle"),
        size: wgpu::Extent3d {
            width: 16,
            height: 16,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Depth32Float,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let shadow_view = shadow.create_view(&Default::default());
    let mut input = FrameInput::new(Camera {
        view: Mat4::IDENTITY,
        projection: Mat4::IDENTITY,
        eye: Vec3::Z * 2.,
    });
    input.directional_lights[0] = Some(crate::DirectionalLight {
        direction: Vec3::NEG_Z,
        shadow: None,
        ..Default::default()
    });
    let mut scenes: Vec<_> = [RayData::Enabled, RayData::Disabled]
        .into_iter()
        .map(|mode| {
            let mut scene = scene(&device, &queue, mode);
            let model = scene
                .add_asset(&device, &queue, masked_skin())
                .unwrap()
                .model;
            let instance = scene
                .add_instance(
                    &device,
                    &queue,
                    InstanceState {
                        model,
                        pose: Mat4::IDENTITY,
                        visible: true,
                        capture_visible: true,
                    },
                    Mobility::Moving,
                )
                .unwrap();
            scene
                .set_instance_deformation(
                    &queue,
                    instance,
                    &[Mat4::from_translation(Vec3::Z * -0.2)],
                    &[],
                )
                .unwrap();
            scene
                .set_dynamic_gi_volume(
                    &device,
                    Some(DynamicGiVolume {
                        origin: Vec3::splat(-1.),
                        spacing: Vec3::splat(2.),
                        probes: [2; 3],
                    }),
                )
                .unwrap();
            scene
        })
        .collect();
    let mut observations = Vec::new();
    for index in [0, 1, 0] {
        let scene = &mut scenes[index];
        let before = crate::counters::snapshot();
        let mut encoder = device.create_command_encoder(&Default::default());
        renderer.render(
            &device,
            &queue,
            &mut encoder,
            scene,
            &input,
            &settings,
            &output,
            None,
        );
        queue.submit([encoder.finish()]);
        renderer.finish_frame(scene);
        let raster = test_support::read(&device, &queue, renderer.targets().depth.texture(), 4);
        let mut frame = renderer.prepare_test_frame(&device, &queue, scene, &input, &settings);
        renderer.set_test_cascade((&device, &queue), scene, &frame, Mat4::IDENTITY);
        let mut encoder = device.create_command_encoder(&Default::default());
        renderer.encode_test_shadows(
            &device,
            &queue,
            &mut encoder,
            scene,
            &frame,
            Some(&shadow_view),
        );
        renderer.encode_test_opaque(&device, &queue, &mut encoder, scene, &mut frame, false);
        queue.submit([encoder.finish()]);
        let shadow = test_support::read(&device, &queue, &shadow, 4);
        for bytes in [&raster, &shadow] {
            let depth: &[f32] = bytemuck::cast_slice(bytes);
            for y in 0..16 {
                // Away from the mask's filtered boundary and outer edges.
                for x in [2usize, 3, 12, 13] {
                    let expected = if x < 8 { 0. } else { 0.4 };
                    assert!(
                        (depth[y * 16 + x] - expected).abs() < 1e-5,
                        "mode {:?}, ({x},{y}): {} vs {expected}",
                        scene.ray_data(),
                        depth[y * 16 + x]
                    );
                }
            }
        }
        if index == 1 {
            assert_eq!(
                renderer.dynamic_gi_in_effect(&settings, scene),
                DynamicGiQuality::Off
            );
            assert!(!renderer.ray_tracing_in_effect(&settings, scene));
            assert!(!renderer.ray_traced_shadows_in_effect(&settings, scene));
            assert!(
                renderer
                    .ray_tracing_error()
                    .unwrap()
                    .contains("RayData::Disabled")
            );
            let delta = crate::counters::snapshot().since(&before);
            assert_eq!(
                delta
                    .step(crate::counters::BuildStep::StaticInstanceBvh)
                    .calls,
                0
            );
            assert_eq!(
                delta
                    .step(crate::counters::BuildStep::MovingInstanceBvh)
                    .calls,
                0
            );
            assert_eq!(
                delta.blas_builds + delta.deformed_blas_builds + delta.tlas_builds,
                0
            );
        }
        observations.push((raster, shadow));
    }
    assert_eq!(observations[0], observations[1]);
    assert_eq!(observations[0], observations[2]);
    assert_eq!(settings, requested);
    if let Some(error) = pollster::block_on(validation.pop()) {
        panic!("{error}")
    }
}
