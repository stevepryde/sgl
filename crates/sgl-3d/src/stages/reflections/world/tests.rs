//! World-space reflection rays observed in the reflection composite of real
//! frames.
use crate::asset::{CpuMesh, Vertex};
use crate::renderer::Renderer;
use crate::settings::WorldSpaceReflections::{All, Moving};
use crate::settings::{self, Settings};
use crate::shading::RayQueryForm;
use crate::{Backdrop, Camera, FrameInput, InstanceState, Mobility, Scene, test_support};
use glam::{Mat4, Vec3};

const SIZE: [u32; 2] = [64, 64];

/// A mirror floor facing +Y at height 0, 50 m from the origin to each side.
fn floor() -> crate::asset::Asset {
    let mut asset = test_support::cube();
    asset.meshes = vec![CpuMesh {
        vertices: [(-1., -1.), (1., -1.), (1., 1.), (-1., 1.)]
            .map(|(x, z)| Vertex {
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
fn composite(device: &wgpu::Device, queue: &wgpu::Queue, renderer: &Renderer) -> Vec<f32> {
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
    let mut renderer = Renderer::for_test(device, queue, SIZE, &settings);
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
        assert_eq!(renderer.ray_tracing_in_effect(settings), hardware);
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
    renderer: &mut Renderer,
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
        let mut renderer = Renderer::for_test(&device, &queue, size, &settings);
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
