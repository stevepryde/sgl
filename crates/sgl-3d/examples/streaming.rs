//! A streamed and edited voxel world at the scale of a block game: 16 m
//! chunks of terrain meshed into quads, each one model placed as a static
//! instance at its integer chunk origin, streamed in around a camera that
//! walks, sprints or flies in a straight line and out behind it, edited
//! block by block, remeshed in waves, and lit by a sky, a sun with cascaded
//! shadows and torches. Water is a blended receiver of screen-space reflections, a
//! moving instance per surface chunk that holds any. Fifty creatures walk
//! about the camera. The render origin follows the camera, chunk-aligned.
//!
//! `cargo run --release -p sgl-3d --example streaming [-- RUN... [--split]
//! [--visibility] [--hardware-ray-tracing] | --check [--hardware-ray-tracing]]`
//!
//! The game's side is modelled on a block game's: its mesher finishes up to
//! 24 chunks a 33 ms tick, nearest the camera first, meshing and preparing
//! them (`PreparedModel::new`) on four worker threads, and the game gives
//! them to the scene under a budget of 16 chunks and 8 MiB of mesh a frame.
//! Remeshes are meshed and prepared on the workers too, then placed in the
//! same frame. Up to
//! 30 block edits a second raise or lower a column's top, each remeshing the
//! chunk that holds the block it changed and that block's solid neighbours'
//! chunks across a border; waves remesh 5 to 20 chunks every 16 frames; and
//! one frame places a torch and remeshes the 27 chunks about it. Every run
//! renders at 1920×1080 with TAA, full-resolution screen-space reflections,
//! world-space reflections and shadows. It streams its first window in with
//! the camera still, then measures `--frames` frames (600) and prints, as
//! median / p95: the CPU time of each scene operation by kind and size; the
//! CPU time a frame spends in scene calls, apart from the game's meshing and
//! preparing, and recording; what the library counted
//! (`diagnostics::counters`, each thread's own): the workers' steps of
//! preparing models and, on the thread that edits the scene, bytes uploaded
//! by call site, buffers created, the steps of placing and writing models,
//! static-edit boxes, ray-source growths and, with hardware ray tracing,
//! the acceleration structures built and compacted; the scene's buffer
//! sizes and BLASes (`Scene::diagnostic_resources`); what the TLAS held
//! (`Renderer::ray_tracing_stats`); draws per view
//! (`Renderer::diagnostic_draws`); the local-light shadow faces and layers
//! redrawn; what streamed; and GPU time per pass group. The last frame of
//! each run is written to `target/streaming-example/<run>.png`. Examples
//! build with the `diagnostics` feature, whose counters add a thread-local
//! update to each upload and build step, so these CPU times sit slightly
//! above a game's without it. `--hardware-ray-tracing` opts in to hardware
//! ray tracing: the device is requested with its feature
//! (`graphics_device::ray_tracing_features`, under wgpu's experimental
//! token) and `Settings::hardware_ray_tracing` is on, so a device that has
//! it builds the scene's acceleration structures; without the flag the runs
//! are as before it existed, comparable with earlier measurements.
//!
//! Each run then prints what occlusion culling could save on its route
//! (`support/culling.rs`): the CPU time each view's draw list takes to build
//! and record; with `--visibility`, frames alternately draw every instance,
//! observing which of the camera's the frame drew without a pixel, and skip
//! those hidden instances, and it prints the hidden share and each pass
//! group's GPU time in both kinds of frame. `--split` renders the opaque
//! stage's two-pass form instead of its fused pass.
//!
//! `--check` holds the camera still in the world while the render origin
//! moves by a chunk and by 256 m, and fails unless static content shows no
//! motion and no shadow is redrawn; then it remeshes the chunks holding
//! shadowed torches and fails unless the next submitted frame redraws their
//! static shadow layers and the one after redraws none. An abandoned frame
//! precedes each submitted one. `--hardware-ray-tracing` applies to it too.
use sgl_3d::diagnostics::{Counters, DiagnosticTarget, SceneResources};
use sgl_3d::glam::{DVec3, IVec3, Mat4, Vec3};
use sgl_3d::{
    EnvironmentId, FrameInput, InstanceId, InstanceState, LightId, MaterialId, Mobility, ModelId,
    ModelMesh, PreparedModel, RayTracingStats, Renderer, Scene, SceneError,
    timing::{FrameTime, GpuTiming},
};
use std::collections::{BTreeMap, VecDeque};
use std::error::Error;
use std::path::Path;
use std::time::Instant;
use voxel_world::{
    CHUNK, ChunkSet, Chunks, Mesher, Prepared, SEA, SIZE, World, chunk_of, creature, hash,
    settings, sky,
};

#[path = "support/culling.rs"]
mod culling;
#[path = "support/voxel_world.rs"]
mod voxel_world;

/// Frames the first window may take to stream in.
const STREAM_IN_LIMIT: usize = 2_000;
/// The simulation's frame time: 60 frames a second.
const DT: f64 = 1. / 60.;
/// Meshes the game's mesher finishes per tick, and the tick.
const MESHED_PER_TICK: usize = 24;
const TICK: f64 = 0.033;
/// What the game gives the scene in one frame at most.
const CHUNKS_PER_FRAME: usize = 16;
const BYTES_PER_FRAME: usize = 8 << 20;
/// Block edits a second, and frames between remeshing waves.
const EDITS_PER_SECOND: f64 = 30.;
const WAVE_FRAMES: usize = 16;

/// One run of the example.
#[derive(Clone)]
struct Run {
    name: &'static str,
    /// Metres a second along +x, from the first to the last frame.
    speed: [f64; 2],
    /// Chunks streamed about the camera's: ±x and ±z, and below and above.
    radius: [i32; 2],
    /// Chunks between render origin moves, along x.
    origin_cell: i32,
    /// The sun's shadow distance in metres, in four cascades.
    shadow_distance: f32,
    /// Torches in each surface chunk.
    torches: u32,
    /// Every water chunk in view is meshed anew each frame.
    water_remesh: bool,
    /// Each block a mesh of its own material, rather than one atlas mesh.
    per_block: bool,
    /// Block edits, waves and the torch frame happen.
    edits: bool,
    /// Frames measured once the first window streamed in.
    frames: usize,
}

