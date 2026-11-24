//! The `loading` example's loop on the browser's WebGPU: a loading screen
//! that keeps running while a level's shaders and pipelines compile
//! (`Renderer::compile`). Built for wasm32 as a `cdylib`, bound with
//! `wasm-bindgen --target web` and driven in headless Chromium by
//! `bun scripts/tasks.ts measure-browser --loading [--ordinary]
//! [--jobs-per-frame N]`, which prints its report.
//!
//! Each animation frame the page draws its loading screen (a pass that
//! clears the page's canvas, standing in for one), asks the renderer what
//! the level's first frame still lacks, and runs the compile jobs returned,
//! all at once or up to `jobs_per_frame` a frame: each starts WebGPU's
//! asynchronous pipeline creation, which compiles while the page keeps
//! presenting, until `compile` reports ready. Starting a creation still
//! costs the browser's GPU process some work, so a handful a frame keeps
//! the loading screen smoother than all at once, at some cost in time to
//! ready. Every program the frame needs is a job; a job that fails fails
//! the report. `--ordinary` skips `compile`: the first `render` creates
//! everything in one frame.
//!
//! It reports the wall time from the loop's start to the first level
//! frame's completion on the GPU, the longest gap between two animation
//! frames and the longest wait for a loading-screen frame to complete on
//! the GPU over that time, and the same for the level's live probe's
//! first refresh, for the first frame after the camera moved and after a
//! settings change (contact shadows and motion blur turned on, compiled
//! the same way first unless `--ordinary`).
#![cfg(target_arch = "wasm32")]

#[path = "support/level.rs"]
mod level;
#[allow(dead_code, reason = "the streaming examples use the rest")]
#[path = "support/voxel_world.rs"]
mod voxel_world;

use level::{Level, SIZE};
use sgl_3d::{CompileJob, FrameInput};
use std::collections::VecDeque;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use wasm_bindgen::prelude::*;

/// Runs the loop, ordinary or compiled, starting up to `jobs_per_frame`
/// jobs an animation frame (all at 0), and returns the report, or the line
/// that failed.
#[wasm_bindgen]
pub async fn measure(ordinary: bool, jobs_per_frame: u32) -> String {
    console_error_panic_hook::set_once();
    let budget = if jobs_per_frame == 0 {
        usize::MAX
    } else {
        jobs_per_frame as usize
    };
    measured(ordinary, budget)
        .await
        .unwrap_or_else(|error| format!("FAIL {error}\n"))
}

/// The page's loading screen, standing in for one: a pass that clears the
/// page's canvas each animation frame and presents it, with the gaps
/// between frames and how long each took to complete on the GPU.
struct LoadingScreen {
    surface: wgpu::Surface<'static>,
    last: f64,
    longest_gap: f64,
    longest_completion: Arc<Mutex<f64>>,
    frames: u32,
}

impl LoadingScreen {
    fn new(surface: wgpu::Surface<'static>) -> Self {
        Self {
            surface,
            last: now(),
            longest_gap: 0.,
            longest_completion: Arc::default(),
            frames: 0,
        }
    }

    /// Forgets the gaps so far.
    fn restart(&mut self) {
        self.last = now();
        self.longest_gap = 0.;
        self.longest_completion = Arc::default();
    }

