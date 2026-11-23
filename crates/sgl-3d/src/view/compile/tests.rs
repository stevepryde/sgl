//! A `Compiled` slot's states, with a counting closure standing for the
//! programs: where they are created, and how often.
use super::{Compiled, Compiler};
use crate::test_support;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;

const LABEL: &str = "test programs";

/// A slot's creating closure, which counts its runs in `runs` and returns
/// the run's number.
fn counting(runs: &Arc<AtomicUsize>) -> impl FnOnce() -> usize + Send + 'static {
    let runs = runs.clone();
    move || runs.fetch_add(1, Ordering::SeqCst) + 1
}

// Plausible defect: a planning pass creates the programs on its own thread
// (the stall `Renderer::compile` exists to remove), a job's result never
// reaches the slot, or the frame after it creates them a second time. The
// oracle is the contract (the architecture's Program compilation): the
// closure runs once, where the job runs, and the frame reads its value.
#[test]
fn a_planning_pass_queues_what_a_frame_would_create() {
    let Some((device, _)) = test_support::device() else {
        return;
    };
    let compiler = Compiler::new(&device);
    let runs = Arc::new(AtomicUsize::new(0));
    let mut slot = Compiled::<usize>::default();
    let (jobs, missing) = compiler.plan(|| {
        assert!(slot.get(&compiler, LABEL, || counting(&runs)).is_none());
        // Asked again in the pass, as each cascade asks for its casters'.
        assert!(slot.get(&compiler, LABEL, || counting(&runs)).is_none());
    });
    assert_eq!((jobs.len(), missing), (1, 1), "one program, one job");
    assert_eq!(runs.load(Ordering::SeqCst), 0, "the pass created nothing");
    assert_eq!(compiler.outstanding(), 1);
    for job in jobs {
        std::thread::spawn(move || job.run()).join().unwrap();
    }
    assert_eq!(compiler.outstanding(), 0);
    let (jobs, missing) = compiler.plan(|| {
        assert_eq!(slot.get(&compiler, LABEL, || counting(&runs)), Some(&1));
    });
    assert_eq!((jobs.len(), missing), (0, 0));
    assert_eq!(slot.get(&compiler, LABEL, || counting(&runs)), Some(&1));
    assert_eq!(runs.load(Ordering::SeqCst), 1);
}

// Plausible defect: a frame that meets a job nobody has run yet waits for
// it for ever, or creates the programs beside it so they compile twice
// when the job runs later. The oracle is the contract: drawing an
// unprepared variant creates it in place, once.
#[test]
fn a_frame_runs_a_job_that_has_not_started_in_place() {
    let Some((device, _)) = test_support::device() else {
        return;
    };
    let compiler = Compiler::new(&device);
    let runs = Arc::new(AtomicUsize::new(0));
    let mut slot = Compiled::<usize>::default();
    let (jobs, _) = compiler.plan(|| {
        slot.get(&compiler, LABEL, || counting(&runs));
    });
    assert_eq!(slot.get(&compiler, LABEL, || counting(&runs)), Some(&1));
    assert_eq!(compiler.outstanding(), 0, "the frame settled the job");
    for job in jobs {
        job.run();
    }
    assert_eq!(
        runs.load(Ordering::SeqCst),
        1,
        "the late job compiled nothing"
    );
    assert_eq!(slot.get(&compiler, LABEL, || counting(&runs)), Some(&1));
}

// Plausible defect: a frame that meets a job a worker is running compiles
// the programs again instead of sharing the worker's result, or never
// wakes. The oracle is the contract: one compile per slot, whoever asks.
// The frame asks while the job runs unless the release, sent from a thread
// started as it asks, overtakes it; the contract holds either way, and the
// wait is what the usual order exercises.
#[test]
fn a_frame_waits_for_a_job_a_thread_is_running() {
    let Some((device, _)) = test_support::device() else {
        return;
    };
    let compiler = Compiler::new(&device);
    let runs = Arc::new(AtomicUsize::new(0));
    let (started, has_started) = mpsc::channel();
    let (release, released) = mpsc::channel::<()>();
    let mut slot = Compiled::<usize>::default();
    let (jobs, _) = compiler.plan(|| {
        let count = counting(&runs);
        slot.get(&compiler, LABEL, || {
            move || {
                started.send(()).unwrap();
                released.recv().unwrap();
                count()
            }
        });
    });
    let worker = std::thread::spawn(move || jobs.into_iter().for_each(|job| job.run()));
    has_started.recv().unwrap();
    // Released as the frame asks: it finds the job running or just done.
    let releaser = std::thread::spawn(move || release.send(()).unwrap());
    assert_eq!(slot.get(&compiler, LABEL, || counting(&runs)), Some(&1));
    releaser.join().unwrap();
    worker.join().unwrap();
    assert_eq!(runs.load(Ordering::SeqCst), 1);
}

