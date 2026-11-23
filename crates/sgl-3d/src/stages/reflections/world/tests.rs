//! World-space reflection rays observed in the reflection composite of real
//! frames, and the classification's ray list over fixture receivers.
use crate::asset::{CpuMesh, Vertex};
use crate::renderer::test_frame::TestRenderer;
use crate::settings::WorldSpaceReflections::{All, Moving};
use crate::settings::{self, Settings};
use crate::shading::RayQueryForm;
use crate::{Backdrop, Camera, FrameInput, InstanceState, Mobility, Scene, test_support};
use glam::{Mat4, Vec3};

const SIZE: [u32; 2] = [64, 64];

// Plausible defect: the world resolve adds a Euclidean hit length to axial
// view depth, so off-axis virtual points gain too much distance. Dispatch
// the production resolve over a plane 10m away with confirmed 10m rays;
// reconstruct its exported reprojection point and measure the extension in
// world metres. This geometric oracle is independent of the depth formula.
#[test]
fn resolved_virtual_points_preserve_reflected_ray_length() {
    use wgpu::util::DeviceExt;
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let size = [64, 64];
    let targets = super::Targets::new(&device, size);
    let reduced = targets.reduced;
    let texels = (size[0] * size[1]) as usize;
    let rays = (reduced[0] * reduced[1]) as usize;
    let normal = test_support::hdr_texture(&device, &queue, size, &vec![[0.; 4]; texels]);
    let material =
        test_support::hdr_texture(&device, &queue, size, &vec![[0., 0.2, 0., 1.]; texels]);
    let f0 = test_support::hdr_texture(&device, &queue, size, &vec![[1.; 4]; texels]);
    let indirect = test_support::hdr_texture(
        &device,
        &queue,
        reduced,
        &vec![[0.25, 0.25, 0.25, 1.]; rays],
    );
    let direction =
        test_support::hdr_texture(&device, &queue, reduced, &vec![[0., 0., 1., 1.]; rays]);
    let r32 = |label, usage| {
        super::texture(
            &device,
            label,
            reduced,
            wgpu::TextureFormat::R32Float,
            usage,
        )
    };
    let length = r32("reflection lengths", wgpu::TextureUsages::COPY_DST);
    queue.write_texture(
        length.texture().as_image_copy(),
        bytemuck::cast_slice(&vec![10f32; rays]),
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(reduced[0] * 4),
            rows_per_image: Some(reduced[1]),
        },
        length.texture().size(),
    );
    let reprojection = r32(
        "resolved virtual depths",
        wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
    );
    let depth = crate::view::targets::target(
        &device,
        "receiver depth",
        size,
        crate::shading::gbuffer::DEPTH,
    );
    let projection = crate::perspective(std::f32::consts::FRAC_PI_2, 1., 0.1);
    let [w, h] = size.map(|v| v as f32);
    let [rw, rh] = reduced.map(|v| v as f32);
    let params = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: None,
        contents: bytemuck::bytes_of(&super::Params {
            inverse_view_projection: projection.inverse().to_cols_array_2d(),
            previous_view_projection: projection.to_cols_array_2d(),
            eye: [0., 0., 0., 0.1],
            full: [w, h, 1. / w, 1. / h],
            reduced: [rw, rh, 1. / rw, 1. / rh],
            frame: 0,
            rays: rays as u32,
            traced: 0.49,
            range: super::RANGE,
            previous_near: 0.1,
            fade: 0.1,
            padding: [0; 2],
        }),
        usage: wgpu::BufferUsages::UNIFORM,
    });
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("world reflection virtual points"),
        source: wgpu::ShaderSource::Wgsl(crate::shading::compose(&[&super::DENOISE]).into()),
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("world reflection virtual points"),
        layout: None,
        module: &shader,
        entry_point: Some(super::WORLD_RESOLVE_ENTRY),
        compilation_options: Default::default(),
        cache: None,
    });
    let bind = |index, entries: &[(u32, wgpu::BindingResource)]| {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &pipeline.get_bind_group_layout(index),
            entries: &entries
                .iter()
                .map(|(binding, resource)| wgpu::BindGroupEntry {
                    binding: *binding,
                    resource: resource.clone(),
                })
                .collect::<Vec<_>>(),
        })
    };
    let texture = wgpu::BindingResource::TextureView;
    let inputs = bind(
        0,
        &[
            (10, texture(&indirect)),
            (11, texture(&direction)),
            (12, texture(&length)),
            (20, texture(&targets.resolve)),
            (21, texture(&targets.resolve_variance)),
            (22, texture(&reprojection)),
        ],
    );
    let receivers = bind(
        3,
        &[
            (0, texture(&depth)),
            (1, texture(&normal)),
            (2, texture(&material)),
            (3, texture(&f0)),
            (4, params.as_entire_binding()),
        ],
    );
    let mut encoder = device.create_command_encoder(&Default::default());
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: None,
        depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
            view: &depth,
            depth_ops: Some(wgpu::Operations {
                load: wgpu::LoadOp::Clear(0.01),
                store: wgpu::StoreOp::Store,
            }),
            stencil_ops: None,
        }),
        ..Default::default()
    });
    {
        let mut pass = encoder.begin_compute_pass(&Default::default());
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &inputs, &[]);
        pass.set_bind_group(3, &receivers, &[]);
        pass.dispatch_workgroups(reduced[0].div_ceil(8), reduced[1].div_ceil(8), 1);
    }
    queue.submit([encoder.finish()]);
    let bytes = test_support::read(&device, &queue, reprojection.texture(), 4);
    let depths: &[f32] = bytemuck::cast_slice(&bytes);
    for (x, y) in [(8, 8), (16, 16), (24, 24)] {
        let screen = Vec3::new(
            (x as f32 + 0.5) / rw * 2. - 1.,
            1. - (y as f32 + 0.5) / rh * 2.,
            0.01,
        );
        let receiver = projection.inverse().project_point3(screen);
        let virtual_point = projection.inverse().project_point3(Vec3::new(
            screen.x,
            screen.y,
            depths[(y * reduced[0] + x) as usize],
        ));
        let extension = virtual_point.distance(receiver);
        assert!(
            (extension - 10.).abs() < 1e-4,
            "({x},{y}): 10m ray resolved to {extension}m"
        );
    }
}

/// A mirror floor facing +Y at height 0, 50 m from the origin to each side.
fn floor() -> crate::asset::Asset {
    let mut asset = test_support::cube();
    asset.meshes = vec![CpuMesh {
        vertices: [(-1., -1.), (1., -1.), (1., 1.), (-1., 1.)]
            .map(|(x, z)| Vertex {
                uv1: [0.0; 2],
                tangent: [0.; 4],
                lightmap_bounds: [0., 0., 1., 1.],
                lightmap_uv: [0.; 2],
                position: [x * 50., 0., z * 50.],
                normal: [0., 1., 0.],
                uv: [0.; 2],
                color: [1.; 4],
            })
            .to_vec(),
        indices: vec![0, 2, 1, 0, 3, 2],
        material: 0,
        deformation: Default::default(),
    }];
    let material = &mut asset.materials[0];
    material.base = [1.; 4];
    material.metallic = 1.;
    material.roughness = 0.05;
    asset
}

/// An unlit white unit cube that does not emit into global illumination,
/// which reflections still show glowing.
fn wall() -> crate::asset::Asset {
    let mut asset = test_support::cube();
    asset.materials[0].unlit = true;
    asset.materials[0].base = [1.; 4];
    asset.materials[0].emits_into_gi = false;
    asset
}

/// The reflection composite's red at every pixel.
fn composite(device: &wgpu::Device, queue: &wgpu::Queue, renderer: &TestRenderer) -> Vec<f32> {
    let texels = test_support::read(device, queue, renderer.targets().composite.texture(), 8);
    texels.chunks_exact(8).map(test_support::half).collect()
}

/// `asset` deformed: each vertex whole on one joint, at its bind pose.
fn deforming(mut asset: crate::asset::Asset) -> crate::asset::Asset {
    for mesh in &mut asset.meshes {
        mesh.deformation.influences = vec![
            crate::deformation::Influence {
                joints: [0; 4],
                weights: [1., 0., 0., 0.],
            };
            mesh.vertices.len()
        ];
    }
    asset
}

/// The wall the floor reflects: of `mobility`, deformed where `deforms`
/// (moving, since a deforming instance is), and masked over
/// `test_support::half_cut_out` where `masked`, which cuts out the half of
/// its front and back faces at negative x.
#[derive(Clone, Copy)]
struct Wall {
    mobility: Mobility,
    deforms: bool,
    masked: bool,
}

const MOVING: Wall = Wall {
    mobility: Mobility::Moving,
    deforms: false,
    masked: false,
};
const STATIC: Wall = Wall {
    mobility: Mobility::Static,
    deforms: false,
    masked: false,
};
const DEFORMING: Wall = Wall {
    mobility: Mobility::Moving,
    deforms: true,
    masked: false,
};
const MASKED: Wall = Wall {
    mobility: Mobility::Static,
    deforms: false,
    masked: true,
};

/// The reflection composite's red at every pixel of frames on `device`
/// with the hardware path as `hardware` says: a camera 2 m above a mirror
/// floor looking down 45° at an unlit white `wall` 300 m ahead, one frame
/// for each of `reaches`, then one more with the last 1.1 km ahead, with an
/// instance out of sight added before it, which grows the instance entries
/// and so replaces the hardware path's TLAS. Each frame is a camera cut, so
/// each is the same first frame of the effect: frames that take the same
/// rays on any renderer match.
fn reflected_wall(
    (device, queue): (&wgpu::Device, &wgpu::Queue),
    hardware: bool,
    wall: Wall,
    reaches: &[settings::WorldSpaceReflections],
) -> Vec<Vec<f32>> {
    let mut settings = Settings {
        antialiasing: settings::Antialiasing::Off,
        bloom: settings::Bloom::Off,
        atmosphere: false,
        screen_space_reflections: settings::ScreenSpaceReflections::Half,
        hardware_ray_tracing: hardware,
        ..Settings::default()
    };
    let mut renderer = TestRenderer::for_test(device, queue, SIZE, &settings);
    let mut scene = Scene::new(device, queue);
    test_support::add_static(device, queue, &mut scene, floor());
    let asset = if wall.deforms {
        deforming(self::wall())
    } else {
        self::wall()
    };
    let asset = if wall.masked {
        test_support::masked(asset, 0.5)
    } else {
        asset
    };
    let model = scene.add_asset(device, queue, asset).unwrap().model;
    let wall_at = |z: f32| InstanceState {
        model,
        pose: Mat4::from_scale_rotation_translation(
            Vec3::new(1200., 1100., 10.),
            glam::Quat::IDENTITY,
            Vec3::new(0., 560., z),
        ),
        visible: true,
        capture_visible: true,
    };
    let instance = scene
        .add_instance(device, queue, wall_at(-300.), wall.mobility)
        .unwrap();
    let eye = Vec3::new(0., 2., 0.);
    let mut input = FrameInput::new(Camera {
        view: glam::camera::rh::view::look_at_mat4(eye, eye + Vec3::new(0., -1., -1.), Vec3::Y),
        projection: crate::perspective(1., 1., 0.1),
        eye,
    });
    input.backdrop = Backdrop::Color([0.; 3]);
    input.camera_cut = true;
    let output = crate::view::targets::target(
        device,
        "world reflection frames",
        SIZE,
        crate::shading::gbuffer::COLOR,
    );
    let mut frame = |scene: &mut Scene, settings: &Settings| {
        let mut encoder = device.create_command_encoder(&Default::default());
        renderer.render(
            device,
            queue,
            &mut encoder,
            scene,
            &input,
            settings,
            &output,
            None,
        );
        queue.submit([encoder.finish()]);
        renderer.finish_frame(scene);
        assert_eq!(renderer.ray_tracing_in_effect(settings, scene), hardware);
        // A candidate program the device fails to compile falls back to
        // the baseline, which would pass these frames unseen.
        assert_eq!(renderer.ray_tracing_error(), None);
        (
            composite(device, queue, &renderer),
            renderer.ray_tracing_stats(),
        )
    };
    let mut composites = Vec::new();
    for &reach in reaches {
        settings.world_space_reflections = reach;
        let (composite, stats) = frame(&mut scene, &settings);
        if hardware && wall.masked {
            // Where the masked wall is traced: in the TLAS beside the floor
            // under the candidate form, on the portable BVHs under the
            // baseline, so the masked wall's frames compare the form the
            // device runs with the portable path, not the walk with itself.
            let (hardware, portable) = match RayQueryForm::of_backend(device.adapter_info().backend)
            {
                RayQueryForm::Baseline => (1, 1),
                RayQueryForm::Candidates => (2, 0),
            };
            assert_eq!(
                stats,
                crate::RayTracingStats {
                    hardware,
                    portable,
                    left_out: 0,
                }
            );
        }
        composites.push(composite);
    }
    scene
        .set_instance(queue, instance, wall_at(-1100.))
        .unwrap();
    let cube = scene
        .add_asset(device, queue, test_support::cube())
        .unwrap()
        .model;
    let below = InstanceState {
        pose: Mat4::from_translation(Vec3::new(0., -50., 0.)),
        ..InstanceState::new(cube)
    };
    scene
        .add_instance(device, queue, below, Mobility::Static)
        .unwrap();
    composites.push(frame(&mut scene, &settings).0);
    composites
}

