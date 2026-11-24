//! Program compilation (the architecture's Shared contracts): the slot a
//! stage keeps shader modules and pipelines in (`Compiled`), the renderer's
//! `Compiler`, which says whether the pass in progress is a frame or a
//! planning pass (`Renderer::compile`) and collects what a planning pass
//! queues, the job a game runs (`CompileJob`), and the `Creator` a slot's
//! closure creates its pipelines through.
//!
//! A frame that meets an empty slot creates its programs in place. A
//! planning pass queues a job for them instead and reports the slot
//! missing. A job runs once, wherever the game runs it: a frame that meets
//! a queued job runs it in place. On native a job creates at once, and a
//! frame that meets a running job waits for it. In the browser a job starts
//! WebGPU's asynchronous creation and its slot settles when that resolves;
//! a frame cannot wait for it, so it creates the programs in place and the
//! job's late result is dropped. A job that fails leaves its slot failed,
//! which a planning pass counts as settled and a frame creates in place, so
//! drawing reports the failure the way it always has.
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use wgpu::WasmNotSend;

/// A program that failed to compile in a [`CompileJob`]: wgpu's validation
/// or internal error for it. Drawing the variant creates it in place and
/// reports through the device as drawing without [`crate::Renderer::compile`]
/// does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompileError {
    /// The program's label.
    pub label: String,
    /// wgpu's description of the failure.
    pub message: String,
}

impl std::fmt::Display for CompileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} failed to compile: {}", self.label, self.message)
    }
}

impl std::error::Error for CompileError {}

/// What one [`crate::Renderer::compile`] found.
#[derive(Debug, Default)]
#[must_use = "run or drop every job: a job held neither run nor dropped keeps `compile` reporting it pending"]
pub struct Compilation {
    /// Programs to compile: run each ([`CompileJob::run`]) as it comes, on
    /// a thread of the game's choosing on native. A job dropped without
    /// running is offered again.
    pub jobs: Vec<CompileJob>,
    /// How many programs the frame still waits for: one for each job not
    /// finished, these included. At 0 a `render` with the same arguments
    /// creates none, but for those in `errors`. It can rise again between
    /// calls: a stage whose programs are missing leaves its commands out,
    /// so what it feeds may be asked for only once its job has finished.
    /// In the browser a job is finished once the browser has compiled what
    /// it started, after `run` returned.
    pub pending: usize,
    /// Programs whose jobs failed since the last call. Each is reported
    /// once and is no longer pending: the frame that draws it creates it in
    /// place.
    pub errors: Vec<CompileError>,
}

impl Compilation {
    /// Whether nothing is pending: a `render` with the same arguments
    /// creates no program, but for those in `errors`.
    pub fn ready(&self) -> bool {
        self.pending == 0
    }
}

/// One program set to compile, from [`crate::Renderer::compile`]. `Send`
/// on native. A frame that needs it first runs it in place, or on native
/// waits for the thread running it.
pub struct CompileJob {
    cell: Erased,
    ran: bool,
}

impl CompileJob {
    /// Compiles the programs. On native it creates them with the device's
    /// ordinary creation calls on the calling thread. In the browser it
    /// starts WebGPU's asynchronous pipeline creation and returns, and the
    /// browser compiles them without stalling the page: keep presenting
    /// until [`Compilation::ready`]. The browser's device must come from
    /// `sgl_webgpu::instance`.
    pub fn run(mut self) {
        self.ran = true;
        self.cell.clone().run();
    }

    /// The program set it compiles, as [`CompileError::label`] names it:
    /// the stage's name for it, for a loading screen to show.
    pub fn label(&self) -> &str {
        self.cell.label()
    }
}

impl Drop for CompileJob {
    fn drop(&mut self) {
        if !self.ran {
            self.cell.release();
        }
    }
}

impl std::fmt::Debug for CompileJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompileJob")
            .field("label", &self.cell.label())
            .finish()
    }
}

#[cfg(not(target_arch = "wasm32"))]
type Erased = Arc<dyn Run + Send + Sync>;
#[cfg(target_arch = "wasm32")]
type Erased = Arc<dyn Run>;