fn runs(frames: usize) -> Vec<Run> {
    let walk = Run {
        name: "walk",
        speed: [4.3, 4.3],
        radius: [4, 2],
        origin_cell: 1,
        shadow_distance: 150.,
        torches: 1,
        water_remesh: false,
        per_block: false,
        edits: true,
        frames,
    };
    vec![
        walk.clone(),
        Run {
            name: "sprint",
            speed: [5.6, 5.6],
            ..walk.clone()
        },
        Run {
            name: "fly",
            speed: [20., 50.],
            ..walk.clone()
        },
        Run {
            name: "fly-margin",
            speed: [20., 50.],
            radius: [5, 3],
            ..walk.clone()
        },
        Run {
            name: "origin-256",
            speed: [20., 50.],
            origin_cell: 16,
            ..walk.clone()
        },
        Run {
            name: "cascades-512",
            shadow_distance: 512.,
            ..walk.clone()
        },
        Run {
            name: "torches-1024",
            torches: 8,
            radius: [5, 3],
            ..walk.clone()
        },
        Run {
            name: "per-block-materials",
            per_block: true,
            ..walk.clone()
        },
        Run {
            name: "water-static",
            edits: false,
            ..walk.clone()
        },
        Run {
            name: "water-remesh",
            edits: false,
            water_remesh: true,
            ..walk.clone()
        },
        Run {
            name: "headroom",
            speed: [20., 50.],
            radius: [14, 3],
            ..walk
        },
    ]
}

/// A chunk in the scene.
#[derive(Default)]
struct Resident {
    terrain: Option<(ModelId, InstanceId)>,
    water: Option<(ModelId, InstanceId)>,
    lights: Vec<LightId>,
    /// Whether any of its lights casts a shadow.
    shadowed: bool,
    /// Its terrain's quads.
    quads: usize,
}

/// A chunk the game's mesher finished, waiting to be given to the scene.
struct Meshed {
    chunk: IVec3,
    prepared: Prepared,
}

/// The game's worker threads, which mesh and prepare chunks off the thread
/// that edits the scene (S3D-1: the game schedules them).
const WORKERS: usize = 4;

/// Prepares `jobs` with `mesher` on the game's worker threads and returns
/// them in order, with what the library counted on the workers: counters
/// are each thread's own. A game keeps a worker pool and takes what it
/// prepared on a later frame; the example waits within the frame so each
/// frame's figures hold its own preparation.
fn prepare_all(
    mesher: &Mesher<'_>,
    jobs: &[(IVec3, [bool; 2])],
) -> Result<(Vec<Prepared>, Vec<Counters>), SceneError> {
    if jobs.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    std::thread::scope(|scope| {
        let workers: Vec<_> = jobs
            .chunks(jobs.len().div_ceil(WORKERS))
            .map(|part| {
                scope.spawn(move || {
                    let before = sgl_3d::diagnostics::counters();
                    let prepared: Result<Vec<_>, _> = part
                        .iter()
                        .map(|&(chunk, which)| mesher.prepare(chunk, which))
                        .collect();
                    (prepared, sgl_3d::diagnostics::counters().since(&before))
                })
            })
            .collect();
        let mut prepared = Vec::with_capacity(jobs.len());
        let mut counted = Vec::with_capacity(workers.len());
        for worker in workers {
            let (part, steps) = worker.join().expect("a worker finishes");
            prepared.extend(part?);
            counted.push(steps);
        }
        Ok((prepared, counted))
    })
}

/// CPU times of scene operations of one kind, in microseconds, by size, and
/// the frame's time in scene calls so far.
#[derive(Default)]
struct OperationTimes {
    times: BTreeMap<(&'static str, usize), Vec<f64>>,
    frame_ms: f64,
}

impl OperationTimes {
    fn time<T>(&mut self, kind: &'static str, size: usize, operation: impl FnOnce() -> T) -> T {
        let started = Instant::now();
        let result = operation();
        let elapsed = started.elapsed().as_secs_f64();
        self.frame_ms += elapsed * 1e3;
        self.times
            .entry((kind, size))
            .or_default()
            .push(elapsed * 1e6);
        result
    }