/// Whether the floor reflects the wall in each of `near`: at more than a
/// quarter of the pixels; and nowhere in `far`, the wall beyond the rays'
/// 1000 m.
fn reflects(composites: &[Vec<f32>], label: &str) -> Vec<bool> {
    let (far, near) = composites.split_last().unwrap();
    assert!(
        far.iter().all(|&red| red < 0.01),
        "{label}: the floor reflects the wall 1.1 km away, beyond the rays' 1000 m"
    );
    near.iter()
        .map(|near| near.iter().filter(|&&red| red > 0.25).count() > near.len() / 4)
        .collect()
}

/// The largest difference between two composites' pixels.
fn largest_difference(a: &[f32], b: &[f32]) -> f32 {
    a.iter()
        .zip(b)
        .map(|(a, b)| (a - b).abs())
        .fold(0., f32::max)
}

// Plausible defects: world-space rays stopping short of Wicked's 1000 m
// range (`Postprocess_RTReflection`), such as at the 100 m they reached
// before, or a range lost between the parameters and the trace. The oracle
// is geometric: a camera 2 m above a mirror floor looks down 45°, so the
// floor it sees reflects directions 16° to 74° above the horizon, which the
// frame does not show and screen-space reflections cannot trace. A moving
// unlit white wall 300 m ahead spans those directions up to about 1000 m
// away, so the floor reflects it through world-space rays. Moved 1.1 km
// away it still spans some of those directions, but every ray to it is longer
// than 1100 m, so a range of 1000 m reflects nothing there and the floor shows
// only the black backdrop; an unlimited trace would still reflect it. The
// wall does not emit into GI, which keeps its light out of the dynamic GI
// probes alone: a reflection ray's hit that dropped it would show it black.
#[test]
fn world_space_rays_reflect_a_moving_wall_300_metres_away() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    assert_eq!(
        reflects(
            &reflected_wall((&device, &queue), false, MOVING, &[Moving]),
            "portable"
        ),
        [true],
        "the floor reflects the wall 300 m away"
    );
}

// The `All` reach (`WorldSpaceReflections::All`) on the portable path.
// Plausible defects: `All` taking the moving-only rays, or keeping their
// static visibility test, which leaves a static wall to the probes and sky;
// `All` taking static geometry alone, which misses a moving wall; a static
// hit shaded or composed otherwise than a moving one; the trace's pipelines
// not keyed by the reach, so a renderer that ran `Moving` keeps its rays
// under `All`; `All`'s rays without the 1000 m range. The oracle is the
// geometry above: under `Moving` the floor reflects the static wall
// nowhere, under `All` it reflects it, and the moving wall under both;
// and a static wall reflects under `All` exactly as a moving wall in its
// place does under `Moving`, since both are unlit and the rays meet the
// same triangles.
#[test]
fn the_all_reach_reflects_a_static_wall_that_moving_does_not() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let gpu = (&device, &queue);
    let still = reflected_wall(gpu, false, STATIC, &[Moving, All]);
    assert_eq!(
        reflects(&still, "static wall"),
        [false, true],
        "the floor reflects the static wall under All alone"
    );
    let moving = reflected_wall(gpu, false, MOVING, &[All, Moving]);
    assert_eq!(
        reflects(&moving, "moving wall"),
        [true, true],
        "the floor reflects the moving wall under both reaches"
    );
    let difference = largest_difference(&still[1], &moving[1]);
    eprintln!("static wall under All against moving wall under Moving: {difference}");
    assert!(
        difference < 1e-3,
        "the static wall reflects under All as the moving wall does under Moving: {difference}"
    );
}

// The same rays on the hardware path (the architecture's Hardware ray
// tracing), on a device with ray queries; reported unsupported, never
// passed, elsewhere. Plausible defects: the trace's pipeline composing the
// portable function set, binding no TLAS, or selecting static instances
// rather than moving ones on the hardware path, which then reflects
// nothing or something else; its cached group binding a TLAS the scene
// replaced, which still holds the wall 300 m ahead; a deforming instance missing from the
// hardware path's rays although its TLAS holds it as a moving instance; or
// one reaching the portable path's, which sees no deforming instance. The
// oracle is the geometry above: the hardware path reflects the wall as the
// portable path does, and a deforming wall in its place only on the
// hardware path.
#[test]
fn world_space_rays_on_the_hardware_path_reflect_deforming_instances() {
    let Some((device, queue)) = test_support::ray_tracing_device(|limits| limits) else {
        return;
    };
    for (hardware, wall, reflected) in [
        (true, MOVING, true),
        (true, DEFORMING, true),
        (false, DEFORMING, false),
    ] {
        let label = format!("hardware {hardware}, deforming {}", wall.deforms);
        assert_eq!(
            reflects(
                &reflected_wall((&device, &queue), hardware, wall, &[Moving]),
                &label
            ),
            [reflected],
            "{label}"
        );
    }
}

// The `All` reach on the hardware path, on a device with ray queries;
// reported unsupported, never passed, elsewhere. Plausible defects: `All`'s
// hardware query masking one kind (the static wall, or the deforming wall
// the TLAS holds as moving, missed), or tracing another function set than
// the portable path's. The oracle is the geometry above and the portable
// path: the static wall reflects under `All` alone, the deforming wall
// under `All` too, and the hardware path's static wall under `All` matches
// the portable path's within 1 % of the wall's radiance: both meet the
// same triangles, at distances that differ by the f32 rounding of two
// solves, which only weights the denoiser, and the composite is f16.
#[test]
fn the_all_reach_on_the_hardware_path_matches_the_portable_path() {
    let Some((device, queue)) = test_support::ray_tracing_device(|limits| limits) else {
        return;
    };
    let gpu = (&device, &queue);
    let hardware = reflected_wall(gpu, true, STATIC, &[Moving, All]);
    assert_eq!(
        reflects(&hardware, "hardware, static wall"),
        [false, true],
        "the floor reflects the static wall under All alone"
    );
    assert_eq!(
        reflects(
            &reflected_wall(gpu, true, DEFORMING, &[All]),
            "hardware, deforming wall"
        ),
        [true],
        "the floor reflects the deforming wall under All"
    );
    let portable = reflected_wall(gpu, false, STATIC, &[Moving, All]);
    let difference = largest_difference(&hardware[1], &portable[1]);
    eprintln!("All on the hardware path against the portable path: {difference}");
    assert!(
        difference < 0.01,
        "the hardware path's All matches the portable path's: {difference}"
    );
}

// A masked static wall under `All` on the hardware path, on a device with
// ray queries; reported unsupported, never passed, elsewhere. Under the
// candidate form (the architecture's Hardware ray tracing, *Candidate
// form*: Metal's, #211) the wall is in the TLAS and the candidate loop in
// this stage's trace, a compute program of its own, cuts its texels out,
// which the scene ray tests' dispatch does not compose; under the baseline it is a
// predicate instance on the portable walk. Plausible defects: the loop
// confirming cut-out candidates or never confirming kept ones, which
// reflects the wall whole or not at all; a confirmed candidate decoded as
// another triangle; the masked wall left off both the TLAS and the walk;
// or, under the candidate form, the wall kept on the walk (its BLAS
// pending), which would compare the walk with itself: `reflected_wall`
// asserts where the form puts it (`RayTracingStats`).
// The oracle is the portable path, whose walk cuts the same texels out, and
// the geometry above: the floor reflects the masked wall at a quarter to
// three quarters of the pixels where it reflects the whole wall, since half
// of the wall is cut out, and the hardware path's composite matches the
// portable path's within 1 % of the wall's radiance.
#[test]
fn a_masked_wall_cuts_out_on_the_hardware_path_as_on_the_portable_path() {
    let Some((device, queue)) = test_support::ray_tracing_device(|limits| limits) else {
        return;
    };
    let gpu = (&device, &queue);
    let shown = |composite: &[f32]| composite.iter().filter(|&&red| red > 0.25).count();
    let whole = shown(&reflected_wall(gpu, false, STATIC, &[All])[0]);
    let portable = reflected_wall(gpu, false, MASKED, &[All]);
    let cut = shown(&portable[0]);
    assert!(
        cut * 4 > whole && cut * 4 < whole * 3,
        "the floor reflects the masked wall at {cut} pixels, the whole wall at {whole}"
    );
    let hardware = reflected_wall(gpu, true, MASKED, &[All]);
    let difference = largest_difference(&hardware[0], &portable[0]);
    eprintln!("the masked wall on the hardware path against the portable path: {difference}");
    assert!(
        difference < 0.01,
        "the hardware path cuts the masked wall out as the portable path does: {difference}"
    );
}

/// A square facing +Y at height `y` over `x` and `z`, of one material of
/// base `base`, `metallic` and perceptual `roughness`.
fn plane(
    x: [f32; 2],
    z: [f32; 2],
    y: f32,
    base: f32,
    metallic: f32,
    roughness: f32,
) -> crate::asset::Asset {
    let mut asset = floor();
    asset.meshes[0].vertices = [(x[0], z[0]), (x[1], z[0]), (x[1], z[1]), (x[0], z[1])]
        .map(|(x, z)| Vertex {
            uv1: [0.0; 2],
            tangent: [0.; 4],
            lightmap_bounds: [0., 0., 1., 1.],
            lightmap_uv: [0.; 2],
            position: [x, y, z],
            normal: [0., 1., 0.],
            uv: [0.; 2],
            color: [1.; 4],
        })
        .to_vec();
    let material = &mut asset.materials[0];
    material.base = [base, base, base, 1.];
    material.metallic = metallic;
    material.roughness = roughness;
    asset
}

/// A frame size whose reduced grid (130 by 98) is no multiple of the
/// classification's tiles, so its last column and row of workgroups are
/// partial, and leaves a remainder column and row at full resolution; its
/// mirror needs more rays than one row of the trace's indirect dispatch
/// holds.
const COVERAGE: [u32; 2] = [261, 197];

/// A camera 2 m above a rough floor, looking down 45°, over a mirror on it
/// from x = -0.3 and from z = -3 towards the camera, past the frame's right
/// and bottom edges, which reflects an unlit white moving wall 100 m ahead,
/// out of view.
struct MirrorScene {
    scene: Scene,
    mirror: crate::InstanceId,
    input: FrameInput,
}