/// A slot's closure: it describes its programs to a creator, once for a job
/// and again where a frame creates them in place, which in the browser it
/// does when the job's asynchronous creation has not resolved.
#[cfg(not(target_arch = "wasm32"))]
type Make<T> = Box<dyn Fn(&Creator<'_>) -> Creation<T> + Send>;
#[cfg(target_arch = "wasm32")]
type Make<T> = Box<dyn Fn(&Creator<'_>) -> Creation<T>>;

trait Run {
    fn run(self: Arc<Self>);
    /// Its handle was dropped without running it.
    fn release(&self);
    fn label(&self) -> &'static str;
}

/// What a slot's closure creates its pipelines through, the only way it
/// creates them: at once (on native, and for a frame creating in place on
/// either target), or for a job in the browser with WebGPU's asynchronous
/// creation (`sgl_webgpu::create_render_pipeline_async`). What else the
/// closure makes (shader modules, layouts, buffers, samplers) it makes at
/// once with [`Self::device`].
pub(crate) struct Creator<'a> {
    device: &'a wgpu::Device,
    /// A job in the browser: pipeline creation is asynchronous.
    #[cfg(target_arch = "wasm32")]
    asynchronous: bool,
}

impl<'a> Creator<'a> {
    /// One that creates at once.
    pub(crate) fn now(device: &'a wgpu::Device) -> Self {
        Self {
            device,
            #[cfg(target_arch = "wasm32")]
            asynchronous: false,
        }
    }

    /// The device, for what the closure makes at once.
    pub fn device(&self) -> &'a wgpu::Device {
        self.device
    }

    /// `desc`'s pipeline. `desc` is read before this returns.
    pub fn render_pipeline(
        &self,
        desc: &wgpu::RenderPipelineDescriptor<'_>,
    ) -> Creation<wgpu::RenderPipeline> {
        #[cfg(target_arch = "wasm32")]
        if self.asynchronous {
            return Creation::compiling(sgl_webgpu::create_render_pipeline_async(
                self.device,
                desc,
            ));
        }
        Creation::Ready(self.device.create_render_pipeline(desc))
    }

    /// `desc`'s pipeline, as [`Self::render_pipeline`].
    pub fn compute_pipeline(
        &self,
        desc: &wgpu::ComputePipelineDescriptor<'_>,
    ) -> Creation<wgpu::ComputePipeline> {
        #[cfg(target_arch = "wasm32")]
        if self.asynchronous {
            return Creation::compiling(sgl_webgpu::create_compute_pipeline_async(
                self.device,
                desc,
            ));
        }
        Creation::Ready(self.device.create_compute_pipeline(desc))
    }
}

/// Programs as a slot's closure created them: ready, or, for a job in the
/// browser, what the browser is compiling. A closure that creates several
/// pipelines combines their creations into its programs ([`Self::map`],
/// [`Self::zip`], [`Self::array`], [`Self::all`]), which on native are
/// the values themselves.
pub(crate) enum Creation<T> {
    Ready(T),
    #[cfg(target_arch = "wasm32")]
    Compiling(std::pin::Pin<Box<dyn Future<Output = Result<T, wgpu::Error>>>>),
}

impl<T: 'static> Creation<T> {
    #[cfg(target_arch = "wasm32")]
    fn compiling(future: impl Future<Output = Result<T, wgpu::Error>> + 'static) -> Self {
        Self::Compiling(Box::pin(future))
    }

    /// The programs once created; the browser's first failure if any.
    #[cfg(target_arch = "wasm32")]
    async fn settled(self) -> Result<T, wgpu::Error> {
        match self {
            Self::Ready(value) => Ok(value),
            Self::Compiling(future) => future.await,
        }
    }

    /// The programs of a creator that creates at once ([`Creator::now`]).
    pub(crate) fn now(self) -> T {
        match self {
            Self::Ready(value) => value,
            #[cfg(target_arch = "wasm32")]
            Self::Compiling(_) => {
                unreachable!("a creator that creates at once returns the programs")
            }
        }
    }

    /// `f` of the programs, once they are created.
    pub fn map<U: 'static>(self, f: impl FnOnce(T) -> U + 'static) -> Creation<U> {
        match self {
            Self::Ready(value) => Creation::Ready(f(value)),
            #[cfg(target_arch = "wasm32")]
            Self::Compiling(future) => Creation::compiling(async move { future.await.map(f) }),
        }
    }

    /// Both, once both are created.
    pub fn zip<U: 'static>(self, other: Creation<U>) -> Creation<(T, U)> {
        match (self, other) {
            (Self::Ready(a), Creation::Ready(b)) => Creation::Ready((a, b)),
            #[cfg(target_arch = "wasm32")]
            (a, b) => {
                Creation::compiling(async move { Ok((a.settled().await?, b.settled().await?)) })
            }
        }
    }

    /// Each of `creations`, once all are created.
    pub fn array<const N: usize>(creations: [Self; N]) -> Creation<[T; N]> {
        Self::all(creations).map(|all| {
            all.try_into()
                .unwrap_or_else(|_| unreachable!("as many created as asked"))
        })
    }

    /// Each of `creations`, in order, once all are created.
    pub fn all(creations: impl IntoIterator<Item = Self>) -> Creation<Vec<T>> {
        let creations: Vec<_> = creations.into_iter().collect();
        #[cfg(target_arch = "wasm32")]
        if creations
            .iter()
            .any(|creation| matches!(creation, Self::Compiling(_)))
        {
            return Creation::compiling(async move {
                let mut values = Vec::with_capacity(creations.len());
                for creation in creations {
                    values.push(creation.settled().await?);
                }
                Ok(values)
            });
        }
        Creation::Ready(creations.into_iter().map(Self::now).collect())
    }
}