    /// The frame's time in scene calls; the next frame's starts at zero.
    fn take_frame(&mut self) -> f64 {
        std::mem::take(&mut self.frame_ms)
    }
}

/// A run's measurements, from the frame after its first window streamed in.
#[derive(Default)]
struct Measured {
    operations: OperationTimes,
    /// The loop index of the first measured frame.
    from: usize,
    gpu: BTreeMap<&'static str, Vec<f64>>,
    gpu_totals: Vec<f64>,
    recording: Vec<f64>,
    scene_calls: Vec<f64>,
    preparing: Vec<f64>,
    /// The build steps the workers counted preparing models: calls and
    /// nanoseconds by step.
    prepared_steps: BTreeMap<String, (u64, u64)>,
    draws: Vec<sgl_3d::diagnostics::ViewDraws>,
    triangles: Vec<f64>,
    faces: Vec<f64>,
    layers: Vec<f64>,
    shadow_draws: Vec<f64>,
    given_bytes: Vec<f64>,
    uploaded_bytes: Vec<f64>,
    buffers_created: Vec<f64>,
    /// What the library counted over the measured frames.
    counted: Counters,
    resources: Vec<SceneResources>,
    ray_tracing: Vec<RayTracingStats>,
    resident: Vec<f64>,
    quads: Vec<f64>,
    inserted: usize,
    replaced: usize,
    removed: usize,
    moves: usize,
    lights: usize,
}

impl Measured {
    fn gpu(&mut self, frame: FrameTime) {
        // `begin_frame` numbers loop index i's frame i + 1.
        if (frame.frame as usize) <= self.from {
            return;
        }
        let measured = self.gpu_totals.len();
        self.gpu_totals.push(frame.total_ms);
        for pass in frame.passes {
            let times = self.gpu.entry(pass.name).or_default();
            times.resize(measured + 1, 0.);
            times[measured] += pass.ms;
        }
    }
}

/// The median and 95th percentile of `values`, padded with zeros to `count`.
fn quantiles(values: &[f64], count: usize) -> (f64, f64) {
    let mut sorted = values.to_vec();
    sorted.resize(count.max(values.len()).max(1), 0.);
    sorted.sort_by(f64::total_cmp);
    let at = |q: f64| sorted[((sorted.len() as f64 * q).ceil() as usize).saturating_sub(1)];
    (at(0.5), at(0.95))
}

/// A size bucket: triangles rounded up to a power of two (0 for none).
fn bucket(triangles: usize) -> usize {
    triangles.next_power_of_two() * usize::from(triangles > 0)
}

impl Measured {
    fn report(&self, run: &Run) {
        let frames = self.recording.len();
        let line = |label: &str, values: &[f64], unit: &str| {
            let (median, p95) = quantiles(values, values.len());
            println!("  {label:<34} {median:12.2} / {p95:12.2} {unit}");
        };
        println!(
            "\n{} ({frames} frames measured after {} streaming in; {:.1}-{:.1} m/s, {} chunks streamed about the camera's)",
            run.name,
            self.from,
            run.speed[0],
            run.speed[1],
            (2 * run.radius[0] + 1).pow(2) * (2 * run.radius[1] + 1)
        );
        line("resident chunks with geometry", &self.resident, "");
        line("resident terrain quads", &self.quads, "");
        line("CPU recording a frame", &self.recording, "ms");
        line("CPU in scene calls a frame", &self.scene_calls, "ms");
        line("CPU meshing and preparing a frame", &self.preparing, "ms");
        line("mesh bytes given a frame", &self.given_bytes, "B");
        line("bytes uploaded a frame", &self.uploaded_bytes, "B");
        line("buffers created a frame", &self.buffers_created, "");
        let view = |pick: &dyn Fn(&sgl_3d::diagnostics::ViewDraws) -> f64| {
            quantiles(&self.draws.iter().map(pick).collect::<Vec<_>>(), frames).0
        };
        let cascades = self.draws.first().map_or(0, |draws| draws.cascades.len());
        let cascade_draws: Vec<String> = (0..cascades)
            .map(|cascade| format!("{:.0}", view(&|draws| draws.cascades[cascade] as f64)))
            .collect();
        println!(
            "  draws per view (median): camera {:.0}, blended {:.0}, cascades {}",
            view(&|draws| draws.camera as f64),
            view(&|draws| draws.blended as f64),
            cascade_draws.join(", ")
        );
        line("camera triangles", &self.triangles, "");
        line("local shadow faces drawn", &self.faces, "");
        line("local shadow layers drawn", &self.layers, "");
        line("local shadow atlas draws", &self.shadow_draws, "");
        let counted = &self.counted;
        let per_frame = |count: u64| count as f64 / frames.max(1) as f64;
        println!(
            "  static-edit boxes {:.2} a frame, {:.2} merged into another; ray source growths {}, geometry slab growths {}",
            per_frame(counted.static_edit_boxes),
            per_frame(counted.static_edit_boxes_merged),
            counted.ray_source_growths,
            counted.geometry_growths
        );
        println!(
            "  acceleration structures a frame: {:.2} model BLASes of {:.0} vertices, {:.2} deformed BLASes, {:.2} TLASes; {} BLASes of {} vertices compacted",
            per_frame(counted.blas_builds),
            per_frame(counted.blas_build_vertices),
            per_frame(counted.deformed_blas_builds),
            per_frame(counted.tlas_builds),
            counted.blas_compactions,
            counted.blas_compacted_vertices
        );
        let held = |pick: fn(&RayTracingStats) -> u32| {
            self.ray_tracing
                .iter()
                .map(|stats| f64::from(pick(stats)))
                .collect::<Vec<_>>()
        };
        line("instances the TLAS held", &held(|s| s.hardware), "");
        line("instances on the portable BVHs", &held(|s| s.portable), "");
        line("instances left out", &held(|s| s.left_out), "");
        println!("  on the {WORKERS} worker threads that mesh and prepare:");
        for (step, (calls, nanoseconds)) in &self.prepared_steps {
            println!(
                "  {step:<34} {:12.1} us a call ({calls} calls)",
                *nanoseconds as f64 / *calls as f64 / 1e3,
            );
        }
        println!("  on the thread that edits the scene:");
        for time in &counted.steps {
            println!(
                "  {:<34} {:12.1} us a call ({} calls)",
                format!("{:?}", time.step),
                time.nanoseconds as f64 / time.calls as f64 / 1e3,
                time.calls
            );
        }
        let mut sites = counted.uploads.clone();
        sites.sort_by_key(|site| std::cmp::Reverse(site.bytes));
        for site in sites.iter().take(12) {
            let at = format!(
                "{}:{}",
                site.file.trim_start_matches("crates/sgl-3d/"),
                site.line
            );
            println!(
                "  upload at {at:<40} {:12.0} B a frame, {:6.1} writes",
                per_frame(site.bytes),
                per_frame(site.writes)
            );
        }
        if let (Some(first), Some(last)) = (self.resources.first(), self.resources.last()) {
            let most = |pick: fn(&SceneResources) -> u64| {
                self.resources.iter().map(pick).max().unwrap_or(0)
            };
            let row = |label: &str, pick: fn(&SceneResources) -> u64| {
                println!(
                    "  {label:<34} first {:>11} last {:>11} most {:>11}",
                    pick(first),
                    pick(last),
                    most(pick)
                );
            };
            row("ray source bytes", |r| r.ray_source);
            row("ray source bytes to its last word", |r| r.ray_source_used);
            row("ray source bytes content holds", |r| r.ray_source_live);
            row("object record bytes", |r| r.object_records);
            row("instance entry bytes", |r| r.instance_entries);
            row("light record bytes", |r| r.light_records);
            row("geometry buffer bytes", |r| r.geometry);
            row("geometry bytes content holds", |r| r.geometry_live);
            row("geometry buffers", |r| r.geometry_buffers);
            row("BLASes", |r| r.blases);
            row("BLAS triangles", |r| r.blas_triangles);
            let quads = self.quads.last().copied().unwrap_or(0.).max(1.);
            println!(
                "  bytes a resident quad: ray source {:.0}, geometry {:.0}",
                last.ray_source_live as f64 / quads,
                last.geometry_live as f64 / quads
            );
        }
        println!(
            "  streamed: {} inserted, {} replaced, {} removed; {} origin moves; {} lights at the end",
            self.inserted, self.replaced, self.removed, self.moves, self.lights
        );
        for ((kind, size), times) in &self.operations.times {
            let (median, p95) = quantiles(times, times.len());
            println!(
                "  {kind:<16} {size:>6} {:<10} {median:9.1} / {p95:9.1} us ({} calls)",
                if kind.ends_with("model") {
                    "triangles"
                } else {
                    "instances"
                },
                times.len()
            );
        }
        if self.gpu_totals.is_empty() {
            println!("  no GPU timestamps");
            return;
        }
        let count = self.gpu_totals.len();
        let (median, p95) = quantiles(&self.gpu_totals, count);
        println!("  GPU frame                          {median:12.3} / {p95:12.3} ms");
        let mut groups: Vec<_> = self
            .gpu
            .iter()
            .map(|(name, times)| (name, quantiles(times, count)))
            .collect();
        groups.sort_by(|a, b| b.1.0.total_cmp(&a.1.0));
        for (name, (median, p95)) in groups.into_iter().take(16) {
            println!("    {name:<32} {median:12.3} / {p95:12.3} ms");
        }
    }
}

/// The game's view of one run: its world, what the scene holds of it, and
/// where its camera and render origin are.
struct Game {
    run: Run,
    world: World,
    resident: Chunks<Resident>,
    /// Chunks the mesher works on, nearest first, and what it finished.
    requested: VecDeque<IVec3>,
    meshed: VecDeque<Meshed>,
    tick: f64,
    /// The terrain's materials, by a mesh's material index, and the water's.
    terrain: Vec<MaterialId>,
    water: MaterialId,
    creatures: Vec<InstanceId>,
    creature: ModelId,
    sky: EnvironmentId,
    /// The render origin, in world blocks: a multiple of the origin cell.
    origin: IVec3,
    eye: DVec3,
    seconds: f64,
    edits_owed: f64,
    /// The frame's wall time meshing and preparing so far, in milliseconds.
    preparing_ms: f64,
    /// The frame's remeshes: each resident chunk and whether its terrain,
    /// its water or both change.
    remeshes: Vec<(IVec3, [bool; 2])>,
    measured: Measured,
}

impl Game {
    fn new(
        run: &Run,
        scene: &mut Scene,
        (device, queue): (&wgpu::Device, &wgpu::Queue),
    ) -> Result<Self, Box<dyn Error>> {
        let materials = voxel_world::add_materials(scene, (device, queue), run.per_block)?;
        let creature = creature();
        let creature = scene.add_model(
            device,
            queue,
            PreparedModel::new(vec![ModelMesh {
                vertices: creature.vertices,
                indices: creature.indices,
                material: materials.creature,
                deformation: Default::default(),
            }])?,
        )?;
        let sky = scene.add_environment(device, queue, &sky())?;
        let mut game = Self {
            run: run.clone(),
            world: World::default(),
            resident: Chunks::default(),
            requested: VecDeque::new(),
            meshed: VecDeque::new(),
            tick: 0.,
            terrain: materials.terrain,
            water: materials.water,
            creatures: Vec::new(),
            creature,
            sky,
            origin: IVec3::ZERO,
            eye: DVec3::ZERO,
            seconds: 0.,
            edits_owed: 0.,
            preparing_ms: 0.,
            remeshes: Vec::new(),
            measured: Measured::default(),
        };
        game.walk(0.5, 0.);
        game.origin = game.origin_for(game.eye);
        for index in 0..50 {
            let state = InstanceState {
                pose: game.creature_pose(index),
                ..InstanceState::new(creature)
            };
            game.creatures
                .push(scene.add_instance(device, queue, state, Mobility::Moving)?);
        }
        Ok(game)
    }

