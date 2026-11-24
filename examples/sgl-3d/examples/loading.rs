//! A loading screen that readies a level's shaders and pipelines before
//! its first frame (`Renderer::compile`), and what that costs and buys: a
//! block world (`support/voxel_world.rs`: terrain, water, torches,
//! creatures, a sun with four cascades, TAA and screen- and world-space
//! reflections) with a live specular probe, whose opening frame and the
//! probe's first refresh create no pipeline.
//!
//! `cargo run --release -p sgl-3d-examples --example loading [-- --ordinary] [--workers N]`
//!
//! The loop below is the one a game copies. Each loading frame it draws
//! its loading screen (here a pass that clears a small texture, standing
//! in for one), asks the renderer what the level's first frame still
//! lacks, and hands the returned compile jobs to its own worker threads.
//! When nothing is pending it draws the level. Every shader and pipeline
//! the frame needs is one of those jobs, so the loop's thread creates none
//! and the loading screen keeps its pace while they compile; `--workers 1`
//! against the default four shows what several threads save in startup
//! time. `--ordinary` skips `compile`, so the first `render` creates
//! everything on the loop's thread, as a game without a loading step does.
//!
//! It prints, for the run: the wall time from the loop's start to the
//! first level frame's completion on the GPU; the longest gap between two
//! loading-screen frames (the stall a player sees); the time `render`
//! took for the first level frame, for the first frame after the camera
//! moved, and for the first frame after a settings change (contact
//! shadows and motion blur turned on, compiled the same way behind the
//! loading screen first unless `--ordinary`), and for the live probe's
//! first refresh after the first level frame; and how many pipelines and
//! shader modules wgpu holds after each step
//! (`wgpu::Instance::generate_report`).
//!
//! The graphics driver caches compiled shaders across runs, so these
//! times are a warm start's unless its cache was cleared first. On macOS
//! Metal keeps one for every unbundled program in
//! `$(getconf DARWIN_USER_CACHE_DIR)/com.apple.metal`; moving that
//! directory aside gives a cold start, and slows whatever else is running.
//!
//! `browser_loading` is the same loop in the browser, where the page runs
//! the jobs itself: each starts the browser's asynchronous compile and
//! returns.

