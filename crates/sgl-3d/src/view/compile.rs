//! Program compilation (the architecture's Shared contracts): the slot a
//! stage keeps shader modules and pipelines in (`Compiled`), the renderer's
//! `Compiler`, which says whether the pass in progress is a frame or a
//! planning pass (`Renderer::compile`) and collects what a planning pass
//! queues, and the job a game runs (`CompileJob`).
//!
//! A frame that meets an empty slot creates its programs in place. A
//! planning pass queues a job for them instead and reports the slot
//! missing. A job runs once, wherever the game runs it: a frame that meets
//! a queued job runs it in place, and one that meets a running job waits
//! for it. A job that fails leaves its slot failed, which a planning pass
//! counts as settled and a frame creates in place, so drawing reports the
//! failure the way it always has.
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
    /// Programs to compile: run each ([`CompileJob::run`]) on a thread of
    /// the game's choosing on native, and a few per animation frame in the
    /// browser. A job dropped without running is offered again.
    pub jobs: Vec<CompileJob>,
    /// How many programs the frame still waits for: one for each job not
    /// finished, these included. At 0 a `render` with the same arguments
    /// creates none, but for those in `errors`.
    pub pending: usize,
    /// Programs whose jobs failed since the last call (native; in the
    /// browser a failure reaches the device as drawing's does). Each is
    /// reported once and is no longer pending: the frame that draws it
    /// creates it in place.
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
/// on native. A frame that needs it first runs it in place, or waits for
/// the thread running it.
pub struct CompileJob {
    cell: Erased,
    ran: bool,
}

impl CompileJob {
    /// Compiles the programs with the device's ordinary creation calls on
    /// the calling thread. In the browser those calls return at once and
    /// the browser compiles in its GPU process, which presents nothing
    /// until it has: pace jobs between animation frames.
    pub fn run(mut self) {
        self.ran = true;
        self.cell.run();
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

#[cfg(not(target_arch = "wasm32"))]
type Make<T> = Box<dyn FnOnce() -> T + Send>;
#[cfg(target_arch = "wasm32")]
type Make<T> = Box<dyn FnOnce() -> T>;

trait Run {
    fn run(&self);
    /// Its handle was dropped without running it.
    fn release(&self);
    fn label(&self) -> &'static str;
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
/// needed or by a [`CompileJob`]. `T` holds programs only: no target, bind
/// group or history.
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
    /// Whether a game holds its job, or a thread is running it.
    held: AtomicBool,
    /// The planning pass that last counted the slot missing.
    counted: AtomicU64,
}

enum Progress<T> {
    Queued(Make<T>),
    Running,
    Done(T),
    Failed,
    Taken,
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

    /// The programs. In a frame, always: created by `make`'s closure in
    /// place where no job made them, or by the job a planning pass queued,
    /// run in place if it has not started and waited for if a thread is
    /// running it. In a planning pass, none while they are missing: the
    /// pass queues `make`'s closure as a job and counts the slot, once
    /// however often the pass asks. `make` runs only for a slot that is
    /// empty or whose job failed, and its closure owns what it reads.
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
    ) -> Option<&T>
    where
        F: FnOnce() -> T + WasmNotSend + 'static,
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
            State::Empty | State::Failed => self.state = State::Ready(make()()),
            State::Job(_) => unreachable!("a job's slot was settled above"),
        }
        self.ready()
    }
}

enum Settled<T> {
    Done(T),
    Failed,
    Missing,
}

impl<T: WasmNotSend + 'static> Cell<T> {
    /// What the slot becomes. A frame runs a queued job in place and waits
    /// for a running one. A planning pass leaves both missing and counts
    /// the slot, handing its job out again if it was dropped unrun: decided
    /// under the job's own lock, so never for a job that has since
    /// finished.
    fn settle(self: &Arc<Self>, compiler: &Compiler) -> Settled<T> {
        let planning = compiler.planning();
        let mut progress = lock(&self.progress);
        loop {
            match std::mem::replace(&mut *progress, Progress::Taken) {
                Progress::Done(value) => return Settled::Done(value),
                Progress::Failed | Progress::Taken => return Settled::Failed,
                Progress::Queued(make) if !planning => {
                    // In place, as a frame without a job creates it: its
                    // errors reach the device's handler, not a scope.
                    drop(progress);
                    self.release();
                    return Settled::Done(make());
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

    /// Its job is no longer a game's to run: it finished, a frame ran it
    /// in place, or its handle was dropped.
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
    fn run(&self) {
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
        let mut unwound = Unwound(Some(self));
        let (value, error) = scoped(&self.shared.device, make);
        unwound.0 = None;
        match error {
            None => self.finish(Progress::Done(value)),
            Some(message) => {
                lock(&self.shared.errors).push(CompileError {
                    label: self.label.to_owned(),
                    message,
                });
                self.finish(Progress::Failed);
            }
        }
    }

    fn release(&self) {
        Cell::release(self);
    }

    fn label(&self) -> &'static str {
        self.label
    }
}

/// Runs `make` with the calling thread's validation and internal errors
/// captured: wgpu's error scopes are per thread.
#[cfg(not(target_arch = "wasm32"))]
fn scoped<T>(device: &wgpu::Device, make: Make<T>) -> (T, Option<String>) {
    let internal = device.push_error_scope(wgpu::ErrorFilter::Internal);
    let validation = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let value = make();
    let validation = pollster::block_on(validation.pop());
    let internal = pollster::block_on(internal.pop());
    (
        value,
        validation.or(internal).map(|error| error.to_string()),
    )
}

/// In the browser an error scope's result arrives later, and wgpu's
/// synchronous creation reports through the device as drawing does.
#[cfg(target_arch = "wasm32")]
fn scoped<T>(_: &wgpu::Device, make: Make<T>) -> (T, Option<String>) {
    (make(), None)
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
