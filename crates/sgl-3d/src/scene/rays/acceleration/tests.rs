//! The acceleration structures on a device that traces rays in hardware,
//! built and bound as a frame builds them. Each test reports itself
//! unsupported, never passed, where the adapter has no ray queries.
use super::RayTracingStats;
use crate::asset::{Asset, CpuMesh, Vertex};
use crate::shading::{SCENE_RAYS_QUERY_OPAQUE, bind, compose};
use crate::test_support;
use crate::{InstanceState, Mobility, Scene};
use glam::{Mat4, Vec3};

/// A mesh of `triangles` separate triangles about `centre`.
fn triangles(centre: Vec3, triangles: u32) -> CpuMesh {
    let vertices = (0..triangles * 3)
        .map(|vertex| {
            let corner = [[-0.5, -0.5], [0.5, -0.5], [0., 0.5]][vertex as usize % 3];
            let layer = (vertex / 3) as f32 * 0.01;
            Vertex {
                tangent: [0.; 4],
                lightmap_bounds: [0., 0., 1., 1.],
                lightmap_uv: [0.; 2],
                position: (centre + Vec3::new(corner[0], corner[1], layer)).to_array(),
                normal: [0., 0., 1.],
                uv: [0.; 2],
                color: [1.; 4],
            }
        })
        .collect();
    CpuMesh {
        vertices,
        indices: (0..triangles * 3).collect(),
        material: 0,
        deformation: Default::default(),
    }
}

/// An opaque asset of `meshes`.
fn asset(meshes: Vec<CpuMesh>) -> Asset {
    let mut asset = test_support::cube();
    asset.meshes = meshes;
    asset
}

/// Adds a capture-visible `mobility` instance of `model` at `pose`.
fn place(
    gpu: (&wgpu::Device, &wgpu::Queue),
    scene: &mut Scene,
    model: crate::ModelId,
    pose: Mat4,
    mobility: Mobility,
) -> crate::InstanceId {
    scene
        .add_instance(
            gpu.0,
            gpu.1,
            InstanceState {
                pose,
                ..InstanceState::new(model)
            },
            mobility,
        )
        .unwrap()
}

/// Builds `scene`'s acceleration structures for a frame seen from `eye`,
/// records them, submits and finishes the frame, as a renderer's frame
/// does, and returns what the frame held.
fn build(
    (device, queue): (&wgpu::Device, &wgpu::Queue),
    scene: &mut Scene,
    eye: Vec3,
) -> RayTracingStats {
    let stats = scene
        .prepare_acceleration_structures(device, queue, eye)
        .expect("the device holds a TLAS");
    let mut encoder = device.create_command_encoder(&Default::default());
    scene.encode_acceleration_structures(&mut encoder);
    queue.submit([encoder.finish()]);
    scene.finish_frame();
    stats
}

/// Binds `scene`'s TLAS at the entry a tracing pass binds it at, in a
/// compute pass that dispatches once: wgpu then requires it, and every
/// BLAS it names, built.
fn bind_tlas((device, queue): (&wgpu::Device, &wgpu::Queue), scene: &Scene) {
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("TLAS test"),
        entries: &[bind::tlas_entry()],
    });
    let source =
        compose(&[&SCENE_RAYS_QUERY_OPAQUE]) + "@compute @workgroup_size(1) fn bind_tlas() {}\n";
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("TLAS test"),
        source: wgpu::ShaderSource::Wgsl(source.into()),
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("TLAS test"),
        layout: Some(
            &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: None,
                bind_group_layouts: &[None, None, None, Some(&layout)],
                immediate_size: 0,
            }),
        ),
        module: &module,
        entry_point: Some("bind_tlas"),
        compilation_options: Default::default(),
        cache: None,
    });
    let tlas = scene
        .acceleration_structures()
        .expect("structures were built")
        .tlas()
        .0;
    let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("TLAS test"),
        layout: &layout,
        entries: &[wgpu::BindGroupEntry {
            binding: bind::hardware::SCENE_TLAS,
            resource: tlas.as_binding(),
        }],
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_compute_pass(&Default::default());
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(3, &group, &[]);
        pass.dispatch_workgroups(1, 1, 1);
    }
    queue.submit([encoder.finish()]);
}