fn mirror_scene((device, queue): (&wgpu::Device, &wgpu::Queue), size: [u32; 2]) -> MirrorScene {
    let mut scene = Scene::new(device, queue);
    test_support::add_static(
        device,
        queue,
        &mut scene,
        plane([-50., 50.], [-50., 50.], 0., 0.2, 0., 0.9),
    );
    let model = scene
        .add_asset(
            device,
            queue,
            plane([-0.3, 20.], [-3., 1.], 0.002, 1., 1., 0.05),
        )
        .unwrap()
        .model;
    let mirror = scene
        .add_instance(device, queue, InstanceState::new(model), Mobility::Static)
        .unwrap();
    let model = scene.add_asset(device, queue, wall()).unwrap().model;
    scene
        .add_instance(
            device,
            queue,
            InstanceState {
                pose: Mat4::from_scale_rotation_translation(
                    Vec3::new(1200., 600., 10.),
                    glam::Quat::IDENTITY,
                    Vec3::new(0., 300., -100.),
                ),
                ..InstanceState::new(model)
            },
            Mobility::Moving,
        )
        .unwrap();
    let eye = Vec3::new(0., 2., 0.);
    let mut input = FrameInput::new(Camera {
        view: glam::camera::rh::view::look_at_mat4(eye, eye + Vec3::new(0., -1., -1.), Vec3::Y),
        projection: crate::perspective(1., size[0] as f32 / size[1] as f32, 0.1),
        eye,
    });
    input.backdrop = Backdrop::Color([0.; 3]);
    MirrorScene {
        scene,
        mirror,
        input,
    }
}

/// Renders `frame`'s scene once through `renderer` at `size`.
fn render_mirror(
    (device, queue): (&wgpu::Device, &wgpu::Queue),
    renderer: &mut TestRenderer,
    frame: &mut MirrorScene,
    settings: &Settings,
    size: [u32; 2],
) {
    let output = crate::view::targets::target(
        device,
        "world reflection coverage",
        size,
        crate::shading::gbuffer::COLOR,
    );
    let mut encoder = device.create_command_encoder(&Default::default());
    renderer.render(
        device,
        queue,
        &mut encoder,
        &mut frame.scene,
        &frame.input,
        settings,
        &output,
        None,
    );
    queue.submit([encoder.finish()]);
    renderer.finish_frame(&mut frame.scene);
}

fn mirror_settings(reach: settings::WorldSpaceReflections) -> Settings {
    Settings {
        antialiasing: settings::Antialiasing::Off,
        bloom: settings::Bloom::Off,
        atmosphere: false,
        screen_space_reflections: settings::ScreenSpaceReflections::Half,
        world_space_reflections: reach,
        ..Settings::default()
    }
}

// The classification's coverage of the tracing grid, on the first frame,
// where every tracing pixel starts as a miss. Plausible defects: the
// classification dispatched over whole tiles alone, so the grid's last
// partial column and row of tiles list nothing; rays listed past the first
// row of the trace's indirect dispatch left untraced, or traced at another
// tracing pixel than their own (a wrong ray texel, packing or row); a
// workgroup's rays written over another's (a wrong base); a tracing pixel
// that needs a ray left off the list. The oracle is geometric: every ray
// over the mirror meets the unlit white wall, so every tracing pixel whose
// full-resolution block lies on the mirror (the jitter picks a pixel of
// its block) holds a hit in the trace's radiance target (alpha 1, the
// share of its rays that hit), and, through the composite against the same
// frame without world-space reflections, every mirror pixel at least 4
// pixels inside the mirror (the denoiser's reach; the frame's edges count
// as inside, since the mirror passes them), the remainder column and row
// included, gains the wall's reflection, about 1 (a metallic white mirror
// of roughness 0.05, a black sky). The mirror's pixels come from the
// G-buffer's source identities; more tracing pixels lie on it than one row
// of the trace's dispatch holds.
#[test]
fn every_tracing_pixel_over_the_mirror_hits_and_every_mirror_pixel_reflects() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let gpu = (&device, &queue);
    let size = COVERAGE;
    let mut composites = Vec::new();
    let mut identities = Vec::new();
    let mut hits = Vec::new();
    let mut reduced = [0; 2];
    for reach in [settings::WorldSpaceReflections::Off, Moving] {
        let settings = mirror_settings(reach);
        let mut renderer = TestRenderer::for_test(&device, &queue, size, &settings);
        let mut frame = mirror_scene(gpu, size);
        frame.input.camera_cut = true;
        render_mirror(gpu, &mut renderer, &mut frame, &settings, size);
        composites.push(composite(&device, &queue, &renderer));
        let ids = test_support::read(&device, &queue, renderer.targets().source_id.texture(), 8);
        let mirror = crate::diagnostics::source_id(frame.mirror);
        identities = bytemuck::cast_slice::<u8, [u32; 2]>(&ids)
            .iter()
            .map(|id| id[0] == mirror)
            .collect::<Vec<_>>();
        if let Some(world) = renderer.world_reflections() {
            let (radiance, grid) = world.test_radiance();
            reduced = grid;
            hits = test_support::read(&device, &queue, radiance.texture(), 8)
                .chunks_exact(8)
                .map(|texel| test_support::half(&texel[6..8]))
                .collect();
        }
    }
    let [width, height] = size.map(|side| side as i32);
    let mirror = |x: i32, y: i32| identities[(y * width + x) as usize];
    let mirror_pixels = identities.iter().filter(|&&on| on).count() as u32;
    assert!(
        mirror_pixels / 4 > super::classify::GROUP_ROW * super::classify::TRACE_THREADS,
        "the mirror's {mirror_pixels} pixels need more than one row of the trace's dispatch"
    );
    let [rw, rh] = reduced.map(|side| side as i32);
    assert_eq!(
        reduced,
        [130, 98],
        "the reduced grid is no multiple of the tiles"
    );
    let (mut over, mut missed) = (0, Vec::new());
    for y in 0..rh {
        for x in 0..rw {
            if (0..2).all(|dy| (0..2).all(|dx| mirror(2 * x + dx, 2 * y + dy))) {
                over += 1;
                if hits[(y * rw + x) as usize] < 0.5 {
                    missed.push((x, y));
                }
            }
        }
    }
    let partial = |&(x, y): &(i32, i32)| x >= rw / 8 * 8 || y >= rh / 8 * 8;
    assert!(over > 1000, "{over} tracing pixels over the mirror");
    assert!(
        missed.is_empty(),
        "{} of {over} tracing pixels over the mirror hold no hit ({} in the last partial tiles): {:?}",
        missed.len(),
        missed.iter().filter(|pixel| partial(pixel)).count(),
        &missed[..missed.len().min(8)]
    );
    let interior = |x: i32, y: i32| {
        (-4..=4).all(|dy| {
            (-4..=4).all(|dx| {
                let (x, y) = (x + dx, y + dy);
                x < 0 || y < 0 || x >= width || y >= height || mirror(x, y)
            })
        })
    };
    let (mut inside, mut dark, mut least) = (0, Vec::new(), f32::MAX);
    for y in 0..height {
        for x in 0..width {
            if mirror(x, y) && interior(x, y) {
                let index = (y * width + x) as usize;
                let gain = composites[1][index] - composites[0][index];
                inside += 1;
                least = least.min(gain);
                if gain <= 0.9 {
                    dark.push((x, y, gain));
                }
            }
        }
    }
    eprintln!(
        "{over} tracing pixels over the mirror; {inside} interior mirror pixels, least gain {least}"
    );
    let remainder = |&(x, y, _): &(i32, i32, f32)| x == width - 1 || y == height - 1;
    assert!(inside > 1000, "{inside} interior mirror pixels");
    assert!(
        dark.is_empty(),
        "{} of {inside} interior mirror pixels gain less than 0.9 ({} in the remainder column or row): {:?}",
        dark.len(),
        dark.iter().filter(|pixel| remainder(pixel)).count(),
        &dark[..dark.len().min(8)]
    );
}

/// The rays the classification lists over a `size` frame of one receiver
/// everywhere: lit, traced perceptual `roughness`, uncoated, under no
/// blended receiver, where the screen-space method returned `confidence`
/// and has Velvet's cutoff and fade.
fn listed_rays(
    (device, queue): (&wgpu::Device, &wgpu::Queue),
    size: [u32; 2],
    roughness: f32,
    confidence: f32,
) -> u32 {
    let texels = (size[0] * size[1]) as usize;
    let material =
        test_support::hdr_texture(device, queue, size, &vec![[0., roughness, 0., 1.]; texels]);
    let screen_space =
        test_support::hdr_texture(device, queue, size, &vec![[0., 0., 0., confidence]; texels]);
    let target = |label, format| crate::view::targets::target(device, label, size, format);
    let f0 = target("lit F0", crate::shading::gbuffer::F0);
    let depth = target("receiver depth", crate::shading::gbuffer::DEPTH);
    let mut encoder = device.create_command_encoder(&Default::default());
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("lit F0"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: &f0,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color::WHITE),
                store: wgpu::StoreOp::Store,
            },
        })],
        ..Default::default()
    });
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("receiver depth"),
        depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
            view: &depth,
            depth_ops: Some(wgpu::Operations {
                load: wgpu::LoadOp::Clear(0.5),
                store: wgpu::StoreOp::Store,
            }),
            stencil_ops: None,
        }),
        ..Default::default()
    });
    let targets = super::Targets::new(device, size);
    let [rw, rh] = targets.reduced.map(|side| side as f32);
    let [w, h] = size.map(|side| side as f32);
    let identity = Mat4::IDENTITY.to_cols_array_2d();
    let cutoff = crate::stages::reflections::velvet::ROUGHNESS_CUTOFF;
    let params = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("world reflection parameters"),
        size: std::mem::size_of::<super::Params>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    queue.write_buffer(
        &params,
        0,
        bytemuck::bytes_of(&super::Params {
            inverse_view_projection: identity,
            previous_view_projection: identity,
            eye: [0., 0., 0., 0.1],
            full: [w, h, 1. / w, 1. / h],
            reduced: [rw, rh, 1. / rw, 1. / rh],
            frame: 0,
            rays: 0,
            traced: cutoff * cutoff,
            range: super::RANGE,
            previous_near: 0.1,
            fade: crate::stages::reflections::velvet::ROUGHNESS_FADE,
            padding: [0; 2],
        }),
    );
    let mut classify = super::classify::Classify::new(device);
    classify.encode(
        device,
        &mut encoder,
        targets.reduced,
        super::classify::Bindings {
            targets: [&targets.indirect, &targets.direction_pdf, &targets.length],
            lists: &targets.lists,
            depth: &depth,
            material: &material,
            f0: &f0,
            params: &params,
            screen_space: &screen_space,
            surface_depth: &depth,
        },
        None,
    );
    queue.submit([encoder.finish()]);
    test_support::read_words(device, queue, &targets.lists.count)[0]
}

// Plausible defect: the classification treats the screen-space method's
// confidence as the share of the lobe it resolved, so a confident receiver
// inside the roughness fade traces no ray, although composition takes the
// method's result times the fade and leaves the rest, 1 − confidence ×
// fade, to the fallback that world-space rays fill (specular_traced). The
// oracle is that composition rule: in the middle of Velvet's fade the
// method's share is a half (the fade's smoothstep at its midpoint), so a
// fully confident receiver leaves half its lobe to the fallback and every
// tracing pixel needs a ray; below the fade the method takes the whole
// lobe, so none does.
#[test]
fn confident_receivers_in_the_roughness_fade_still_trace_world_rays() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let gpu = (&device, &queue);
    let size = [32, 16];
    let tracing = size[0] / super::DOWNSCALE * (size[1] / super::DOWNSCALE);
    let velvet = crate::stages::reflections::velvet::ROUGHNESS_CUTOFF;
    let fade = crate::stages::reflections::velvet::ROUGHNESS_FADE;
    let in_fade = listed_rays(gpu, size, velvet - fade / 2., 1.);
    let below_fade = listed_rays(gpu, size, velvet - fade * 2., 1.);
    assert_eq!(
        in_fade, tracing,
        "every confident receiver in the fade leaves half its lobe to world rays"
    );
    assert_eq!(
        below_fade, 0,
        "a confident receiver below the fade takes its whole lobe from the screen-space method"
    );
}