    /// Waits for the next animation frame, records the gap since the last
    /// and draws the loading screen.
    async fn frame(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        next_animation_frame().await;
        let at = now();
        self.longest_gap = self.longest_gap.max(at - self.last);
        self.last = at;
        let (wgpu::CurrentSurfaceTexture::Success(frame)
        | wgpu::CurrentSurfaceTexture::Suboptimal(frame)) = self.surface.get_current_texture()
        else {
            return;
        };
        let target = frame.texture.create_view(&Default::default());
        let mut encoder = device.create_command_encoder(&Default::default());
        let pulse = f64::from(self.frames % 120) / 120.;
        encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("loading screen"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &target,
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
        queue.present(frame);
        let longest = self.longest_completion.clone();
        queue.on_submitted_work_done(move || {
            let mut longest = longest.lock().unwrap();
            *longest = longest.max(now() - at);
        });
        self.frames += 1;
    }

    fn report(&self) -> String {
        format!(
            "longest gap between animation frames {:.1} ms, longest loading-screen frame completion {:.1} ms",
            self.longest_gap,
            *self.longest_completion.lock().unwrap()
        )
    }
}

/// Loading frames until the frame `input` lacks nothing: each draws the
/// loading screen, asks what is pending once the jobs it was given have
/// all started, and starts up to `jobs_per_frame` of them. Returns the
/// jobs run, or the first job that failed.
async fn compile(
    level: &mut Level,
    (device, queue): (&wgpu::Device, &wgpu::Queue),
    input: &FrameInput,
    screen: &mut LoadingScreen,
    jobs_per_frame: usize,
) -> Result<usize, String> {
    let mut queued: VecDeque<CompileJob> = VecDeque::new();
    let mut ran = 0;
    loop {
        screen.frame(device, queue).await;
        if queued.is_empty() {
            let compilation = level.compile(device, queue, input);
            if let Some(error) = compilation.errors.first() {
                return Err(format!("compile job: {error}"));
            }
            if compilation.ready() {
                return Ok(ran);
            }
            queued.extend(compilation.jobs);
        }
        for job in queued.drain(..jobs_per_frame.min(queued.len())) {
            job.run();
            ran += 1;
        }
    }
}

/// Waits for everything submitted to complete on the GPU, the loading
/// screen running meanwhile.
async fn settle(device: &wgpu::Device, queue: &wgpu::Queue, screen: &mut LoadingScreen) {
    let done = Arc::new(AtomicBool::new(false));
    let completed = done.clone();
    queue.on_submitted_work_done(move || completed.store(true, Ordering::Release));
    while !done.load(Ordering::Acquire) {
        screen.frame(device, queue).await;
    }
}

async fn measured(ordinary: bool, jobs_per_frame: usize) -> Result<String, String> {
    let canvas: web_sys::HtmlCanvasElement =
        js_sys::Reflect::get(&js_sys::global(), &"document".into())
            .and_then(|document| {
                js_sys::Reflect::get(&document, &"getElementById".into())?
                    .dyn_into::<js_sys::Function>()?
                    .call1(&document, &"screen".into())
            })
            .and_then(JsCast::dyn_into)
            .map_err(|_| "the page has no canvas with id `screen`")?;
    let (width, height) = (canvas.width(), canvas.height());
    let instance = sgl_webgpu::instance(&wgpu::InstanceDescriptor::new_without_display_handle());
    let surface = instance
        .create_surface(wgpu::SurfaceTarget::Canvas(canvas))
        .map_err(|error| format!("create_surface: {error}"))?;
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            compatible_surface: Some(&surface),
            ..Default::default()
        })
        .await
        .map_err(|error| format!("no WebGPU adapter: {error}"))?;
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: Some("loading example"),
            required_features: sgl_3d::graphics_device::features(&adapter),
            required_limits: sgl_3d::graphics_device::limits(&adapter),
            ..Default::default()
        })
        .await
        .map_err(|error| format!("request_device: {error}"))?;
    let gpu = (&device, &queue);
    let info = adapter.get_info();
    let mut report = String::new();
    let _ = writeln!(
        report,
        "{} ({:?}), {}x{}, {}",
        info.name,
        info.backend,
        SIZE[0],
        SIZE[1],
        if ordinary {
            "ordinary: the first render creates what it needs".to_owned()
        } else {
            if jobs_per_frame == usize::MAX {
                "compiled, every job started as it comes".to_owned()
            } else {
                format!("compiled, up to {jobs_per_frame} jobs started a frame")
            }
        }
    );
    let configuration = surface
        .get_default_config(&adapter, width, height)
        .ok_or("the canvas has no surface configuration")?;
    surface.configure(&device, &configuration);
    let mut level = Level::load(&device, &queue)?;
    let mut screen = LoadingScreen::new(surface);
    settle(&device, &queue, &mut screen).await;
    for _ in 0..30 {
        screen.frame(&device, &queue).await;
    }
    // The loading screen is up: the loop's start.
    screen.restart();
    let start = now();
    let first = level.input(0., true);
    if !ordinary {
        let jobs = compile(&mut level, gpu, &first, &mut screen, jobs_per_frame).await?;
        let _ = writeln!(report, "compiled in {:.0} ms: {jobs} jobs", now() - start);
    }
    let rendering = now();
    level.render(&device, &queue, &first);
    let first_render = now() - rendering;
    settle(&device, &queue, &mut screen).await;
    let _ = writeln!(
        report,
        "first level frame: render {first_render:.1} ms, complete on the GPU {:.0} ms later",
        now() - rendering - first_render
    );
    let _ = writeln!(
        report,
        "startup, loop start to the first level frame complete: {:.0} ms; {}",
        now() - start,
        screen.report()
    );
    // The live probe's first refresh, which `compile` readied too.
    screen.restart();
    let refreshing = now();
    level.refresh(&device, &queue, &first)?;
    let refresh = now() - refreshing;
    settle(&device, &queue, &mut screen).await;
    let _ = writeln!(
        report,
        "first live probe refresh: {refresh:.1} ms, complete {:.0} ms later; {}",
        now() - refreshing - refresh,
        screen.report()
    );
    // The camera's first movement.
    screen.restart();
    let moved = level.input(0.07, false);
    let rendering = now();
    level.render(&device, &queue, &moved);
    let moved_render = now() - rendering;
    settle(&device, &queue, &mut screen).await;
    let _ = writeln!(
        report,
        "first frame after the camera moved: render {moved_render:.1} ms, complete {:.0} ms later; {}",
        now() - rendering - moved_render,
        screen.report()
    );
    // A settings change, compiled first unless ordinary.
    screen.restart();
    level.change_settings();
    let changed = level.input(0.14, false);
    if !ordinary {
        let jobs = compile(&mut level, gpu, &changed, &mut screen, jobs_per_frame).await?;
        let _ = writeln!(report, "settings change compiled: {jobs} jobs");
    }
    let rendering = now();
    level.render(&device, &queue, &changed);
    let changed_render = now() - rendering;
    settle(&device, &queue, &mut screen).await;
    let _ = writeln!(
        report,
        "first frame after the settings change: render {changed_render:.1} ms, complete {:.0} ms later; {}",
        now() - rendering - changed_render,
        screen.report()
    );
    Ok(report)
}

/// Resolves at the page's next animation frame.
async fn next_animation_frame() {
    let promise = js_sys::Promise::new(&mut |resolve, _| {
        let request: js_sys::Function =
            js_sys::Reflect::get(&js_sys::global(), &"requestAnimationFrame".into())
                .expect("the page has animation frames")
                .dyn_into()
                .expect("requestAnimationFrame is a function");
        request
            .call1(&js_sys::global(), &resolve)
            .expect("requestAnimationFrame");
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
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
