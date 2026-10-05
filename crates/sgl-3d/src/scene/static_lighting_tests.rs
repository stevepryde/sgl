//! Observe the real bake shaders in raster and secondary shading.
use super::*;
use crate::renderer::Renderer;
use crate::settings::Settings;
use crate::view::pipelines::GeometryPass;
use crate::{
    static_lighting::{AmbientCube, IrradianceAtlas, Lightmap},
    *,
};
use glam::camera;
use glam::{Mat4, Vec3};

fn receiver(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    renderer: &mut Renderer,
    scene: &mut Scene,
    input: &FrameInput,
) -> [f32; 4] {
    observe(
        device,
        queue,
        renderer,
        scene,
        input,
        &Settings::default(),
        r#"
@group(3) @binding(0) var<storage,read_write> output:array<vec4<f32>>;
@compute @workgroup_size(1) fn observe() {
 var cube:array<vec4<f32>,6>;
 output[0]=vec4(surface_fixed_irradiance(true,vec2(0.5),vec2(0.5),vec4(0.,0.,1.,1.),vec3(0.,1.,0.),true,false,cube),1.);
}
"#,
    )
}

/// Runs `observation` over the frame of `scene` that `input` describes,
/// prepared as a frame is, with the camera's lit group 0 and the scene's
/// group 1. The observation may trace rays where the frame does not, so it
/// updates the scene's rays itself.
fn observe(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    renderer: &mut Renderer,
    scene: &mut Scene,
    input: &FrameInput,
    settings: &Settings,
    observation: &str,
) -> [f32; 4] {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("persistent irradiance receiver"),
        source: wgpu::ShaderSource::Wgsl(
            format!("{}\n{}", crate::shading::lit_compute_library(), observation).into(),
        ),
    });
    let observation_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("irradiance observation"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only: false },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: None,
        layout: Some(
            &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("actual scene lighting observation"),
                bind_group_layouts: &[
                    Some(renderer.test_lit_layout()),
                    Some(scene.scene_layout()),
                    None,
                    Some(&observation_layout),
                ],
                immediate_size: 0,
            }),
        ),
        module: &shader,
        entry_point: Some("observe"),
        compilation_options: Default::default(),
        cache: None,
    });
    let output = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: 16,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: 16,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &observation_layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: output.as_entire_binding(),
        }],
    });
    renderer.prepare_test_frame(device, queue, scene, input, settings);
    scene.update_rays(device, queue, input.visibility_mask);
    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_compute_pass(&Default::default());
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, renderer.test_camera_lit(), &[]);
        pass.set_bind_group(1, &scene.scene_group, &[]);
        pass.set_bind_group(3, &group, &[]);
        pass.dispatch_workgroups(1, 1, 1);
    }
    encoder.copy_buffer_to_buffer(&output, 0, &readback, 0, 16);
    queue.submit([encoder.finish()]);
    scene.finish_frame();
    let (send, recv) = std::sync::mpsc::channel();
    readback.map_async(wgpu::MapMode::Read, .., move |r| send.send(r).unwrap());
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    recv.recv().unwrap().unwrap();
    bytemuck::cast_slice::<u8, f32>(&readback.get_mapped_range(..))
        .try_into()
        .unwrap()
}

#[test]
fn lightmap_is_baked_until_baked_lighting_is_switched_off() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    pollster::block_on(async {
        let mut scene = Scene::new(&device, &queue);
        let (world, _) =
            test_support::add_static(&device, &queue, &mut scene, test_support::cube());
        let mut renderer = Renderer::for_test(&device, &queue, [64, 64], &Settings::default());
        let map = Lightmap {
            directionality: vec![],
            size: [2, 2],
            uv_scale_offset: [1., 1., 0., 0.],
            irradiance: vec![[0.12, 0., 0.28]; 4],
        };
        let mut frame = FrameInput::new(Camera {
            view: Mat4::IDENTITY,
            projection: Mat4::IDENTITY,
            eye: Vec3::Z,
        });
        frame.diffuse_environment.intensity = 0.;
        frame.visibility_mask = 1;
        let absent = receiver(&device, &queue, &mut renderer, &mut scene, &frame);
        scene
            .set_lightmap(&device, &queue, &map, &world.materials)
            .unwrap();
        let clear = receiver(&device, &queue, &mut renderer, &mut scene, &frame);
        assert!(
            clear[0] > absent[0] + 0.01 && clear[2] > absent[2] + 0.01,
            "bake failed to supply both lamp colors"
        );
        frame.baked_lighting = false;
        let disabled = receiver(&device, &queue, &mut renderer, &mut scene, &frame);
        assert!(
            disabled[..3].iter().all(|v| *v == 0.),
            "runtime light switch left baked illumination active"
        );
        frame.baked_lighting = true;
        assert_eq!(
            receiver(&device, &queue, &mut renderer, &mut scene, &frame),
            clear,
            "reenabling lamps failed to restore bake"
        );
    });
}