impl<T> From<T> for Creation<T> {
    fn from(value: T) -> Self {
        Self::Ready(value)
    }
}

/// What the renderer lends its stages for a pass: whether it is a planning
/// pass, and what that pass queued and found missing. Clones share one
/// state.
#[derive(Clone)]
pub(crate) struct Compiler {
    shared: Arc<Shared>,
}

struct Shared {
    device: wgpu::Device,
    planning: AtomicBool,
    /// Jobs a game holds that have neither finished nor been dropped.
    outstanding: AtomicUsize,
    pass: Mutex<Pass>,
    errors: Mutex<Vec<CompileError>>,
}

#[derive(Default)]
struct Pass {
    /// Which planning pass of the compiler this is, from 1.
    number: u64,
    jobs: Vec<CompileJob>,
    /// The slots it found missing, each counted once.
    missing: usize,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Compiler {
    #[cfg_attr(
        target_arch = "wasm32",
        allow(
            clippy::arc_with_non_send_sync,
            reason = "one type for both targets: jobs cross threads on native"
        )
    )]
    pub fn new(device: &wgpu::Device) -> Self {
        Self {
            shared: Arc::new(Shared {
                device: device.clone(),
                planning: AtomicBool::new(false),
                outstanding: AtomicUsize::new(0),
                pass: Mutex::default(),
                errors: Mutex::default(),
            }),
        }
    }

    /// Whether the pass in progress is a planning pass.
    pub fn planning(&self) -> bool {
        self.shared.planning.load(Ordering::Relaxed)
    }

    /// Jobs handed out that have neither finished nor been dropped.
    pub fn outstanding(&self) -> usize {
        self.shared.outstanding.load(Ordering::Acquire)
    }

    /// Runs `pass` as a planning pass: the jobs it queued and how many
    /// programs it found missing.
    pub fn plan(&self, pass: impl FnOnce()) -> (Vec<CompileJob>, usize) {
        /// Ends the planning pass, also where `pass` unwinds.
        struct Planning<'a>(&'a Shared);
        impl Drop for Planning<'_> {
            fn drop(&mut self) {
                self.0.planning.store(false, Ordering::Relaxed);
            }
        }
        {
            let mut current = lock(&self.shared.pass);
            *current = Pass {
                number: current.number + 1,
                ..Pass::default()
            };
        }
        self.shared.planning.store(true, Ordering::Relaxed);
        let planning = Planning(&self.shared);
        pass();
        drop(planning);
        let mut pass = lock(&self.shared.pass);
        (std::mem::take(&mut pass.jobs), pass.missing)
    }

    /// The failures of jobs since the last call.
    pub fn take_errors(&self) -> Vec<CompileError> {
        std::mem::take(&mut *lock(&self.shared.errors))
    }
}

/// A stage's shader modules and pipelines, compiled where they are first
/// needed or by a [`CompileJob`]. The closure that makes `T` creates the
/// programs and what the stage makes once beside them from the device
/// alone, and nothing sized by a view; where `T` is the stage's whole
/// bundle, the stage adds its targets and bind groups where it encodes.
pub(crate) struct Compiled<T> {
    state: State<T>,
}

