//! The frame boundary between a renderer and its views: which frames a
//! view's history continues from (the register's F-1 and F-12) and which
//! views a renderer renders.
use super::{Renderer, View, ViewFrame, ViewOutput};
use crate::settings::Settings;
use crate::{Camera, FrameInput, Scene, test_support};

const SIZE: [u32; 2] = [16, 16];

fn input() -> FrameInput {
    FrameInput::new(Camera {
        view: glam::Mat4::IDENTITY,
        projection: crate::perspective(1., 1., 0.1),
        eye: glam::Vec3::ZERO,
    })
}

fn output(device: &wgpu::Device) -> wgpu::TextureView {
    crate::view::targets::target(
        device,
        "view boundary",
        SIZE,
        crate::shading::gbuffer::COLOR,
    )
}

fn view(renderer: &Renderer, device: &wgpu::Device, queue: &wgpu::Queue) -> View {
    View::new(
        renderer,
        device,
        queue,
        crate::shading::gbuffer::COLOR,
        SIZE,
        1.,
        &Settings::default(),
    )
    .unwrap()
}

/// Renders and submits one frame of `scene` seen by `view` alone, and
/// returns whether its history continued.
fn render(
    (device, queue): (&wgpu::Device, &wgpu::Queue),
    renderer: &mut Renderer,
    view: &mut View,
    scene: &mut Scene,
    output: &wgpu::TextureView,
) -> bool {
    let mut encoder = device.create_command_encoder(&Default::default());
    renderer.render(
        device,
        queue,
        &mut encoder,
        scene,
        &mut [ViewFrame {
            view,
            input: &input(),
            settings: &Settings::default(),
            output: ViewOutput::Texture(output),
            timing: None,
        }],
    );
    queue.submit([encoder.finish()]);
    view.rendered.as_ref().unwrap().history.valid
}

// Plausible defect: `finish_frame` of another scene than the one rendered
// commits the frame for the renderer's views though its scene committed
// nothing (they then settle it as finished, no longer follow the scene's
// last finished frame, and restart), or drops it (finishing the right
// scene afterwards commits nothing). The oracle is the contract
// (`Renderer::finish_frame`, F-3): finishing another scene commits
// nothing, so the rendered scene records no finished frame until it is
// finished itself, and an unfinished frame restarts no view.
#[test]
fn finish_frame_of_another_scene_commits_nothing() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let gpu = (&device, &queue);
    let mut renderer = Renderer::new(&device);
    let mut view = view(&renderer, &device, &queue);
    let output = output(&device);
    let mut scene = Scene::new(&device, &queue);
    let mut other = Scene::new(&device, &queue);
    render(gpu, &mut renderer, &mut view, &mut scene, &output);
    renderer.finish_frame(&mut other);
    assert_eq!(
        other.last_finished(),
        None,
        "the other scene finished nothing"
    );
    assert_eq!(
        scene.last_finished(),
        None,
        "the rendered scene was not finished"
    );
    renderer.finish_frame(&mut scene);
    assert!(
        scene.last_finished().is_some(),
        "the rendered scene's frame commits after another scene's finish"
    );
    assert!(
        render(gpu, &mut renderer, &mut view, &mut scene, &output),
        "the view continues from its scene's finished frame"
    );
    // A frame finished as another scene's is a frame not finished, which
    // restarts nothing: the view continues from its last finished frame,
    // not from the one another scene's finish claimed.
    renderer.finish_frame(&mut other);
    assert!(
        render(gpu, &mut renderer, &mut view, &mut scene, &output),
        "a frame finished as another scene's restarted the view"
    );
    renderer.finish_frame(&mut scene);
}