    /// Puts the eye at `x` along the line it travels.
    fn walk(&mut self, x: f64, speed: f64) {
        self.eye = voxel_world::eye(&self.world, x, speed);
    }

    /// The render origin for an eye at `eye`: its chunk's corner, rounded
    /// down to the origin cell along x and z.
    fn origin_for(&self, eye: DVec3) -> IVec3 {
        let cell = self.run.origin_cell * CHUNK;
        let chunk = chunk_of(eye) * CHUNK;
        IVec3::new(
            chunk.x.div_euclid(cell) * cell,
            0,
            chunk.z.div_euclid(cell) * cell,
        )
    }

    /// World position `at` in the render frame.
    fn render(&self, at: DVec3) -> Vec3 {
        (at - self.origin.as_dvec3()).as_vec3()
    }

    /// Creature `index`'s pose: walking circles about the eye.
    fn creature_pose(&self, index: usize) -> Mat4 {
        let place = voxel_world::creature_place(&self.world, self.eye, self.seconds, index);
        voxel_world::creature_pose(place, self.origin.as_dvec3())
    }

    /// The chunks streamed about the eye's, nearest first.
    fn window(&self) -> Vec<IVec3> {
        let centre = chunk_of(self.eye);
        let [across, up] = self.run.radius;
        let mut chunks = Vec::new();
        for dy in -up..=up {
            for dz in -across..=across {
                for dx in -across..=across {
                    chunks.push(centre + IVec3::new(dx, dy, dz));
                }
            }
        }
        chunks.sort_by_key(|chunk| (*chunk - centre).length_squared());
        chunks
    }

    /// Meshes and prepares `jobs` on the game's workers, timed apart from
    /// the scene's calls.
    fn prepare(&mut self, jobs: &[(IVec3, [bool; 2])]) -> Result<Vec<Prepared>, SceneError> {
        let started = Instant::now();
        let mesher = Mesher {
            world: &self.world,
            seconds: self.seconds as f32,
            per_block: self.run.per_block,
            terrain: &self.terrain,
            water: self.water,
        };
        let (prepared, counted) = prepare_all(&mesher, jobs)?;
        self.preparing_ms += started.elapsed().as_secs_f64() * 1e3;
        for steps in counted.iter().flat_map(|counted| &counted.steps) {
            let total = self
                .measured
                .prepared_steps
                .entry(format!("{:?}", steps.step))
                .or_default();
            total.0 += steps.calls;
            total.1 += steps.nanoseconds;
        }
        Ok(prepared)
    }

    /// Removes the chunks that left the window, with their lights, and
    /// meshes those that entered it, nearest first.
    fn stream(&mut self, scene: &mut Scene) -> Result<(), Box<dyn Error>> {
        let window = self.window();
        let wanted: ChunkSet = window.iter().copied().collect();
        let leaving: Vec<IVec3> = self
            .resident
            .keys()
            .filter(|chunk| !wanted.contains(chunk))
            .copied()
            .collect();
        for chunk in leaving {
            let resident = self.resident.remove(&chunk).unwrap();
            let ops = &mut self.measured.operations;
            for (model, instance) in [resident.terrain, resident.water].into_iter().flatten() {
                ops.time("remove_instance", 1, || scene.remove_instance(instance))?;
                ops.time("remove_model", 1, || scene.remove_model(model))?;
                self.measured.removed += 1;
            }
            for light in resident.lights {
                ops.time("remove_light", 1, || scene.remove_light(light))?;
            }
        }
        self.meshed.retain(|meshed| wanted.contains(&meshed.chunk));
        // What the mesher has yet to finish, nearest the eye first as it
        // moves.
        let finished: ChunkSet = self.meshed.iter().map(|meshed| meshed.chunk).collect();
        self.requested = window
            .into_iter()
            .filter(|chunk| !self.resident.contains_key(chunk) && !finished.contains(chunk))
            .collect();
        // The mesher finishes up to 24 chunks a tick.
        self.tick += DT;
        let mut jobs = Vec::new();
        while self.tick >= TICK {
            self.tick -= TICK;
            for _ in 0..MESHED_PER_TICK {
                let Some(chunk) = self.requested.pop_front() else {
                    break;
                };
                jobs.push((chunk, [true, true]));
            }
        }
        let prepared = self.prepare(&jobs)?;
        for (&(chunk, _), prepared) in jobs.iter().zip(prepared) {
            self.meshed.push_back(Meshed { chunk, prepared });
        }
        Ok(())
    }