#[cfg(not(target_arch = "wasm32"))]
#[path = "support/level.rs"]
mod level;
#[cfg(not(target_arch = "wasm32"))]
#[allow(dead_code, reason = "the streaming examples use the rest")]
#[path = "support/voxel_world.rs"]
mod voxel_world;

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use super::level::{Level, SIZE};
    use sgl_3d::FrameInput;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    /// The game's loading screen, standing in for one: a pass that clears a
    /// small texture, submitted and polled as a presented frame is.
    struct LoadingScreen {
        target: wgpu::TextureView,
        last: Instant,
        longest: Duration,
        frames: u32,
    }

    impl LoadingScreen {
        fn new(device: &wgpu::Device) -> Self {
            let target = device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some("loading screen"),
                    size: wgpu::Extent3d {
                        width: 256,
                        height: 256,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                    view_formats: &[],
                })
                .create_view(&Default::default());
            Self {
                target,
                last: Instant::now(),
                longest: Duration::ZERO,
                frames: 0,
            }
        }

        /// Draws a frame and records the gap since the last.
        fn frame(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
            let mut encoder = device.create_command_encoder(&Default::default());
            let pulse = f64::from(self.frames % 120) / 120.;
            encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("loading screen"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: pulse,
                            g: pulse,
                            b: pulse,
                            a: 1.,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            queue.submit([encoder.finish()]);
            let _ = device.poll(wgpu::PollType::Poll);
            let now = Instant::now();
            self.longest = self.longest.max(now - self.last);
            self.last = now;
            self.frames += 1;
        }
    }

    /// The game's worker threads, which run compile jobs as they arrive.
    struct Workers {
        jobs: mpsc::Sender<sgl_3d::CompileJob>,
    }

    impl Workers {
        fn new(count: usize) -> Self {
            let (jobs, queue) = mpsc::channel::<sgl_3d::CompileJob>();
            let queue = std::sync::Arc::new(std::sync::Mutex::new(queue));
            for _ in 0..count {
                let queue = queue.clone();
                std::thread::spawn(move || {
                    loop {
                        let job = queue.lock().unwrap().recv();
                        match job {
                            Ok(job) => job.run(),
                            Err(_) => break,
                        }
                    }
                });
            }
            Self { jobs }
        }
    }

    /// Loading frames until the frame `input` lacks nothing: each draws the
    /// loading screen, asks what is pending and hands its jobs to
    /// `workers`. Returns the passes that queued jobs and the jobs.
    fn compile(
        level: &mut Level,
        (device, queue): (&wgpu::Device, &wgpu::Queue),
        input: &FrameInput,
        screen: &mut LoadingScreen,
        workers: &Workers,
    ) -> Result<(u32, usize), String> {
        let (mut passes, mut jobs) = (0, 0);
        loop {
            screen.frame(device, queue);
            let compilation = level.compile(device, queue, input);
            if let Some(error) = compilation.errors.first() {
                return Err(error.to_string());
            }
            if compilation.ready() {
                return Ok((passes, jobs));
            }
            passes += u32::from(!compilation.jobs.is_empty());
            jobs += compilation.jobs.len();
            for job in compilation.jobs {
                workers.jobs.send(job).map_err(|error| error.to_string())?;
            }
            // A loading screen's frame pacing.
            std::thread::sleep(Duration::from_millis(4));
        }
    }

    /// The render and compute pipelines and the shader modules wgpu holds.
    fn programs(instance: &wgpu::Instance) -> (usize, usize) {
        let report = instance.generate_report().expect("a native instance");
        let hub = report.hub_report();
        (
            hub.render_pipelines.num_allocated + hub.compute_pipelines.num_allocated,
            hub.shader_modules.num_allocated,
        )
    }

    /// Waits for everything submitted to complete on the GPU.
    fn settle(device: &wgpu::Device) {
        let _ = device.poll(wgpu::PollType::wait_indefinitely());
    }

    fn milliseconds(duration: Duration) -> f64 {
        duration.as_secs_f64() * 1e3
    }

    pub fn run() -> Result<(), String> {
        let mut ordinary = false;
        let mut workers = 4;
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--ordinary" => ordinary = true,
                "--workers" => {
                    workers = args
                        .next()
                        .and_then(|count| count.parse().ok())
                        .filter(|&count| count > 0)
                        .ok_or("--workers takes a count")?;
                }
                _ => return Err(format!("unknown option {arg}")),
            }
        }
        let instance = wgpu::Instance::default();
        let adapter = pollster::block_on(instance.request_adapter(&Default::default()))
            .map_err(|error| error.to_string())?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("loading example"),
            required_features: sgl_3d::graphics_device::features(&adapter),
            required_limits: sgl_3d::graphics_device::limits(&adapter),
            ..Default::default()
        }))
        .map_err(|error| error.to_string())?;
        let gpu = (&device, &queue);
        let info = adapter.get_info();
        println!(
            "{} ({:?}), {}x{}, {}",
            info.name,
            info.backend,
            SIZE[0],
            SIZE[1],
            if ordinary {
                "ordinary: the first render creates what it needs".to_owned()
            } else {
                format!("compiled on {workers} worker threads")
            }
        );
        let loaded = Instant::now();
        let mut level = Level::load(&device, &queue)?;
        settle(&device);
        println!(
            "content loaded in {:.0} ms; {:?} pipelines and shader modules",
            milliseconds(loaded.elapsed()),
            programs(&instance)
        );
        let workers = Workers::new(workers);
        let mut screen = LoadingScreen::new(&device);
        // The loading screen is up: the loop's start.
        for _ in 0..30 {
            screen.frame(&device, &queue);
            std::thread::sleep(Duration::from_millis(4));
        }
        screen.longest = Duration::ZERO;
        let start = Instant::now();
        let first = level.input(0., true);
        if !ordinary {
            let (passes, jobs) = compile(&mut level, gpu, &first, &mut screen, &workers)?;
            screen.frame(&device, &queue);
            println!(
                "compiled in {:.0} ms: {jobs} jobs over {passes} passes; {:?} pipelines and shader modules",
                milliseconds(start.elapsed()),
                programs(&instance)
            );
        }
        let before = programs(&instance);
        let rendering = Instant::now();
        level.render(&device, &queue, &first);
        let first_render = rendering.elapsed();
        settle(&device);
        // The loading screen's next frame would have been due meanwhile.
        screen.frame(&device, &queue);
        let after = programs(&instance);
        println!(
            "first level frame: render {:.1} ms, created {} pipelines and {} shader modules",
            milliseconds(first_render),
            after.0 - before.0,
            after.1 - before.1
        );
        println!(
            "startup, loop start to the first level frame complete: {:.0} ms",
            milliseconds(start.elapsed())
        );
        println!(
            "longest gap between loading-screen frames: {:.1} ms over {} frames",
            milliseconds(screen.longest),
            screen.frames
        );
        // The live probe's first refresh, which `compile` readied too.
        let before = programs(&instance);
        let refreshing = Instant::now();
        level.refresh(&device, &queue, &first)?;
        let first_refresh = refreshing.elapsed();
        settle(&device);
        let after = programs(&instance);
        println!(
            "first live probe refresh: {:.1} ms, created {} pipelines and {} shader modules",
            milliseconds(first_refresh),
            after.0 - before.0,
            after.1 - before.1
        );
        // The camera's first movement.
        let before = programs(&instance);
        let moved = level.input(0.07, false);
        let rendering = Instant::now();
        level.render(&device, &queue, &moved);
        let moved_render = rendering.elapsed();
        settle(&device);
        let after = programs(&instance);
        println!(
            "first frame after the camera moved: render {:.1} ms, created {} pipelines",
            milliseconds(moved_render),
            after.0 - before.0
        );
        // A settings change, compiled behind the loading screen first
        // unless ordinary.
        level.change_settings();
        let changed = level.input(0.14, false);
        if !ordinary {
            screen.longest = Duration::ZERO;
            screen.last = Instant::now();
            let (_, jobs) = compile(&mut level, gpu, &changed, &mut screen, &workers)?;
            screen.frame(&device, &queue);
            println!(
                "settings change compiled: {jobs} jobs, longest gap {:.1} ms",
                milliseconds(screen.longest)
            );
        }
        let before = programs(&instance);
        let rendering = Instant::now();
        level.render(&device, &queue, &changed);
        let changed_render = rendering.elapsed();
        settle(&device);
        let after = programs(&instance);
        println!(
            "first frame after the settings change: render {:.1} ms, created {} pipelines",
            milliseconds(changed_render),
            after.0 - before.0
        );
        Ok(())
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn main() -> Result<(), String> {
    native::run()
}

/// `browser_loading` is the browser's.
#[cfg(target_arch = "wasm32")]
fn main() {}