// Plausible defects: the lightmap's sampler wraps X or clamps Y, the chart
// transform is dropped or misapplied, or the texels are not filtered. The
// oracle is `Lightmap`'s documented addressing, computed here: bilinear
// interpolation of the texels at the transformed material UV, with X clamped
// and Y wrapped. The texel values and filter weights are exact in RGBA16F and
// in 8-bit subtexel precision.
#[test]
fn lightmap_filters_bilinearly_with_x_clamped_and_y_repeated() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    let (world, _) = test_support::add_static(&device, &queue, &mut scene, test_support::cube());
    let mut renderer = Renderer::for_test(&device, &queue, [8, 8], &Settings::default());
    const SIZE: usize = 4;
    let texel = |x: usize, y: usize| (1 + x + SIZE * y) as f32;
    // Chart UV = material UV * (0.5, 2) + (0.25, -0.5).
    let transform = [0.5, 2., 0.25, -0.5];
    scene
        .set_lightmap(
            &device,
            &queue,
            &Lightmap {
                size: [SIZE as u32; 2],
                uv_scale_offset: transform,
                irradiance: (0..SIZE * SIZE)
                    .map(|i| [texel(i % SIZE, i / SIZE), 0., 0.])
                    .collect(),
                directionality: vec![],
            },
            &world.materials,
        )
        .unwrap();
    let filtered = |chart: [f32; 2]| {
        let position = chart.map(|v| v * SIZE as f32 - 0.5);
        let [x, y] = position.map(f32::floor);
        let [fx, fy] = [position[0] - x, position[1] - y];
        let at = |x: f32, y: f32| {
            texel(
                x.clamp(0., (SIZE - 1) as f32) as usize,
                (y as i32).rem_euclid(SIZE as i32) as usize,
            )
        };
        let row = |y: f32| at(x, y) * (1. - fx) + at(x + 1., y) * fx;
        row(y) * (1. - fy) + row(y + 1.) * fy
    };
    let input = FrameInput::new(Camera {
        view: Mat4::IDENTITY,
        projection: Mat4::IDENTITY,
        eye: Vec3::Z,
    });
    // Interior, X beyond both edges, Y across the seam at 0 and 1, and Y a
    // whole repeat further on.
    for chart in [
        [0.4375, 0.625],
        [-0.5, 0.3125],
        [1.25, 0.5625],
        [0.5, 0.],
        [0.6875, 1.],
        [0.3125, 2.4375],
    ] {
        let uv = [
            (chart[0] - transform[2]) / transform[0],
            (chart[1] - transform[3]) / transform[1],
        ];
        let actual = observe(
            &device,
            &queue,
            &mut renderer,
            &mut scene,
            &input,
            &Settings::default(),
            &format!(
                r#"
@group(3) @binding(0) var<storage,read_write> output:array<vec4<f32>>;
@compute @workgroup_size(1) fn observe() {{
 var cube:array<vec4<f32>,6>;
 output[0]=vec4(surface_fixed_irradiance(true,vec2<f32>({:?},{:?}),vec2(.5),vec4(0.,0.,1.,1.),vec3(0.,1.,0.),true,false,cube),1.);
}}
"#,
                uv[0], uv[1]
            ),
        );
        let expected = filtered(chart);
        assert!(
            (actual[0] - expected).abs() < expected * 1e-3,
            "chart UV {chart:?}: {} expected {expected}",
            actual[0]
        );
    }
}