    /// Gives the scene what the mesher finished, within the frame's budget,
    /// and returns the mesh bytes given.
    fn arrive(
        &mut self,
        scene: &mut Scene,
        (device, queue): (&wgpu::Device, &wgpu::Queue),
    ) -> Result<usize, Box<dyn Error>> {
        let mut given = 0;
        for _ in 0..CHUNKS_PER_FRAME {
            let Some(next) = self.meshed.front() else {
                break;
            };
            let size = next.prepared.bytes;
            if given > 0 && given + size > BYTES_PER_FRAME {
                break;
            }
            let Meshed { chunk, prepared } = self.meshed.pop_front().unwrap();
            given += size;
            let mut resident = Resident {
                quads: prepared.quads,
                ..Resident::default()
            };
            let pose = Mat4::from_translation(self.render((chunk * CHUNK).as_dvec3()));
            for (model, mobility) in [
                (prepared.terrain, Mobility::Static),
                (prepared.water, Mobility::Moving),
            ] {
                let Some((model, triangles)) = model.filter(|(_, triangles)| *triangles > 0) else {
                    continue;
                };
                let ops = &mut self.measured.operations;
                let model = ops.time("add_model", bucket(triangles), || {
                    scene.add_model(device, queue, model)
                })?;
                let state = InstanceState {
                    pose,
                    ..InstanceState::new(model)
                };
                let instance = ops.time("add_instance", 1, || {
                    scene.add_instance(device, queue, state, mobility)
                })?;
                self.measured.inserted += 1;
                match mobility {
                    Mobility::Static => resident.terrain = Some((model, instance)),
                    Mobility::Moving => resident.water = Some((model, instance)),
                }
            }
            if resident.terrain.is_some() {
                (resident.lights, resident.shadowed) =
                    self.torches(scene, chunk, (device, queue))?;
            }
            self.resident.insert(chunk, resident);
        }
        Ok(given)
    }

    /// Adds a torch's light over world block `at`, `shadowed` or not.
    fn torch(
        &mut self,
        scene: &mut Scene,
        at: IVec3,
        shadowed: bool,
        (device, queue): (&wgpu::Device, &wgpu::Queue),
    ) -> Result<LightId, Box<dyn Error>> {
        let light = voxel_world::torch(at, shadowed, self.origin.as_dvec3());
        Ok(self
            .measured
            .operations
            .time("add_light", 1, || scene.add_light(device, queue, light))?)
    }

    /// The torches chunk `chunk` holds, `torches` on its surface's blocks
    /// and one in eight shadowed, and whether any is.
    fn torches(
        &mut self,
        scene: &mut Scene,
        chunk: IVec3,
        gpu: (&wgpu::Device, &wgpu::Queue),
    ) -> Result<(Vec<LightId>, bool), Box<dyn Error>> {
        let mut lights = Vec::new();
        let mut any_shadowed = false;
        for (at, shadowed) in voxel_world::torches(&self.world, chunk, self.run.torches) {
            any_shadowed |= shadowed;
            lights.push(self.torch(scene, at, shadowed, gpu)?);
        }
        Ok((lights, any_shadowed))
    }

    /// Asks for resident chunk `chunk`'s `[terrain, water]` to be meshed
    /// anew this frame (`remesh_all`).
    fn remesh(&mut self, chunk: IVec3, [terrain, water]: [bool; 2]) {
        if !self.resident.contains_key(&chunk) {
            return;
        }
        // A chunk asked for twice in a frame is meshed and prepared once.
        let at = match self.remeshes.iter().position(|(asked, _)| *asked == chunk) {
            Some(at) => at,
            None => {
                self.remeshes.push((chunk, [false; 2]));
                self.remeshes.len() - 1
            }
        };
        let parts = &mut self.remeshes[at].1;
        if terrain && !parts[0] {
            *self.world.revisions.entry(chunk).or_default() += 1;
        }
        parts[0] |= terrain;
        parts[1] |= water;
    }

    /// Meshes and prepares the frame's remeshes on the workers, then gives
    /// the scene their geometry.
    fn remesh_all(
        &mut self,
        scene: &mut Scene,
        gpu: (&wgpu::Device, &wgpu::Queue),
    ) -> Result<(), Box<dyn Error>> {
        let jobs = std::mem::take(&mut self.remeshes);
        let prepared = self.prepare(&jobs)?;
        for (&(chunk, _), prepared) in jobs.iter().zip(prepared) {
            if let Some(model) = prepared.terrain {
                self.resident.get_mut(&chunk).unwrap().quads = prepared.quads;
                self.replace(scene, chunk, model, Mobility::Static, gpu)?;
            }
            if let Some(model) = prepared.water {
                self.replace(scene, chunk, model, Mobility::Moving, gpu)?;
            }
        }
        Ok(())
    }

    /// Gives resident chunk `chunk`'s terrain (static) or water (moving)
    /// prepared `model` and its triangles, adding the model and its
    /// instance if it had none and `model` has triangles.
    fn replace(
        &mut self,
        scene: &mut Scene,
        chunk: IVec3,
        (prepared, triangles): (PreparedModel, usize),
        mobility: Mobility,
        (device, queue): (&wgpu::Device, &wgpu::Queue),
    ) -> Result<(), Box<dyn Error>> {
        let pose = Mat4::from_translation(self.render((chunk * CHUNK).as_dvec3()));
        let resident = self.resident.get_mut(&chunk).unwrap();
        let held = match mobility {
            Mobility::Static => &mut resident.terrain,
            Mobility::Moving => &mut resident.water,
        };
        let ops = &mut self.measured.operations;
        match held {
            Some((model, _)) => {
                let model = *model;
                ops.time("set_model", bucket(triangles), || {
                    scene.set_model(device, queue, model, prepared)
                })?;
                self.measured.replaced += 1;
            }
            None if triangles > 0 => {
                let model = ops.time("add_model", bucket(triangles), || {
                    scene.add_model(device, queue, prepared)
                })?;
                let state = InstanceState {
                    pose,
                    ..InstanceState::new(model)
                };
                let instance = ops.time("add_instance", 1, || {
                    scene.add_instance(device, queue, state, mobility)
                })?;
                *held = Some((model, instance));
                self.measured.inserted += 1;
            }
            None => {}
        }
        Ok(())
    }