// The smallest build: one BLAS of one triangle and one TLAS instance.
// Plausible defects: the TLAS left unbuilt, or naming a BLAS never built
// (bookkeeping committed for work never recorded); the ray source without
// `BLAS_INPUT`; a first vertex or index the source does not hold; a custom
// index past 24 bits; or a TLAS a pass cannot bind at the shared entry.
// wgpu's validation refuses each when the frame is submitted or the TLAS
// bound, the independent oracle; and the structures hold the scene's one
// triangle.
#[test]
fn one_triangle_builds_and_binds() {
    let Some((device, queue)) = test_support::ray_tracing_device(|limits| limits) else {
        return;
    };
    let gpu = (&device, &queue);
    let mut scene = Scene::new(&device, &queue);
    let model = scene
        .add_asset(&device, &queue, asset(vec![triangles(Vec3::ZERO, 1)]))
        .unwrap()
        .model;
    place(gpu, &mut scene, model, Mat4::IDENTITY, Mobility::Static);
    let validation = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let stats = build(gpu, &mut scene, Vec3::Z * 5.);
    bind_tlas(gpu, &scene);
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    let error = pollster::block_on(validation.pop());
    assert!(error.is_none(), "{error:?}");
    assert_eq!(
        stats,
        RayTracingStats {
            hardware: 1,
            portable: 0,
            left_out: 0,
        }
    );
    let resources = scene.diagnostic_resources();
    assert_eq!((resources.blases, resources.blas_triangles), (1, 1));
}

/// `asset` with its material's alpha mode `alpha`.
fn with_alpha(mut asset: Asset, alpha: crate::AlphaMode) -> Asset {
    asset.materials[0].alpha = alpha;
    asset
}

/// The entry indices the TLAS holds, in order, and their masks.
fn held(scene: &Scene) -> Vec<(u32, u8)> {
    scene
        .acceleration_structures()
        .expect("structures were built")
        .tlas()
        .0
        .get()
        .iter()
        .map_while(|instance| instance.as_ref())
        .map(|instance| (instance.custom_data, instance.mask))
        .collect()
}

