//! The frame boundary between a renderer and its views: which frames a
//! view's history continues from (the register's F-1 and F-12) and which
//! views a renderer renders.
use super::{Renderer, View, ViewFrame};
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
            output,
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
