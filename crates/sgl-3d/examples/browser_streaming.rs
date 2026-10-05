//! The `streaming` example's world at its scale (#19), rendered on the
//! browser's WebGPU to measure what the CPU spends building and recording
//! each view's draw list there (#24). Built for wasm32 as a `cdylib`, bound
//! with `wasm-bindgen --target web` and driven in headless Chromium by
//! `bun scripts/tasks.ts measure-browser`, which prints its report.
//!
//! It holds a window of chunks about the camera's meshed whole (the `walk`
//! run's, within four across and two up and down, unless the page asks for
//! another, as `headroom`'s 14 and 3), with the run's torches, water and
//! fifty creatures, and the same settings, camera and sun at 1920×1080.
//! Without the threads and files of the native example, nothing streams or
//! is edited: the camera walks at the run's 4.3 m/s through the window for
//! the warm-up and the measured frames, and the render origin stays put.
//! Each frame waits for the one before it to complete, as the native loop
//! keeps one frame in flight. It reports, median / p95 over the measured
//! frames, what `support/culling.rs` reports without `--visibility`, and the
//! draws each view encoded.
#![cfg(target_arch = "wasm32")]

#[allow(dead_code, reason = "the native examples take its options")]
#[path = "support/culling.rs"]
mod culling;
#[allow(dead_code, reason = "the native example uses the rest")]
#[path = "support/voxel_world.rs"]
mod voxel_world;

use sgl_3d::glam::{IVec3, Mat4};
use sgl_3d::{
    InstanceState, Mobility, ModelMesh, PreparedModel, Renderer, Scene, timing::GpuTiming,
};
use std::fmt::Write as _;
use voxel_world::{CHUNK, Mesher, SIZE, World};
use wasm_bindgen::prelude::*;

/// The `walk` run's speed, torches a surface chunk and shadow distance.
const SPEED: f64 = 4.3;
const TORCHES: u32 = 1;
const SHADOW_DISTANCE: f32 = 150.;
const CREATURES: usize = 50;
const WARM_UP: usize = 60;
const DT: f64 = 1. / 60.;

/// Renders `frames` measured frames after the warm-up of a window of the
/// chunks within `across` of the camera's along x and z and `up` along y,
/// and returns the report, or the line that failed.
#[wasm_bindgen]
pub async fn measure(frames: u32, across: i32, up: i32) -> String {
    console_error_panic_hook::set_once();
    measured(frames as usize, [across, up])
        .await
        .unwrap_or_else(|error| format!("FAIL {error}\n"))
}