enum State<T> {
    Empty,
    Job(Arc<Cell<T>>),
    Ready(T),
    /// Its job failed: a frame creates it in place.
    Failed,
}

struct Cell<T> {
    label: &'static str,
    shared: Arc<Shared>,
    progress: Mutex<Progress<T>>,
    finished: Condvar,
    /// Whether a game holds its job, or the job is compiling.
    held: AtomicBool,
    /// The planning pass that last counted the slot missing.
    counted: AtomicU64,
}

enum Progress<T> {
    Queued(Make<T>),
    Running,
    /// The browser is compiling what the job started; the closure stays
    /// for a frame that cannot wait for it.
    #[cfg(target_arch = "wasm32")]
    Compiling(Make<T>),
    Done(T),
    Failed,
    Taken,
}

/// Programs made where their owner is, itself inside a compile job.
impl<T> From<T> for Compiled<T> {
    fn from(value: T) -> Self {
        Self {
            state: State::Ready(value),
        }
    }
}

impl<T> Default for Compiled<T> {
    fn default() -> Self {
        Self {
            state: State::Empty,
        }
    }
}

impl<T: WasmNotSend + 'static> Compiled<T> {
    /// The programs, where a frame has them or a job finished them.
    pub fn ready(&self) -> Option<&T> {
        match &self.state {
            State::Ready(value) => Some(value),
            _ => None,
        }
    }

    /// `ready`, to change what the stage keeps beside its programs.
    pub fn ready_mut(&mut self) -> Option<&mut T> {
        match &mut self.state {
            State::Ready(value) => Some(value),
            _ => None,
        }
    }

    /// Drops the programs, or leaves their job to finish for nothing.
    pub fn clear(&mut self) {
        self.state = State::Empty;
    }

    /// The programs. In a frame, always: created by `make`'s closure in
    /// place where no job made them, or by the job a planning pass queued,
    /// run in place if it has not started and waited for if a thread is
    /// running it; in the browser, made again in place while the browser
    /// is still compiling the job's. In a planning pass, none while they are missing: the
    /// pass queues `make`'s closure as a job and counts the slot, once
    /// however often the pass asks. `make` runs only for a slot that is
    /// empty or whose job failed; its closure owns what it reads and
    /// creates its pipelines through the [`Creator`] it is lent where it
    /// runs.
    #[cfg_attr(
        target_arch = "wasm32",
        allow(
            clippy::arc_with_non_send_sync,
            reason = "one type for both targets: jobs cross threads on native"
        )
    )]
    pub fn get<F>(
        &mut self,
        compiler: &Compiler,
        label: &'static str,
        make: impl FnOnce() -> F,
    ) -> Option<&mut T>
    where
        F: Fn(&Creator<'_>) -> Creation<T> + WasmNotSend + 'static,
    {
        let planning = compiler.planning();
        if let State::Job(cell) = &self.state {
            let cell = cell.clone();
            self.state = match cell.settle(compiler) {
                Settled::Done(value) => State::Ready(value),
                Settled::Failed => State::Failed,
                Settled::Missing => return None,
            };
        }
        match &self.state {
            State::Ready(_) => {}
            // Settled for a planning pass, which reported the failure.
            State::Failed if planning => return None,
            State::Empty if planning => {
                let cell = Arc::new(Cell {
                    label,
                    shared: compiler.shared.clone(),
                    progress: Mutex::new(Progress::Queued(Box::new(make()))),
                    finished: Condvar::new(),
                    held: AtomicBool::new(false),
                    counted: AtomicU64::new(0),
                });
                cell.offer(&mut lock(&compiler.shared.pass), true);
                self.state = State::Job(cell);
                return None;
            }
            State::Empty | State::Failed => {
                #[cfg(all(test, not(target_arch = "wasm32")))]
                in_place::record(label);
                self.state = State::Ready(make()(&Creator::now(&compiler.shared.device)).now());
            }
            State::Job(_) => unreachable!("a job's slot was settled above"),
        }
        self.ready_mut()
    }
}

enum Settled<T> {
    Done(T),
    Failed,
    Missing,
}