#[test]
fn static_atlas_and_moving_cube_transport() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    pollster::block_on(async {
        let mut world = test_support::cube();
        world.materials[0].double_sided = true;
        world.meshes[0].vertices = [(-4., -4.), (-4., 4.), (4., 4.), (4., -4.)]
            .map(|(x, z)| asset::Vertex {
                tangent: [0.0; 4],
                lightmap_bounds: [0.25, 0.25, 0.75, 0.75],
                position: [x, 0., z],
                normal: [0., 1., 0.],
                uv: [0.; 2],
                color: [1.; 4],
                lightmap_uv: [(x + 4.) / 8., (z + 4.) / 8.],
            })
            .to_vec();
        world.meshes[0].indices = vec![0, 1, 2, 0, 2, 3];
        let mut scene = Scene::new(&device, &queue);
        let (world, _) = test_support::add_static(&device, &queue, &mut scene, world);
        let cube_model = scene
            .add_asset(&device, &queue, test_support::cube())
            .unwrap()
            .model;
        let mut renderer = Renderer::for_test(&device, &queue, [8, 8], &Settings::default());
        let settings = Settings::default();
        // Analytic irradiance from a unit point source one metre above the plane.
        // The atlas samples actual texel-center positions. A triangle-corner bake
        // would lose the bright interior by more than two orders of magnitude.
        let size = 17u32;
        let front = (0..size * size)
            .map(|i| {
                let x = ((i % size) as f32 + 0.5) / size as f32 * 8. - 4.;
                let z = ((i / size) as f32 + 0.5) / size as f32 * 8. - 4.;
                if x.abs() > 2. || z.abs() > 2. {
                    [9., 0., 0.]
                } else {
                    [
                        1. / (std::f32::consts::PI * (1. + x * x + z * z).powf(1.5)),
                        0.,
                        0.,
                    ]
                }
            })
            .collect();
        let back = (0..size * size)
            .map(|i| {
                let x = ((i % size) as f32 + 0.5) / size as f32 * 8. - 4.;
                let z = ((i / size) as f32 + 0.5) / size as f32 * 8. - 4.;
                [
                    0.,
                    0.,
                    2. / (std::f32::consts::PI * (1. + x * x + z * z).powf(1.5)),
                ]
            })
            .collect();
        scene
            .set_static_irradiance_atlas(
                &device,
                &queue,
                &IrradianceAtlas {
                    directionality: vec![],
                    back_directionality: vec![],
                    size: [size, size],
                    irradiance: front,
                    back_irradiance: back,
                },
            )
            .unwrap();
        let mut input = FrameInput::new(Camera {
            view: Mat4::IDENTITY,
            projection: Mat4::IDENTITY,
            eye: Vec3::Y * 5.,
        });
        let sample = |renderer: &mut Renderer,
                      scene: &mut Scene,
                      input: &FrameInput,
                      origin: Vec3,
                      direction: Vec3| {
            let observation = format!(
                r#"
@group(3) @binding(0) var<storage,read_write> output:array<vec4<f32>>;
@compute @workgroup_size(1) fn observe() {{
 let origin=vec3<f32>({},{},{});let direction=vec3<f32>({},{},{});
 let raw=scene_trace_nearest(SceneRay(vec4(origin,0.),vec4(direction,100.)),SCENE_SIDES_AS_RASTER);
 let h=scene_decode_hit(raw,origin,direction);
 let normal=select(-h.geometric_normal,h.geometric_normal,h.front_face);
 output[0]=vec4(surface_fixed_irradiance(false,h.uv,h.lightmap_uv,h.lightmap_bounds,normal,h.front_face,(h.instance_flags&OBJECT_STATIC)==0u,objects[h.instance_id].baked_irradiance),select(0.,1.,h.hit));
}}
"#,
                origin.x, origin.y, origin.z, direction.x, direction.y, direction.z
            );
            observe(
                &device,
                &queue,
                renderer,
                scene,
                input,
                &settings,
                &observation,
            )
        };
        let center = sample(&mut renderer, &mut scene, &input, Vec3::Y * 5., -Vec3::Y);
        assert!(
            (center[0] - 1. / std::f32::consts::PI).abs() < 0.0002 && center[3] == 1.,
            "front interior lost: {center:?}"
        );
        let corner = sample(
            &mut renderer,
            &mut scene,
            &input,
            Vec3::new(3.9, 5., 3.9),
            -Vec3::Y,
        );
        assert_eq!(
            corner[0], 0.,
            "UV outside cropped chart picked up neighboring atlas illumination"
        );
        assert!(
            center[0] > corner[0] * 100.,
            "triangle interior pool collapsed to corner values"
        );
        let underside = sample(&mut renderer, &mut scene, &input, -Vec3::Y * 5., Vec3::Y);
        assert!(
            (underside[2] - 2. / std::f32::consts::PI).abs() < 0.0004 && underside[0] == 0.,
            "wrong atlas side: {underside:?}"
        );
        input.camera.eye = Vec3::new(900., 10., 100.);
        assert_eq!(
            center,
            sample(&mut renderer, &mut scene, &input, Vec3::Y * 5., -Vec3::Y),
            "camera moved a baked pool"
        );
        // A distant source straight above follows Lambert's cosine law: its
        // lobe is w=N, a=0, so +Y keeps the bake, 45 degrees receives cos 45
        // and +X/-Y receive nothing. Exercise the uploaded atlas and
        // lightmap, including back faces.
        scene
            .set_static_irradiance_atlas(
                &device,
                &queue,
                &IrradianceAtlas {
                    size: [2, 2],
                    irradiance: vec![[2., 1., 0.5]; 4],
                    back_irradiance: vec![[0.5, 1., 2.]; 4],
                    directionality: vec![[0.5, 0.625, 0.5, 0.]; 4],
                    back_directionality: vec![[0.5, 0.375, 0.5, 0.]; 4],
                },
            )
            .unwrap();
        scene
            .set_lightmap(
                &device,
                &queue,
                &Lightmap {
                    size: [2, 2],
                    uv_scale_offset: [1., 1., 0., 0.],
                    irradiance: vec![[2., 1., 0.5]; 4],
                    directionality: vec![[0.5, 0.625, 0.5, 0.]; 4],
                },
                &world.materials,
            )
            .unwrap();
        use std::f32::consts::{FRAC_1_SQRT_2, SQRT_2};
        for (lightmapped, front, normal, expected) in [
            (false, true, [0., 1., 0.], [2., 1., 0.5]),
            (
                false,
                true,
                [1., 1., 0.],
                [SQRT_2, FRAC_1_SQRT_2, FRAC_1_SQRT_2 / 2.],
            ),
            (false, true, [1., 0., 0.], [0.; 3]),
            (false, true, [0., -1., 0.], [0.; 3]),
            (false, false, [0., -1., 0.], [0.5, 1., 2.]),
            (
                false,
                false,
                [0., -1., 1.],
                [FRAC_1_SQRT_2 / 2., FRAC_1_SQRT_2, SQRT_2],
            ),
            (false, false, [0., 1., 0.], [0.; 3]),
            (true, true, [0., 1., 0.], [2., 1., 0.5]),
            (
                true,
                true,
                [1., 1., 0.],
                [SQRT_2, FRAC_1_SQRT_2, FRAC_1_SQRT_2 / 2.],
            ),
            (true, true, [0., -1., 0.], [0.; 3]),
        ] {
            let actual = observe(
                &device,
                &queue,
                &mut renderer,
                &mut scene,
                &input,
                &settings,
                &format!(
                    r#"
@group(3) @binding(0) var<storage,read_write> output:array<vec4<f32>>;
@compute @workgroup_size(1) fn observe() {{
 var cube:array<vec4<f32>,6>;
 output[0]=vec4(surface_fixed_irradiance({lightmapped},vec2(.5),vec2(.5),vec4(0.,0.,1.,1.),vec3<f32>({},{},{}),{front},false,cube),1.);
}}
"#,
                    normal[0], normal[1], normal[2]
                ),
            );
            // Both maps store lobes as RGBA8: each component may round by half
            // a step, moving w by up to 4/255 per axis and a by up to 1/255.
            let n = Vec3::from(normal).normalize();
            let quantization = (4. * n.abs().element_sum() + 1.) / 255. * 2.;
            for channel in 0..3 {
                assert!(
                    (actual[channel] - expected[channel]).abs() < 0.005 + quantization,
                    "directional normal response lightmapped={lightmapped} front={front} n={normal:?}: {actual:?} expected {expected:?}"
                );
            }
        }
        // A fixed source above the moving cube lights its +Y face. Rotation
        // changes which authored face receives that light; the field stays world-aligned.
        let mut cube = AmbientCube::default();
        cube.irradiance[2] = [1. / std::f32::consts::PI, 0., 0.];
        let posed = |pose| InstanceState {
            model: cube_model,
            pose,
            visible: true,
            capture_visible: true,
        };
        let mut moving = None;
        for (rotation, origin, direction, expected) in [
            (glam::Quat::IDENTITY, Vec3::new(25., 0., 0.), -Vec3::X, 0.),
            (
                glam::Quat::from_rotation_z(std::f32::consts::FRAC_PI_2),
                Vec3::new(20., 5., 0.),
                -Vec3::Y,
                1. / std::f32::consts::PI,
            ),
        ] {
            let state = posed(Mat4::from_rotation_translation(rotation, Vec3::X * 20.));
            let instance = *moving.get_or_insert_with(|| {
                scene
                    .add_instance(&device, &queue, state, Mobility::Moving)
                    .unwrap()
            });
            scene.set_instance(&queue, instance, state).unwrap();
            scene
                .set_instance_baked_irradiance(&queue, instance, cube)
                .unwrap();
            let value = sample(&mut renderer, &mut scene, &input, origin, direction);
            assert!(
                (value[0] - expected).abs() < 0.0001 && value[3] == 1.,
                "field rotated with object: {value:?}"
            );
        }
        // A new instance at the removed one's index has no ambient cube.
        scene.remove_instance(moving.unwrap()).unwrap();
        scene
            .add_instance(
                &device,
                &queue,
                posed(Mat4::from_translation(Vec3::X * 20.)),
                Mobility::Moving,
            )
            .unwrap();
        assert_eq!(
            sample(
                &mut renderer,
                &mut scene,
                &input,
                Vec3::new(20., 5., 0.),
                -Vec3::Y
            )[0],
            0.,
            "a reused index retained another receiver field"
        );
    });
}