async fn measured(frames: usize, [across, up]: [i32; 2]) -> Result<String, String> {
    let adapter = wgpu::Instance::default()
        .request_adapter(&Default::default())
        .await
        .map_err(|e| format!("no WebGPU adapter: {e}"))?;
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: Some("SGL3D browser streaming measurement"),
            required_features: sgl_3d::graphics_device::features(&adapter)
                | (adapter.features() & wgpu::Features::TIMESTAMP_QUERY),
            required_limits: sgl_3d::graphics_device::limits(&adapter),
            ..Default::default()
        })
        .await
        .map_err(|e| format!("request_device: {e}"))?;
    let gpu = (&device, &queue);
    let mut scene = Scene::new(&device, &queue);
    let materials =
        voxel_world::add_materials(&mut scene, gpu, false).map_err(|e| e.to_string())?;
    let sky = scene
        .add_environment(&device, &queue, &voxel_world::sky())
        .map_err(|e| e.to_string())?;
    let world = World::default();
    let mut eye = voxel_world::eye(&world, 0.5, SPEED);
    // The render origin: the first eye's chunk corner across x and z.
    let corner = voxel_world::chunk_of(eye) * CHUNK;
    let origin = IVec3::new(corner.x, 0, corner.z).as_dvec3();
    let mesher = Mesher {
        world: &world,
        seconds: 0.,
        per_block: false,
        terrain: &materials.terrain,
        water: materials.water,
    };
    let centre = voxel_world::chunk_of(eye);
    let (mut instances, mut lights) = (0, 0);
    for dy in -up..=up {
        for dz in -across..=across {
            for dx in -across..=across {
                let chunk = centre + IVec3::new(dx, dy, dz);
                let prepared = mesher
                    .prepare(chunk, [true, true])
                    .map_err(|e| e.to_string())?;
                let pose = Mat4::from_translation(((chunk * CHUNK).as_dvec3() - origin).as_vec3());
                let mut terrain = false;
                for (model, mobility) in [
                    (prepared.terrain, Mobility::Static),
                    (prepared.water, Mobility::Moving),
                ] {
                    let Some((model, _)) = model.filter(|(_, triangles)| *triangles > 0) else {
                        continue;
                    };
                    let model = scene
                        .add_model(&device, &queue, model)
                        .map_err(|e| e.to_string())?;
                    let state = InstanceState {
                        pose,
                        ..InstanceState::new(model)
                    };
                    scene
                        .add_instance(&device, &queue, state, mobility)
                        .map_err(|e| e.to_string())?;
                    instances += 1;
                    terrain |= mobility == Mobility::Static;
                }
                if terrain {
                    for (at, shadowed) in voxel_world::torches(&world, chunk, TORCHES) {
                        let light = voxel_world::torch(at, shadowed, origin);
                        scene
                            .add_light(&device, &queue, light)
                            .map_err(|e| e.to_string())?;
                        lights += 1;
                    }
                }
            }
        }
    }
    let creature = voxel_world::creature();
    let creature = scene
        .add_model(
            &device,
            &queue,
            PreparedModel::new(vec![ModelMesh {
                vertices: creature.vertices,
                indices: creature.indices,
                material: materials.creature,
                deformation: Default::default(),
            }])
            .map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
    let mut creatures = Vec::new();
    for _ in 0..CREATURES {
        creatures.push(
            scene
                .add_instance(
                    &device,
                    &queue,
                    InstanceState::new(creature),
                    Mobility::Moving,
                )
                .map_err(|e| e.to_string())?,
        );
    }
    let mut settings = voxel_world::settings();
    let options = culling::Options::default();
    let mut culling = culling::Culling::new(options);
    let output = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("browser streaming output"),
        size: wgpu::Extent3d {
            width: SIZE[0],
            height: SIZE[1],
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let view = output.create_view(&Default::default());
    let mut renderer = Renderer::new(&device, &queue, output.format(), SIZE, 1., &settings)
        .map_err(|e| e.to_string())?;
    let mut timing = GpuTiming::new(&device, &queue);
    let mut draws = Vec::new();
    let mut in_flight = None;
    let mut seconds = 0.;
    for index in 0..WARM_UP + frames {
        seconds += DT;
        eye = voxel_world::eye(&world, eye.x + SPEED * DT, SPEED);
        for (number, &instance) in creatures.iter().enumerate() {
            let place = voxel_world::creature_place(&world, eye, seconds, number);
            let state = InstanceState {
                pose: voxel_world::creature_pose(place, origin),
                ..InstanceState::new(creature)
            };
            scene
                .set_instance(&queue, instance, state)
                .map_err(|e| e.to_string())?;
        }
        let camera = voxel_world::camera((eye - origin).as_vec3());
        let mut input = voxel_world::input(camera, seconds, sky, SHADOW_DISTANCE);
        input.camera_cut = index == 0;
        options.apply(&mut settings, index);
        if let Some(timing) = &mut timing {
            for done in timing.begin_frame(&device, &queue) {
                culling.gpu(&done);
            }
        }
        let mut encoder = device.create_command_encoder(&Default::default());
        let started = now();
        renderer.render(
            &device,
            &queue,
            &mut encoder,
            &mut scene,
            &input,
            &settings,
            &view,
            timing.as_ref(),
        );
        let rendered = now() - started;
        let commands = encoder.finish();
        let finished = now() - started - rendered;
        queue.submit([commands]);
        if let Some(timing) = &mut timing {
            timing.submitted(&queue);
        }
        let (done, completed) = futures::channel::oneshot::channel();
        queue.on_submitted_work_done(move || {
            let _ = done.send(());
        });
        if let Some(earlier) = in_flight.replace(completed) {
            let _: Result<(), _> = earlier.await;
        }
        renderer.finish_frame(&mut scene);
        let measure = index >= WARM_UP;
        culling.frame(
            (&mut renderer, &device),
            index,
            measure,
            (rendered, finished),
        );
        if measure {
            draws.push(renderer.diagnostic_draws());
        }
    }
    if let Some(earlier) = in_flight.take() {
        let _: Result<(), _> = earlier.await;
    }
    let mut report = String::new();
    let info = adapter.get_info();
    let _ = writeln!(
        report,
        "{frames} frames at {}x{} after {WARM_UP}, on {} ({:?}): chunks within {across} across \
         and {up} up, {instances} chunk instances, {CREATURES} creatures, {lights} lights; \
         walking {SPEED} m/s",
        SIZE[0], SIZE[1], info.name, info.backend,
    );
    let median = |pick: &dyn Fn(&sgl_3d::diagnostics::ViewDraws) -> usize| {
        let mut values: Vec<usize> = draws.iter().map(pick).collect();
        values.sort_unstable();
        values.get(values.len() / 2).copied().unwrap_or(0)
    };
    let cascades = draws.first().map_or(0, |draws| draws.cascades.len());
    let cascade_draws: Vec<String> = (0..cascades)
        .map(|cascade| median(&|draws| draws.cascades[cascade]).to_string())
        .collect();
    let _ = writeln!(
        report,
        "  draws per view (median): camera {}, blended {}, cascades {}",
        median(&|draws| draws.camera),
        median(&|draws| draws.blended),
        cascade_draws.join(", ")
    );
    report.push_str(&culling.report());
    Ok(report)
}

/// The page's `performance.now()`, in milliseconds.
fn now() -> f64 {
    let performance = js_sys::Reflect::get(&js_sys::global(), &"performance".into())
        .expect("the page has a performance clock");
    let now: js_sys::Function = js_sys::Reflect::get(&performance, &"now".into())
        .expect("performance.now")
        .dyn_into()
        .expect("performance.now is a function");
    now.call0(&performance)
        .ok()
        .and_then(|value| value.as_f64())
        .unwrap_or(0.)
}