impl<T: WasmNotSend + 'static> Cell<T> {
    /// What the slot becomes. A frame runs a queued job in place, waits for
    /// a running one and creates in place what the browser is compiling. A
    /// planning pass leaves all three missing and counts the slot, handing
    /// its job out again if it was dropped unrun: decided under the job's
    /// own lock, so never for a job that has since finished.
    fn settle(self: &Arc<Self>, compiler: &Compiler) -> Settled<T> {
        let planning = compiler.planning();
        let mut progress = lock(&self.progress);
        loop {
            match std::mem::replace(&mut *progress, Progress::Taken) {
                Progress::Done(value) => return Settled::Done(value),
                Progress::Failed | Progress::Taken => return Settled::Failed,
                Progress::Queued(make) if !planning => {
                    drop(progress);
                    return Settled::Done(self.create_in_place(&make));
                }
                // Left `Taken`, so the job's late result is dropped.
                #[cfg(target_arch = "wasm32")]
                Progress::Compiling(make) if !planning => {
                    drop(progress);
                    return Settled::Done(self.create_in_place(&make));
                }
                Progress::Running if !planning => {
                    *progress = Progress::Running;
                    progress = self
                        .finished
                        .wait(progress)
                        .unwrap_or_else(PoisonError::into_inner);
                }
                unfinished => {
                    let queued = matches!(unfinished, Progress::Queued(_));
                    *progress = unfinished;
                    self.offer(&mut lock(&compiler.shared.pass), queued);
                    return Settled::Missing;
                }
            }
        }
    }

    /// In place, as a frame without a job creates it: its errors reach the
    /// device's handler, not a scope. The job is no longer the game's.
    fn create_in_place(&self, make: &Make<T>) -> T {
        self.release();
        #[cfg(all(test, not(target_arch = "wasm32")))]
        in_place::record(self.label);
        make(&Creator::now(&self.shared.device)).now()
    }

    /// Counts the slot missing in the planning pass, once a pass, and
    /// queues its job where it is `queued` and no game holds it.
    fn offer(self: &Arc<Self>, pass: &mut Pass, queued: bool) {
        if self.counted.swap(pass.number, Ordering::Relaxed) != pass.number {
            pass.missing += 1;
        }
        if queued && !self.held.swap(true, Ordering::AcqRel) {
            self.shared.outstanding.fetch_add(1, Ordering::AcqRel);
            pass.jobs.push(CompileJob {
                cell: self.clone(),
                ran: false,
            });
        }
    }

    /// Its job is no longer a game's to run: it finished, a frame created
    /// its programs in place, or its handle was dropped.
    fn release(&self) {
        if self.held.swap(false, Ordering::AcqRel) {
            self.shared.outstanding.fetch_sub(1, Ordering::AcqRel);
        }
    }

    fn finish(&self, result: Progress<T>) {
        *lock(&self.progress) = result;
        self.release();
        self.finished.notify_all();
    }

    /// The job's result: the programs, or wgpu's description of why they
    /// failed, reported once. In the browser the job's creation resolves
    /// later (`resolve`).
    #[cfg(not(target_arch = "wasm32"))]
    fn complete(&self, result: Result<T, String>) {
        match result {
            Ok(value) => self.finish(Progress::Done(value)),
            Err(message) => {
                self.report(message);
                self.finish(Progress::Failed);
            }
        }
    }

    fn report(&self, message: String) {
        lock(&self.shared.errors).push(CompileError {
            label: self.label.to_owned(),
            message,
        });
    }

    /// The browser's answer to the creation a job started, unless a frame
    /// created the programs in place meanwhile: then it is dropped.
    #[cfg(target_arch = "wasm32")]
    fn resolve(&self, result: Result<T, String>) {
        let mut progress = lock(&self.progress);
        if !matches!(*progress, Progress::Compiling(_)) {
            return;
        }
        *progress = match result {
            Ok(value) => Progress::Done(value),
            Err(message) => {
                self.report(message);
                Progress::Failed
            }
        };
        drop(progress);
        self.release();
    }

    /// Calls `make` with `creator`, the job failed if it unwinds.
    fn created(&self, make: &Make<T>, creator: &Creator<'_>) -> Creation<T> {
        let mut unwound = Unwound(Some(self));
        let creation = make(creator);
        unwound.0 = None;
        creation
    }
}