// Plausible defects: a model past the device's BLAS limits, an instance
// past its TLAS capacity, a mesh's dangling index or a mesh without a whole
// triangle reaching wgpu (whose validation refuses the frame), the nearest instances left out rather than the farthest, a
// predicate instance (a masked mesh) or one rays pass through (blended
// only) given to the TLAS, one not capture-visible counted, or a material's
// alpha mode edited without its users' ray class following. The oracles
// are wgpu's validation and the architecture's rules over the test's
// content, on a device whose limits the test lowered: four primitives a
// BLAS, two instances a TLAS.
#[test]
fn what_the_device_cannot_hold_is_left_out_and_counted() {
    let Some((device, queue)) = test_support::ray_tracing_device(|limits| wgpu::Limits {
        max_blas_primitive_count: 4,
        max_tlas_instance_count: 2,
        ..limits
    }) else {
        return;
    };
    let gpu = (&device, &queue);
    let mut scene = Scene::new(&device, &queue);
    // One triangle and an index past it, which the scene accepts and the
    // BLAS must not take (wgpu refuses a count of indices not a multiple of
    // three), and a mesh with no whole triangle, whose geometry holds none
    // and keeps the next mesh's geometry index its own.
    let mut dangling = triangles(Vec3::ZERO, 1);
    dangling.indices.push(0);
    let mut empty = triangles(Vec3::X, 1);
    empty.indices.truncate(2);
    let small = scene
        .add_asset(&device, &queue, asset(vec![dangling, empty]))
        .unwrap();
    // Five triangles: one more than a BLAS holds.
    let large = scene
        .add_asset(&device, &queue, asset(vec![triangles(Vec3::ZERO, 5)]))
        .unwrap()
        .model;
    let masked = scene
        .add_asset(
            &device,
            &queue,
            test_support::masked(test_support::cube(), 0.5),
        )
        .unwrap()
        .model;
    let blended = scene
        .add_asset(
            &device,
            &queue,
            with_alpha(
                test_support::cube(),
                crate::AlphaMode::Blend {
                    receives_screen_space_reflections: false,
                },
            ),
        )
        .unwrap()
        .model;
    let at = |z: f32| Mat4::from_translation(Vec3::Z * z);
    let near = place(gpu, &mut scene, small.model, at(0.), Mobility::Static);
    let middle = place(gpu, &mut scene, small.model, at(-10.), Mobility::Moving);
    place(gpu, &mut scene, small.model, at(-20.), Mobility::Static);
    place(gpu, &mut scene, large, at(0.), Mobility::Static);
    place(gpu, &mut scene, masked, at(0.), Mobility::Static);
    place(gpu, &mut scene, blended, at(0.), Mobility::Static);
    let hidden = place(gpu, &mut scene, small.model, at(1.), Mobility::Static);
    scene
        .set_instance(
            &queue,
            hidden,
            InstanceState {
                capture_visible: false,
                ..*scene.instance(hidden).unwrap()
            },
        )
        .unwrap();
    let eye = Vec3::Z * 5.;
    let validation = device.push_error_scope(wgpu::ErrorFilter::Validation);
    // The TLAS holds the two nearest instances of the small model; the
    // farthest and the large model's are left out, and with the masked
    // model's the portable BVHs cover them.
    let stats = build(gpu, &mut scene, eye);
    assert_eq!(
        stats,
        RayTracingStats {
            hardware: 2,
            portable: 3,
            left_out: 2,
        }
    );
    let index = |id: crate::InstanceId| crate::content::identity::Identity::index(id) as u32;
    let mut kept = held(&scene);
    kept.sort_unstable();
    assert_eq!(
        kept,
        [
            (index(near), super::MASK_STATIC),
            (index(middle), super::MASK_MOVING)
        ]
    );
    bind_tlas(gpu, &scene);
    // Masked, the small model's instances are predicate instances: the
    // portable BVHs cover all three and its BLAS is dropped.
    let opaque = scene.material(small.materials[0]).unwrap();
    let mask = crate::SurfaceMaterial {
        alpha: crate::AlphaMode::Mask { cutoff: 0.5 },
        ..opaque
    };
    scene
        .set_material(&queue, small.materials[0], mask)
        .unwrap();
    assert_eq!(
        build(gpu, &mut scene, eye),
        RayTracingStats {
            hardware: 0,
            portable: 5,
            left_out: 1,
        }
    );
    assert_eq!(scene.diagnostic_resources().blases, 0);
    // Opaque again, it is built again.
    scene
        .set_material(&queue, small.materials[0], opaque)
        .unwrap();
    assert_eq!(build(gpu, &mut scene, eye), stats);
    bind_tlas(gpu, &scene);
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    let error = pollster::block_on(validation.pop());
    assert!(error.is_none(), "{error:?}");
}