// Plausible defect: a job a game dropped is lost, so `compile` reports its
// program pending for ever and the loading screen never ends. The oracle is
// the contract: a job dropped without running is offered again.
#[test]
fn a_dropped_job_is_offered_again() {
    let Some((device, _)) = test_support::device() else {
        return;
    };
    let compiler = Compiler::new(&device);
    let runs = Arc::new(AtomicUsize::new(0));
    let mut slot = Compiled::<usize>::default();
    let (jobs, _) = compiler.plan(|| {
        slot.get(&compiler, LABEL, || counting(&runs));
    });
    drop(jobs);
    assert_eq!(compiler.outstanding(), 0);
    let (jobs, missing) = compiler.plan(|| {
        slot.get(&compiler, LABEL, || counting(&runs));
    });
    assert_eq!((jobs.len(), missing), (1, 1));
    // Held, it is not handed out twice.
    let (again, missing) = compiler.plan(|| {
        slot.get(&compiler, LABEL, || counting(&runs));
    });
    assert_eq!((again.len(), missing), (0, 1));
    jobs.into_iter().for_each(|job| job.run());
    assert_eq!(slot.get(&compiler, LABEL, || counting(&runs)), Some(&1));
}

// Plausible defect: a program wgpu rejects is kept as compiled, its error
// is lost or raised on the worker, or the slot stays pending for ever. The
// oracle is wgpu's validation of a compute pipeline whose entry point its
// module lacks: the job reports that error once, a planning pass counts
// the slot settled, and a frame creates the programs in place as it would
// without a job.
#[test]
fn a_failed_job_is_reported_once_and_leaves_drawing_as_it_was() {
    let Some((device, _)) = test_support::device() else {
        return;
    };
    let compiler = Compiler::new(&device);
    let runs = Arc::new(AtomicUsize::new(0));
    let mut slot = Compiled::<usize>::default();
    let invalid = {
        let device = device.clone();
        move || {
            let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: None,
                source: wgpu::ShaderSource::Wgsl("@compute @workgroup_size(1) fn main() {}".into()),
            });
            let _ = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: None,
                layout: None,
                module: &module,
                entry_point: Some("absent"),
                compilation_options: Default::default(),
                cache: None,
            });
            0
        }
    };
    let (jobs, _) = compiler.plan(|| {
        slot.get(&compiler, LABEL, || invalid);
    });
    for job in jobs {
        std::thread::spawn(move || job.run()).join().unwrap();
    }
    let errors = compiler.take_errors();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].label, LABEL);
    assert!(
        errors[0].message.contains("absent"),
        "{}",
        errors[0].message
    );
    assert!(compiler.take_errors().is_empty(), "reported once");
    let (jobs, missing) = compiler.plan(|| {
        assert!(slot.get(&compiler, LABEL, || counting(&runs)).is_none());
    });
    assert_eq!((jobs.len(), missing), (0, 0), "a failure is settled");
    assert_eq!(slot.get(&compiler, LABEL, || counting(&runs)), Some(&1));
}

// Plausible defect: a job whose closure panics leaves its slot running for
// ever, so the frame that draws the variant waits on a thread that is gone
// and `compile` never reports ready. The oracle is the contract: `render`
// never depends on a job, so the frame creates the programs in place and
// nothing stays pending.
#[test]
fn a_job_that_panics_leaves_its_programs_to_the_frame() {
    let Some((device, _)) = test_support::device() else {
        return;
    };
    let compiler = Compiler::new(&device);
    let runs = Arc::new(AtomicUsize::new(0));
    let mut slot = Compiled::<usize>::default();
    let (jobs, _) = compiler.plan(|| {
        slot.get(&compiler, LABEL, || -> fn() -> usize {
            || panic!("the job's own failure, for the test")
        });
    });
    for job in jobs {
        assert!(std::thread::spawn(move || job.run()).join().is_err());
    }
    assert_eq!(compiler.outstanding(), 0);
    let (jobs, missing) = compiler.plan(|| {
        slot.get(&compiler, LABEL, || counting(&runs));
    });
    assert_eq!((jobs.len(), missing), (0, 0));
    assert_eq!(slot.get(&compiler, LABEL, || counting(&runs)), Some(&1));
}

// Plausible defect: a slot its cache dropped while its job was out (the
// geometry cache drops a specialisation's pipelines when the scene's lit
// constants change twice) leaves the compiler counting that job for ever,
// so `compile` never plans again. The oracle is the contract: a job is
// pending until it finishes or is dropped, whatever became of its slot.
#[test]
fn a_job_outlives_its_slot_without_staying_pending() {
    let Some((device, _)) = test_support::device() else {
        return;
    };
    let compiler = Compiler::new(&device);
    let runs = Arc::new(AtomicUsize::new(0));
    let mut slot = Compiled::<usize>::default();
    let (jobs, _) = compiler.plan(|| {
        slot.get(&compiler, LABEL, || counting(&runs));
    });
    drop(slot);
    assert_eq!(compiler.outstanding(), 1);
    jobs.into_iter().for_each(|job| job.run());
    assert_eq!(compiler.outstanding(), 0);
}