// Plausible defect: a view settles its pending frame as unfinished at any
// `&mut self` method, so a game that reads its geometry statistics or
// resizes it to the same size between the submission and `finish_frame`
// restarts its history every frame. The oracle is the frame contract: the
// frame was finished, so the next frame of the view continues (F-12).
#[test]
fn a_view_used_between_render_and_finish_keeps_its_history() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let gpu = (&device, &queue);
    let mut renderer = Renderer::new(&device);
    let mut view = view(&renderer, &device, &queue);
    let output = output(&device);
    let mut scene = Scene::new(&device, &queue);
    for frame in 0..3 {
        let continued = render(gpu, &mut renderer, &mut view, &mut scene, &output);
        assert_eq!(continued, frame > 0, "frame {frame}");
        view.geometry_stats(&device);
        view.resize(&device, SIZE, 1., &Settings::default());
        renderer.finish_frame(&mut scene);
    }
}

// Plausible defect: a renderer renders a view another renderer created,
// whose finished frames it never records, so the view settles every frame
// as unfinished and silently never accumulates history. The oracle is the
// contract (`Renderer::render`): it panics.
#[test]
fn a_view_of_another_renderer_is_refused() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let first = Renderer::new(&device);
    let mut second = Renderer::new(&device);
    let mut view = view(&first, &device, &queue);
    let output = output(&device);
    let mut scene = Scene::new(&device, &queue);
    let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        render(
            (&device, &queue),
            &mut second,
            &mut view,
            &mut scene,
            &output,
        )
    }))
    .expect_err("a renderer rendered another renderer's view");
    let message = refused
        .downcast_ref::<&str>()
        .map(|message| message.to_string())
        .or_else(|| refused.downcast_ref::<String>().cloned());
    assert!(
        message.is_some_and(|message| message.contains("created from it")),
        "the refusal names the rule"
    );
}

// Plausible defect: a view settles its pending frame against the renderer's
// last finished frame rather than its own frame's outcome, so a view left
// out of the next finished frame (one drawn at half rate, as the guides
// recommend) settles its own finished frame as unfinished: its statistics
// and probe readbacks, its culling pyramid, exposure and local shadow cache
// never commit. The fixture: views A and B with one camera and one
// instance added before each frame, so each frame draws one more cube
// than the last; A in every frame, B in even ones, each view's statistics
// asked for before its render and after the GPU finished the frame before.
// The oracle is equivalence: B's statistics, once the frame after its own
// was finished without it, are A's for that same frame, which differ from
// every other frame's.
#[test]
fn a_view_left_out_of_a_frame_reads_back_its_own_finished_frame() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let settings = Settings {
        occlusion_culling: false,
        ..Settings::default()
    };
    let mut renderer = Renderer::new(&device);
    let [mut a, mut b] = [(); 2].map(|_| {
        View::new(
            &renderer,
            &device,
            &queue,
            crate::shading::gbuffer::COLOR,
            SIZE,
            1.,
            &settings,
        )
        .unwrap()
    });
    let outputs = [output(&device), output(&device)];
    let mut scene = Scene::new(&device, &queue);
    let model = scene
        .add_asset(&device, &queue, test_support::cube())
        .unwrap()
        .model;
    let wait = || device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    // The statistics of `view`'s last frame, once the GPU finished it.
    let read = |view: &mut View| {
        view.geometry_stats(&device);
        wait();
        view.geometry_stats(&device)
    };
    let input = input();
    let frames = 7;
    let mut of_a = Vec::new();
    let mut of_b = Vec::new();
    for frame in 0..frames {
        let x = -3.5 + frame as f32;
        scene
            .add_instance(
                &device,
                &queue,
                crate::InstanceState {
                    pose: glam::Mat4::from_translation(glam::Vec3::new(x, 0., -8.)),
                    ..crate::InstanceState::new(model)
                },
                crate::Mobility::Static,
            )
            .unwrap();
        if frame > 0 {
            of_a.push(read(&mut a));
        }
        let with_b = frame % 2 == 0;
        if with_b && frame > 0 {
            of_b.push(read(&mut b));
        }
        let mut encoder = device.create_command_encoder(&Default::default());
        let mut list = vec![ViewFrame {
            view: &mut a,
            input: &input,
            settings: &settings,
            output: ViewOutput::Texture(&outputs[0]),
            timing: None,
        }];
        if with_b {
            list.push(ViewFrame {
                view: &mut b,
                input: &input,
                settings: &settings,
                output: ViewOutput::Texture(&outputs[1]),
                timing: None,
            });
        }
        renderer.render(&device, &queue, &mut encoder, &mut scene, &mut list);
        drop(list);
        queue.submit([encoder.finish()]);
        renderer.finish_frame(&mut scene);
        wait();
    }
    // A's frames 0 to 5, and B's frames 0, 2 and 4.
    assert!(
        of_a.windows(2)
            .all(|pair| pair[0].is_some() && pair[0] != pair[1]),
        "each frame's statistics are its own: {of_a:?}"
    );
    for (index, b) in of_b.iter().enumerate() {
        let frame = 2 * index;
        assert_eq!(
            *b,
            of_a[frame],
            "B's statistics after its frame {frame}, left out of frame {}",
            frame + 1
        );
    }
}

