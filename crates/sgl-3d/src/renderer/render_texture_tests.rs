//! Render textures at the frame boundary: what a frame that draws one
//! leaves in the view that shows it (the register's M-16) and in the view
//! that draws it (F-12), and what `Renderer::render` refuses.
use super::{Renderer, View, ViewFrame, ViewOutput};
use crate::asset::Asset;
use crate::settings::{Antialiasing, Bloom, Settings};
use crate::{
    Camera, FrameInput, InstanceId, InstanceState, Mobility, RenderTexture, RenderTextureId, Scene,
    SurfaceMaterial, test_support,
};
use glam::{Mat4, Vec3};

const SIZE: [u32; 2] = [64, 64];
const FEED: [u32; 2] = [16, 16];

/// TAA, whose resolve reads the temporal masks.
fn settings() -> Settings {
    Settings {
        antialiasing: Antialiasing::Taa,
        bloom: Bloom::Off,
        atmosphere: false,
        ..Settings::default()
    }
}

/// A camera at `eye` looking down -Z.
fn input(eye: Vec3) -> FrameInput {
    FrameInput::new(Camera {
        view: Mat4::from_translation(-eye),
        projection: crate::perspective(1., 1., 0.1),
        eye,
    })
}

/// An unlit black square 0.8 m wide, 3 m before the origin, that emits its
/// emission map: 15.6 texels about the centre of a 64-texel view from the
/// origin (cot(0.5) = 1.830, so a metre at 3 m spans 19.5 texels).
fn screen() -> Asset {
    let mut asset = test_support::cube();
    asset.meshes[0] = crate::asset::CpuMesh {
        vertices: [(-1., -1.), (1., -1.), (1., 1.), (-1., 1.)]
            .map(|(x, y): (f32, f32)| crate::asset::Vertex {
                tangent: [1., 0., 0., 1.],
                lightmap_bounds: [0., 0., 1., 1.],
                lightmap_uv: [0.; 2],
                position: [x * 0.4, y * 0.4, -3.],
                normal: [0., 0., 1.],
                uv: [(x + 1.) / 2., (1. - y) / 2.],
                color: [1.; 4],
            })
            .to_vec(),
        indices: vec![0, 1, 2, 0, 2, 3],
        material: 0,
        deformation: Default::default(),
    };
    asset.materials[0].base = [0., 0., 0., 1.];
    asset.materials[0].emissive = [1.; 3];
    asset.materials[0].unlit = true;
    asset
}

/// A main view at the origin before a screen that shows the render
/// texture a feed view, far along +X, draws.
struct Stage {
    device: wgpu::Device,
    queue: wgpu::Queue,
    scene: Scene,
    renderer: Renderer,
    main: View,
    feed: View,
    output: wgpu::TextureView,
    texture: RenderTextureId,
    screen: InstanceId,
}

impl Stage {
    /// None without a device, or where TAA does not run.
    fn new() -> Option<Stage> {
        let (device, queue) = test_support::device()?;
        let mut scene = Scene::new(&device, &queue);
        let ids = scene.add_asset(&device, &queue, screen()).unwrap();
        let screen = scene
            .add_instance(
                &device,
                &queue,
                InstanceState::new(ids.model),
                Mobility::Static,
            )
            .unwrap();
        let description = RenderTexture::new(FEED);
        let texture = scene.add_render_texture(&device, description).unwrap();
        let values = scene.material(ids.materials[0]).unwrap();
        scene
            .set_material(
                &queue,
                ids.materials[0],
                SurfaceMaterial {
                    emission_render_texture: Some(texture),
                    ..values
                },
            )
            .unwrap();
        let renderer = Renderer::new(&device);
        let format = crate::shading::gbuffer::COLOR;
        let view = |format, size| {
            View::new(&renderer, &device, &queue, format, size, 1., &settings()).unwrap()
        };
        let main = view(format, SIZE);
        if main.antialiasing_in_effect(&settings()) != Antialiasing::Taa {
            eprintln!("skipping: TAA does not run here");
            return None;
        }
        Some(Stage {
            feed: view(description.format(), FEED),
            output: crate::view::targets::target(&device, "render texture frame", SIZE, format),
            main,
            renderer,
            scene,
            device,
            queue,
            texture,
            screen,
        })
    }