#[test]
fn moving_cube_uses_shaded_normals_in_raster_and_secondary() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    pollster::block_on(async {
        let mut model = test_support::cube();
        model.materials[0].base = [1.; 4];
        model.materials[0].metallic = 0.;
        model.materials[0].roughness = 0.5;
        model.materials[0].emissive = [0.; 3];
        model.meshes[0].vertices = [(-1., -1., -1.), (1., -1., 1.), (0., 1., 0.)]
            .map(|(x, y, bend)| asset::Vertex {
                tangent: [0.0; 4],
                position: [x, y, 0.],
                normal: Vec3::new(bend, 0., 1.).normalize().to_array(),
                uv: [(x + 1.) * 0.5, (y + 1.) * 0.5],
                color: [1.; 4],
                lightmap_uv: [0.; 2],
                lightmap_bounds: [0., 0., 1., 1.],
            })
            .to_vec();
        model.meshes[0].indices = vec![0, 1, 2];
        let mut mapped = model.clone();
        for v in &mut mapped.meshes[0].vertices {
            v.normal = [0., 0., 1.];
        }
        mapped.materials[0].normal_texture = Some(mapped.images.len());
        mapped
            .images
            .push(crate::asset::Image::Rgba8(image::RgbaImage::from_pixel(
                2,
                2,
                image::Rgba([218, 128, 218, 255]),
            )));
        let mut scene = Scene::new(&device, &queue);
        let models =
            [model, mapped].map(|asset| scene.add_asset(&device, &queue, asset).unwrap().model);
        let mut moving = None;
        let mut renderer = Renderer::for_test(&device, &queue, [64, 64], &Settings::default());
        let settings = Settings::default();
        let mut cube = AmbientCube::default();
        cube.irradiance[0] = [1. / std::f32::consts::PI, 0., 0.];
        let size = [64, 64];
        // Fused opaque attachments; lit color is location 4.
        let targets = [
            shading::gbuffer::NORMAL,
            shading::gbuffer::MATERIAL,
            shading::gbuffer::MOTION,
            shading::gbuffer::F0,
            shading::gbuffer::COLOR,
            shading::gbuffer::AMBIENT,
            shading::gbuffer::SOURCE_ID,
            shading::gbuffer::ANISOTROPY,
        ]
        .map(|format| view::targets::target(&device, "bent normal response", size, format));
        let depth = device
            .create_texture(&wgpu::TextureDescriptor {
                label: None,
                size: wgpu::Extent3d {
                    width: 64,
                    height: 64,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Depth32Float,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            })
            .create_view(&Default::default());
        for model in [0, 1] {
            let state = InstanceState {
                model: models[model],
                pose: Mat4::IDENTITY,
                visible: true,
                capture_visible: true,
            };
            let instance = *moving.get_or_insert_with(|| {
                scene
                    .add_instance(&device, &queue, state, Mobility::Moving)
                    .unwrap()
            });
            scene.set_instance(&queue, instance, state).unwrap();
            scene
                .set_instance_baked_irradiance(&queue, instance, cube)
                .unwrap();
            let eye_z = 3.;
            let input = FrameInput::new(Camera {
                eye: Vec3::Z * eye_z,
                view: camera::rh::view::look_at_mat4(Vec3::Z * eye_z, Vec3::ZERO, Vec3::Y),
                projection: camera::rh::proj::directx::orthographic(-1., 1., -1., 1., 10., 0.1),
            });
            renderer.prepare_test_frame(&device, &queue, &mut scene, &input, &settings);
            let mut encoder = device.create_command_encoder(&Default::default());
            {
                let attachments: Vec<_> = targets.iter().map(view::targets::attachment).collect();
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: None,
                    color_attachments: &attachments,
                    depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                        view: &depth,
                        depth_ops: Some(wgpu::Operations {
                            load: wgpu::LoadOp::Clear(0.),
                            store: wgpu::StoreOp::Store,
                        }),
                        stencil_ops: None,
                    }),
                    ..Default::default()
                });
                renderer.draw_test_camera(&scene, &mut pass, GeometryPass::Fused);
            }
            queue.submit([encoder.finish()]);
            scene.finish_frame();
            let bytes = test_support::read(&device, &queue, targets[4].texture(), 8);
            let mut responses = Vec::new();
            for pixel_x in [17u32, 46u32] {
                let pixel_y = 47u32;
                let x = (pixel_x as f32 + 0.5) / 32. - 1.;
                let y = 1. - (pixel_y as f32 + 0.5) / 32.;
                let raster = test_support::half(&bytes[((pixel_y * 64 + pixel_x) * 8) as usize..]);
                let observation = format!(
                    r#"
@group(3) @binding(0) var<storage,read_write> output:array<vec4<f32>>;
@compute @workgroup_size(1) fn observe() {{
 let origin=vec3<f32>({x},{y},{eye_z});let direction=vec3(0.,0.,-1.);
 let raw=scene_trace_nearest(SceneRay(vec4(origin,0.),vec4(direction,10.)),SCENE_SIDES_AS_RASTER);
 let hit=scene_decode_hit(raw,origin,direction);
 output[0]=vec4(shade_ray_hit(hit,-direction,SHADOW_RECEIVER_CAPTURE,vec3(0.)),select(0.,1.,hit.hit));
}}
"#
                );
                let secondary = observe(
                    &device,
                    &queue,
                    &mut renderer,
                    &mut scene,
                    &input,
                    &settings,
                    &observation,
                );
                assert!(
                    secondary[3] == 1. && (raster - secondary[0]).abs() < 0.001,
                    "raster/secondary normal mismatch: model {model}, x {x}, {raster} vs {secondary:?}"
                );
                if model == 0 {
                    // Independently interpolate the authored normal directions.
                    // A flat geometric face has zero +X response everywhere;
                    // the right half's positive shading cosine must remain visible.
                    let top = (y + 1.) * 0.5;
                    let n = Vec3::new(
                        x / std::f32::consts::SQRT_2,
                        0.,
                        (1. - top) / std::f32::consts::SQRT_2 + top,
                    )
                    .normalize();
                    let expected = 0.96 * n.x.max(0.).powi(2) / std::f32::consts::PI;
                    assert!(
                        (raster - expected).abs() < 0.0003,
                        "bent-normal irradiance was flattened: {raster} vs {expected}"
                    );
                }
                responses.push(raster);
            }
            if model == 0 {
                assert!(
                    responses[0] < 0.0001 && responses[1] > 0.03,
                    "actual bent triangle lost directional shading: {responses:?}"
                );
            } else {
                assert!(
                    responses.iter().all(|v| *v > 0.1),
                    "normal map was ignored by fixed illumination: {responses:?}"
                );
            }
        }
    });
}