    /// Raises or lowers column (x, z)'s top by a block and remeshes what
    /// that changed. A solid block owns its faces toward air, so the block
    /// that came or went changes its own chunk and those of its solid
    /// neighbours across a chunk border: the block below, and the columns
    /// beside it that reach as high. The block above is air. The water over
    /// the column changes when the top came out of or went under the sea.
    fn edit_column(&mut self, (x, z): (i32, i32), change: i32) {
        let before = self.world.height(x, z);
        *self.world.edits.entry((x, z)).or_default() += change;
        let after = before + change;
        let block = IVec3::new(x, before.max(after), z);
        let chunk = block.div_euclid(IVec3::splat(CHUNK));
        let mut chunks = vec![chunk];
        for step in [IVec3::NEG_Y, IVec3::X, IVec3::NEG_X, IVec3::Z, IVec3::NEG_Z] {
            let next = block + step;
            let across = next.div_euclid(IVec3::splat(CHUNK));
            if across != chunk && next.y <= self.world.height(next.x, next.z) {
                chunks.push(across);
            }
        }
        for chunk in chunks {
            self.remesh(chunk, [true, false]);
        }
        if (before < SEA) != (after < SEA) {
            let sea = IVec3::new(chunk.x, (SEA - 1).div_euclid(CHUNK), chunk.z);
            self.remesh(sea, [false, true]);
        }
    }

    /// Measured frame `frame`'s edits: block edits, a wave of remeshes
    /// every 16 frames, the torch frame halfway, and with `water_remesh`
    /// every water chunk.
    fn edit(
        &mut self,
        scene: &mut Scene,
        frame: usize,
        gpu: (&wgpu::Device, &wgpu::Queue),
    ) -> Result<(), Box<dyn Error>> {
        let centre = chunk_of(self.eye);
        let near: Vec<IVec3> = self
            .resident
            .iter()
            .filter(|(chunk, resident)| {
                resident.terrain.is_some() && (**chunk - centre).abs().max_element() <= 2
            })
            .map(|(chunk, _)| *chunk)
            .collect();
        if self.run.edits && !near.is_empty() {
            self.edits_owed += EDITS_PER_SECOND * DT;
            while self.edits_owed >= 1. {
                self.edits_owed -= 1.;
                let roll = hash(frame as u64 * 977 + self.edits_owed.to_bits());
                let base = near[roll as usize % near.len()] * CHUNK;
                let column = (
                    base.x + ((roll >> 20) % CHUNK as u64) as i32,
                    base.z + ((roll >> 28) % CHUNK as u64) as i32,
                );
                let change = if roll >> 40 & 1 == 0 { 1 } else { -1 };
                self.edit_column(column, change);
            }
            if frame.is_multiple_of(WAVE_FRAMES) {
                let count = 5 + hash(frame as u64) as usize % 16;
                for index in 0..count.min(near.len()) {
                    let chunk = near[(hash(frame as u64 + index as u64) as usize) % near.len()];
                    self.remesh(chunk, [true, false]);
                }
            }
            if frame == self.run.frames / 2 {
                // A torch placed ahead of the eye: its light, and the 27
                // chunks about it remeshed for the light the game bakes into
                // their shading.
                let (x, z) = (self.eye.x.floor() as i32 + 3, self.eye.z.floor() as i32);
                let at = IVec3::new(x, self.world.height(x, z) + 1, z);
                let chunk = at.div_euclid(IVec3::splat(CHUNK));
                if self.resident.contains_key(&chunk) {
                    let light = self.torch(scene, at, true, gpu)?;
                    let resident = self.resident.get_mut(&chunk).unwrap();
                    resident.lights.push(light);
                    resident.shadowed = true;
                }
                for dy in -1..=1 {
                    for dz in -1..=1 {
                        for dx in -1..=1 {
                            let about = chunk + IVec3::new(dx, dy, dz);
                            self.remesh(about, [true, false]);
                        }
                    }
                }
            }
        }
        if self.run.water_remesh {
            let water: Vec<IVec3> = self
                .resident
                .iter()
                .filter(|(_, resident)| resident.water.is_some())
                .map(|(chunk, _)| *chunk)
                .collect();
            for chunk in water {
                self.remesh(chunk, [false, true]);
            }
        }
        self.remesh_all(scene, gpu)
    }

    /// Moves the render origin with the eye.
    fn follow(
        &mut self,
        scene: &mut Scene,
        (device, queue): (&wgpu::Device, &wgpu::Queue),
    ) -> Result<(), Box<dyn Error>> {
        let origin = self.origin_for(self.eye);
        if origin == self.origin {
            return Ok(());
        }
        let to = (origin - self.origin).as_vec3();
        let instances = self
            .resident
            .values()
            .map(|r| usize::from(r.terrain.is_some()) + usize::from(r.water.is_some()))
            .sum::<usize>()
            + self.creatures.len();
        self.measured
            .operations
            .time("move_origin", instances.next_power_of_two(), || {
                scene.move_origin(device, queue, to)
            })?;
        self.origin = origin;
        self.measured.moves += 1;
        Ok(())
    }

    /// Poses the creatures for the frame.
    fn creatures(&mut self, scene: &mut Scene, queue: &wgpu::Queue) -> Result<(), Box<dyn Error>> {
        for index in 0..self.creatures.len() {
            let state = InstanceState {
                pose: self.creature_pose(index),
                ..InstanceState::new(self.creature)
            };
            let creature = self.creatures[index];
            self.measured.operations.time("set_instance", 1, || {
                scene.set_instance(queue, creature, state)
            })?;
        }
        Ok(())
    }

    fn input(&self) -> FrameInput {
        let camera = voxel_world::camera(self.render(self.eye));
        voxel_world::input(camera, self.seconds, self.sky, self.run.shadow_distance)
    }
}

/// The frame's output texture, which the last frame is read back from.
fn output(device: &wgpu::Device) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("streaming example output"),
        size: wgpu::Extent3d {
            width: SIZE[0],
            height: SIZE[1],
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&Default::default());
    (texture, view)
}