/// Each tracing pixel's `world_traced_pixel`, x and y, row by row.
static TRACED_PIXELS: crate::shading::Module = crate::shading::Module {
    name: "world_traced_pixel_test",
    source: "@group(0) @binding(0) var<storage,read_write> traced:array<vec2<i32>>;\n@compute @workgroup_size(8,8) fn traced_pixels(@builtin(global_invocation_id) id:vec3<u32>) {\n let reduced=vec2<u32>(world.reduced.xy);\n if all(id.xy<reduced) {\n  traced[id.x+id.y*reduced.x]=world_traced_pixel(id.xy);\n }\n}\n",
    deps: &[&super::COMMON],
};

/// The full-resolution pixel each tracing pixel of a `size` frame traces on
/// frame `frame`, row by row over the reduced grid.
fn traced_pixels(
    (device, queue): (&wgpu::Device, &wgpu::Queue),
    size: [u32; 2],
    frame: u32,
) -> Vec<[i32; 2]> {
    let reduced = super::Targets::new(device, size).reduced;
    let [rw, rh] = reduced.map(|side| side as f32);
    let [w, h] = size.map(|side| side as f32);
    let identity = Mat4::IDENTITY.to_cols_array_2d();
    let params = wgpu::util::DeviceExt::create_buffer_init(
        device,
        &wgpu::util::BufferInitDescriptor {
            label: Some("world reflection parameters"),
            contents: bytemuck::bytes_of(&super::Params {
                inverse_view_projection: identity,
                previous_view_projection: identity,
                eye: [0., 0., 0., 0.1],
                full: [w, h, 1. / w, 1. / h],
                reduced: [rw, rh, 1. / rw, 1. / rh],
                frame,
                rays: 0,
                traced: 0.,
                range: super::RANGE,
                previous_near: 0.1,
                fade: 0.,
                padding: [0; 2],
            }),
            usage: wgpu::BufferUsages::UNIFORM,
        },
    );
    let output = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("traced pixels"),
        size: 8 * u64::from(reduced[0] * reduced[1]),
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("traced pixels"),
        source: wgpu::ShaderSource::Wgsl(crate::shading::compose(&[&TRACED_PIXELS]).into()),
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("traced pixels"),
        layout: None,
        module: &module,
        entry_point: Some("traced_pixels"),
        compilation_options: Default::default(),
        cache: None,
    });
    let group = |index, binding, buffer: &wgpu::Buffer| {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &pipeline.get_bind_group_layout(index),
            entries: &[wgpu::BindGroupEntry {
                binding,
                resource: buffer.as_entire_binding(),
            }],
        })
    };
    let (outputs, world) = (group(0, 0, &output), group(3, 4, &params));
    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_compute_pass(&Default::default());
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &outputs, &[]);
        pass.set_bind_group(3, &world, &[]);
        pass.dispatch_workgroups(reduced[0].div_ceil(8), reduced[1].div_ceil(8), 1);
    }
    queue.submit([encoder.finish()]);
    test_support::read_words(device, queue, &output)
        .chunks_exact(2)
        .map(|pixel| [pixel[0] as i32, pixel[1] as i32])
        .collect()
}

// Plausible defects: a jitter of 2 on the frames where world_random's
// hash rounds up to 1 (frame 4181424 on x, 9574095 on y), so every tracing
// pixel traces the next block's pixel and the last block of an even side
// traces outside the frame; or, on a side of 1 pixel, whose reduced grid
// still has one tracing pixel, a jitter of 1 tracing the pixel past the
// frame. The oracle is the tracing grid's definition: tracing pixel t
// traces a pixel of its own block, t × 2 to t × 2 + 1, inside the frame.
// Frames 0 to 7 give each axis both jitters.
#[test]
fn every_traced_pixel_lies_in_its_block_and_the_frame() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let frames = (0..8).chain([4_181_424, 9_574_095]);
    for frame in frames {
        for size in [[8, 6], [1, 4], [5, 1]] {
            let reduced = super::Targets::new(&device, size).reduced;
            let pixels = traced_pixels((&device, &queue), size, frame);
            for (index, pixel) in pixels.iter().enumerate() {
                let tracing = [index as u32 % reduced[0], index as u32 / reduced[0]];
                for axis in 0..2 {
                    let block = tracing[axis] * super::DOWNSCALE;
                    let end = (block + super::DOWNSCALE).min(size[axis]);
                    assert!(
                        (block as i32..end as i32).contains(&pixel[axis]),
                        "frame {frame}, size {size:?}: tracing pixel {tracing:?} traces {pixel:?}"
                    );
                }
            }
        }
    }
}

// All reach variants of a lighting specialisation share its retention slot.
// Exercise real trace pipelines so dropped/recompiled resources are observable.
#[test]
fn world_trace_pipelines_retain_current_and_previous() {
    use super::WorldReflections;
    use crate::view::pipelines::LitConstants;
    let Some((device, _queue)) = test_support::device() else {
        return;
    };
    let lit = crate::shading::bind::lit(&device, crate::shading::bind::BindingTier::Basic);
    let scene = crate::shading::bind::scene(&device);
    let mut stage = WorldReflections::new(&device, &lit, &scene, SIZE);
    let a = LitConstants::default();
    let b = LitConstants {
        rect_lights: true,
        ..a
    };
    let c = LitConstants {
        rect_lights: true,
        films: true,
        ..a
    };
    for all in [false, true] {
        stage.trace(&device, a, None, all);
    }
    let originals = [false, true].map(|all| stage.trace[&((a, all), None)].clone());
    stage.trace(&device, b, None, false);
    stage.trace(&device, b, None, true);
    stage.trace(&device, a, None, false);
    for (all, original) in [false, true].into_iter().zip(originals) {
        assert_eq!(
            stage.trace[&((a, all), None)],
            original,
            "returning to A reuses each reach's pipeline"
        );
    }
    for all in [false, true] {
        stage.trace(&device, b, None, all);
    }
    let previous = [false, true].map(|all| stage.trace[&((b, all), None)].clone());
    stage.trace(&device, c, None, false);
    assert!(
        !stage.trace.keys().any(|&((lit, _), _)| lit == a),
        "C drops every A reach"
    );
    for (all, pipeline) in [false, true].into_iter().zip(previous) {
        assert_eq!(
            stage.trace[&((b, all), None)],
            pipeline,
            "C keeps every B reach"
        );
    }
    assert!(
        stage.trace.contains_key(&((c, false), None)),
        "C is compiled"
    );
}

/// The full-resolution side of the temporal fixture, a power of two so that
/// its traced grid's texel centres are exact in UV, and the frames a blended
/// receiver crosses it before it is removed.
const TEMPORAL_SIZE: u32 = 64;
const RECEIVER_FRAMES: u32 = 9;
/// The receiver's full-resolution rows, and its width; it moves a traced
/// texel (2 pixels) a frame from `receiver_left(0)`.
const RECEIVER_ROWS: std::ops::Range<u32> = 16..48;
const RECEIVER_WIDTH: u32 = 16;
fn receiver_left(frame: u32) -> u32 {
    8 + 2 * frame
}

/// Resolved world-ray radiance at traced texel `(x, y)` on content frame
/// `frame`: varied (0.5 to 1.5 per channel, a new draw each frame) so that
/// accumulated history differs from any one frame's rays and the colour
/// box admits history, and, under a receiver, the near-empty colour the
/// resolve leaves where the classification traced nothing.
fn resolved(x: u32, y: u32, frame: u32, covered: bool) -> [f32; 4] {
    if covered {
        return [0.00001; 4];
    }
    std::array::from_fn(|channel| {
        let mut h = x.wrapping_mul(73_856_093)
            ^ y.wrapping_mul(19_349_663)
            ^ frame.wrapping_mul(83_492_791)
            ^ (channel as u32).wrapping_mul(2_654_435_761);
        h ^= h >> 13;
        h = h.wrapping_mul(0x5bd1_e995);
        h ^= h >> 15;
        if channel == 3 {
            1.
        } else {
            0.5 + (h & 0xffff) as f32 / 65535.
        }
    })
}

/// Fills its viewport at the viewport's depth: a blended receiver drawn
/// over a copy of the opaque depth makes the surface depth (the Surface
/// contract).
fn receiver_depth(device: &wgpu::Device) -> wgpu::RenderPipeline {
    let fill = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("receiver depth"),
        source: wgpu::ShaderSource::Wgsl(
            "@vertex fn fill(@builtin(vertex_index) i:u32)->@builtin(position) vec4<f32> {\n let uv=vec2<f32>(f32((i<<1u)&2u),f32(i&2u));\n return vec4(uv*2.-1.,0.,1.);\n}\n"
                .into(),
        ),
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("receiver depth"),
        layout: None,
        vertex: wgpu::VertexState {
            module: &fill,
            entry_point: Some("fill"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        fragment: None,
        primitive: Default::default(),
        depth_stencil: Some(wgpu::DepthStencilState {
            format: crate::shading::gbuffer::DEPTH,
            depth_write_enabled: Some(true),
            depth_compare: Some(wgpu::CompareFunction::Always),
            stencil: Default::default(),
            bias: Default::default(),
        }),
        multisample: Default::default(),
        multiview_mask: None,
        cache: None,
    })
}

/// Clears `view` to a floor facing the camera 10 m away and, over
/// `receiver`'s full-resolution pixels (left, top, width, height), draws a
/// blended receiver 5 m away.
fn fixture_depth(
    encoder: &mut wgpu::CommandEncoder,
    view: &wgpu::TextureView,
    fill: &wgpu::RenderPipeline,
    receiver: Option<[u32; 4]>,
) {
    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("fixture depth"),
        depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
            view,
            depth_ops: Some(wgpu::Operations {
                load: wgpu::LoadOp::Clear(0.01),
                store: wgpu::StoreOp::Store,
            }),
            stencil_ops: None,
        }),
        ..Default::default()
    });
    if let Some([left, top, width, height]) = receiver {
        pass.set_pipeline(fill);
        pass.set_viewport(
            left as f32,
            top as f32,
            width as f32,
            height as f32,
            0.02,
            0.02,
        );
        pass.draw(0..3, 0..1);
    }
}

/// One frame of the temporal fixture: the stage's history count, the
/// content frame of its rays, and the receiver's left edge, if it holds one.
#[derive(Clone, Copy)]
struct TemporalFrame {
    count: u32,
    content: u32,
    receiver: Option<u32>,
}