/// Marks the job failed if its closure unwinds, so a frame waiting for it
/// creates the programs in place instead of waiting for ever.
struct Unwound<'a, T: WasmNotSend + 'static>(Option<&'a Cell<T>>);

impl<T: WasmNotSend + 'static> Drop for Unwound<'_, T> {
    fn drop(&mut self) {
        if let Some(cell) = self.0 {
            cell.finish(Progress::Failed);
        }
    }
}

impl<T: WasmNotSend + 'static> Run for Cell<T> {
    fn run(self: Arc<Self>) {
        let make = {
            let mut progress = lock(&self.progress);
            match std::mem::replace(&mut *progress, Progress::Running) {
                Progress::Queued(make) => make,
                // A frame ran it in place, or took its result.
                other => {
                    *progress = other;
                    return;
                }
            }
        };
        let device = &self.shared.device;
        // wgpu's error scopes are per thread on native and per device in
        // the browser, where this runs in one task: either way they capture
        // this job's calls alone.
        let internal = device.push_error_scope(wgpu::ErrorFilter::Internal);
        let validation = device.push_error_scope(wgpu::ErrorFilter::Validation);
        #[cfg(not(target_arch = "wasm32"))]
        {
            let value = self.created(&make, &Creator::now(device)).now();
            let validation = pollster::block_on(validation.pop());
            let internal = pollster::block_on(internal.pop());
            self.complete(match validation.or(internal) {
                None => Ok(value),
                Some(error) => Err(error.to_string()),
            });
        }
        #[cfg(target_arch = "wasm32")]
        {
            let creator = Creator {
                device,
                asynchronous: true,
            };
            let creation = self.created(&make, &creator);
            // Its synchronous calls' errors (a shader module's), popped now
            // and read once the browser has answered.
            let (validation, internal) = (validation.pop(), internal.pop());
            *lock(&self.progress) = Progress::Compiling(make);
            wasm_bindgen_futures::spawn_local(async move {
                let created = creation.settled().await;
                let scoped = validation.await.or(internal.await);
                self.resolve(match (created, scoped) {
                    (Ok(value), None) => Ok(value),
                    (_, Some(error)) | (Err(error), None) => Err(error.to_string()),
                });
            });
        }
    }

    fn release(&self) {
        Cell::release(self);
    }

    fn label(&self) -> &'static str {
        self.label
    }
}

/// A shader module that program sets compiling apart share: composed and
/// created by the first of them that needs it, on its thread, so never
/// where a planning pass asks. Clones share the module.
#[derive(Clone)]
pub(crate) struct SharedModule {
    label: std::borrow::Cow<'static, str>,
    source: Source,
    module: Arc<std::sync::OnceLock<wgpu::ShaderModule>>,
}

type Source = Arc<dyn Fn() -> String + Send + Sync>;

impl SharedModule {
    /// The module of `source`'s WGSL, which it composes where the module
    /// is first needed.
    #[cfg_attr(
        target_arch = "wasm32",
        allow(
            clippy::arc_with_non_send_sync,
            reason = "one type for both targets: jobs cross threads on native"
        )
    )]
    pub fn new(
        label: impl Into<std::borrow::Cow<'static, str>>,
        source: impl Fn() -> String + Send + Sync + 'static,
    ) -> Self {
        Self {
            label: label.into(),
            source: Arc::new(source),
            module: Arc::default(),
        }
    }

    /// The module of the shading library's `root` composed with what it
    /// uses.
    pub fn composed(label: &'static str, root: &'static crate::shading::Module) -> Self {
        Self::new(label, move || crate::shading::compose(&[root]))
    }

    /// The module, created on the calling thread if nothing has needed it
    /// yet: call it where a pipeline is created, inside a slot's closure.
    pub fn get(&self, device: &wgpu::Device) -> &wgpu::ShaderModule {
        self.module.get_or_init(|| {
            device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(&self.label),
                source: wgpu::ShaderSource::Wgsl((self.source)().into()),
            })
        })
    }

    /// Whether a pipeline has needed it.
    #[cfg(all(test, not(target_arch = "wasm32")))]
    pub fn created(&self) -> bool {
        self.module.get().is_some()
    }
}

/// The sets this thread created in place, for a test's failure message to
/// name: its oracle is elsewhere (wgpu's own counts).
#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) mod in_place {
    use std::cell::RefCell;

    thread_local! {
        static CREATED: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
    }

    pub(super) fn record(label: &'static str) {
        CREATED.with_borrow_mut(|created| created.push(label));
    }

    /// The labels of the sets created in place on this thread since the
    /// last call.
    pub fn take() -> Vec<&'static str> {
        CREATED.with_borrow_mut(std::mem::take)
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