/// Whether the mesher has finished every chunk of the window and the scene
/// holds them all.
fn streamed_in(game: &Game) -> bool {
    !game.resident.is_empty() && game.requested.is_empty() && game.meshed.is_empty()
}

/// Renders `run` as `options` choose: streams its first window in, then
/// measures `run.frames` frames and writes the last to `directory`.
fn render(
    run: &Run,
    gpu: (&wgpu::Device, &wgpu::Queue),
    directory: &Path,
    options: culling::Options,
    hardware: bool,
) -> Result<(Game, culling::Culling), Box<dyn Error>> {
    let (device, queue) = gpu;
    let mut settings = settings();
    settings.hardware_ray_tracing = hardware;
    let mut culling = culling::Culling::new(options);
    let mut scene = Scene::new(device, queue);
    let mut game = Game::new(run, &mut scene, gpu)?;
    let (texture, output) = output(device);
    let mut renderer = Renderer::new(
        device,
        queue,
        wgpu::TextureFormat::Rgba8UnormSrgb,
        SIZE,
        1.,
        &settings,
    )?;
    let mut timing = GpuTiming::new(device, queue);
    let mut in_flight = None;
    // The loop index of the first measured frame, and the counters then.
    let mut from = None;
    let mut counted = Counters::default();
    for index in 0.. {
        if from.is_none() && streamed_in(&game) {
            from = Some(index);
            game.measured = Measured {
                from: index,
                ..Measured::default()
            };
            counted = sgl_3d::diagnostics::counters();
        }
        let step = from.map(|from| index - from);
        if step == Some(run.frames) {
            break;
        }
        if index == STREAM_IN_LIMIT && from.is_none() {
            return Err(format!("{}'s window did not stream in", run.name).into());
        }
        game.seconds += DT;
        if let Some(step) = step {
            let progress = step as f64 / run.frames as f64;
            let speed = run.speed[0] + (run.speed[1] - run.speed[0]) * progress;
            game.walk(game.eye.x + speed * DT, speed);
        } else {
            game.walk(game.eye.x, run.speed[0]);
        }
        let before = sgl_3d::diagnostics::counters();
        game.follow(&mut scene, gpu)?;
        game.stream(&mut scene)?;
        let given = game.arrive(&mut scene, gpu)?;
        if let Some(step) = step {
            game.edit(&mut scene, step, gpu)?;
        }
        game.creatures(&mut scene, queue)?;
        let scene_calls = game.measured.operations.take_frame();
        let preparing = std::mem::take(&mut game.preparing_ms);
        let mut input = game.input();
        input.camera_cut = index == 0;
        options.apply(&mut settings, index);
        if let Some(timing) = &mut timing {
            for done in timing.begin_frame(device, queue) {
                culling.gpu(&done);
                game.measured.gpu(done);
            }
        }
        let mut encoder = device.create_command_encoder(&Default::default());
        let started = Instant::now();
        renderer.render(
            device,
            queue,
            &mut encoder,
            &mut scene,
            &input,
            &settings,
            &output,
            timing.as_ref(),
        );
        let rendered = started.elapsed().as_secs_f64() * 1e3;
        let commands = encoder.finish();
        let recording = started.elapsed().as_secs_f64() * 1e3;
        let submission = queue.submit([commands]);
        if let Some(timing) = &mut timing {
            timing.submitted(queue);
        }
        if let Some(earlier) = in_flight.replace(submission) {
            let _ = device.poll(wgpu::PollType::Wait {
                submission_index: Some(earlier),
                timeout: None,
            });
        }
        renderer.finish_frame(&mut scene);
        culling.frame(
            (&mut renderer, device),
            index,
            step.is_some(),
            (rendered, recording - rendered),
        );
        if step.is_some() {
            let frame = sgl_3d::diagnostics::counters().since(&before);
            let m = &mut game.measured;
            m.recording.push(recording);
            m.scene_calls.push(scene_calls);
            m.preparing.push(preparing);
            m.given_bytes.push(given as f64);
            m.uploaded_bytes.push(frame.uploaded_bytes() as f64);
            m.buffers_created.push(frame.buffers_created as f64);
            m.draws.push(renderer.diagnostic_draws());
            m.triangles.push(renderer.geometry_stats().total().1 as f64);
            let shadows = renderer.local_shadow_stats();
            m.faces.push(shadows.faces_drawn as f64);
            m.layers.push(shadows.layers_drawn as f64);
            m.shadow_draws.push(shadows.draws as f64);
            m.resources.push(scene.diagnostic_resources());
            m.ray_tracing.push(renderer.ray_tracing_stats());
            m.quads
                .push(game.resident.values().map(|r| r.quads).sum::<usize>() as f64);
            m.resident.push(
                game.resident
                    .values()
                    .filter(|r| r.terrain.is_some() || r.water.is_some())
                    .count() as f64,
            );
        }
    }
    game.measured.counted = sgl_3d::diagnostics::counters().since(&counted);
    if let Some(timing) = &mut timing {
        for _ in 0..3 {
            let _ = device.poll(wgpu::PollType::wait_indefinitely());
            for done in timing.begin_frame(device, queue) {
                culling.gpu(&done);
                game.measured.gpu(done);
            }
        }
    }
    let _ = device.poll(wgpu::PollType::wait_indefinitely());
    culling.take_visibility(&mut renderer, device);
    game.measured.lights = game.resident.values().map(|r| r.lights.len()).sum();
    let pixels = sgl_3d::diagnostics::read(device, queue, &texture, 4);
    image::save_buffer(
        directory.join(format!("{}.png", run.name)),
        &pixels,
        SIZE[0],
        SIZE[1],
        image::ColorType::Rgba8,
    )?;
    Ok((game, culling))
}

/// The largest motion, in UV, the last frame's motion target holds.
fn largest_motion(
    renderer: &Renderer,
    (device, queue): (&wgpu::Device, &wgpu::Queue),
) -> Result<f32, Box<dyn Error>> {
    let motion = renderer
        .diagnostic_target(DiagnosticTarget::Motion)
        .ok_or("no motion target")?;
    let bytes = sgl_3d::diagnostics::read(device, queue, motion.texture(), 4);
    Ok(bytes
        .chunks_exact(2)
        .map(|half| sgl_3d::diagnostics::half(half).abs())
        .fold(0., f32::max))
}