/// The production temporal pass's colour output after `frames`, run in
/// order over one history pair: a static camera 10 m in front of a lit,
/// traced floor facing it (the opaque depth), and, where a frame holds a
/// receiver, a blended receiver 5 m away drawn over a copy of that depth
/// (the surface depth) and moving right, whose motion the G-buffer's
/// motion holds under it (the Surface contract).
fn temporal_output(
    (device, queue): (&wgpu::Device, &wgpu::Queue),
    frames: &[TemporalFrame],
) -> Vec<[f32; 4]> {
    use wgpu::util::DeviceExt;
    let size = [TEMPORAL_SIZE; 2];
    let reduced = size.map(|side| side / super::DOWNSCALE);
    let full_texels = (size[0] * size[1]) as usize;
    let normal = test_support::hdr_texture(device, queue, size, &vec![[0.; 4]; full_texels]);
    let material =
        test_support::hdr_texture(device, queue, size, &vec![[0., 0.2, 0., 1.]; full_texels]);
    let f0 = test_support::hdr_texture(device, queue, size, &vec![[1.; 4]; full_texels]);
    let traced = |label, format, usage| super::texture(device, label, reduced, format, usage);
    let hdr = crate::shading::gbuffer::COLOR;
    let r32 = wgpu::TextureFormat::R32Float;
    let written = wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC;
    let colour = [0, 1].map(|_| traced("temporal colour", hdr, written));
    let variance = [0, 1].map(|_| traced("temporal variance", hdr, written));
    let depth_history = [0, 1].map(|_| traced("temporal depth", r32, written));
    let reprojection = traced("hit depth", r32, wgpu::TextureUsages::COPY_DST);
    queue.write_texture(
        reprojection.texture().as_image_copy(),
        bytemuck::cast_slice(&vec![0.005f32; (reduced[0] * reduced[1]) as usize]),
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(reduced[0] * 4),
            rows_per_image: Some(reduced[1]),
        },
        reprojection.texture().size(),
    );
    let variance_current = test_support::hdr_texture(
        device,
        queue,
        reduced,
        &vec![[0.01; 4]; (reduced[0] * reduced[1]) as usize],
    );
    let depth_target =
        |label| crate::view::targets::target(device, label, size, crate::shading::gbuffer::DEPTH);
    let opaque_depth = depth_target("floor depth");
    let surface_depth = depth_target("surface depth");
    let receiver_pass = receiver_depth(device);
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("world reflection temporal"),
        source: wgpu::ShaderSource::Wgsl(crate::shading::compose(&[&super::DENOISE]).into()),
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("world reflection temporal"),
        layout: None,
        module: &shader,
        entry_point: Some(super::WORLD_TEMPORAL_ENTRY),
        compilation_options: Default::default(),
        cache: None,
    });
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });
    let bind = |index, entries: &[(u32, wgpu::BindingResource)]| {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &pipeline.get_bind_group_layout(index),
            entries: &entries
                .iter()
                .map(|(binding, resource)| wgpu::BindGroupEntry {
                    binding: *binding,
                    resource: resource.clone(),
                })
                .collect::<Vec<_>>(),
        })
    };
    let projection = crate::perspective(std::f32::consts::FRAC_PI_2, 1., 0.1);
    let [w, h] = size.map(|v| v as f32);
    let [rw, rh] = reduced.map(|v| v as f32);
    let covered = |receiver: Option<u32>, x: u32, y: u32| {
        receiver.is_some_and(|left| {
            (left..left + RECEIVER_WIDTH).contains(&x) && RECEIVER_ROWS.contains(&y)
        })
    };
    let mut written = 0;
    for (index, frame) in frames.iter().enumerate() {
        written = index % 2;
        let previous = 1 - written;
        let texture = wgpu::BindingResource::TextureView;
        let current = test_support::hdr_texture(
            device,
            queue,
            reduced,
            &(0..reduced[1])
                .flat_map(|y| (0..reduced[0]).map(move |x| (x, y)))
                .map(|(x, y)| {
                    let d = super::DOWNSCALE;
                    resolved(x, y, frame.content, covered(frame.receiver, x * d, y * d))
                })
                .collect::<Vec<_>>(),
        );
        // Current minus previous UV, a frame's move, under the receiver;
        // every surface keeps its view depth, validly (gbuffer.wgsl).
        let motion = test_support::hdr_texture(
            device,
            queue,
            size,
            &(0..size[1])
                .flat_map(|y| (0..size[0]).map(move |x| (x, y)))
                .map(|(x, y)| {
                    if covered(frame.receiver, x, y) {
                        [2. / w, 0., 0., 1.]
                    } else {
                        [0., 0., 0., 1.]
                    }
                })
                .collect::<Vec<_>>(),
        );
        let params = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: None,
            contents: bytemuck::bytes_of(&super::Params {
                inverse_view_projection: projection.inverse().to_cols_array_2d(),
                previous_view_projection: projection.to_cols_array_2d(),
                eye: [0., 0., 0., 0.1],
                full: [w, h, 1. / w, 1. / h],
                reduced: [rw, rh, 1. / rw, 1. / rh],
                frame: frame.count,
                rays: 0,
                traced: 0.49,
                range: super::RANGE,
                previous_near: 0.1,
                fade: 0.1,
                padding: [0; 2],
            }),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        fixture_depth(&mut encoder, &opaque_depth, &receiver_pass, None);
        fixture_depth(
            &mut encoder,
            &surface_depth,
            &receiver_pass,
            frame.receiver.map(|left| {
                [
                    left,
                    RECEIVER_ROWS.start,
                    RECEIVER_WIDTH,
                    RECEIVER_ROWS.len() as u32,
                ]
            }),
        );
        let inputs = bind(
            0,
            &[
                (8, wgpu::BindingResource::Sampler(&sampler)),
                (30, texture(&current)),
                (31, texture(&colour[previous])),
                (32, texture(&variance_current)),
                (33, texture(&variance[previous])),
                (34, texture(&reprojection)),
                (35, texture(&motion)),
                (36, texture(&depth_history[previous])),
                (40, texture(&colour[written])),
                (41, texture(&variance[written])),
                (42, texture(&depth_history[written])),
            ],
        );
        let receivers = bind(
            3,
            &[
                (0, texture(&opaque_depth)),
                (1, texture(&normal)),
                (2, texture(&material)),
                (3, texture(&f0)),
                (4, params.as_entire_binding()),
                (7, texture(&surface_depth)),
            ],
        );
        {
            let mut pass = encoder.begin_compute_pass(&Default::default());
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &inputs, &[]);
            pass.set_bind_group(3, &receivers, &[]);
            pass.dispatch_workgroups(reduced[0].div_ceil(8), reduced[1].div_ceil(8), 1);
        }
        queue.submit([encoder.finish()]);
    }
    test_support::read(device, queue, colour[written].texture(), 8)
        .chunks_exact(8)
        .map(|texel| std::array::from_fn(|c| test_support::half(&texel[c * 2..c * 2 + 2])))
        .collect()
}

// Plausible defect: under a blended receiver the classification traces no
// world ray, but the temporal pass keeps accumulating the opaque surface's
// history there, reprojected by the receiver's motion and tested against
// the opaque depth, which agrees with itself; when the receiver moves on,
// the uncovered texels blend this frame's rays with that dragged, emptied
// history at Wicked's 0.95 response, with no disocclusion raised. Oracle:
// an independent run that never had the receiver and restarts its history
// on the frame the receiver is removed, whose output is that frame's rays
// whole; the uncovered texels must equal it. Texels at least a texel inside the
// last covered region are compared, since Wicked's disocclusion search
// takes a valid neighbour's history within one texel of a disoccluded
// one. Texels two or more from the receiver's whole path, whose inputs
// and 3 by 3 colour box are the same with and without it, must equal a
// run that never had the receiver, over all its frames: the fix keeps
// their accumulation, which differs from their current rays.
#[test]
fn texels_a_receiver_uncovers_take_their_current_rays_whole() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let gpu = (&device, &queue);
    let removed = RECEIVER_FRAMES;
    let frames = |receiver: bool| {
        (0..=removed)
            .map(|frame| TemporalFrame {
                count: frame,
                content: frame,
                receiver: (receiver && frame < removed).then(|| receiver_left(frame)),
            })
            .collect::<Vec<_>>()
    };
    let crossed = temporal_output(gpu, &frames(true));
    let never = temporal_output(gpu, &frames(false));
    let restarted = temporal_output(
        gpu,
        &[TemporalFrame {
            count: 0,
            content: removed,
            receiver: None,
        }],
    );
    let side = TEMPORAL_SIZE / super::DOWNSCALE;
    let traced =
        |full: std::ops::Range<u32>| full.start / super::DOWNSCALE..full.end / super::DOWNSCALE;
    let last = receiver_left(removed - 1);
    let last_x = traced(last..last + RECEIVER_WIDTH);
    let rows = traced(RECEIVER_ROWS);
    let path_x = traced(receiver_left(0)..last + RECEIVER_WIDTH);
    let mut uncovered = Vec::new();
    let mut kept = Vec::new();
    let mut accumulated = 0;
    for y in 0..side {
        for x in 0..side {
            let i = (y * side + x) as usize;
            let inside = |range: &std::ops::Range<u32>, v: u32, margin: u32| {
                v >= range.start + margin && v + margin < range.end
            };
            if inside(&last_x, x, 1) && inside(&rows, y, 1) {
                if crossed[i] != restarted[i] {
                    uncovered.push(((x, y), crossed[i], restarted[i]));
                }
            } else {
                let near =
                    |range: &std::ops::Range<u32>, v: u32| v + 2 > range.start && v < range.end + 1;
                if !(near(&path_x, x) && near(&rows, y)) {
                    accumulated += usize::from(never[i] != restarted[i]);
                    if crossed[i] != never[i] {
                        kept.push(((x, y), crossed[i], never[i]));
                    }
                }
            }
        }
    }
    eprintln!(
        "uncovered texels unlike a restarted run: {}; texels off the receiver's path unlike a run without it: {}; of those, accumulated: {accumulated}",
        uncovered.len(),
        kept.len()
    );
    assert!(
        uncovered.is_empty(),
        "{} uncovered texels kept history from under the receiver: {:?}",
        uncovered.len(),
        &uncovered[..uncovered.len().min(4)]
    );
    assert!(
        accumulated > 0,
        "the fixture must accumulate history off the receiver's path"
    );
    assert!(
        kept.is_empty(),
        "{} texels off the receiver's path differ from a run without it: {:?}",
        kept.len(),
        &kept[..kept.len().min(4)]
    );
}

/// The upsample fixture's full-resolution side; its floor's roughness,
/// where the blur takes its largest radius; and the full-resolution
/// columns a blended receiver covers, an odd count, so that one traced
/// texel straddles its edge.
const UPSAMPLE_SIZE: u32 = 64;
const UPSAMPLE_ROUGHNESS: f32 = 0.125;
const UPSAMPLE_EDGE: u32 = 33;

/// The production resolve, then the production upsample, over a static
/// camera 10 m in front of a lit, traced floor facing it, which, with
/// `receiver`, a blended receiver 5 m away covers left of `UPSAMPLE_EDGE`.
/// Rays hit uniform radiance but for a checkerboard at traced columns 24 on,
/// and hold nothing where the receiver covers a traced texel's first pixel,
/// as the classification leaves them. Its variance is `variance`, standing
/// in for noisy rays (uniform rays have none) (`upsampled_floor`).
fn upsampled(gpu: (&wgpu::Device, &wgpu::Queue), receiver: bool, variance: f32) -> Vec<[f32; 4]> {
    upsampled_floor(
        gpu,
        &UpsampleFloor {
            roughness: UPSAMPLE_ROUGHNESS,
            empty: receiver.then_some(Empty {
                kind: EmptyKind::Receiver,
                rows: false,
                edge: UPSAMPLE_EDGE,
            }),
            variance,
            radiance: &|[x, y]| {
                if x >= 24 && (x + y) % 2 == 0 {
                    [0.2, 0.2, 0.2, 1.]
                } else {
                    UNIFORM
                }
            },
        },
    )
}

/// The uniform radiance the upsample fixture's rays hit.
const UNIFORM: [f32; 4] = [0.6, 0.8, 1., 1.];

/// What holds no rays where an upsample fixture's floor has an empty region:
/// an opaque surface the screen-space method does not trace, or one under a
/// blended receiver 5 m away.
#[derive(Clone, Copy, Debug)]
enum EmptyKind {
    Untraced,
    Receiver,
}

/// An upsample fixture's empty region: the full-resolution pixels left of
/// column `edge`, or above row `edge` where `rows`.
#[derive(Clone, Copy, Debug)]
struct Empty {
    kind: EmptyKind,
    rows: bool,
    edge: u32,
}

impl Empty {
    fn contains(&self, [x, y]: [u32; 2]) -> bool {
        (if self.rows { y } else { x }) < self.edge
    }
}

/// An upsample fixture: a floor of perceptual `roughness`, the region whose
/// pixels hold no rays, the variance of the texels that hold rays, and the
/// radiance their rays hit at each traced texel.
struct UpsampleFloor<'a> {
    roughness: f32,
    empty: Option<Empty>,
    variance: f32,
    radiance: &'a dyn Fn([u32; 2]) -> [f32; 4],
}