#[test]
fn fixed_bakes_use_material_normal_texels_in_raster_and_secondary() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    pollster::block_on(async {
        let mut model = test_support::cube();
        model.materials[0].base = [1.; 4];
        model.materials[0].double_sided = true;
        model.materials[0].metallic = 0.;
        model.materials[0].roughness = 0.5;
        model.materials[0].emissive = [0.; 3];
        model.meshes[0].vertices = [(-1., -1., -1.), (1., -1., 1.), (0., 1., 0.)]
            .map(|(x, y, bend)| asset::Vertex {
                tangent: [0.0; 4],
                position: [x, y, 0.],
                normal: Vec3::new(bend, 0., 1.).normalize().to_array(),
                uv: [(x + 1.) * 0.5, (y + 1.) * 0.5],
                color: [1.; 4],
                lightmap_uv: [0.; 2],
                lightmap_bounds: [0., 0., 1., 1.],
            })
            .to_vec();
        model.meshes[0].indices = vec![0, 1, 2];
        for vertex in &mut model.meshes[0].vertices {
            vertex.normal = [0., 0., 1.];
            vertex.lightmap_uv = [0.5; 2];
        }
        model.materials[0].normal_texture = Some(model.images.len());
        // Two wide, constant normal regions avoid the filter seam. The tangent
        // follows +X, and these texels tilt the normal toward -X and +X.
        model
            .images
            .push(crate::asset::Image::Rgba8(image::RgbaImage::from_fn(
                8,
                2,
                |x, _| image::Rgba([if x < 4 { 37 } else { 218 }, 128, 218, 255]),
            )));
        let mut scene = Scene::new(&device, &queue);
        let (world, _) = test_support::add_static(&device, &queue, &mut scene, model);
        let mut renderer = Renderer::for_test(&device, &queue, [64, 64], &Settings::default());
        let settings = Settings::default();
        // Reference normal +Z sees E/PI=1/PI. The RGBA8-exact lobe
        // w=8*v.xyz-4, a=2*v.w responds with (a+w.n)/PI: about (1+n.x)/PI.
        let irradiance = [1. / std::f32::consts::PI, 0., 0.];
        let lobe = [159. / 255., 128. / 255., 128. / 255., 128. / 255.];
        scene
            .set_static_irradiance_atlas(
                &device,
                &queue,
                &IrradianceAtlas {
                    size: [2, 2],
                    irradiance: vec![irradiance; 4],
                    back_irradiance: vec![[0.; 3]; 4],
                    directionality: vec![lobe; 4],
                    back_directionality: vec![],
                },
            )
            .unwrap();
        let size = [64, 64];
        // Fused opaque attachments; lit color is location 4.
        let formats = [
            shading::gbuffer::NORMAL,
            shading::gbuffer::MATERIAL,
            shading::gbuffer::MOTION,
            shading::gbuffer::F0,
            shading::gbuffer::COLOR,
            shading::gbuffer::AMBIENT,
            shading::gbuffer::SOURCE_ID,
            shading::gbuffer::ANISOTROPY,
        ];
        let targets: Vec<_> = formats
            .into_iter()
            .map(|format| view::targets::target(&device, "bent normal response", size, format))
            .collect();
        let depth = device
            .create_texture(&wgpu::TextureDescriptor {
                label: None,
                size: wgpu::Extent3d {
                    width: 64,
                    height: 64,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Depth32Float,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            })
            .create_view(&Default::default());
        for lightmapped in [false, true] {
            if lightmapped {
                scene
                    .set_lightmap(
                        &device,
                        &queue,
                        &Lightmap {
                            size: [2, 2],
                            uv_scale_offset: [1., 1., 0., 0.],
                            irradiance: vec![irradiance; 4],
                            directionality: vec![lobe; 4],
                        },
                        &world.materials,
                    )
                    .unwrap();
            }
            let eye_z = 3.;
            let input = FrameInput::new(Camera {
                eye: Vec3::Z * eye_z,
                view: camera::rh::view::look_at_mat4(Vec3::Z * eye_z, Vec3::ZERO, Vec3::Y),
                projection: camera::rh::proj::directx::orthographic(-1., 1., -1., 1., 10., 0.1),
            });
            renderer.prepare_test_frame(&device, &queue, &mut scene, &input, &settings);
            let mut encoder = device.create_command_encoder(&Default::default());
            {
                let attachments: Vec<_> = targets.iter().map(view::targets::attachment).collect();
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: None,
                    color_attachments: &attachments,
                    depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                        view: &depth,
                        depth_ops: Some(wgpu::Operations {
                            load: wgpu::LoadOp::Clear(0.),
                            store: wgpu::StoreOp::Store,
                        }),
                        stencil_ops: None,
                    }),
                    ..Default::default()
                });
                renderer.draw_test_camera(&scene, &mut pass, GeometryPass::Fused);
            }
            queue.submit([encoder.finish()]);
            scene.finish_frame();
            let bytes = test_support::read(&device, &queue, targets[4].texture(), 8);
            let mut responses = Vec::new();
            for pixel_x in [17u32, 46u32] {
                let pixel_y = 47u32;
                let x = (pixel_x as f32 + 0.5) / 32. - 1.;
                let y = 1. - (pixel_y as f32 + 0.5) / 32.;
                let raster = test_support::half(&bytes[((pixel_y * 64 + pixel_x) * 8) as usize..]);
                let observation = format!(
                    r#"
@group(3) @binding(0) var<storage,read_write> output:array<vec4<f32>>;
@compute @workgroup_size(1) fn observe() {{
 let origin=vec3<f32>({x},{y},{eye_z});let direction=vec3(0.,0.,-1.);
 let raw=scene_trace_nearest(SceneRay(vec4(origin,0.),vec4(direction,10.)),SCENE_SIDES_AS_RASTER);
 let hit=scene_decode_hit(raw,origin,direction);
 output[0]=vec4(shade_ray_hit(hit,-direction,SHADOW_RECEIVER_CAPTURE,vec3(0.)),select(0.,1.,hit.hit));
}}
"#
                );
                let secondary = observe(
                    &device,
                    &queue,
                    &mut renderer,
                    &mut scene,
                    &input,
                    &settings,
                    &observation,
                );
                assert!(
                    secondary[3] == 1. && (raster - secondary[0]).abs() < 0.001,
                    "raster/secondary normal mismatch: lightmapped {lightmapped}, x {x}, {raster} vs {secondary:?}"
                );
                let normal =
                    Vec3::new(if pixel_x < 32 { -181. } else { 181. }, 1., 181.).normalize();
                let response = 2. * lobe[3]
                    + Vec3::from_slice(&lobe[..3])
                        .map(|v| 8. * v - 4.)
                        .dot(normal);
                let expected = 0.96 * response / std::f32::consts::PI;
                assert!(
                    (raster - expected).abs() < 0.002,
                    "fixed bake ignored material normal: lightmapped={lightmapped}, x={x}: {raster} vs {expected}"
                );
                responses.push(raster);
            }
            assert!(
                responses[1] > responses[0] * 5.,
                "fixed directional bake flattened normal texture: lightmapped={lightmapped}, {responses:?}"
            );
        }
    });
}