/// One frame of `scene` through `renderer` into `output`, submitted and
/// finished unless `abandoned`.
fn frame(
    (device, queue): (&wgpu::Device, &wgpu::Queue),
    (renderer, scene): (&mut crate::Renderer, &mut Scene),
    settings: &crate::settings::Settings,
    output: &wgpu::TextureView,
    abandoned: bool,
) -> RayTracingStats {
    let eye = Vec3::new(0., 1., 6.);
    let input = crate::FrameInput::new(crate::Camera {
        view: glam::camera::rh::view::look_at_mat4(eye, Vec3::ZERO, Vec3::Y),
        projection: crate::perspective(1., 1., 0.1),
        eye,
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    renderer.render(
        device,
        queue,
        &mut encoder,
        scene,
        &input,
        settings,
        output,
        None,
    );
    if !abandoned {
        queue.submit([encoder.finish()]);
        renderer.finish_frame(scene);
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    }
    renderer.ray_tracing_stats()
}

// The structures through the renderer's frames, on frames that trace
// world-space rays. Plausible defects: builds committed for an abandoned
// frame (wgpu's validation then refuses the next frame's TLAS, which names
// a BLAS never built), or an abandoned frame's builds recorded by a later
// frame that traces nothing, against the ray source as it then is; a
// deforming instance's BLAS not rebuilt when its deformation changes, or
// rebuilt when it does not; a model's BLAS built again every frame, or
// compacted never or more than once (Bevy's queue compacts each once);
// the structures kept with the setting off, or not built again when it
// is turned on. The oracles are wgpu's validation and the architecture's
// rules, counted through `counters`. Whether the builds follow the deform
// pass shows only in what rays hit, which the hardware trace's oracle test
// checks.
#[test]
fn frames_build_the_structures_after_their_deformations() {
    let Some((device, queue)) = test_support::ray_tracing_device(|limits| limits) else {
        return;
    };
    let gpu = (&device, &queue);
    let validation = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let mut scene = Scene::new(&device, &queue);
    let opaque = scene
        .add_asset(&device, &queue, asset(vec![triangles(Vec3::ZERO, 2)]))
        .unwrap()
        .model;
    place(gpu, &mut scene, opaque, Mat4::IDENTITY, Mobility::Static);
    let masked = scene
        .add_asset(
            &device,
            &queue,
            test_support::masked(test_support::cube(), 0.5),
        )
        .unwrap()
        .model;
    place(
        gpu,
        &mut scene,
        masked,
        Mat4::from_translation(Vec3::X * 3.),
        Mobility::Static,
    );
    // One skinned triangle, whole on its one joint.
    let mut skinned = triangles(Vec3::ZERO, 1);
    skinned.deformation.influences = vec![
        crate::deformation::Influence {
            joints: [0; 4],
            weights: [1., 0., 0., 0.],
        };
        3
    ];
    let skinned = scene
        .add_asset(&device, &queue, asset(vec![skinned]))
        .unwrap()
        .model;
    let deforming = place(
        gpu,
        &mut scene,
        skinned,
        Mat4::from_translation(Vec3::X * -3.),
        Mobility::Moving,
    );
    let pose = |scene: &mut Scene, lift: f32| {
        scene
            .set_instance_deformation(
                &queue,
                deforming,
                &[Mat4::from_translation(Vec3::Y * lift)],
                &[],
            )
            .unwrap();
    };
    let mut settings = crate::settings::Settings {
        screen_space_reflections: crate::settings::ScreenSpaceReflections::Half,
        world_space_reflections: true,
        hardware_ray_tracing: true,
        ..Default::default()
    };
    let size = [64, 64];
    let mut renderer = crate::Renderer::for_test(&device, &queue, size, &settings);
    let output = crate::view::targets::target(
        &device,
        "acceleration structure frames",
        size,
        crate::shading::gbuffer::COLOR,
    );
    let frame = |renderer: &mut crate::Renderer,
                 scene: &mut Scene,
                 settings: &crate::settings::Settings,
                 abandoned: bool| {
        let before = crate::counters::snapshot();
        let stats = frame(gpu, (renderer, scene), settings, &output, abandoned);
        (stats, crate::counters::snapshot().since(&before))
    };
    let counts = |counted: &crate::counters::Counters| {
        [
            counted.blas_builds,
            counted.deformed_blas_builds,
            counted.tlas_builds,
        ]
    };
    let held = RayTracingStats {
        hardware: 2,
        portable: 1,
        left_out: 0,
    };
    // The first frame builds the model's BLAS, the deforming instance's
    // and the TLAS.
    pose(&mut scene, 0.5);
    let (stats, counted) = frame(&mut renderer, &mut scene, &settings, false);
    assert_eq!(stats, held);
    assert_eq!(counts(&counted), [1, 1, 1]);
    // The model's BLAS is compacted once, in whichever later frame finds it
    // ready.
    let mut compactions = 0;
    // An abandoned frame commits nothing, and a frame that traces no rays
    // records nothing of it: the next traced frame records its
    // deformation's build again.
    pose(&mut scene, 0.25);
    let (_, abandoned) = frame(&mut renderer, &mut scene, &settings, true);
    assert_eq!(counts(&abandoned), [0, 1, 1]);
    compactions += abandoned.blas_compactions;
    let untraced = crate::settings::Settings {
        world_space_reflections: false,
        ..settings
    };
    let (stats, counted) = frame(&mut renderer, &mut scene, &untraced, false);
    assert_eq!(stats, RayTracingStats::default());
    assert_eq!(counts(&counted), [0, 0, 0]);
    compactions += counted.blas_compactions;
    let (stats, counted) = frame(&mut renderer, &mut scene, &settings, false);
    assert_eq!(stats, held);
    assert_eq!(counts(&counted), [0, 1, 1]);
    compactions += counted.blas_compactions;
    // A deformation unchanged since the last submitted frame keeps its
    // BLAS.
    for _ in 0..6 {
        let (stats, counted) = frame(&mut renderer, &mut scene, &settings, false);
        assert_eq!(stats, held);
        assert_eq!(counts(&counted), [0, 0, 1]);
        compactions += counted.blas_compactions;
    }
    assert_eq!(compactions, 1);
    // Off frees the structures; on builds them again.
    settings.hardware_ray_tracing = false;
    let (stats, counted) = frame(&mut renderer, &mut scene, &settings, false);
    assert_eq!(stats, RayTracingStats::default());
    assert_eq!(counts(&counted), [0, 0, 0]);
    assert!(scene.acceleration_structures().is_none());
    assert_eq!(scene.diagnostic_resources().blases, 0);
    settings.hardware_ray_tracing = true;
    let (stats, counted) = frame(&mut renderer, &mut scene, &settings, false);
    assert_eq!(stats, held);
    assert_eq!(counts(&counted), [1, 1, 1]);
    bind_tlas(gpu, &scene);
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    let error = pollster::block_on(validation.pop());
    assert!(error.is_none(), "{error:?}");
}

// What a game is told of the hardware path, on a device without ray queries
// and on one with them. Plausible defects: the hardware path reported in
// effect on a device that has no ray queries, which then traces the
// portable BVHs, or no reason given; or a reason given where it runs. The
// oracle is the device each frame ran on: one requested without the
// feature, and one with it.
#[test]
fn the_renderer_reports_whether_the_hardware_path_traces() {
    let settings = crate::settings::Settings {
        screen_space_reflections: crate::settings::ScreenSpaceReflections::Half,
        world_space_reflections: true,
        hardware_ray_tracing: true,
        ..Default::default()
    };
    let devices = [
        test_support::device(),
        test_support::ray_tracing_device(|limits| limits),
    ];
    for (ray_queries, device) in [false, true].into_iter().zip(devices) {
        let Some((device, queue)) = device else {
            return;
        };
        let gpu = (&device, &queue);
        let mut scene = Scene::new(&device, &queue);
        let model = scene
            .add_asset(&device, &queue, asset(vec![triangles(Vec3::ZERO, 1)]))
            .unwrap()
            .model;
        place(gpu, &mut scene, model, Mat4::IDENTITY, Mobility::Static);
        let size = [16, 16];
        let mut renderer = crate::Renderer::for_test(&device, &queue, size, &settings);
        let output = crate::view::targets::target(
            &device,
            "ray tracing reports",
            size,
            crate::shading::gbuffer::COLOR,
        );
        let stats = frame(gpu, (&mut renderer, &mut scene), &settings, &output, false);
        assert_eq!(renderer.ray_tracing_in_effect(&settings), ray_queries);
        assert_eq!(renderer.ray_tracing_error().is_none(), ray_queries);
        assert_eq!(stats.hardware, u32::from(ray_queries));
        let off = crate::settings::Settings {
            hardware_ray_tracing: false,
            ..settings
        };
        frame(gpu, (&mut renderer, &mut scene), &off, &output, false);
        assert!(!renderer.ray_tracing_in_effect(&off));
        assert!(
            renderer.ray_tracing_error().is_none(),
            "nothing asked for it"
        );
    }
}