/// A second traced surface over the full-resolution pixels left of column
/// `edge`, or above row `edge` where `rows`, of an upsample fixture: 5 m
/// away, in front of the floor, where `nearer`, else at the floor's depth;
/// of the floor's roughness and the unit `normal`; its rays, cast along that
/// normal, hit `radiance`.
#[derive(Clone, Copy, Debug)]
struct Near {
    rows: bool,
    edge: u32,
    nearer: bool,
    normal: [f32; 3],
    radiance: [f32; 4],
}

impl Near {
    fn contains(&self, [x, y]: [u32; 2]) -> bool {
        (if self.rows { y } else { x }) < self.edge
    }
}

/// The floor's normal, facing the camera.
const FACING: [f32; 3] = [0., 0., 1.];

/// `normal` as the G-buffer encodes it (gbuffer_octahedral_encode), for
/// normals in the upper hemisphere (z at least 0), whose encoding is the
/// projection onto the octahedron.
fn octahedral([x, y, z]: [f32; 3]) -> [f32; 2] {
    assert!(z >= 0.);
    let sum = x.abs() + y.abs() + z.abs();
    [x / sum, y / sum]
}

/// `upsampled_surfaces` without a second surface.
fn upsampled_floor(gpu: (&wgpu::Device, &wgpu::Queue), floor: &UpsampleFloor) -> Vec<[f32; 4]> {
    upsampled_surfaces(gpu, floor, None)
}

/// The upsample fixture's floor: its device depth over the frame,
/// `a + b u + c v` at UV (u, v) (0 at the top left) where `tilt` holds
/// `[a, b, c]`, else 10 m away facing the camera; and whether the temporal
/// pass takes the resolve's output, as the stage runs it, or the rays
/// themselves, so that the upsample reads them unchanged.
#[derive(Clone, Copy)]
struct Plane {
    tilt: Option<[f32; 3]>,
    resolved: bool,
}

/// The fixture's floor 10 m away, as the stage runs it.
const LEVEL: Plane = Plane {
    tilt: None,
    resolved: true,
};

/// `upsampled_plane` over the floor 10 m away.
fn upsampled_surfaces(
    gpu: (&wgpu::Device, &wgpu::Queue),
    floor: &UpsampleFloor,
    near: Option<Near>,
) -> Vec<[f32; 4]> {
    upsampled_plane(gpu, floor, near, LEVEL)
}

/// Draws a plane over the whole of its target at the device depth
/// `a + b u + c v` at UV (u, v) (0 at the top left), which the rasterizer
/// interpolates affinely in screen space.
fn tilted_depth(device: &wgpu::Device, [a, b, c]: [f32; 3]) -> wgpu::RenderPipeline {
    let plane = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("tilted depth"),
        source: wgpu::ShaderSource::Wgsl(
            format!(
                "@vertex fn plane(@builtin(vertex_index) i:u32)->@builtin(position) vec4<f32> {{\n let uv=vec2<f32>(f32(i&1u),f32(i>>1u));\n return vec4(uv*2.-1.,{a:.6}+{b:.6}*uv.x+{c:.6}*(1.-uv.y),1.);\n}}\n"
            )
            .into(),
        ),
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("tilted depth"),
        layout: None,
        vertex: wgpu::VertexState {
            module: &plane,
            entry_point: Some("plane"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        fragment: None,
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleStrip,
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: crate::shading::gbuffer::DEPTH,
            depth_write_enabled: Some(true),
            depth_compare: Some(wgpu::CompareFunction::Always),
            stencil: Default::default(),
            bias: Default::default(),
        }),
        multisample: Default::default(),
        multiview_mask: None,
        cache: None,
    })
}

/// The production resolve, then the production upsample, over a static
/// camera in front of a lit floor, `plane`, and `near` where it holds a
/// pixel. The floor's rays hit `radiance` and hold nothing where
/// `empty` holds a traced texel's first pixel, as the classification leaves
/// them; there, the floor is rough enough that the screen-space method does
/// not trace it, or a blended receiver covers it. The production temporal
/// pass runs between them on a first frame, which passes the resolve's
/// colour through, and its variance, which stands in for noisy rays
/// (uniform rays have none), is `variance` where a texel holds rays and 0
/// where it does not.
fn upsampled_plane(
    (device, queue): (&wgpu::Device, &wgpu::Queue),
    floor: &UpsampleFloor,
    near: Option<Near>,
    plane: Plane,
) -> Vec<[f32; 4]> {
    use wgpu::util::DeviceExt;
    let near_at = |pixel| near.filter(|near| near.contains(pixel));
    let size = [UPSAMPLE_SIZE; 2];
    let reduced = size.map(|side| side / super::DOWNSCALE);
    let full_texels = (size[0] * size[1]) as usize;
    let pixels = || (0..size[1]).flat_map(|y| (0..size[0]).map(move |x| [x, y]));
    let normal = test_support::hdr_texture(
        device,
        queue,
        size,
        &pixels()
            .map(|pixel| {
                let [x, y] = octahedral(near_at(pixel).map_or(FACING, |near| near.normal));
                [x, y, 0., 0.]
            })
            .collect::<Vec<_>>(),
    );
    let untraced = |pixel| {
        floor
            .empty
            .is_some_and(|empty| matches!(empty.kind, EmptyKind::Untraced) && empty.contains(pixel))
    };
    let material = test_support::hdr_texture(
        device,
        queue,
        size,
        &pixels()
            .map(|pixel| {
                [
                    0.,
                    if untraced(pixel) { 1. } else { floor.roughness },
                    0.,
                    1.,
                ]
            })
            .collect::<Vec<_>>(),
    );
    let f0 = test_support::hdr_texture(device, queue, size, &vec![[1.; 4]; full_texels]);
    let traced = |texel: &dyn Fn([u32; 2], bool) -> [f32; 4]| {
        let texels = (0..reduced[1])
            .flat_map(|y| (0..reduced[0]).map(move |x| [x, y]))
            .map(|texel_at| {
                let first = texel_at.map(|v| v * super::DOWNSCALE);
                texel(
                    texel_at,
                    floor.empty.is_some_and(|empty| empty.contains(first)),
                )
            })
            .collect::<Vec<_>>();
        test_support::hdr_texture(device, queue, reduced, &texels)
    };
    let judged = |texel: [u32; 2]| near_at(texel.map(|v| v * super::DOWNSCALE));
    let indirect = traced(&|texel, empty| match (empty, judged(texel)) {
        (true, _) => [0.; 4],
        (false, Some(near)) => near.radiance,
        (false, None) => (floor.radiance)(texel),
    });
    let direction = traced(&|texel, empty| {
        if empty {
            [0.; 4]
        } else {
            let [x, y, z] = judged(texel).map_or(FACING, |near| near.normal);
            [x, y, z, 1.]
        }
    });
    let variance = traced(&|_, empty| [if empty { 0. } else { floor.variance }; 4]);
    let r32 = wgpu::TextureFormat::R32Float;
    let hdr = crate::shading::gbuffer::COLOR;
    let length = super::texture(
        device,
        "lengths",
        reduced,
        r32,
        wgpu::TextureUsages::COPY_DST,
    );
    queue.write_texture(
        length.texture().as_image_copy(),
        bytemuck::cast_slice(&vec![10f32; (reduced[0] * reduced[1]) as usize]),
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(reduced[0] * 4),
            rows_per_image: Some(reduced[1]),
        },
        length.texture().size(),
    );
    let storage = wgpu::TextureUsages::STORAGE_BINDING;
    let resolve = super::texture(device, "resolve", reduced, hdr, storage);
    let resolve_variance = super::texture(device, "resolve variance", reduced, hdr, storage);
    let reprojection = super::texture(device, "hit depth", reduced, r32, storage);
    let temporal = super::texture(device, "temporal", reduced, hdr, storage);
    let temporal_variance = super::texture(device, "temporal variance", reduced, hdr, storage);
    let depth_history = [0, 1].map(|_| super::texture(device, "depth", reduced, r32, storage));
    let motion = test_support::hdr_texture(device, queue, size, &vec![[0.; 4]; full_texels]);
    let output = super::texture(
        device,
        "upsampled",
        size,
        hdr,
        storage | wgpu::TextureUsages::COPY_SRC,
    );
    let depth_target =
        |label| crate::view::targets::target(device, label, size, crate::shading::gbuffer::DEPTH);
    let opaque_depth = depth_target("floor depth");
    let surface_depth = depth_target("surface depth");
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("world reflection denoise"),
        source: wgpu::ShaderSource::Wgsl(crate::shading::compose(&[&super::DENOISE]).into()),
    });
    let pipeline = |entry_point| {
        device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some(entry_point),
            layout: None,
            module: &shader,
            entry_point: Some(entry_point),
            compilation_options: Default::default(),
            cache: None,
        })
    };
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });
    let projection = crate::perspective(std::f32::consts::FRAC_PI_2, 1., 0.1);
    let [w, h] = size.map(|v| v as f32);
    let [rw, rh] = reduced.map(|v| v as f32);
    let params = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: None,
        contents: bytemuck::bytes_of(&super::Params {
            inverse_view_projection: projection.inverse().to_cols_array_2d(),
            previous_view_projection: projection.to_cols_array_2d(),
            eye: [0., 0., 0., 0.1],
            full: [w, h, 1. / w, 1. / h],
            reduced: [rw, rh, 1. / rw, 1. / rh],
            frame: 0,
            rays: 0,
            traced: 0.49,
            range: super::RANGE,
            previous_near: 0.1,
            fade: 0.1,
            padding: [0; 2],
        }),
        usage: wgpu::BufferUsages::UNIFORM,
    });
    let texture = wgpu::BindingResource::TextureView;
    let receivers = [
        (0, texture(&opaque_depth)),
        (1, texture(&normal)),
        (2, texture(&material)),
        (3, texture(&f0)),
        (4, params.as_entire_binding()),
    ];
    let mut encoder = device.create_command_encoder(&Default::default());
    let fill = receiver_depth(device);
    let region = |rows, edge| match rows {
        false => [0, 0, edge, size[1]],
        true => [0, 0, size[0], edge],
    };
    // A nearer surface is the opaque surface and so the surface there: the
    // fixture holds it or a receiver, not both.
    let nearer = near
        .filter(|near| near.nearer)
        .map(|near| region(near.rows, near.edge));
    assert!(nearer.is_none() || floor.empty.is_none());
    let surface = floor
        .empty
        .filter(|empty| matches!(empty.kind, EmptyKind::Receiver))
        .map(|empty| region(empty.rows, empty.edge))
        .or(nearer);
    match plane.tilt {
        None => {
            fixture_depth(&mut encoder, &opaque_depth, &fill, nearer);
            fixture_depth(&mut encoder, &surface_depth, &fill, surface);
        }
        Some(tilt) => {
            // The tilted floor, then what is in front of it, at 0.05 (2 m),
            // clear of the floor everywhere (`STEEP`).
            let tilted = tilted_depth(device, tilt);
            for (view, rect) in [(&opaque_depth, nearer), (&surface_depth, surface)] {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("tilted fixture depth"),
                    depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                        view,
                        depth_ops: Some(wgpu::Operations {
                            load: wgpu::LoadOp::Clear(0.),
                            store: wgpu::StoreOp::Store,
                        }),
                        stencil_ops: None,
                    }),
                    ..Default::default()
                });
                pass.set_pipeline(&tilted);
                pass.draw(0..4, 0..1);
                if let Some([left, top, width, height]) = rect {
                    pass.set_pipeline(&fill);
                    pass.set_viewport(
                        left as f32,
                        top as f32,
                        width as f32,
                        height as f32,
                        0.05,
                        0.05,
                    );
                    pass.draw(0..3, 0..1);
                }
            }
        }
    }
    let mut dispatch = |entry_point,
                        inputs: &[(u32, wgpu::BindingResource)],
                        receivers: &[(u32, wgpu::BindingResource)],
                        grid: [u32; 2]| {
        let pipeline = pipeline(entry_point);
        let bind = |index, entries: &[(u32, wgpu::BindingResource)]| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &pipeline.get_bind_group_layout(index),
                entries: &entries
                    .iter()
                    .map(|(binding, resource)| wgpu::BindGroupEntry {
                        binding: *binding,
                        resource: resource.clone(),
                    })
                    .collect::<Vec<_>>(),
            })
        };
        let (inputs, receivers) = (bind(0, inputs), bind(3, receivers));
        let mut pass = encoder.begin_compute_pass(&Default::default());
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &inputs, &[]);
        pass.set_bind_group(3, &receivers, &[]);
        pass.dispatch_workgroups(grid[0].div_ceil(8), grid[1].div_ceil(8), 1);
    };
    dispatch(
        super::WORLD_RESOLVE_ENTRY,
        &[
            (10, texture(&indirect)),
            (11, texture(&direction)),
            (12, texture(&length)),
            (20, texture(&resolve)),
            (21, texture(&resolve_variance)),
            (22, texture(&reprojection)),
        ],
        &receivers,
        reduced,
    );
    let surfaces = [receivers.as_slice(), &[(7, texture(&surface_depth))]].concat();
    // A first frame, whose history the pass does not read.
    dispatch(
        super::WORLD_TEMPORAL_ENTRY,
        &[
            (8, wgpu::BindingResource::Sampler(&sampler)),
            (
                30,
                texture(if plane.resolved { &resolve } else { &indirect }),
            ),
            (31, texture(&resolve)),
            (32, texture(&variance)),
            (33, texture(&variance)),
            (34, texture(&reprojection)),
            (35, texture(&motion)),
            (36, texture(&depth_history[0])),
            (40, texture(&temporal)),
            (41, texture(&temporal_variance)),
            (42, texture(&depth_history[1])),
        ],
        &surfaces,
        reduced,
    );
    dispatch(
        super::WORLD_UPSAMPLE_ENTRY,
        &[
            (8, wgpu::BindingResource::Sampler(&sampler)),
            (50, texture(&temporal)),
            (51, texture(&temporal_variance)),
            (52, texture(&output)),
        ],
        &surfaces,
        size,
    );
    queue.submit([encoder.finish()]);
    test_support::read(device, queue, output.texture(), 8)
        .chunks_exact(8)
        .map(|texel| std::array::from_fn(|c| test_support::half(&texel[c * 2..c * 2 + 2])))
        .collect()
}