// A compressed atlas shades exactly as the float atlas holding what its blocks
// decode to, times its scale. Every BC6H block is mode 11 (one region, 10-bit
// endpoints) with both endpoints (495, 0, 0) and zero indices, which the D3D11
// BC6H format decodes to (1, 0, 0); every BC7 block is mode 6 with both
// endpoints (128, 160, 128, 0) and zero indices. At scale 0.5 the frame must
// match float irradiance (0.5, 0, 0) with those directionality bytes, and
// differ from a black atlas.
#[test]
fn compressed_static_atlas_shades_as_its_decoded_float_atlas_times_its_scale() {
    use crate::static_lighting::{CompressedIrradianceAtlas, IrradianceAtlas};
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    if !device
        .features()
        .contains(wgpu::Features::TEXTURE_COMPRESSION_BC)
    {
        return;
    }
    const SIZE: [u32; 2] = [32, 24];
    let mut world = crate::test_support::cube();
    for v in &mut world.meshes[0].vertices {
        v.lightmap_uv = [0.5; 2];
    }
    world.materials[0].base = [0.5, 0.5, 0.5, 1.];
    world.materials[0].metallic = 0.;
    world.materials[0].roughness = 0.7;
    let mut scene = Scene::new(&device, &queue);
    crate::test_support::add_static(&device, &queue, &mut scene, world);
    let eye = Vec3::new(2.4, 2., 3.3);
    let mut input = FrameInput::new(Camera {
        eye,
        view: camera::rh::view::look_at_mat4(eye, Vec3::ZERO, Vec3::Y),
        projection: perspective(55f32.to_radians(), SIZE[0] as f32 / SIZE[1] as f32, 0.1),
    });
    input.backdrop = Backdrop::Color([0.; 3]);
    input.reflection_environment = EnvironmentLight {
        yaw: 0.,
        intensity: 0.,
    };
    input.camera_cut = true;
    let settings = Settings {
        bloom: settings::Bloom::Off,
        atmosphere: false,
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
        "compressed atlas output",
        SIZE,
        wgpu::TextureFormat::Rgba8Unorm,
    );
    let mut color = |scene: &mut Scene| {
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
        crate::test_support::read(&device, &queue, renderer.targets().color.texture(), 8)
    };
    let float = |rgb: [f32; 3]| IrradianceAtlas {
        size: [4, 4],
        irradiance: vec![rgb; 16],
        back_irradiance: vec![rgb; 16],
        directionality: vec![[128. / 255., 160. / 255., 128. / 255., 0.]; 16],
        back_directionality: vec![[128. / 255., 160. / 255., 128. / 255., 0.]; 16],
    };
    scene
        .set_static_irradiance_atlas(&device, &queue, &float([0.; 3]))
        .unwrap();
    let black = color(&mut scene);
    scene
        .set_static_irradiance_atlas(&device, &queue, &float([0.5, 0., 0.]))
        .unwrap();
    let expected = color(&mut scene);
    let bc6h = (0b00011u128 | 495 << 5 | 495 << 35).to_le_bytes();
    let bc7 = [128u128, 128, 160, 160, 128, 128, 0, 0]
        .iter()
        .enumerate()
        .fold(1u128 << 6, |block, (i, &c)| block | (c >> 1) << (7 + 7 * i))
        .to_le_bytes();
    scene
        .set_compressed_static_irradiance_atlas(
            &device,
            &queue,
            &CompressedIrradianceAtlas {
                size: [4, 4],
                irradiance: bc6h.repeat(2),
                directionality: bc7.repeat(2),
                scale: 0.5,
            },
        )
        .unwrap();
    let compressed = color(&mut scene);
    assert_ne!(expected, black, "the fixed atlas lights the cube");
    assert_eq!(compressed, expected);
}