// Plausible defect: SMAA's lookup tables are decoded and uploaded for every
// renderer or every view whether or not SMAA ever runs, or again for each
// view that runs it (#893). The oracle is the library's upload counters at
// SMAA's own call sites: nothing while a renderer and a view render with
// SMAA off; then the two tables, area 160x560 and search 66x33 RGBA8 as
// the README gives their PNGs, written once when two views run SMAA.
#[test]
fn smaa_tables_upload_once_for_the_first_view_that_may_run_smaa() {
    use crate::settings::Antialiasing;
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let uploads = |since: &crate::counters::Counters| {
        let counted = crate::counters::snapshot().since(since);
        counted
            .uploads
            .iter()
            .filter(|site| site.file.ends_with("smaa/mod.rs"))
            .fold((0, 0), |(writes, bytes), site| {
                (writes + site.writes, bytes + site.bytes)
            })
    };
    let output = output(&device);
    let mut scene = Scene::new(&device, &queue);
    let frame = |renderer: &mut Renderer,
                 scene: &mut Scene,
                 views: &mut [&mut View],
                 settings: &Settings| {
        let mut encoder = device.create_command_encoder(&Default::default());
        let input = input();
        let mut list: Vec<ViewFrame<'_>> = views
            .iter_mut()
            .map(|view| ViewFrame {
                view,
                input: &input,
                settings,
                output: ViewOutput::Texture(&output),
                timing: None,
            })
            .collect();
        renderer.render(&device, &queue, &mut encoder, scene, &mut list);
        drop(list);
        queue.submit([encoder.finish()]);
        renderer.finish_frame(scene);
    };
    let started = crate::counters::snapshot();
    let mut renderer = Renderer::new(&device);
    let off = Settings {
        antialiasing: Antialiasing::Off,
        ..Settings::default()
    };
    let format = crate::shading::gbuffer::COLOR;
    let mut plain = View::new(&renderer, &device, &queue, format, SIZE, 1., &off).unwrap();
    for _ in 0..2 {
        frame(&mut renderer, &mut scene, &mut [&mut plain], &off);
    }
    assert_eq!(uploads(&started), (0, 0), "no SMAA table without SMAA");
    let smaa = Settings {
        antialiasing: Antialiasing::Smaa,
        ..Settings::default()
    };
    let mut first = View::new(&renderer, &device, &queue, format, SIZE, 1., &smaa).unwrap();
    let mut second = View::new(&renderer, &device, &queue, format, SIZE, 1., &smaa).unwrap();
    assert_eq!(uploads(&started), (0, 0), "a view's creation uploads none");
    frame(&mut renderer, &mut scene, &mut [&mut first], &smaa);
    frame(
        &mut renderer,
        &mut scene,
        &mut [&mut second, &mut first],
        &smaa,
    );
    // Beside the tables, each view's SMAA writes its own inverse size, two
    // f32s, when it first runs.
    assert_eq!(
        uploads(&started),
        (2 + 2, 4 * (160 * 560 + 66 * 33) + 2 * 8),
        "both tables, once, for the renderer's views"
    );
}