// Plausible defect: the upsample's edge-aware blur skips a neighbour only
// where its opaque surface would not trace, so one under a blended
// receiver, where the classification traced no ray and the temporal pass
// keeps none, enters the blur at full depth and normal weight with the
// colour the resolve left there, empty where its reuse of nearby rays
// reached none, and composition gives the opaque pixels beside the
// receiver that share of their reflection's fallback. Oracle: the same
// floor without the receiver. Over uniform radiance, opaque pixels from the
// receiver's edge out to the blur's radius must equal it, and so must
// those beyond, so that a blur changed everywhere fails too; the
// checkerboard, away from the edge, shows the blur ran. Rows within the
// resolve's largest reach (4 traced texels) of the frame's top and bottom
// are left out: off the grid there, its samples can leave the texel that
// straddles the edge empty, which the first, bilinear sample of the
// reduced grid reads before any blur (#811).
#[test]
fn opaque_pixels_beside_a_receiver_keep_their_blurred_reflection() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let gpu = (&device, &queue);
    let covered = upsampled(gpu, true, 0.1);
    let uncovered = upsampled(gpu, false, 0.1);
    let unblurred = upsampled(gpu, false, 0.);
    let blurred = uncovered
        .iter()
        .zip(&unblurred)
        .filter(|(a, b)| a != b)
        .count();
    // The resolve's weighted sums over uniform rays, its samples under the
    // receiver dropped, round to within a binary16 step (2^-11 below 1) of
    // the floor's radiance; two steps allow for both runs.
    const ROUNDING: f32 = 1. / 1024.;
    let radius = UPSAMPLE_EDGE..=UPSAMPLE_EDGE + 2;
    let (mut beside, mut beyond) = (Vec::new(), Vec::new());
    for y in 8..UPSAMPLE_SIZE - 8 {
        for x in UPSAMPLE_EDGE..UPSAMPLE_SIZE {
            let i = (y * UPSAMPLE_SIZE + x) as usize;
            if largest_difference(&covered[i], &uncovered[i]) > ROUNDING {
                let differs = ((x, y), covered[i], uncovered[i]);
                if radius.contains(&x) {
                    beside.push(differs);
                } else {
                    beyond.push(differs);
                }
            }
        }
    }
    eprintln!(
        "pixels the blur changed: {blurred}; opaque pixels unlike the floor without the receiver, beside it: {}, beyond its radius: {}",
        beside.len(),
        beyond.len()
    );
    assert!(blurred > 0, "the fixture's blur must run");
    assert!(
        beside.is_empty(),
        "{} opaque pixels beside the receiver lost reflection to it: {:?}",
        beside.len(),
        &beside[..beside.len().min(4)]
    );
    assert!(
        beyond.is_empty(),
        "{} opaque pixels beyond the blur's radius differ: {:?}",
        beyond.len(),
        &beyond[..beyond.len().min(4)]
    );
}

/// The traced pixels of the upsample fixture over `kind`'s empty region
/// that differ from the same floor holding rays everywhere, smooth (no
/// blur) and at `UPSAMPLE_ROUGHNESS` (the blur's full radius), with the
/// region left of an even and an odd column and above an even and an odd
/// row: for each run, its description, how many differ, and the largest
/// share of its reflection a pixel lost at each distance from the edge.
fn beside_empty(gpu: (&wgpu::Device, &wgpu::Queue), kind: EmptyKind) -> Vec<(String, usize)> {
    // Uniform rays resolve and filter to within a binary16 step (2^-11
    // below 1) of their radiance; two steps allow for both runs.
    const ROUNDING: f32 = 1. / 1024.;
    let mut runs = Vec::new();
    for roughness in [0., UPSAMPLE_ROUGHNESS] {
        let floor = |empty| UpsampleFloor {
            roughness,
            empty,
            variance: 0.1,
            radiance: &|_| UNIFORM,
        };
        let everywhere = upsampled_floor(gpu, &floor(None));
        for rows in [false, true] {
            for edge in [32, 33] {
                let empty = Empty { kind, rows, edge };
                let upsampled = upsampled_floor(gpu, &floor(Some(empty)));
                let mut differ = 0;
                let mut lost = [0f32; 6];
                for y in 0..UPSAMPLE_SIZE {
                    for x in 0..UPSAMPLE_SIZE {
                        if empty.contains([x, y]) {
                            continue;
                        }
                        let i = (y * UPSAMPLE_SIZE + x) as usize;
                        if largest_difference(&upsampled[i], &everywhere[i]) > ROUNDING {
                            differ += 1;
                            let distance = (if rows { y } else { x } - edge) as usize;
                            let share = 1. - upsampled[i][3] / everywhere[i][3];
                            if let Some(lost) = lost.get_mut(distance) {
                                *lost = lost.max(share);
                            }
                        }
                    }
                }
                let description = format!(
                    "{kind:?} {} {edge}, roughness {roughness}: {differ} traced pixels differ; largest share lost by distance from the edge {:?}",
                    if rows { "above row" } else { "left of column" },
                    lost.map(|share| (share * 1000.).round() / 1000.),
                );
                eprintln!("{description}");
                runs.push((description, differ));
            }
        }
    }
    runs
}

// Plausible defect: the upsample reads the reduced grid by bilinear
// filtering, at the pixel and at each blur tap, so a reduced texel whose
// opaque surface does not trace, and holds no rays, enters the read at its
// full weight with its empty colour, and composition gives that share of a
// traced pixel's reflection beside it to the fallback: 25% or 75% of the
// nearest pixel's by the edge's parity, and some of the next pixels' through
// the blur. Oracle: the same floor traced everywhere, over uniform
// radiance; every traced pixel must equal it, with the edge on both
// parities of a column and of a row, smooth, where neither the resolve's
// reuse nor the blur reaches across it, and rough, where the blur takes its
// full radius.
#[test]
fn traced_pixels_beside_an_untraced_surface_keep_their_whole_reflection() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let faded = beside_empty((&device, &queue), EmptyKind::Untraced)
        .into_iter()
        .filter(|(_, differ)| *differ > 0)
        .collect::<Vec<_>>();
    assert!(
        faded.is_empty(),
        "traced pixels beside an untraced surface lost reflection: {faded:#?}"
    );
}

// Plausible defect: as beside an untraced surface, a reduced texel under a
// blended receiver holds no rays, and enters the upsample's bilinear read
// with whatever colour the resolve's reuse left it, empty where its reuse
// reached no ray; its offsets are floored, so it reaches across the edge
// only from the right and below. Oracle: the same floor without the
// receiver, over uniform radiance; every opaque pixel beside a receiver
// left of it or above it must equal it, on both parities of the edge,
// smooth, where the reuse cannot reach across it, and rough.
#[test]
fn opaque_pixels_beside_a_receiver_keep_their_whole_reflection() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let faded = beside_empty((&device, &queue), EmptyKind::Receiver)
        .into_iter()
        .filter(|(_, differ)| *differ > 0)
        .collect::<Vec<_>>();
    assert!(
        faded.is_empty(),
        "opaque pixels beside a receiver lost reflection: {faded:#?}"
    );
}

// Plausible defect: the upsample reads the reduced variance by bilinear
// filtering too, so a texel that holds no rays, whose variance is 0,
// dilutes it beside the edge by 25% or 75% by its parity, and the blur,
// which runs only where the variance exceeds its thresholds, stops there
// while it runs on the rest of the floor. Oracle: the blur's output depends
// on the variance only through its thresholds, so a floor whose variance
// (0.018) exceeds them, but would fall below them diluted by either share,
// must blur as one whose variance (1) exceeds them by far. Every traced
// pixel of a checkerboard of radiance beside an untraced surface must equal
// that floor's, on both parities of a column edge; the checkerboard,
// against a floor without variance, shows the blur ran.
#[test]
fn the_blur_beside_an_untraced_surface_runs_as_it_does_away_from_it() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let gpu = (&device, &queue);
    let mut unlike = Vec::new();
    let mut blurred = 0;
    for edge in [32, 33] {
        let floor = |variance| {
            let empty = Empty {
                kind: EmptyKind::Untraced,
                rows: false,
                edge,
            };
            let upsampled = upsampled_floor(
                gpu,
                &UpsampleFloor {
                    roughness: UPSAMPLE_ROUGHNESS,
                    empty: Some(empty),
                    variance,
                    radiance: &|[x, y]| {
                        if (x + y) % 2 == 0 {
                            [0.2, 0.2, 0.2, 1.]
                        } else {
                            UNIFORM
                        }
                    },
                },
            );
            (0..UPSAMPLE_SIZE * UPSAMPLE_SIZE)
                .filter(|&i| !empty.contains([i % UPSAMPLE_SIZE, i / UPSAMPLE_SIZE]))
                .map(|i| (i % UPSAMPLE_SIZE, upsampled[i as usize]))
                .collect::<Vec<_>>()
        };
        let (low, high, none) = (floor(0.018), floor(1.), floor(0.));
        blurred += low.iter().zip(&none).filter(|(a, b)| a != b).count();
        let differ = low.iter().zip(&high).filter(|(a, b)| a != b);
        let columns = differ.map(|((x, _), _)| x - edge).collect::<Vec<_>>();
        eprintln!(
            "edge {edge}: {} traced pixels blur unlike a floor of variance 1, at columns {:?} from the edge",
            columns.len(),
            columns.iter().collect::<std::collections::BTreeSet<_>>()
        );
        unlike.extend(columns.into_iter().map(|column| (edge, column)));
    }
    assert!(blurred > 0, "the fixture's blur must run");
    assert!(
        unlike.is_empty(),
        "{} traced pixels beside an untraced surface blur unlike the rest of the floor: {:?}",
        unlike.len(),
        &unlike[..unlike.len().min(8)]
    );
}