/// `--check`: a still camera across render origin moves of one chunk and
/// of 256 m, then a remesh of the chunks holding shadowed torches, each
/// followed by an abandoned frame and then a submitted one.
fn check(gpu: (&wgpu::Device, &wgpu::Queue), hardware: bool) -> Result<(), Box<dyn Error>> {
    let (device, queue) = gpu;
    let run = Run {
        name: "check",
        speed: [0., 0.],
        radius: [3, 1],
        origin_cell: 1,
        shadow_distance: 150.,
        torches: 2,
        water_remesh: false,
        per_block: false,
        edits: false,
        frames: 0,
    };
    let settings = sgl_3d::settings::Settings {
        hardware_ray_tracing: hardware,
        ..settings()
    };
    let mut scene = Scene::new(device, queue);
    let mut game = Game::new(&run, &mut scene, gpu)?;
    // Creatures move; this check is about static content.
    for &creature in &game.creatures {
        let mut state = *scene.instance(creature)?;
        state.visible = false;
        state.capture_visible = false;
        scene.set_instance(queue, creature, state)?;
    }
    let (_, output) = output(device);
    let mut renderer = Renderer::new(
        device,
        queue,
        wgpu::TextureFormat::Rgba8UnormSrgb,
        SIZE,
        1.,
        &settings,
    )?;
    let frame = |renderer: &mut Renderer, scene: &mut Scene, game: &Game, submit: bool| {
        let mut encoder = device.create_command_encoder(&Default::default());
        renderer.render(
            device,
            queue,
            &mut encoder,
            scene,
            &game.input(),
            &settings,
            &output,
            None,
        );
        if submit {
            queue.submit([encoder.finish()]);
            renderer.finish_frame(scene);
        }
    };
    // Stream the window in, then let every cache settle.
    let mut settled = 0;
    for _ in 0..STREAM_IN_LIMIT {
        game.stream(&mut scene)?;
        game.arrive(&mut scene, gpu)?;
        frame(&mut renderer, &mut scene, &game, true);
        settled = if streamed_in(&game) { settled + 1 } else { 0 };
        if settled == 30 {
            break;
        }
    }
    if settled < 30 {
        return Err("the check's window did not stream in".into());
    }
    for (label, by) in [("one chunk", CHUNK), ("256 m", 256)] {
        let to = IVec3::new(by, 0, -by);
        scene.move_origin(device, queue, to.as_vec3())?;
        game.origin += to;
        frame(&mut renderer, &mut scene, &game, false);
        frame(&mut renderer, &mut scene, &game, true);
        let motion = largest_motion(&renderer, gpu)?;
        let shadows = renderer.local_shadow_stats();
        println!(
            "origin moved by {label}: largest static motion {motion:e} UV, {} local shadow faces of {} lights redrawn",
            shadows.faces_drawn, shadows.shadowed
        );
        // 1e-4 UV is a fifth of a pixel across 1920.
        if motion > 1e-4 {
            return Err(format!("static content moved {motion} UV after the {label} move").into());
        }
        if shadows.shadowed == 0 {
            return Err("no shadowed torch is in view, so the check sees no local shadow".into());
        }
        if shadows.draws != 0 {
            return Err(format!("the {label} move redrew shadows: {shadows:?}").into());
        }
    }
    // A streaming edit: new geometry for every chunk holding a shadowed
    // torch, whose shadows the next submitted frame redraws once, though a
    // frame was abandoned between.
    let shadowed: Vec<IVec3> = game
        .resident
        .iter()
        .filter(|(_, resident)| resident.shadowed)
        .map(|(chunk, _)| *chunk)
        .collect();
    for chunk in shadowed {
        game.remesh(chunk, [true, false]);
    }
    game.remesh_all(&mut scene, gpu)?;
    frame(&mut renderer, &mut scene, &game, false);
    frame(&mut renderer, &mut scene, &game, true);
    let edited = renderer.local_shadow_stats();
    frame(&mut renderer, &mut scene, &game, true);
    let next = renderer.local_shadow_stats();
    println!(
        "remeshed chunks holding shadowed torches: {} static shadow layers redrawn, then {}",
        edited.layers_drawn, next.layers_drawn
    );
    if edited.layers_drawn == 0 {
        return Err(format!("the remesh redrew no static shadow layer: {edited:?}").into());
    }
    if next.layers_drawn != 0 {
        return Err(format!("the frame after the remesh redrew shadows: {next:?}").into());
    }
    println!("check passed");
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut frames = 600;
    let mut names = Vec::new();
    let mut check_only = false;
    let mut options = culling::Options::default();
    let mut hardware = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--frames" => frames = args.next().ok_or("--frames requires a count")?.parse()?,
            "--check" => check_only = true,
            "--hardware-ray-tracing" => hardware = true,
            option if options.take(option) => {}
            name => names.push(name.to_owned()),
        }
    }
    if frames < 60 {
        return Err("--frames must be at least 60".into());
    }
    let adapter =
        pollster::block_on(wgpu::Instance::default().request_adapter(&Default::default()))?;
    let ray_tracing = if hardware {
        sgl_3d::graphics_device::ray_tracing_features(&adapter)
    } else {
        wgpu::Features::empty()
    };
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        required_features: (adapter.features() & wgpu::Features::TIMESTAMP_QUERY)
            | sgl_3d::graphics_device::features(&adapter)
            | ray_tracing,
        required_limits: sgl_3d::graphics_device::limits(&adapter),
        experimental_features: if hardware {
            // SAFETY: with `--hardware-ray-tracing` the example accepts
            // wgpu's experimental ray queries.
            unsafe { wgpu::ExperimentalFeatures::enabled() }
        } else {
            wgpu::ExperimentalFeatures::disabled()
        },
        ..Default::default()
    }))?;
    let gpu = (&device, &queue);
    if check_only {
        return check(gpu, hardware);
    }
    let all = runs(frames);
    let chosen: Vec<&Run> = if names.is_empty() {
        all.iter().collect()
    } else {
        names
            .iter()
            .map(|name| {
                all.iter()
                    .find(|run| run.name == name)
                    .ok_or(format!("unknown run {name}"))
            })
            .collect::<Result<_, _>>()?
    };
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/streaming-example");
    std::fs::create_dir_all(&directory)?;
    let directory = directory.canonicalize()?;
    println!(
        "{frames} frames a run at {}x{}, measured once the first window streamed in; median / p95; last frames in {}",
        SIZE[0],
        SIZE[1],
        directory.display()
    );
    for run in chosen {
        let (game, culling) = render(run, gpu, &directory, options, hardware)?;
        game.measured.report(run);
        print!("{}", culling.report());
    }
    Ok(())
}