    /// Renders, submits and finishes a frame of the main view and, where
    /// `feed` says, the feed, both listed with the main view first.
    fn frame(&mut self, feed: bool) {
        let settings = settings();
        let [main_input, feed_input] = [input(Vec3::ZERO), input(Vec3::X * 100.)];
        let mut views = vec![ViewFrame {
            view: &mut self.main,
            input: &main_input,
            settings: &settings,
            output: ViewOutput::Texture(&self.output),
            timing: None,
        }];
        if feed {
            views.push(ViewFrame {
                view: &mut self.feed,
                input: &feed_input,
                settings: &settings,
                output: ViewOutput::RenderTexture(self.texture),
                timing: None,
            });
        }
        let mut encoder = self.device.create_command_encoder(&Default::default());
        self.renderer.render(
            &self.device,
            &self.queue,
            &mut encoder,
            &mut self.scene,
            &mut views,
        );
        self.queue.submit([encoder.finish()]);
        self.renderer.finish_frame(&mut self.scene);
    }

    /// The main view's transparency and composition mask at texel
    /// (column, row), in 0..=1.
    fn composition(&self, (column, row): (u32, u32)) -> f32 {
        let masks = self.main.targets.temporal_masks.as_ref().unwrap();
        let texels = test_support::read(&self.device, &self.queue, masks[1].texture(), 1);
        f32::from(texels[(row * SIZE[0] + column) as usize]) / 255.
    }
}

// M-16, at the renderer. Plausible defects: the frame never tells the scene
// which render textures it draws, or tells it after the views that show
// them have marked their masks, so a screen whose picture changes is never
// marked, or a frame late; or a screen is marked in every frame. The
// fixture: a main view under TAA before a screen, and a feed that draws
// the screen's render texture in frames 0, 1 and 4 and is not listed in
// frames 2 and 3. The oracle is the register's rule: the screen's texels
// are marked transparency and composition 1 in the frames its producer
// renders and in the frame after it stops (frame 2), and 0 in frame 3; a
// texel beside the screen is never marked.
#[test]
fn a_screen_is_marked_in_the_frames_its_producer_draws_and_the_next() {
    let Some(mut stage) = Stage::new() else {
        return;
    };
    for (frame, (feed, marked)) in [(true, 1.), (true, 1.), (false, 1.), (false, 0.), (true, 1.)]
        .into_iter()
        .enumerate()
    {
        stage.frame(feed);
        let screen = stage.composition((32, 32));
        assert!(
            (screen - marked).abs() <= 1. / 255.,
            "frame {frame}: the screen's composition mask is {screen}, expected {marked}"
        );
        let beside = stage.composition((4, 4));
        assert!(
            beside <= 1. / 255.,
            "frame {frame}: the composition mask beside the screen is {beside}"
        );
    }
}