/// The radiance a second surface's rays hit in the upsample fixture.
const NEAR_RADIANCE: [f32; 4] = [0.2, 0.2, 0.2, 1.];

/// The side of an edge between two surfaces a comparison takes.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Side {
    /// The floor's pixels.
    Floor,
    /// The second surface's pixels.
    Second,
}

/// The upsample fixture's floor of uniform radiance with a second surface
/// (`Near`) of `NEAR_RADIANCE`, `nearer` and of `normal`, left of each of
/// the columns `edges` and above each of those rows, smooth (no blur) and
/// at `UPSAMPLE_ROUGHNESS` (the blur's full radius), against that `side`'s
/// surface alone: the floor without the second surface, or the second
/// surface over the whole frame. For each run, its description and how
/// many of `side`'s pixels differ from the surface alone by more than two
/// binary16 steps.
fn beside_an_edge(
    gpu: (&wgpu::Device, &wgpu::Queue),
    nearer: bool,
    normal: [f32; 3],
    side: Side,
    edges: &[u32],
) -> Vec<(String, usize)> {
    // Uniform rays resolve and filter to within a binary16 step (2^-11
    // below 1) of their radiance; two steps allow for both runs.
    const ROUNDING: f32 = 1. / 1024.;
    let mut runs = Vec::new();
    for roughness in [0., UPSAMPLE_ROUGHNESS] {
        let floor = UpsampleFloor {
            roughness,
            empty: None,
            variance: 0.1,
            radiance: &|_| UNIFORM,
        };
        let second = |rows, edge| Near {
            rows,
            edge,
            nearer,
            normal,
            radiance: NEAR_RADIANCE,
        };
        let alone = upsampled_surfaces(
            gpu,
            &floor,
            (side == Side::Second).then_some(second(false, UPSAMPLE_SIZE)),
        );
        for rows in [false, true] {
            for &edge in edges {
                let near = second(rows, edge);
                let upsampled = upsampled_surfaces(gpu, &floor, Some(near));
                let mut differ = 0;
                let mut largest = [0f32; 6];
                for y in 0..UPSAMPLE_SIZE {
                    for x in 0..UPSAMPLE_SIZE {
                        if near.contains([x, y]) != (side == Side::Second) {
                            continue;
                        }
                        let i = (y * UPSAMPLE_SIZE + x) as usize;
                        let difference = largest_difference(&upsampled[i], &alone[i]);
                        if difference > ROUNDING {
                            differ += 1;
                            let along = if rows { y } else { x };
                            let distance =
                                along.abs_diff(edge) as usize - usize::from(side == Side::Second);
                            if let Some(largest) = largest.get_mut(distance) {
                                *largest = largest.max(difference);
                            }
                        }
                    }
                }
                let description = format!(
                    "{side:?} side, second surface {} {edge}, roughness {roughness}: {differ} pixels differ; largest difference by distance from the edge {:?}",
                    if rows { "above row" } else { "left of column" },
                    largest.map(|difference| (difference * 1000.).round() / 1000.),
                );
                eprintln!("{description}");
                runs.push((description, differ));
            }
        }
    }
    runs
}

/// The runs of `beside_an_edge` whose pixels differ.
fn differing(runs: Vec<(String, usize)>) -> Vec<(String, usize)> {
    runs.into_iter().filter(|(_, differ)| *differ > 0).collect()
}

// Plausible defect: world-space reflections trace at half resolution; the
// resolve's spatial reuse takes a neighbouring texel wherever its depth is
// above 0 (Wicked's ssr_resolveCS), and the upsample's read of the reduced
// grid, at the pixel and at each blur tap, weighs its four texels by the
// filter and by whether they hold rays alone, so at a depth silhouette the
// farther surface's pixels take the reflection of texels whose surface,
// judged at their block's first pixel, is the nearer one. Oracle: the same
// floor without the nearer surface, over uniform radiance on each; every
// floor pixel must equal it, with the edge on both parities of a column and
// of a row, smooth, where the reuse reaches one texel across and the blur
// does not run, and rough, where the reuse reaches three and the blur its
// full radius.
#[test]
fn the_far_side_of_a_silhouette_keeps_its_own_reflection() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let taken = differing(beside_an_edge(
        (&device, &queue),
        true,
        FACING,
        Side::Floor,
        &[32, 33],
    ));
    assert!(
        taken.is_empty(),
        "floor pixels beside a nearer surface took its reflection: {taken:#?}"
    );
}

// Plausible defect: as on the far side, the nearer surface's pixels beside
// the silhouette read texels judged on the floor behind it, through the
// resolve's reuse and the upsample's reads. Oracle: the nearer surface
// alone, filling the frame, over uniform radiance; every pixel of it must
// equal that, on both parities of a column and of a row edge, smooth and
// rough.
#[test]
fn the_near_side_of_a_silhouette_keeps_its_own_reflection() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let taken = differing(beside_an_edge(
        (&device, &queue),
        true,
        FACING,
        Side::Second,
        &[32, 33],
    ));
    assert!(
        taken.is_empty(),
        "pixels of a nearer surface took the reflection of the floor behind it: {taken:#?}"
    );
}

// Plausible defect: a crease, where a wall meets a floor at the same depth,
// passes a test of depth alone, so the reuse and the upsample's reads take
// the other surface's reflection across it. The ported weight
// (world_same_surface, Godot's) compares normals too: between perpendicular
// normals at equal depth and roughness its depth and roughness terms are 1
// and its normal term exp(-32), about 1.3e-14, below binary16's resolution
// beside the weights of a pixel's own surface, so it prescribes what a
// silhouette gets: each side keeps its own reflection. Oracle: each surface
// alone, over uniform radiance on each, on both parities of a column and
// of a row edge, smooth and rough.
#[test]
fn each_side_of_a_crease_keeps_its_own_reflection() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let gpu = (&device, &queue);
    const UP: [f32; 3] = [0., 1., 0.];
    let taken = differing(
        [Side::Floor, Side::Second]
            .into_iter()
            .flat_map(|side| beside_an_edge(gpu, false, UP, side, &[32, 33]))
            .collect(),
    );
    assert!(
        taken.is_empty(),
        "pixels beside a crease took the other surface's reflection: {taken:#?}"
    );
}

// Plausible defect: the upsample's window gate compares each texel of a
// workgroup's window with the next in x, y and both, so one that skips the
// pairs at the window's last row or column, or a window that does not hold
// every texel its reads take, leaves a silhouette there to the hardware's
// bilinear read, which takes the other surface's reflection. The windows of
// adjacent workgroups overlap, and the pattern repeats each workgroup (8
// pixels), so an edge at each of the 8 columns 40 to 47, and each of those
// rows, crosses every place in a window. Oracle: each surface alone, as
// for the silhouette's far and near sides, smooth and rough.
#[test]
fn a_silhouette_keeps_each_side_its_own_reflection_wherever_it_meets_the_workgroups() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let gpu = (&device, &queue);
    let edges: Vec<u32> = (40..48).collect();
    let taken = differing(
        [Side::Floor, Side::Second]
            .into_iter()
            .flat_map(|side| beside_an_edge(gpu, true, FACING, side, &edges))
            .collect(),
    );
    assert!(
        taken.is_empty(),
        "pixels beside a silhouette took the other surface's reflection: {taken:#?}"
    );
}

/// A floor steep along x and one steep along y, from 0.001 to 0.019 in
/// device depth across the frame (1000 m to 5.3 m at the fixture's 0.1 m
/// near plane): neighbouring texels differ by 5.6e-4, which Godot's weight
/// takes at exp(-1.15), about 0.32, so a gate on the step between
/// neighbours would hold every window an edge. Behind a nearer surface at
/// 0.05 (2 m), which Godot's depth term then takes at below exp(-63).
const STEEP: [[f32; 3]; 2] = [[0.001, 0.018, 0.], [0.001, 0., 0.018]];

// Plausible defect: the window gate takes the step in depth between
// neighbouring texels, which is large on a steep surface, so a steep plane
// is held an edge throughout and every read of it is Godot's weighted read,
// whose depth weights there fall below 1 and differ between the four
// texels: slower, and unlike the hardware's bilinear read. Oracle: with the
// rays read unresolved, so that the upsample reads the same texels, and
// smooth, so that no blur runs, the upsample's output over a steep plane
// must equal, to the bit, its output over the level floor, where every read
// is the hardware's; varied radiance makes any other read show.
#[test]
fn a_steep_plane_takes_the_hardware_reads_a_level_one_does() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let gpu = (&device, &queue);
    let floor = UpsampleFloor {
        roughness: 0.,
        empty: None,
        variance: 0.1,
        radiance: &|[x, y]| {
            let h = (x.wrapping_mul(73_856_093) ^ y.wrapping_mul(19_349_663)) % 1000;
            [0.2 + h as f32 / 1000., 0.5, 1.2 - h as f32 / 1000., 1.]
        },
    };
    let unresolved = |tilt| Plane {
        tilt,
        resolved: false,
    };
    let level = upsampled_plane(gpu, &floor, None, unresolved(None));
    let unlike = STEEP
        .map(|tilt| {
            let steep = upsampled_plane(gpu, &floor, None, unresolved(Some(tilt)));
            let differ = steep.iter().zip(&level).filter(|(a, b)| a != b).count();
            eprintln!("floor of depth {tilt:?}: {differ} pixels unlike the level floor's");
            differ
        })
        .to_vec();
    assert!(
        unlike.iter().all(|&differ| differ == 0),
        "a steep plane's reads are not the hardware's: {unlike:?} pixels differ"
    );
}

// Plausible defect: a gate that holds a steep plane no edge by loosening its
// threshold, or by testing the step's change alone, misses a silhouette
// on that plane, or the resolve's depth term, taken on the steep plane's
// own step, rejects its own neighbours there. Oracle: the steep plane
// alone, over uniform radiance; every pixel of the plane beside a nearer
// surface must equal it, on both parities of a column and of a row edge,
// on planes steep along x and along y, smooth and rough.
#[test]
fn the_far_side_of_a_silhouette_on_a_steep_plane_keeps_its_own_reflection() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let gpu = (&device, &queue);
    // Uniform rays resolve and filter to within a binary16 step (2^-11
    // below 1) of their radiance; two steps allow for both runs.
    const ROUNDING: f32 = 1. / 1024.;
    let mut taken = Vec::new();
    for tilt in STEEP {
        let plane = Plane {
            tilt: Some(tilt),
            resolved: true,
        };
        for roughness in [0., UPSAMPLE_ROUGHNESS] {
            let floor = UpsampleFloor {
                roughness,
                empty: None,
                variance: 0.1,
                radiance: &|_| UNIFORM,
            };
            let alone = upsampled_plane(gpu, &floor, None, plane);
            for rows in [false, true] {
                for edge in [32, 33] {
                    let near = Near {
                        rows,
                        edge,
                        nearer: true,
                        normal: FACING,
                        radiance: NEAR_RADIANCE,
                    };
                    let upsampled = upsampled_plane(gpu, &floor, Some(near), plane);
                    let (mut differ, mut largest) = (0, 0f32);
                    for y in 0..UPSAMPLE_SIZE {
                        for x in 0..UPSAMPLE_SIZE {
                            if near.contains([x, y]) {
                                continue;
                            }
                            let i = (y * UPSAMPLE_SIZE + x) as usize;
                            let difference = largest_difference(&upsampled[i], &alone[i]);
                            if difference > ROUNDING {
                                differ += 1;
                                largest = largest.max(difference);
                            }
                        }
                    }
                    let description = format!(
                        "floor of depth {tilt:?}, nearer surface {} {edge}, roughness {roughness}: {differ} floor pixels differ, by up to {largest:.3}",
                        if rows { "above row" } else { "left of column" },
                    );
                    eprintln!("{description}");
                    if differ > 0 {
                        taken.push(description);
                    }
                }
            }
        }
    }
    assert!(
        taken.is_empty(),
        "floor pixels beside a nearer surface on a steep plane took its reflection: {taken:#?}"
    );
}