// F-12. Plausible defects: a producer the frame leaves out is begun all the
// same, so it settles and commits a frame it never drew, or its history
// continues across the gap when it next renders. The fixture: the feed
// listed in every frame, its screen hidden in frame 2. The oracles are the
// contract's: the feed's first frame restarts and its second continues;
// frame 2 leaves the feed's last render as it was, frame 1's; frame 3
// restarts the feed, and the main view, rendered throughout, continues.
#[test]
fn a_producer_left_out_of_a_frame_restarts_when_it_returns() {
    let Some(mut stage) = Stage::new() else {
        return;
    };
    let continued = |view: &View| view.rendered.as_ref().unwrap().history.valid;
    let attempt = |view: &View| view.rendered.as_ref().unwrap().attempt;
    stage.frame(true);
    assert!(!continued(&stage.feed), "the feed's first frame restarts");
    stage.frame(true);
    assert!(continued(&stage.feed), "the feed's second frame continues");
    let drawn = attempt(&stage.feed);
    let show = |stage: &mut Stage, visible: bool| {
        let state = InstanceState {
            visible,
            ..*stage.scene.instance(stage.screen).unwrap()
        };
        stage
            .scene
            .set_instance(&stage.queue, stage.screen, state)
            .unwrap();
    };
    show(&mut stage, false);
    stage.frame(true);
    assert_eq!(
        attempt(&stage.feed),
        drawn,
        "a frame with its screen hidden rendered the feed"
    );
    assert!(continued(&stage.main), "the main view continues");
    show(&mut stage, true);
    stage.frame(true);
    assert_ne!(attempt(&stage.feed), drawn, "the feed renders again");
    assert!(
        !continued(&stage.feed),
        "the feed continued its history across the frame that left it out"
    );
    assert!(continued(&stage.main), "the main view continues");
}

/// A frame of `stage`'s scene in which `first` and, where there is one,
/// `second` draw its render texture.
fn draw(stage: &mut Stage, first: &mut View, second: Option<&mut View>) {
    let settings = settings();
    let input = input(Vec3::ZERO);
    let mut views = vec![ViewFrame {
        view: first,
        input: &input,
        settings: &settings,
        output: ViewOutput::RenderTexture(stage.texture),
        timing: None,
    }];
    if let Some(second) = second {
        views.push(ViewFrame {
            view: second,
            input: &input,
            settings: &settings,
            output: ViewOutput::RenderTexture(stage.texture),
            timing: None,
        });
    }
    let mut encoder = stage.device.create_command_encoder(&Default::default());
    stage.renderer.render(
        &stage.device,
        &stage.queue,
        &mut encoder,
        &mut stage.scene,
        &mut views,
    );
}

/// A view of `stage`'s renderer with a render texture's format, of `size`.
fn feed_view(stage: &Stage, size: [u32; 2]) -> View {
    View::new(
        &stage.renderer,
        &stage.device,
        &stage.queue,
        RenderTexture::new(size).format(),
        size,
        1.,
        &settings(),
    )
    .unwrap()
}

// Plausible defect: a view sized for another texture draws into a render
// texture, stretching its picture with no error. The oracle is
// `Renderer::render`'s documented panic.
#[test]
#[should_panic(expected = "a view that draws a render texture has its size and format")]
fn a_view_of_another_size_than_its_render_texture_is_refused() {
    let Some(mut stage) = Stage::new() else {
        panic!("a view that draws a render texture has its size and format: no device");
    };
    let mut wrong = feed_view(&stage, [32, 32]);
    draw(&mut stage, &mut wrong, None);
}

// Plausible defect: two views draw one render texture in a frame and the
// later silently overwrites the earlier. The oracle is `Renderer::render`'s
// documented panic.
#[test]
#[should_panic(expected = "one view draws a render texture in a frame")]
fn two_views_drawing_one_render_texture_are_refused() {
    let Some(mut stage) = Stage::new() else {
        panic!("one view draws a render texture in a frame: no device");
    };
    let mut first = feed_view(&stage, FEED);
    let mut second = feed_view(&stage, FEED);
    draw(&mut stage, &mut first, Some(&mut second));
}

// Plausible defect: a view lists a render texture the scene removed and
// draws into nothing, or into another texture that took its index. The
// oracle is `Renderer::render`'s documented panic.
#[test]
#[should_panic(expected = "a view draws a render texture of the frame's scene")]
fn a_view_drawing_a_removed_render_texture_is_refused() {
    let Some(mut stage) = Stage::new() else {
        panic!("a view draws a render texture of the frame's scene: no device");
    };
    let spare = stage
        .scene
        .add_render_texture(&stage.device, RenderTexture::new(FEED))
        .unwrap();
    stage.scene.remove_render_texture(spare).unwrap();
    stage.texture = spare;
    let mut view = feed_view(&stage, FEED);
    draw(&mut stage, &mut view, None);
}
