//! `Renderer::compile` against ordinary rendering of the same scene, view,
//! settings and frame input: what each creates, counted by wgpu itself
//! (`wgpu::Instance::generate_report`), and what each draws.
use super::{Renderer, View, ViewFrame, ViewOutput};
use crate::settings::{ContactShadows, Settings};
use crate::{Camera, DirectionalLight, DirectionalShadow, FrameInput, Scene, test_support};
use glam::{Mat4, Vec3};

const SIZE: [u32; 2] = [64, 48];
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// A game's level on a device of its own: an opaque cube, a masked one and
/// a blended one under a sun with two cascades and a point light that
/// casts shadows, seen by one view. The camera's opaque surfaces and the
/// cascades draw from GPU-built lists, the blended cube and the point
/// light's shadow faces from CPU-built ones.
struct Level {
    instance: wgpu::Instance,
    device: wgpu::Device,
    queue: wgpu::Queue,
    scene: Scene,
    renderer: Renderer,
    view: View,
    /// A second view that draws a render texture the opaque cube emits,
    /// where the level has one (`with_feed`).
    feed: Option<(View, crate::RenderTextureId)>,
    /// The opaque cube's material.
    opaque: crate::MaterialId,
    /// The opaque, masked and blended cubes.
    cubes: [crate::InstanceId; 3],
    output: wgpu::Texture,
    settings: Settings,
    /// A game's HDR effect the rich level's frames apply, where it has one.
    effect: Option<crate::HdrEffect>,
    /// The outline the rich level's frames draw, where it has one.
    outline: Option<crate::Outline>,
    /// Its live specular probes.
    probes: Vec<crate::LiveSpecularProbeId>,
    /// The environment `probe_input` frames are under.
    environment: crate::FrameEnvironment,
}

/// The optional device features a level's device is requested with.
#[derive(Clone, Copy, Debug)]
enum Gpu {
    Plain,
    /// `graphics_device::fsr_features`.
    Fsr,
    /// `graphics_device::ray_tracing_features`.
    RayTracing,
    /// A device that refuses a compute shader with more than one
    /// invocation in a workgroup, which every compute stage's has.
    NoWorkgroups,
}

impl Level {
    fn new() -> Option<Self> {
        Self::on(Gpu::Plain, test_support::cube())
    }

    /// The level with `opaque` as its opaque cube.
    fn with_opaque(opaque: crate::asset::Asset) -> Option<Self> {
        Self::on(Gpu::Plain, opaque)
    }

    /// The level, with `opaque` as its opaque cube, on a device with
    /// `gpu`'s features, or `None` after printing that the adapter lacks
    /// them.
    fn on(gpu: Gpu, opaque: crate::asset::Asset) -> Option<Self> {
        let mut supported = true;
        let (instance, device, queue) =
            test_support::counted_device_choosing(|adapter, limits| {
                let features = match gpu {
                    Gpu::Plain => return wgpu::Features::empty(),
                    Gpu::Fsr => crate::graphics_device::fsr_features(adapter),
                    Gpu::RayTracing => crate::graphics_device::ray_tracing_features(adapter),
                    Gpu::NoWorkgroups => {
                        limits.max_compute_invocations_per_workgroup = 1;
                        return wgpu::Features::empty();
                    }
                };
                supported = !features.is_empty();
                features
            })?;
        if !supported {
            eprintln!("unsupported: the adapter lacks the features of {gpu:?}");
            return None;
        }
        let mut scene = Scene::new(&device, &queue);
        let (opaque, opaque_cube) = test_support::add_static(&device, &queue, &mut scene, opaque);
        let (_, masked) = test_support::add_static(
            &device,
            &queue,
            &mut scene,
            test_support::masked(test_support::cube(), 0.5),
        );
        scene
            .set_instance(
                &queue,
                masked,
                crate::InstanceState {
                    pose: Mat4::from_translation(Vec3::new(1.5, 0., 0.)),
                    ..*scene.instance(masked).unwrap()
                },
            )
            .unwrap();
        let mut blended = test_support::cube();
        blended.materials[0].alpha = crate::AlphaMode::Blend {
            receives_screen_space_reflections: false,
            keeps_specular: false,
        };
        blended.materials[0].base[3] = 0.5;
        let (_, blended) = test_support::add_static(&device, &queue, &mut scene, blended);
        scene
            .set_instance(
                &queue,
                blended,
                crate::InstanceState {
                    pose: Mat4::from_translation(Vec3::new(-1.5, 0., 0.)),
                    ..*scene.instance(blended).unwrap()
                },
            )
            .unwrap();
        scene
            .add_light(
                &device,
                &queue,
                crate::Light {
                    position: Vec3::new(0.75, 2., 2.),
                    shape: crate::LightShape::Point {
                        radius: crate::LightShape::DEFAULT_RADIUS,
                    },
                    intensity: 20.,
                    range: 8.,
                    casts_shadow: true,
                    ..Default::default()
                },
            )
            .unwrap();
        let settings = Settings::default();
        let renderer = Renderer::new(&device);
        let view = View::new(&renderer, &device, &queue, FORMAT, SIZE, 1., &settings).unwrap();
        let output = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("compile test output"),
            size: wgpu::Extent3d {
                width: SIZE[0],
                height: SIZE[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        Some(Self {
            instance,
            device,
            queue,
            scene,
            renderer,
            view,
            feed: None,
            effect: None,
            outline: None,
            probes: Vec::new(),
            environment: crate::FrameEnvironment::None,
            opaque: opaque.materials[0],
            cubes: [opaque_cube, masked, blended],
            output,
            settings,
        })
    }

    /// The level with a render texture its first material emits, drawn by
    /// a second view, which a frame renders before the view that shows it.
    fn with_feed() -> Option<Self> {
        let mut level = Self::new()?;
        level.add_feed();
        Some(level)
    }

    /// Adds `with_feed`'s render texture and view, with the level's
    /// settings.
    fn add_feed(&mut self) {
        let description = crate::RenderTexture::new([16, 16]);
        let texture = self
            .scene
            .add_render_texture(&self.device, description)
            .unwrap();
        let values = self.scene.material(self.opaque).unwrap();
        self.scene
            .set_material(
                &self.queue,
                self.opaque,
                crate::SurfaceMaterial {
                    emission_render_texture: Some(texture),
                    ..values
                },
            )
            .unwrap();
        let feed = View::new(
            &self.renderer,
            &self.device,
            &self.queue,
            description.format(),
            [16, 16],
            1.,
            &self.settings,
        )
        .unwrap();
        self.feed = Some((feed, texture));
    }

    /// The frame's views for `render` or `compile`: the level's view into
    /// `output`, then the feed into its render texture.
    fn views<'a>(
        view: &'a mut View,
        feed: &'a mut Option<(View, crate::RenderTextureId)>,
        (input, settings): (&'a FrameInput, &'a Settings),
        output: &'a wgpu::TextureView,
    ) -> Vec<ViewFrame<'a>> {
        let mut views = vec![ViewFrame {
            view,
            input,
            settings,
            output: ViewOutput::Texture(output),
            timing: None,
        }];
        if let Some((feed, texture)) = feed {
            views.push(ViewFrame {
                view: feed,
                input,
                settings,
                output: ViewOutput::RenderTexture(*texture),
                timing: None,
            });
        }
        views
    }

    /// The frame with the camera `along` its track.
    fn input(along: f32) -> FrameInput {
        let eye = Vec3::new(0.75 + along, 1.5, 5.);
        let mut input = FrameInput::new(Camera {
            view: glam::camera::rh::view::look_at_mat4(eye, Vec3::new(0.75, 0., 0.), Vec3::Y),
            projection: crate::perspective(1., SIZE[0] as f32 / SIZE[1] as f32, 0.1),
            eye,
        });
        input.camera_cut = along == 0.;
        input.directional_lights[0] = Some(DirectionalLight {
            direction: Vec3::new(-0.4, -1., -0.3),
            color: [1.; 3],
            illuminance: 3.,
            shadow: Some(DirectionalShadow {
                distance: 20.,
                cascades: 2,
            }),
            ..Default::default()
        });
        input
    }

    /// One `Renderer::compile` of the frame `input`.
    fn compile(&mut self, input: &FrameInput) -> super::Compilation {
        let output = self.output.create_view(&Default::default());
        self.renderer.compile(
            &self.device,
            &self.queue,
            &mut self.scene,
            &mut Self::views(
                &mut self.view,
                &mut self.feed,
                (input, &self.settings),
                &output,
            ),
        )
    }

    /// Compiles the frame `input` until nothing is pending, running each
    /// job on a thread of its own. Returns how many jobs ran.
    fn compile_all(&mut self, input: &FrameInput) -> usize {
        let mut ran = 0;
        for _ in 0..8 {
            let compilation = self.compile(input);
            assert_eq!(compilation.errors, []);
            if compilation.ready() {
                return ran;
            }
            ran += compilation.jobs.len();
            let workers: Vec<_> = compilation
                .jobs
                .into_iter()
                .map(|job| std::thread::spawn(move || job.run()))
                .collect();
            workers
                .into_iter()
                .for_each(|worker| worker.join().unwrap());
        }
        panic!("the frame was never ready");
    }

    /// Renders, submits and finishes the frame `input`, and returns its
    /// output's bytes.
    fn render(&mut self, input: &FrameInput) -> Vec<u8> {
        self.view.resize(&self.device, SIZE, 1., &self.settings);
        let output = self.output.create_view(&Default::default());
        let mut encoder = self.device.create_command_encoder(&Default::default());
        self.renderer.render(
            &self.device,
            &self.queue,
            &mut encoder,
            &mut self.scene,
            &mut Self::views(
                &mut self.view,
                &mut self.feed,
                (input, &self.settings),
                &output,
            ),
        );
        self.queue.submit([encoder.finish()]);
        self.renderer.finish_frame(&mut self.scene);
        test_support::read(&self.device, &self.queue, &self.output, 4)
    }

    /// The pipelines and shader modules wgpu holds for the device.
    fn programs(&self) -> (usize, usize) {
        let report = self.instance.generate_report().unwrap();
        let hub = report.hub_report();
        (
            hub.render_pipelines.num_allocated + hub.compute_pipelines.num_allocated,
            hub.shader_modules.num_allocated,
        )
    }
}

// Plausible defect: `compile` reports ready while the frame still lacks a
// pipeline or shader module (a variant the planning pass did not reach, a
// job's result that never reached its cache), so the first frame compiles
// on the window thread after all. The oracle is wgpu's own count of the
// pipelines and shader modules the device holds: unchanged across a
// compiled level's first frame, and raised by an ordinary one, which
// shows the count sees a frame's creation.
#[test]
fn a_compiled_levels_first_frame_creates_no_program() {
    let (Some(mut compiled), Some(mut ordinary)) = (Level::new(), Level::new()) else {
        return;
    };
    let input = Level::input(0.);
    let jobs = compiled.compile_all(&input);
    assert!(
        jobs > 0,
        "the level draws geometry, whose pipelines are jobs"
    );
    let before = compiled.programs();
    compiled.render(&input);
    assert_eq!(compiled.programs(), before);
    let before = ordinary.programs();
    ordinary.render(&input);
    let after = ordinary.programs();
    assert!(
        after.0 > before.0 && after.1 > before.1,
        "an ordinary first frame creates its programs: {before:?} to {after:?}"
    );
}

// Plausible defect: depth of field, a stage the view did not create its
// pipelines for (it was off when the view was made), left out of the
// planning pass or making them only when it draws, so a level that turns
// it on for a photo mode compiles on the window thread after all. The
// oracle is wgpu's count, as `a_compiled_levels_first_frame_creates_no_program`'s:
// unchanged across the compiled level's first frame with depth of field
// on, which the frame ran.
#[test]
fn a_compiled_level_with_depth_of_field_creates_no_program_at_its_first_frame() {
    let Some(mut compiled) = Level::new() else {
        return;
    };
    compiled.settings.depth_of_field = crate::settings::DepthOfField::High;
    let input = Level::input(0.);
    compiled.compile_all(&input);
    let before = compiled.programs();
    compiled.render(&input);
    assert_eq!(compiled.programs(), before);
    assert!(
        compiled.view.depth_of_field.output().is_some(),
        "the frame ran depth of field"
    );
}

/// What wgpu holds for the device, counted by kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Held {
    textures: usize,
    buffers: usize,
    bind_groups: usize,
    render_pipelines: usize,
    compute_pipelines: usize,
    shader_modules: usize,
}

impl Level {
    fn held(&self) -> Held {
        let report = self.instance.generate_report().unwrap();
        let hub = report.hub_report();
        Held {
            textures: hub.textures.num_allocated,
            buffers: hub.buffers.num_allocated,
            bind_groups: hub.bind_groups.num_allocated,
            render_pipelines: hub.render_pipelines.num_allocated,
            compute_pipelines: hub.compute_pipelines.num_allocated,
            shader_modules: hub.shader_modules.num_allocated,
        }
    }
}

// Plausible defects: a view that applies no HDR effect paying for the stage
// (a target, buffer, pipeline or bind group made at the view, or at a
// scene's first effect, or kept after its last frame with one), or an
// effect costing more than its pass and one target. The oracle is wgpu's
// own count of what the device holds: a level whose scene holds an effect
// that no frame applies holds what a level without one holds, frame for
// frame, and draws the same bytes; a frame that applies it adds exactly
// one texture (its target), two buffers (its frame values and parameters)
// and one render pipeline, and no compute pipeline or shader module (a
// module lives only while its pipeline is created); the frame after,
// without it, holds one texture fewer again.
#[test]
fn an_hdr_effect_costs_nothing_unapplied_and_one_target_applied() {
    let (Some(mut plain), Some(mut holding)) = (Level::new(), Level::new()) else {
        return;
    };
    let effect = test_support::test_effect(&mut holding.scene, [0.1; 3]);
    for along in [0., 0.1] {
        let input = Level::input(along);
        let bytes = [plain.render(&input), holding.render(&input)];
        assert_eq!(plain.held(), holding.held(), "frame at {along}");
        assert!(bytes[0] == bytes[1], "the frames differ at {along}");
    }
    let before = holding.held();
    let mut applied = Level::input(0.2);
    applied.hdr_effect = Some(effect);
    holding.render(&applied);
    let after = holding.held();
    assert!(holding.view.hdr_effect.output().is_some(), "the effect ran");
    assert_eq!(
        after.textures,
        before.textures + 1,
        "{before:?} to {after:?}"
    );
    assert_eq!(after.buffers, before.buffers + 2, "{before:?} to {after:?}");
    assert_eq!(after.render_pipelines, before.render_pipelines + 1);
    assert_eq!(after.shader_modules, before.shader_modules);
    assert_eq!(after.compute_pipelines, before.compute_pipelines);
    holding.render(&Level::input(0.3));
    assert_eq!(
        holding.held().textures,
        before.textures,
        "the target is released"
    );
}

// Plausible defect: the HDR effect's pipeline, a game's program, left out
// of the planning pass or created where the stage draws, so a level that
// turns an effect on compiles on the window thread after all. The oracle
// is wgpu's count, as `a_compiled_levels_first_frame_creates_no_program`'s:
// unchanged across the compiled level's first frame with the effect, which
// the frame ran; and the planning pass found the effect's pipeline among
// its jobs, one more than the same level without it.
#[test]
fn a_compiled_level_with_an_hdr_effect_creates_no_program_at_its_first_frame() {
    let (Some(mut compiled), Some(mut without)) = (Level::new(), Level::new()) else {
        return;
    };
    let mut input = Level::input(0.);
    input.hdr_effect = Some(test_support::test_effect(&mut compiled.scene, [0.1; 3]));
    let jobs = compiled.compile_all(&input);
    let before = compiled.programs();
    compiled.render(&input);
    assert_eq!(compiled.programs(), before);
    assert!(
        compiled.view.hdr_effect.output().is_some(),
        "the frame ran the effect"
    );
    assert_eq!(jobs, without.compile_all(&Level::input(0.)) + 1);
}

// Plausible defect: an effect the scene no longer holds still applied, or
// its pipeline kept by every view for the scene's lifetime. The oracle is
// the contract (a frame whose input names a removed effect applies none, as
// one naming no effect) and wgpu's count of render pipelines, one fewer
// once the frame after the removal has run.
#[test]
fn a_removed_hdr_effect_applies_none_and_its_pipeline_goes() {
    let (Some(mut level), Some(mut plain)) = (Level::new(), Level::new()) else {
        return;
    };
    let mut input = Level::input(0.);
    let effect = test_support::test_effect(&mut level.scene, [0.5; 3]);
    input.hdr_effect = Some(effect);
    level.render(&input);
    let pipelines = level.held().render_pipelines;
    level.scene.remove_hdr_effect(effect.effect()).unwrap();
    let mut next = Level::input(0.);
    next.hdr_effect = Some(effect);
    let bytes = level.render(&next);
    assert!(
        level.view.hdr_effect.output().is_none(),
        "a removed effect ran"
    );
    assert_eq!(level.held().render_pipelines, pipelines - 1);
    plain.render(&Level::input(0.));
    assert!(
        plain.render(&Level::input(0.)) == bytes,
        "the frame differs from one with no effect"
    );
}

// Plausible defects: a view that outlines nothing paying for the stage (a
// target, buffer, pipeline or bind group made at the view, or for a
// selection the scene holds that no frame names, or kept after its last
// outlined frame), or an outline costing more than its three passes'
// targets, buffers, groups and two programs. The oracle is wgpu's own
// count of what the device holds: a level whose scene holds a selection
// that no frame names holds what a level without one holds, frame for
// frame, and draws the same bytes; a frame that outlines the opaque cube,
// whose caster pipeline the point light's shadow already made, adds
// exactly two textures (the mask and the rows), two buffers (the view and
// the outline), three bind groups and two render pipelines, and no compute
// pipeline or shader module; the frame after, without it, holds what it
// held before.
#[test]
fn an_outline_costs_nothing_unnamed_and_two_targets_named() {
    let (Some(mut plain), Some(mut holding)) = (Level::new(), Level::new()) else {
        return;
    };
    let selection = holding.scene.add_selection(&[holding.cubes[0]]).unwrap();
    for along in [0., 0.1] {
        let input = Level::input(along);
        let bytes = [plain.render(&input), holding.render(&input)];
        assert_eq!(plain.held(), holding.held(), "frame at {along}");
        assert!(bytes[0] == bytes[1], "the frames differ at {along}");
    }
    let before = holding.held();
    let mut outlined = Level::input(0.2);
    outlined.outline = Some(crate::Outline::new(selection));
    holding.render(&outlined);
    let after = holding.held();
    assert!(holding.view.outline.holds_targets(), "the outline ran");
    let expected = Held {
        textures: before.textures + 2,
        buffers: before.buffers + 2,
        bind_groups: before.bind_groups + 3,
        render_pipelines: before.render_pipelines + 2,
        ..before
    };
    assert_eq!(after, expected, "{before:?} to {after:?}");
    holding.render(&Level::input(0.3));
    let released = holding.held();
    assert_eq!(
        (released.textures, released.buffers, released.bind_groups),
        (before.textures, before.buffers, before.bind_groups),
        "what the outline held is released"
    );
}

// Plausible defect: the outline's programs, or the mask's caster pipelines
// for a selected material the frame draws no other caster of (a blended
// one, which casts no shadow), left out of the planning pass or created
// where the stage draws, so a game that highlights its first target
// compiles on the window thread after all. The oracle is wgpu's count, as
// `a_compiled_levels_first_frame_creates_no_program`'s: unchanged across
// the compiled level's first frame with every cube outlined, which the
// frame ran; and the planning pass found more jobs than the same level
// without the outline.
#[test]
fn a_compiled_level_with_an_outline_creates_no_program_at_its_first_frame() {
    let (Some(mut compiled), Some(mut without)) = (Level::new(), Level::new()) else {
        return;
    };
    let selection = compiled.scene.add_selection(&compiled.cubes).unwrap();
    let mut input = Level::input(0.);
    input.outline = Some(crate::Outline::new(selection));
    let jobs = compiled.compile_all(&input);
    let before = compiled.programs();
    compiled.render(&input);
    assert_eq!(compiled.programs(), before);
    assert!(compiled.view.outline.holds_targets(), "the frame outlined");
    assert!(jobs > without.compile_all(&Level::input(0.)));
}

// Plausible defect: the planning passes leave something behind that the
// first frames differ by (history advanced, a target written, a stage's
// state changed), a pipeline a job made differs from the one a frame
// makes, or a planning pass asks for a variant no frame draws, so
// compiling costs more than drawing. The oracle is equivalence with
// ordinary rendering on the same inputs: the same bytes in the first
// frame and in the one after the camera moved, and the same number of
// programs held, compiled ahead or not.
#[test]
fn compiled_and_ordinary_frames_are_the_same() {
    let (Some(mut compiled), Some(mut ordinary)) = (Level::new(), Level::new()) else {
        return;
    };
    let (first, moved) = (Level::input(0.), Level::input(0.25));
    compiled.compile_all(&first);
    assert_eq!(compiled.render(&first), ordinary.render(&first));
    assert_eq!(compiled.programs(), ordinary.programs());
    assert_eq!(compiled.render(&moved), ordinary.render(&moved));
    assert_eq!(compiled.programs(), ordinary.programs());
}

// Plausible defect: a frame that draws while jobs are neither run nor
// dropped waits for them for ever, draws without their pipelines, or
// compiles them beside the jobs; or `pending` counts requests, not
// programs, so a loading bar jumps (the two cascades draw their casters
// with one pipeline). The oracle is the contract (one job a program,
// `pending` the jobs not finished) and equivalence with ordinary
// rendering: the same bytes and the same programs held, the late jobs
// compiling nothing more.
#[test]
fn a_frame_drawn_before_its_jobs_ran_is_an_ordinary_frame() {
    let (Some(mut early), Some(mut ordinary)) = (Level::new(), Level::new()) else {
        return;
    };
    let input = Level::input(0.);
    let compilation = early.compile(&input);
    assert!(!compilation.ready());
    // Each program waited for is a job, however many passes and cascades
    // draw with it.
    assert_eq!(compilation.pending, compilation.jobs.len());
    assert_eq!(early.compile(&input).pending, compilation.jobs.len());
    assert_eq!(early.render(&input), ordinary.render(&input));
    compilation.jobs.into_iter().for_each(|job| job.run());
    assert_eq!(early.programs(), ordinary.programs());
    assert!(early.compile(&Level::input(0.25)).ready());
}

// Plausible defect: after a settings change or a resize `compile` reports
// ready from what it compiled before, or compiles everything again. The
// oracle is equivalence with ordinary rendering through the same change:
// contact shadows give the camera's surfaces other pipelines, so there
// are jobs, the changed frame then creates nothing, and both levels hold
// the same programs and draw the same bytes; and the architecture's
// Frame, by which a stage keeps its pipelines across a resize of its
// targets, so after one nothing is pending and none is created.
#[test]
fn a_settings_change_compiles_what_is_new_and_a_resize_nothing() {
    let (Some(mut compiled), Some(mut ordinary)) = (Level::new(), Level::new()) else {
        return;
    };
    let (first, moved) = (Level::input(0.), Level::input(0.25));
    compiled.compile_all(&first);
    compiled.render(&first);
    ordinary.render(&first);
    for level in [&mut compiled, &mut ordinary] {
        level.settings.contact_shadows = ContactShadows::Short;
    }
    let before = ordinary.programs();
    let changed = ordinary.render(&moved);
    assert!(
        ordinary.programs().0 > before.0,
        "the change needs programs"
    );
    assert!(compiled.compile_all(&moved) > 0);
    let before = compiled.programs();
    assert_eq!(compiled.render(&moved), changed);
    assert_eq!(compiled.programs(), before);
    assert_eq!(compiled.programs(), ordinary.programs());

    let resized = [SIZE[0] * 2, SIZE[1] * 2];
    compiled
        .view
        .resize(&compiled.device, resized, 1., &compiled.settings);
    let before = compiled.programs();
    assert!(compiled.compile(&moved).ready());
    assert_eq!(compiled.programs(), before);
}

// Plausible defect: a planning pass renders the views as listed, not in
// the frame's order, or leaves out the view that draws a render texture,
// so the programs of the frame's second view (its own stages') are
// created by the first frame. The oracle is wgpu's count, unchanged
// across the compiled first frame, and equivalence with ordinary
// rendering of the same two views: the same bytes and the same programs
// held.
#[test]
fn a_view_that_draws_a_render_texture_is_compiled_with_the_view_that_shows_it() {
    let (Some(mut compiled), Some(mut ordinary)) = (Level::with_feed(), Level::with_feed()) else {
        return;
    };
    let input = Level::input(0.);
    compiled.compile_all(&input);
    let before = compiled.programs();
    let drawn = compiled.render(&input);
    assert_eq!(compiled.programs(), before);
    assert_eq!(drawn, ordinary.render(&input));
    assert_eq!(compiled.programs(), ordinary.programs());
}

// Plausible defect: the planning pass reaches a material's pipelines
// without the maps it samples in the shader (`Variant::shader_sampled`),
// so the first frame creates them on the window thread. The opaque cube's
// base map repeats and its emission map, on the same image, clamps, so
// the shader samples the emission map. The oracle is wgpu's count of the
// pipelines and shader modules, unchanged across the compiled first frame.
#[test]
fn a_compiled_frame_with_a_map_sampled_in_the_shader_creates_no_program() {
    use crate::asset::{Image, MaterialTexture, Sampler, TextureWrap};
    let mut cube = test_support::cube();
    cube.images = vec![Image::Rgba8(image::RgbaImage::from_pixel(
        2,
        2,
        image::Rgba([200; 4]),
    ))];
    cube.materials[0].base_texture = Some(MaterialTexture::new(0));
    cube.materials[0].emissive_texture = Some(MaterialTexture {
        sampler: Sampler {
            wrap: [TextureWrap::ClampToEdge; 2],
            ..Sampler::default()
        },
        ..MaterialTexture::new(0)
    });
    let Some(mut level) = Level::with_opaque(cube) else {
        return;
    };
    let input = Level::input(0.);
    level.compile_all(&input);
    let before = level.programs();
    level.render(&input);
    assert_eq!(level.programs(), before);
}

// Plausible defect: a view composes its reflections with one source
// completion variant while it shows a planar reflector and another once the
// reflector leaves it, and `compile` readies only the first, so a level
// whose first frame shows a mirror creates the second on the window thread
// the first time the camera turns away. The oracle is wgpu's count,
// unchanged across that frame, after the level was compiled and drawn
// facing the mirror.
#[test]
fn a_compiled_level_creates_nothing_when_its_mirror_leaves_the_view() {
    let Some(mut level) = Level::new() else {
        return;
    };
    let reflector = level
        .scene
        .add_planar_reflector(crate::PlanarReflector::new(Vec3::new(0., 0., 0.5), Vec3::Z))
        .unwrap();
    let values = level.scene.material(level.opaque).unwrap();
    level
        .scene
        .set_material(
            &level.queue,
            level.opaque,
            crate::SurfaceMaterial {
                planar_reflector: Some(reflector),
                ..values
            },
        )
        .unwrap();
    let facing = Level::input(0.);
    level.compile_all(&facing);
    level.render(&facing);
    let mut away = Level::input(0.);
    let eye = away.camera.eye;
    away.camera.view = glam::camera::rh::view::look_at_mat4(eye, eye + Vec3::Z, Vec3::Y);
    away.camera_cut = false;
    let before = level.programs();
    level.render(&away);
    assert_eq!(level.programs(), before);
}

// Plausible defect: a stage's slot keeps a job of programs built for a
// setting the game has since changed (SMAA's preset), so `compile` settles
// that job and reports ready, and the first frame drops it and creates the
// new preset's programs on the window thread. The oracle is wgpu's count,
// unchanged across that frame, after compiling at Medium with its jobs run
// and then at High.
#[test]
fn a_preset_changed_between_compiles_is_compiled_again() {
    use crate::settings::{Antialiasing, SmaaQuality};
    let Some(mut level) = Level::new() else {
        return;
    };
    level.settings.antialiasing = Antialiasing::Smaa;
    level.settings.smaa_quality = SmaaQuality::Medium;
    let input = Level::input(0.);
    level.compile_all(&input);
    level.settings.smaa_quality = SmaaQuality::High;
    assert!(level.compile_all(&input) > 0, "High's preset is new");
    let before = level.programs();
    level.render(&input);
    assert_eq!(level.programs(), before);
}

// Plausible defect: a planning pass dispatches FSR, whose context advances
// its frame state where a dispatch is recorded and cannot take it back
// (history register U-2), or draws the next jitter of its sequence, so
// compiling restarts an FSR view's accumulation or shifts every frame
// after it by a jitter phase. The oracle is an ordinary FSR level given
// the same three frames: the same bytes each frame, with the level
// compiled before its first frame and again between the second and third,
// when FSR has a history to lose.
#[test]
fn compiling_leaves_an_fsr_views_accumulation_and_jitter_alone() {
    let level = || {
        let mut level = Level::on(Gpu::Fsr, test_support::cube())?;
        level.settings.antialiasing = crate::settings::Antialiasing::Fsr2;
        level.view.resize(&level.device, SIZE, 1., &level.settings);
        Some(level)
    };
    let (Some(mut compiled), Some(mut ordinary)) = (level(), level()) else {
        return;
    };
    assert!(compiled.view.antialiasing.fsr_running().is_some());
    let inputs = [0., 0.25, 0.5].map(Level::input);
    compiled.compile_all(&inputs[0]);
    for (frame, input) in inputs.iter().enumerate() {
        if frame == 2 {
            assert!(compiled.compile(input).ready());
        }
        assert!(
            compiled.render(input) == ordinary.render(input),
            "frame {frame} differs from an ordinary FSR level's"
        );
    }
}

/// One of the guard's configurations: what it turns on, which exclude
/// what another does.
struct Stages {
    name: &'static str,
    gpu: Gpu,
    settings: Settings,
    /// A procedural sky with aerial perspective, where the frame has one.
    sky: bool,
    /// A second view, which draws a render texture the first shows.
    feed: bool,
    /// A planar reflector on the opaque cube, a game's HDR effect and an
    /// outline of every cube.
    extras: bool,
    /// Compile jobs the configuration must run, by label: what it adds to
    /// the configurations before it. A fixture that stopped reaching a
    /// stage would pass the guard for it vacuously.
    reaches: &'static [&'static str],
}

/// The settings every configuration starts from: each optional pass on, at
/// the level that runs it.
fn every_pass() -> Settings {
    use crate::settings::*;
    Settings {
        preset: RenderPreset::High,
        scene_resolution: SceneResolution::Full,
        antialiasing: Antialiasing::Taa,
        bloom: Bloom::On,
        ambient_occlusion: AmbientOcclusionQuality::High,
        screen_space_indirect: ScreenSpaceIndirect::Low,
        screen_space_reflections: ScreenSpaceReflections::Full,
        reflection_method: ReflectionMethod::Crystal,
        world_space_reflections: WorldSpaceReflections::All,
        atmosphere: true,
        dynamic_gi: DynamicGiQuality::Low,
        heat_distortion: true,
        depth_of_field: DepthOfField::High,
        motion_blur: MotionBlur::Full,
        sharpening: Sharpening::On,
        ..Settings::default()
    }
}

// The frame's every stage the tests can reach, over the choices that
// exclude one another: the antialiasing methods, the screen-space
// reflection methods, the fused and the two-pass opaque forms, the software
// and the hardware rays, one view and two. Each configuration turns on what
// it adds and leaves out what another covers, so each test runs alone in
// seconds. A new stage joins by turning itself on in `every_pass` (or in the
// configuration that can run it), by adding its content to `Level::rich`,
// and by naming its jobs in that configuration's `reaches` (AR-13).

/// `every_pass` without what the low-preset configuration covers, for the
/// configurations that add one choice to it: each guard test stays cheap.
fn fewer_passes() -> Settings {
    use crate::settings::*;
    Settings {
        dynamic_gi: DynamicGiQuality::Off,
        screen_space_indirect: ScreenSpaceIndirect::Off,
        heat_distortion: false,
        ..every_pass()
    }
}

fn taa() -> Stages {
    Stages {
        name: "TAA, Crystal reflections, the fused opaque pass, a planar reflector, \
               a game's HDR effect, an outline",
        gpu: Gpu::Plain,
        settings: fewer_passes(),
        sky: false,
        feed: false,
        extras: true,
        reaches: &[
            "DiligentFX inputs",
            "HDR effect",
            "ScreenSpaceReflection::ComputeIntersection",
            "TemporalAntiAliasing::ComputeTemporalAccumulation",
            "additive glow",
            "ambient occlusion",
            "auto exposure",
            "backdrop",
            "blended scene geometry",
            "bloom",
            "cull",
            "deform",
            "depth of field",
            "fused scene geometry and lighting",
            "instance footprints",
            "local exposure",
            "local shadow clear",
            "local shadow layer copy",
            "material mip chains",
            "mist",
            "motion blur",
            "motion tiles",
            "outline",
            "planar reflection",
            "planar reflection method",
            "presentation",
            "reflection source completion",
            "shadow caster",
            "specular probe prefilter",
            "sprites",
            "specular probe culling",
            "temporal composition of moving surfaces",
            "transmission",
            "volumetric fog",
            "world-space reflection rays",
            "world-space reflections",
        ],
    }
}

fn smaa() -> Stages {
    use crate::settings::*;
    Stages {
        name: "SMAA, Velvet reflections, two opaque passes with occlusion culling, \
               contact shadows, a scene below the output's size",
        gpu: Gpu::Plain,
        settings: Settings {
            antialiasing: Antialiasing::Smaa,
            reflection_method: ReflectionMethod::Velvet,
            screen_space_reflections: ScreenSpaceReflections::Half,
            world_space_reflections: WorldSpaceReflections::Off,
            contact_shadows: ContactShadows::Medium,
            occlusion_culling: true,
            scene_resolution: SceneResolution::Half,
            upscaling: Upscaling::On,
            motion_blur: MotionBlur::Off,
            depth_of_field: DepthOfField::Off,
            ..fewer_passes()
        },
        sky: false,
        feed: false,
        extras: false,
        reaches: &[
            "FSR1 EASU",
            "SMAA",
            "SMAA blend",
            "Velvet reflections",
            "contact shadows",
            "depth pyramid",
            "late cull",
            "lit scene geometry",
            "scene lighting",
        ],
    }
}

fn low() -> Stages {
    use crate::settings::*;
    Stages {
        name: "no antialiasing, the low preset, a sky, two views",
        gpu: Gpu::Plain,
        settings: Settings {
            preset: RenderPreset::Low,
            antialiasing: Antialiasing::Off,
            screen_space_reflections: ScreenSpaceReflections::Off,
            world_space_reflections: WorldSpaceReflections::Off,
            motion_blur: MotionBlur::Off,
            depth_of_field: DepthOfField::Off,
            ..every_pass()
        },
        sky: true,
        feed: true,
        extras: false,
        reaches: &[
            "aerial perspective",
            "dynamic GI",
            "dynamic GI rays",
            "heat distortion",
            "screen-space indirect light",
            "sky",
        ],
    }
}

fn fsr(antialiasing: crate::settings::Antialiasing) -> Stages {
    Stages {
        name: "FSR",
        gpu: Gpu::Fsr,
        settings: Settings {
            antialiasing,
            fsr_quality: crate::settings::FsrQuality::Quality,
            motion_blur: crate::settings::MotionBlur::Off,
            depth_of_field: crate::settings::DepthOfField::Off,
            ..fewer_passes()
        },
        sky: false,
        feed: false,
        extras: false,
        reaches: &[],
    }
}

fn hardware_rays() -> Stages {
    Stages {
        name: "hardware ray tracing: shadows, reflections and the GI volume",
        gpu: Gpu::RayTracing,
        settings: Settings {
            hardware_ray_tracing: true,
            ray_traced_shadows: true,
            dynamic_gi: crate::settings::DynamicGiQuality::Low,
            antialiasing: crate::settings::Antialiasing::Off,
            reflection_method: crate::settings::ReflectionMethod::Velvet,
            motion_blur: crate::settings::MotionBlur::Off,
            depth_of_field: crate::settings::DepthOfField::Off,
            bloom: crate::settings::Bloom::Off,
            ..fewer_passes()
        },
        sky: false,
        feed: false,
        extras: false,
        reaches: &[
            "ray-traced shadow denoiser",
            "ray-traced shadow rays",
            "ray-traced shadow temporal",
            "ray-traced shadow upsample",
        ],
    }
}

impl Level {
    /// The level with every kind of content a stage draws or reads: a
    /// transmissive surface, a material with a game's shader and a decoded
    /// base map, a deforming instance, a decal, glow, heat and mist, a
    /// masked and a blended sprite batch, a baked specular probe, a live
    /// one and a dynamic GI volume.
    fn rich(stages: &Stages) -> Option<Self> {
        let mut level = Self::on(stages.gpu, test_support::cube())?;
        let (device, queue) = (&level.device, &level.queue);
        let scene = &mut level.scene;
        let place = |scene: &mut Scene, instance, at: Vec3| {
            let state = crate::InstanceState {
                pose: Mat4::from_translation(at),
                ..*scene.instance(instance).unwrap()
            };
            scene.set_instance(queue, instance, state).unwrap();
        };
        let mut glass = test_support::cube();
        glass.materials[0].transmission = 1.;
        glass.materials[0].thickness = 0.5;
        let (_, glass) = test_support::add_static(device, queue, scene, glass);
        place(scene, glass, Vec3::new(0.75, 1.5, 0.));
        // A decoded image, whose mip chain the GPU builds.
        let mut mapped = test_support::cube();
        mapped.images = vec![crate::asset::Image::Rgba8(image::RgbaImage::from_pixel(
            4,
            4,
            image::Rgba([200, 100, 50, 255]),
        ))];
        mapped.materials[0].base_texture = Some(crate::asset::MaterialTexture::new(0));
        let (shaded, instance) = test_support::add_static(device, queue, scene, mapped);
        place(scene, instance, Vec3::new(0.75, -1.5, 0.));
        let shader = test_support::add_test_shader(scene);
        test_support::set_shader(scene, queue, shaded.materials[0], (shader, 0.5));
        let mut deforming = test_support::cube();
        for mesh in &mut deforming.meshes {
            mesh.deformation.influences = vec![
                crate::deformation::Influence {
                    joints: [0; 4],
                    weights: [1., 0., 0., 0.],
                };
                mesh.vertices.len()
            ];
        }
        let deforming = scene.add_asset(device, queue, deforming).unwrap();
        let deformed = scene
            .add_instance(
                device,
                queue,
                crate::InstanceState {
                    pose: Mat4::from_translation(Vec3::new(3., 0., 0.)),
                    ..crate::InstanceState::new(deforming.model)
                },
                crate::Mobility::Moving,
            )
            .unwrap();
        scene
            .set_instance_deformation(queue, deformed, &[Mat4::IDENTITY], &[])
            .unwrap();
        let image = scene
            .add_decal_image(crate::asset::Image::Rgba8(test_support::half_cut_out()))
            .unwrap();
        scene
            .add_decal(
                device,
                queue,
                crate::Decal {
                    size: Vec3::splat(2.),
                    ..crate::Decal::new(image)
                },
            )
            .unwrap();
        let glow =
            [[0., 2., 1.], [1., 2., 1.], [0.5, 3., 1.]].map(|position| crate::effects::Glow {
                position,
                color: [1., 0.5, 0.2, 1.],
                kind: crate::effects::GlowKind::Uniform,
                soft_distance: 0.,
            });
        scene.update_effects(device, queue, &glow).unwrap();
        let heat = [[0., 0., 2.], [1., 0., 2.], [0.5, 1., 2.]].map(|position| {
            crate::heat_distortion::HeatDistortion {
                position,
                displacement: [4., 0.],
                weight: 1.,
            }
        });
        scene.update_heat_distortion(queue, &heat).unwrap();
        scene
            .update_mist(device, &[[0., -0.5, 0.], [1., -0.5, 1.]])
            .unwrap();
        let sprite_image = scene
            .add_sprite_image(
                device,
                queue,
                &crate::asset::Image::Rgba8(image::RgbaImage::from_pixel(
                    2,
                    2,
                    image::Rgba([255; 4]),
                )),
            )
            .unwrap();
        for (blend, x) in [
            (crate::sprite::SpriteBlend::Masked { cutoff: 0.5 }, 0.),
            (crate::sprite::SpriteBlend::Blended, 1.5),
        ] {
            let batch = scene
                .add_sprite_batch(&crate::sprite::SpriteBatch {
                    blend,
                    ..crate::sprite::SpriteBatch::new(sprite_image)
                })
                .unwrap();
            let sprite = crate::sprite::Sprite::new(Vec3::new(x, 1.2, 0.), glam::Vec2::splat(0.5));
            scene.set_sprites(device, queue, batch, &[sprite]).unwrap();
        }
        let face_size = 64_usize;
        let texels: usize = (0..7)
            .map(|level| (face_size >> level) * (face_size >> level) * 6)
            .sum();
        scene
            .set_baked_specular_probes(
                device,
                queue,
                &[crate::BakedSpecularProbe {
                    center: Vec3::ZERO,
                    world_to_local: Mat4::IDENTITY,
                    influence: crate::SpecularProbeBox {
                        min: Vec3::splat(-4.),
                        max: Vec3::splat(4.),
                    },
                    blend: Vec3::splat(0.5),
                    proxy: None,
                    radiance: crate::SpecularProbeRadiance {
                        face_size: face_size as u32,
                        texels: crate::SpecularProbeTexels::Rgba16Float(vec![0; texels * 4]),
                    },
                }],
            )
            .unwrap();
        scene
            .set_dynamic_gi_volume(
                device,
                Some(crate::DynamicGiVolume {
                    origin: Vec3::splat(-3.),
                    spacing: Vec3::splat(2.),
                    probes: [4, 4, 4],
                }),
            )
            .unwrap();
        // A live probe among the cubes, which the guard refreshes after its
        // frames.
        level.add_probe(Vec3::new(0.75, 0.5, 2.), 0);
        level.settings = stages.settings;
        level.view.resize(&level.device, SIZE, 1., &level.settings);
        if stages.extras {
            let reflector = level
                .scene
                .add_planar_reflector(crate::PlanarReflector::new(Vec3::new(0., 0., 0.5), Vec3::Z))
                .unwrap();
            let values = level.scene.material(level.opaque).unwrap();
            level
                .scene
                .set_material(
                    &level.queue,
                    level.opaque,
                    crate::SurfaceMaterial {
                        planar_reflector: Some(reflector),
                        ..values
                    },
                )
                .unwrap();
            level.effect = Some(test_support::test_effect(&mut level.scene, [0.1; 3]));
            let selection = level.scene.add_selection(&level.cubes).unwrap();
            level.outline = Some(crate::Outline::new(selection));
        }
        if stages.feed {
            level.add_feed();
        }
        Some(level)
    }

    /// The rich level's frame `along` the camera's track: fog, mist, local
    /// exposure and automatic exposure beside the sun, under a procedural
    /// sky where `sky`, applying the level's HDR effect where it has one.
    fn rich_input(&self, along: f32, sky: bool) -> FrameInput {
        let mut input = Self::input(along);
        input.hdr_effect = self.effect;
        input.outline = self.outline;
        input.frame_time_ms = 16.;
        input.elapsed_seconds = f64::from(along);
        input.atmosphere = true;
        input.fog = crate::Fog {
            density: 0.05,
            length: 30.,
            ..Default::default()
        };
        input.mist = crate::Mist {
            opacity: 0.5,
            height: 1.,
            ..Default::default()
        };
        input.local_exposure = crate::LocalExposure {
            highlight_contrast: 0.8,
            shadow_contrast: 0.9,
        };
        input.exposure.automatic = Some(crate::AutoExposure::default());
        input.depth_of_field = crate::DepthOfFieldParameters {
            focus_distance: 4.,
            f_number: 1.,
            sensor_height: 0.2,
        };
        if sky {
            input.environment = crate::FrameEnvironment::Sky(crate::Sky::default());
        }
        input
    }

    /// wgpu's own count of the device's shader modules and pipelines,
    /// which must not change across `step`: no thread but this one runs,
    /// so a change is a program this thread created outside a job.
    fn creates_nothing<T>(&mut self, what: &str, step: impl FnOnce(&mut Self) -> T) -> T {
        crate::view::compile::in_place::take();
        let before = self.programs();
        let result = step(self);
        let after = self.programs();
        assert_eq!(
            after,
            before,
            "{what} created programs (pipelines, shader modules) on its thread; \
             slots created in place: {:?}",
            crate::view::compile::in_place::take()
        );
        result
    }

    /// Compiles the frame `input` until nothing is pending, each job on a
    /// thread of its own, with wgpu's count checked unchanged across every
    /// `compile` call (`creates_nothing`). Returns the labels of the jobs
    /// run, sorted, once each.
    fn compile_in_jobs(&mut self, name: &str, input: &FrameInput) -> Vec<String> {
        let mut ran = Vec::new();
        for pass in 0.. {
            assert!(pass < 12, "{name}: the frame was never ready");
            let compilation = self.creates_nothing(&format!("{name}: compile {pass}"), |level| {
                level.compile(input)
            });
            assert_eq!(compilation.errors, [], "{name}");
            if compilation.ready() {
                break;
            }
            ran.extend(compilation.jobs.iter().map(|job| job.label().to_owned()));
            let workers: Vec<_> = compilation
                .jobs
                .into_iter()
                .map(|job| std::thread::spawn(move || job.run()))
                .collect();
            workers
                .into_iter()
                .for_each(|worker| worker.join().unwrap());
        }
        ran.sort();
        ran.dedup();
        ran
    }

    /// Adds a live specular probe at `center` that hides the materials of
    /// `hidden` and whose influence holds the level.
    fn add_probe(&mut self, center: Vec3, hidden: u32) {
        let probe = self
            .scene
            .add_live_specular_probe(
                &self.device,
                &self.queue,
                crate::LiveSpecularProbe {
                    center,
                    world_to_local: Mat4::from_translation(-center),
                    influence: crate::SpecularProbeBox {
                        min: Vec3::splat(-4.),
                        max: Vec3::splat(4.),
                    },
                    blend: Vec3::splat(0.5),
                    proxy: None,
                    face_size: 64,
                    hidden_groups: hidden,
                },
            )
            .unwrap();
        self.probes.push(probe);
    }

    /// Refreshes live probe `probe` whole through the level's view, with
    /// the frame `input`.
    fn refresh(&mut self, probe: crate::LiveSpecularProbeId, input: &FrameInput) {
        let refreshed = self
            .renderer
            .refresh_specular_probe(
                &self.device,
                &self.queue,
                &mut self.scene,
                &mut self.view,
                input,
                &self.settings,
                None,
                probe,
                crate::ProbeRefreshPace::Whole,
            )
            .unwrap();
        assert_eq!(refreshed, crate::ProbeRefresh::Published);
    }

    /// Every level of the scene's probe cube array, read back: the live
    /// probes' cubes as consumers sample them.
    fn cubes(&self) -> Vec<u8> {
        let texture = self.scene.live_probe_cube(self.probes[0]).unwrap().texture;
        let mut bytes = Vec::new();
        for level in 0..texture.mip_level_count() {
            let size = texture
                .size()
                .mip_level_size(level, wgpu::TextureDimension::D2);
            // Rgba16Float.
            let row = (size.width * 8).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
            let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("live probe cubes"),
                size: u64::from(row * size.height * size.depth_or_array_layers),
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let mut encoder = self.device.create_command_encoder(&Default::default());
            encoder.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo {
                    mip_level: level,
                    ..texture.as_image_copy()
                },
                wgpu::TexelCopyBufferInfo {
                    buffer: &buffer,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(row),
                        rows_per_image: Some(size.height),
                    },
                },
                size,
            );
            self.queue.submit([encoder.finish()]);
            buffer.map_async(wgpu::MapMode::Read, .., |result| result.unwrap());
            self.device
                .poll(wgpu::PollType::wait_indefinitely())
                .unwrap();
            let mapped = buffer.get_mapped_range(..).unwrap();
            for line in mapped.chunks(row as usize) {
                bytes.extend_from_slice(&line[..size.width as usize * 8]);
            }
        }
        bytes
    }
}

/// The guard, for one configuration.
///
/// Plausible defect: a stage creates a shader module or a pipeline outside
/// a compile job (with the device where it encodes, or in a slot a
/// planning pass does not reach), so `compile` or the frames after it
/// stall the thread that calls them, which is what `compile` exists to
/// prevent (AR-13). The oracle is wgpu's own registry
/// (`wgpu::Instance::generate_report`): the device's count of shader
/// modules and pipelines is read before and after every `compile` call and
/// each of the first three frames, with every job joined in between, and
/// must not change; it changes only while jobs run. It counts what the
/// device holds, so a step that freed as many programs as it created
/// would pass; the fixture's settings hold still, and no frame frees one.
/// The second and third frames catch a stage that waits for the view's
/// history before it asks. The level's live probe is refreshed after them,
/// and its first refresh must not change the count either: `compile`
/// plans it (its faces' and cascades' variants, under each
/// configuration's content and settings, and the prefilter's programs).
///
/// The one exception is FSR's: its upscaler (sp-fidelity) is not SGL's to
/// make jobs of, and its backend creates a few helper pipelines where it
/// first dispatches, which is the view's first frame (no planning pass
/// dispatches it). That frame alone may create pipelines for an FSR view.
fn compiles_in_jobs(stages: &Stages) {
    let Some(mut level) = Level::rich(stages) else {
        return;
    };
    let name = stages.name;
    let inputs = [0., 0.25, 0.5].map(|along| level.rich_input(along, stages.sky));
    let ran = level.compile_in_jobs(name, &inputs[0]);
    for label in stages.reaches {
        assert!(
            ran.iter().any(|ran| ran == label),
            "{name}: the fixture no longer runs {label:?}; its jobs were {ran:?}"
        );
    }
    for (frame, input) in inputs.iter().enumerate() {
        if frame == 0 && matches!(stages.gpu, Gpu::Fsr) {
            let helpers = fsr_helpers(stages.settings.antialiasing);
            let before = level.programs();
            level.render(input);
            let after = level.programs();
            assert_eq!(
                (after.0 - before.0, after.1 - before.1),
                (helpers, 0),
                "{name}: the first frame created programs beside FSR's own helpers"
            );
            continue;
        }
        level.creates_nothing(
            &format!("{name}: frame {frame} (jobs ran: {ran:?})"),
            |level| level.render(input),
        );
    }
    // The live probe's first refresh, after the frames whose shadows and
    // sky it takes.
    for probe in level.probes.clone() {
        level.creates_nothing(&format!("{name}: the probe's first refresh"), |level| {
            level.refresh(probe, &inputs[2]);
        });
    }
}

/// How many pipelines FSR's upscaler (`sp-fidelity`) creates at its first
/// dispatch with `antialiasing`, read from wgpu across the first frame of
/// the plain level with FSR, compiled ahead: its other programs are jobs,
/// so the frame creates only those. Positive, or the exception is stale.
fn fsr_helpers(antialiasing: crate::settings::Antialiasing) -> usize {
    let mut level = Level::on(Gpu::Fsr, test_support::cube()).expect("the adapter ran FSR");
    level.settings.antialiasing = antialiasing;
    level.view.resize(&level.device, SIZE, 1., &level.settings);
    let input = Level::input(0.);
    level.compile_all(&input);
    let before = level.programs();
    level.render(&input);
    let after = level.programs();
    assert!(
        after.0 > before.0 && after.1 == before.1,
        "FSR's first dispatch creates its helper pipelines and no shader module \
         SGL holds: {before:?} to {after:?}"
    );
    after.0 - before.0
}

#[test]
fn every_stage_compiles_in_jobs_with_taa() {
    compiles_in_jobs(&taa());
}

#[test]
fn every_stage_compiles_in_jobs_with_smaa() {
    compiles_in_jobs(&smaa());
}

#[test]
fn every_stage_compiles_in_jobs_at_the_low_preset_with_two_views() {
    compiles_in_jobs(&low());
}

#[test]
fn every_stage_compiles_in_jobs_with_fsr2() {
    compiles_in_jobs(&fsr(crate::settings::Antialiasing::Fsr2));
}

#[test]
fn every_stage_compiles_in_jobs_with_fsr3() {
    compiles_in_jobs(&fsr(crate::settings::Antialiasing::Fsr3));
}

#[test]
fn every_stage_compiles_in_jobs_with_hardware_rays() {
    compiles_in_jobs(&hardware_rays());
}

// Plausible defect: a stage's job runs outside the compiler's error scope
// or loses its label, so a program the device refuses is reported nowhere,
// or without the stage a game would look at. The oracle is wgpu's own
// validation, on a device whose limits refuse every compute stage's
// shader: the errors `compile` reports after the jobs ran name the cull
// stage, whose lists the level's opaque surfaces draw from, with wgpu's
// message.
#[test]
fn a_stage_job_the_device_refuses_is_reported_by_its_stage() {
    let Some(mut level) = Level::on(Gpu::NoWorkgroups, test_support::cube()) else {
        return;
    };
    let input = Level::input(0.);
    let compilation = level.compile(&input);
    assert_eq!(compilation.errors, []);
    compilation.jobs.into_iter().for_each(|job| job.run());
    let errors = level.compile(&input).errors;
    let cull = errors
        .iter()
        .find(|error| error.label == "cull")
        .unwrap_or_else(|| panic!("no error names the cull stage: {errors:?}"));
    assert!(
        cull.message.contains("invocations"),
        "wgpu's message: {}",
        cull.message
    );
}

// Plausible defect: a constructor creates a shader module or a pipeline, so
// creating the renderer, a scene or a view stalls the thread that does,
// where the guard above cannot see it (it reads the counts afterwards).
// The oracle is wgpu's count, zero on a device that has only been given a
// level's content, its renderer and views of every antialiasing but FSR.
// FSR is the exception, and this is where it creates its upscaler's
// pipelines: the `View::new` or `View::resize` that creates its context.
// The count shows it, which also shows the count sees a constructor's
// pipelines.
#[test]
fn only_an_fsr_views_upscaler_creates_programs_before_a_frame() {
    let Some(level) = Level::rich(&Stages {
        gpu: Gpu::Fsr,
        ..taa()
    }) else {
        return;
    };
    let view = |settings: &Settings| {
        View::new(
            &level.renderer,
            &level.device,
            &level.queue,
            FORMAT,
            SIZE,
            1.,
            settings,
        )
        .unwrap()
    };
    // Held, so that nothing they created is freed before it is counted.
    let (mut other, _low) = (view(&smaa().settings), view(&low().settings));
    assert_eq!(level.programs(), (0, 0));
    let fsr = fsr(crate::settings::Antialiasing::Fsr2).settings;
    other.resize(&level.device, SIZE, 1., &fsr);
    let resized = level.programs();
    assert!(
        resized.0 > 0,
        "FSR's context creates its pipelines where the view takes it"
    );
    let _created = view(&fsr);
    assert!(level.programs().0 > resized.0);
}

/// Plausible defect: a planning pass changes what the frames draw (history
/// advanced, a target written, a stage left in another state, a selector's
/// value taken before its programs exist), or asks for programs no frame
/// or refresh uses, so compiling costs more than drawing. The oracle is an
/// ordinary level given the same three frames and then its live probe's
/// first refresh: the same bytes each frame, the same refreshed cube, and
/// the same programs held after it.
fn draws_what_an_ordinary_level_does(stages: Stages) {
    let (Some(mut compiled), Some(mut ordinary)) = (Level::rich(&stages), Level::rich(&stages))
    else {
        return;
    };
    let name = stages.name;
    let inputs = [0., 0.25, 0.5].map(|along| compiled.rich_input(along, stages.sky));
    let ordinary_inputs = [0., 0.25, 0.5].map(|along| ordinary.rich_input(along, stages.sky));
    compiled.compile_all(&inputs[0]);
    for (frame, (input, ordinary_input)) in inputs.iter().zip(&ordinary_inputs).enumerate() {
        let (drawn, expected) = (compiled.render(input), ordinary.render(ordinary_input));
        let differing = drawn.iter().zip(&expected).filter(|(a, b)| a != b).count();
        assert_eq!(
            differing, 0,
            "{name}: bytes of frame {frame} that differ from an ordinary level's"
        );
    }
    // The live probe's first refresh, which `compile` planned.
    for (probe, ordinary_probe) in compiled
        .probes
        .clone()
        .into_iter()
        .zip(ordinary.probes.clone())
    {
        compiled.refresh(probe, &inputs[2]);
        ordinary.refresh(ordinary_probe, &ordinary_inputs[2]);
    }
    assert!(
        compiled.cubes() == ordinary.cubes(),
        "{name}: the refreshed probe differs from an ordinary level's"
    );
    assert_eq!(
        compiled.programs(),
        ordinary.programs(),
        "{name}: the programs compiled ahead are not the ones the frames and the refresh use"
    );
}

#[test]
#[ignore = "an ordinary level compiles every stage's programs one after another: 15 s"]
fn long_a_compiled_level_draws_what_an_ordinary_one_does_with_taa() {
    draws_what_an_ordinary_level_does(taa());
}

#[test]
#[ignore = "an ordinary level compiles every stage's programs one after another: 15 s"]
fn long_a_compiled_level_draws_what_an_ordinary_one_does_with_smaa() {
    draws_what_an_ordinary_level_does(smaa());
}

#[test]
#[ignore = "an ordinary level compiles every stage's programs one after another: 15 s"]
fn long_a_compiled_level_draws_what_an_ordinary_one_does_with_fsr2() {
    draws_what_an_ordinary_level_does(fsr(crate::settings::Antialiasing::Fsr2));
}

#[test]
#[ignore = "an ordinary level compiles every stage's programs one after another: 15 s"]
fn long_a_compiled_level_draws_what_an_ordinary_one_does_at_the_low_preset() {
    draws_what_an_ordinary_level_does(low());
}

#[test]
#[ignore = "an ordinary level compiles every stage's programs one after another: 20 s"]
fn long_a_compiled_level_draws_what_an_ordinary_one_does_with_hardware_rays() {
    draws_what_an_ordinary_level_does(hardware_rays());
}

/// The visibility group of `Level::with_probes`'s shaded cube, which its
/// first probe hides.
const HIDDEN: u32 = 2;

impl Level {
    /// The level with two live probes beside its cubes, and a cube whose
    /// material has a game's shader in a group of its own (`HIDDEN`) above
    /// them, which the first probe hides and the second shows: only the
    /// second's faces draw that material's variant. Under a uniform
    /// environment of radiance 1, which receivers take until a probe is
    /// published.
    fn with_probes() -> Option<Self> {
        let mut level = Self::new()?;
        let environment = level
            .scene
            .add_environment(
                &level.device,
                &level.queue,
                &test_support::environment([255; 4], &0x3c00u16.to_le_bytes()),
            )
            .unwrap();
        level.environment = crate::FrameEnvironment::Map(environment);
        let mut shown = test_support::cube();
        shown.materials[0].visibility_group = HIDDEN;
        let (ids, cube) =
            test_support::add_static(&level.device, &level.queue, &mut level.scene, shown);
        let shader = test_support::add_test_shader(&mut level.scene);
        test_support::set_shader(
            &mut level.scene,
            &level.queue,
            ids.materials[0],
            (shader, 0.5),
        );
        let state = crate::InstanceState {
            pose: Mat4::from_translation(Vec3::new(0., 2.5, 0.)),
            ..*level.scene.instance(cube).unwrap()
        };
        level.scene.set_instance(&level.queue, cube, state).unwrap();
        level.add_probe(Vec3::new(1.5, 1., 2.), HIDDEN);
        level.add_probe(Vec3::new(-1.5, 1., 2.), 0);
        Some(level)
    }

    /// The frame `along` the camera's track under the level's environment,
    /// with `HIDDEN` shown.
    fn probe_input(&self, along: f32) -> FrameInput {
        FrameInput {
            visibility_mask: HIDDEN,
            environment: self.environment,
            ..Self::input(along)
        }
    }
}

// Plausible defects: `compile` plans no probe capture, or only the first
// live probe's, or the prefilter creates its programs outside its slot, so
// a live probe's first refresh compiles on the thread that calls it after
// the level was compiled ahead. The second probe shows a cube with a game's
// shader that the first hides, whose variant only its faces draw. The oracle is wgpu's
// own count of the device's shader modules and pipelines: unchanged across
// every `compile` call, the first frame and each probe's first refresh,
// with the jobs joined between; raised by an ordinary level's first
// refreshes after its first frame, which shows the count sees what a
// refresh creates; and equal to that ordinary level's after them, so the
// planning asks for nothing the frame and the refreshes do not create.
#[test]
fn a_compiled_levels_first_probe_refreshes_create_no_program() {
    let (Some(mut compiled), Some(mut ordinary)) = (Level::with_probes(), Level::with_probes())
    else {
        return;
    };
    let input = compiled.probe_input(0.);
    let ran = compiled.compile_in_jobs("two live probes", &input);
    assert!(
        ran.iter().any(|label| label == "specular probe prefilter"),
        "the prefilter's programs are a job: {ran:?}"
    );
    compiled.creates_nothing("the first frame", |level| level.render(&input));
    for (index, probe) in compiled.probes.clone().into_iter().enumerate() {
        compiled.creates_nothing(&format!("probe {index}'s first refresh"), |level| {
            level.refresh(probe, &input);
        });
    }
    ordinary.render(&input);
    let before = ordinary.programs();
    for probe in ordinary.probes.clone() {
        ordinary.refresh(probe, &input);
    }
    let after = ordinary.programs();
    assert!(
        after.0 > before.0,
        "an ordinary level's first refreshes create pipelines: {before:?} to {after:?}"
    );
    assert_eq!(
        compiled.programs(),
        after,
        "the programs compiled ahead are not the ones the frame and the refreshes use"
    );
}

// Plausible defect: a planning capture that leaves the scene changed for
// the frames and refreshes after it, such as one that publishes a probe
// (its record then places a cube nothing filled, which receivers take in
// place of the environment). The oracle is an ordinary level given the
// same frames and refreshes: the same bytes in the frame before the
// refreshes and the frame after, and the same cubes, which are not zero.
#[test]
fn a_compiled_level_refreshes_its_probes_as_an_ordinary_one_does() {
    let (Some(mut compiled), Some(mut ordinary)) = (Level::with_probes(), Level::with_probes())
    else {
        return;
    };
    compiled.compile_all(&compiled.probe_input(0.));
    let run = |level: &mut Level| {
        let input = level.probe_input(0.);
        let before = level.render(&input);
        for probe in level.probes.clone() {
            level.refresh(probe, &input);
        }
        let cubes = level.cubes();
        (before, cubes, level.render(&level.probe_input(0.25)))
    };
    let (before, cubes, after) = run(&mut compiled);
    let expected = run(&mut ordinary);
    assert!(
        cubes.iter().any(|&byte| byte != 0),
        "the refreshes show the level"
    );
    assert!(
        before == expected.0,
        "the frame before the refreshes differs"
    );
    assert!(cubes == expected.1, "the refreshed cubes differ");
    assert!(after == expected.2, "the frame after the refreshes differs");
}

// Plausible defect: `compile` asks for a probe capture's programs (the
// prefilter's, a capture's variants) or makes its staging for a scene
// without a live probe, so a level without one pays for them. The oracle
// is an ordinary level given the same three frames: after them the
// compiled level holds what it holds, textures, buffers, bind groups and
// programs alike, and no job was the prefilter's.
#[test]
fn a_compiled_level_without_a_live_probe_compiles_nothing_for_one() {
    let (Some(mut compiled), Some(mut ordinary)) = (Level::new(), Level::new()) else {
        return;
    };
    let ran = compiled.compile_in_jobs("no live probe", &Level::input(0.));
    assert!(
        !ran.iter().any(|label| label == "specular probe prefilter"),
        "{ran:?}"
    );
    for along in [0., 0.25, 0.5] {
        compiled.render(&Level::input(along));
        ordinary.render(&Level::input(along));
    }
    assert_eq!(compiled.held(), ordinary.held());
}

// Plausible defects: the mip chain stage creating its programs in place
// in a planning pass, on the thread that calls `compile`, rather than in a
// job, or where it builds a chain in a frame, so a level's first frame
// compiles them on the window thread; or a planning pass taking the
// textures that wait without building their chains, so the frame draws
// from unwritten levels. The opaque cube's base map is a decoded image of
// one colour, whose chain the GPU builds. The oracles: the chain's last
// level read back after the first planning pass, whose jobs have not run,
// is unwritten (wgpu reads an unwritten level as zero), which a pass that
// created the stage's programs in place would have built; wgpu's count of
// the pipelines and shader modules, unchanged across the compiled first
// frame; and that level read back after it, the image's colour within the
// device's rounding (the mean of one colour is that colour).
#[test]
fn a_compiled_levels_mip_chains_are_jobs_and_built_before_its_first_frame() {
    use crate::asset::{Image, MaterialTexture};
    const COLOUR: [u8; 4] = [200, 100, 50, 255];
    let mut cube = test_support::cube();
    cube.images = vec![Image::Rgba8(image::RgbaImage::from_pixel(
        4,
        4,
        image::Rgba(COLOUR),
    ))];
    cube.materials[0].base_texture = Some(MaterialTexture::new(0));
    let Some(mut level) = Level::with_opaque(cube) else {
        return;
    };
    let texture = level.scene.materials.texture(0);
    let texture = texture.color.as_ref().unwrap().texture().clone();
    assert_eq!(texture.mip_level_count(), 3);
    let last = |level: &Level| test_support::read_mip(&level.device, &level.queue, &texture, 2);
    let input = Level::input(0.);
    let first = level.compile(&input);
    assert!(!first.ready());
    assert_eq!(last(&level), [0; 4], "the chain waits for its job");
    first.jobs.into_iter().for_each(|job| job.run());
    level.compile_all(&input);
    let before = level.programs();
    level.render(&input);
    assert_eq!(level.programs(), before);
    let built = last(&level);
    for (code, expected) in built.iter().zip(COLOUR) {
        assert!(code.abs_diff(expected) <= 1, "{built:?} for {COLOUR:?}");
    }
}

// Plausible defect: sprite batches, whose pipelines are one program set
// of the transparent stage, left out of the planning pass (their passes
// skipped before asking the slot, or the slot asked only in a frame), so a
// level with hit markers, foliage cards or particles compiles on the
// window thread at its first frame. The oracle is wgpu's count of the
// pipelines and shader modules, unchanged across the compiled first frame
// of a level with a masked and a blended batch in view, under TAA, which
// draws both into the reflections' input and the composite with the
// temporal masks.
#[test]
fn a_compiled_level_with_sprites_creates_no_program_at_its_first_frame() {
    use crate::sprite::{Sprite, SpriteBatch, SpriteBlend};
    let Some(mut level) = Level::new() else {
        return;
    };
    let image = level
        .scene
        .add_sprite_image(
            &level.device,
            &level.queue,
            &crate::asset::Image::Rgba8(image::RgbaImage::from_pixel(2, 2, image::Rgba([255; 4]))),
        )
        .unwrap();
    for (blend, x) in [
        (SpriteBlend::Masked { cutoff: 0.5 }, 0.),
        (SpriteBlend::Blended, 1.5),
    ] {
        let batch = level
            .scene
            .add_sprite_batch(&SpriteBatch {
                blend,
                ..SpriteBatch::new(image)
            })
            .unwrap();
        let sprite = Sprite::new(Vec3::new(x, 1.2, 0.), glam::Vec2::splat(0.5));
        level
            .scene
            .set_sprites(&level.device, &level.queue, batch, &[sprite])
            .unwrap();
    }
    let input = Level::input(0.);
    level.compile_all(&input);
    let before = level.programs();
    level.render(&input);
    assert_eq!(level.programs(), before);
}

impl Level {
    /// The level with its blended cube made opaque: no material, effect or
    /// sprite marks a TAA view's temporal masks, so its frames hold them
    /// exactly while instance footprints are pending (the register's
    /// M-23), settled by two frames.
    fn without_mask_writers() -> Option<Self> {
        let mut level = Self::new()?;
        let blended: Vec<_> = level
            .scene
            .materials
            .slots
            .iter()
            .filter(|(_, material)| material.values.blended())
            .map(|(id, material)| (id, material.values))
            .collect();
        for (id, values) in blended {
            let values = crate::SurfaceMaterial {
                alpha: crate::AlphaMode::Opaque,
                ..values
            };
            level.scene.set_material(&level.queue, id, values).unwrap();
        }
        Some(level)
    }

    /// Places another cube in view: a spawn, which leaves a footprint.
    fn spawn(&mut self) {
        let model = self
            .scene
            .add_asset(&self.device, &self.queue, test_support::cube())
            .unwrap()
            .model;
        let state = crate::InstanceState {
            pose: Mat4::from_translation(Vec3::new(0.75, 1., 0.)),
            ..crate::InstanceState::new(model)
        };
        self.scene
            .add_instance(&self.device, &self.queue, state, crate::Mobility::Moving)
            .unwrap();
    }
}

// Plausible defect: where a TAA view's temporal masks follow instance
// footprints alone (no other writer in the scene), a planning pass readies
// only the resolve its frame binds and the footprints' programs only where
// its frame draws one, so the first spawn after loading creates TAA's
// resolve with the masks or the footprints' programs on the window thread.
// The oracle is wgpu's count, as
// `a_compiled_levels_first_frame_creates_no_program`'s: unchanged across a
// compiled level's first frame (which restarts history, so marks no
// footprint and binds no mask), the frames after a spawn, which hold the
// masks for its footprints, and those after they retire.
#[test]
fn a_compiled_level_whose_masks_follow_its_footprints_creates_no_program_as_they_come_and_go() {
    let Some(mut level) = Level::without_mask_writers() else {
        return;
    };
    let first = Level::input(0.);
    let still = FrameInput {
        camera_cut: false,
        ..first
    };
    level.compile_all(&first);
    let before = level.programs();
    level.render(&first);
    level.render(&still);
    level.spawn();
    level.render(&still);
    assert!(
        level.view.targets.temporal_masks().is_some(),
        "the spawn's frame held the masks"
    );
    level.render(&still);
    assert!(level.scene.footprints.is_empty());
    assert_eq!(level.programs(), before);
}

// Plausible defect (#877): a level's first frame readies only the TAA
// resolve it binds, so the first frame after it in which nothing marks the
// masks (the blended cube turned out of view) creates the resolve without
// them on the window thread. The oracle is wgpu's count, as
// `a_compiled_levels_first_frame_creates_no_program`'s: unchanged across a
// compiled level's first frame, which binds the masks for its blended
// cube, and a frame turned away from everything that marks them, which
// binds none.
#[test]
fn a_compiled_level_whose_masks_come_and_go_creates_no_program() {
    let Some(mut level) = Level::new() else {
        return;
    };
    let first = Level::input(0.);
    level.compile_all(&first);
    let before = level.programs();
    level.render(&first);
    assert!(
        level.view.targets.temporal_masks().is_some(),
        "the blended cube marks the first frame"
    );
    let mut away = FrameInput {
        camera_cut: false,
        ..first
    };
    let eye = away.camera.eye;
    away.camera.view = glam::camera::rh::view::look_at_mat4(eye, eye + Vec3::Z, Vec3::Y);
    level.render(&away);
    assert!(
        level.view.targets.temporal_masks().is_none(),
        "nothing marks the frame turned away"
    );
    assert_eq!(level.programs(), before);
}

// Plausible defect (#877): the mask targets follow each frame's answer, so
// content that marks the masks now and then, a spawn every few frames,
// creates and drops two render-size targets each time. The oracle is
// wgpu's count of the textures the device holds against the register's
// F-15: the first frame that binds the masks makes two targets, and frames
// that bind none or bind them again after it make and drop none.
#[test]
fn the_mask_targets_are_made_once_however_often_spawns_come_and_go() {
    let Some(mut level) = Level::without_mask_writers() else {
        return;
    };
    let model = level
        .scene
        .add_asset(&level.device, &level.queue, test_support::cube())
        .unwrap()
        .model;
    let first = Level::input(0.);
    let still = FrameInput {
        camera_cut: false,
        ..first
    };
    level.render(&first);
    level.render(&still);
    let before = level.held().textures;
    for spawn in 0..3 {
        let state = crate::InstanceState {
            pose: Mat4::from_translation(Vec3::new(0.25 + 0.5 * spawn as f32, 1., 0.)),
            ..crate::InstanceState::new(model)
        };
        level
            .scene
            .add_instance(&level.device, &level.queue, state, crate::Mobility::Moving)
            .unwrap();
        for (frame, bound) in [(0, true), (1, false)] {
            level.render(&still);
            assert_eq!(
                level.view.targets.temporal_masks().is_some(),
                bound,
                "spawn {spawn}, frame {frame}: the frame binds the masks"
            );
            assert_eq!(
                level.held().textures,
                before + 2,
                "spawn {spawn}, frame {frame}: the two targets, made once"
            );
        }
    }
}

// Plausible defect: the planning pass reaches a material's pipelines
// without the maps it samples on the second UV set or through a transform
// (`Variant::uv1`, `Variant::transformed`), so the first frame creates
// them on the window thread. The opaque cube's base map is on the second
// set, scaled. The oracle is wgpu's count of the pipelines and shader
// modules, unchanged across the compiled first frame.
#[test]
fn a_compiled_frame_with_a_map_on_the_second_set_and_transformed_creates_no_program() {
    use crate::asset::{Image, MaterialTexture, UvSet, UvTransform};
    let mut cube = test_support::cube();
    cube.images = vec![Image::Rgba8(image::RgbaImage::from_pixel(
        2,
        2,
        image::Rgba([200; 4]),
    ))];
    cube.materials[0].base_texture = Some(MaterialTexture {
        uv_set: UvSet::Uv1,
        transform: UvTransform {
            scale: [2., 2.],
            ..UvTransform::IDENTITY
        },
        ..MaterialTexture::new(0)
    });
    let Some(mut level) = Level::with_opaque(cube) else {
        return;
    };
    let input = Level::input(0.);
    level.compile_all(&input);
    let before = level.programs();
    level.render(&input);
    assert_eq!(level.programs(), before);
}

// A material whose maps take transforms draws with one pipeline variant
// whichever of its maps do (`PipelineKey::new`): a quantised glTF puts a
// transform on every map, so a key per map would give each pattern of
// textured maps pipelines of its own. Plausible defect: the key keeping each
// binding's bit. The oracle is wgpu's count of the pipelines and shader
// modules a compiled level holds, each beside an untransformed cube: a
// level that adds a cube whose base map is transformed holds as many as one
// that adds that cube and one whose emission map alone is transformed, and
// more than one that adds neither, so the count sees the variant.
#[test]
fn materials_whose_maps_take_transforms_differently_share_pipelines() {
    use crate::asset::{Image, MaterialTexture, UvTransform};
    let moved = |image| MaterialTexture {
        transform: UvTransform {
            offset: [0.5, 0.],
            ..UvTransform::IDENTITY
        },
        ..MaterialTexture::new(image)
    };
    let level = |maps: &[(MaterialTexture, MaterialTexture)]| {
        let mut asset = test_support::cube();
        let mesh = asset.meshes[0].clone();
        let material = asset.materials[0].clone();
        asset.images = vec![Image::Rgba8(image::RgbaImage::from_pixel(
            2,
            2,
            image::Rgba([200; 4]),
        ))];
        asset.meshes.clear();
        asset.materials.clear();
        for (index, &(base, emission)) in maps.iter().enumerate() {
            asset.meshes.push(crate::asset::CpuMesh {
                material: index,
                ..mesh.clone()
            });
            asset.materials.push(crate::asset::Material {
                base_texture: Some(base),
                emissive_texture: Some(emission),
                ..material.clone()
            });
        }
        let mut level = Level::with_opaque(asset)?;
        level.compile_all(&Level::input(0.));
        Some(level.programs())
    };
    let plain = MaterialTexture::new(0);
    let Some(one) = level(&[(plain, plain), (moved(0), plain)]) else {
        return;
    };
    let two = level(&[(plain, plain), (moved(0), plain), (plain, moved(0))]).unwrap();
    let none = level(&[(plain, plain)]).unwrap();
    assert_eq!(two, one);
    assert!(one.0 > none.0, "{one:?} programs transformed, {none:?} not");
}

// A material that samples an occlusion map of its own or a specular map
// draws with one pipeline variant whichever of those maps it takes
// (`Variant::occlusion_specular`), which compiles their sampling in, so a
// material without them keeps the pipelines it always drew with.
// Plausible defects: no variant (the sampling compiled into every
// material's pipelines), a variant for each combination of the maps, or
// the planning pass missing the variant, so the first frame creates it.
// The oracle is wgpu's count of the pipelines and shader modules a
// compiled level holds, each beside a plain cube: a level that adds a cube
// with all three maps holds more than one that adds none, so the count sees
// the variant; one that adds that cube, one with an occlusion map alone and
// one with a specular colour map alone holds as many as the first; and its
// compiled first frame creates none.
#[test]
fn occlusion_and_specular_maps_take_one_variant() {
    use crate::asset::{Image, MaterialTexture};
    let level = |maps: &[[bool; 3]]| {
        let mut asset = test_support::cube();
        let mesh = asset.meshes[0].clone();
        let material = asset.materials[0].clone();
        asset.images = vec![Image::Rgba8(image::RgbaImage::from_pixel(
            2,
            2,
            image::Rgba([200, 150, 100, 180]),
        ))];
        asset.meshes.clear();
        asset.materials.clear();
        for (index, &[occlusion, specular, color]) in maps.iter().enumerate() {
            asset.meshes.push(crate::asset::CpuMesh {
                material: index,
                ..mesh.clone()
            });
            let map = |on: bool| on.then(|| MaterialTexture::new(0));
            asset.materials.push(crate::asset::Material {
                base_texture: Some(MaterialTexture::new(0)),
                occlusion_texture: map(occlusion),
                specular_texture: map(specular),
                specular_color_texture: map(color),
                ..material.clone()
            });
        }
        let mut level = Level::with_opaque(asset)?;
        level.compile_all(&Level::input(0.));
        Some(level)
    };
    let plain = [false; 3];
    let Some(none) = level(&[plain]) else {
        return;
    };
    let one = level(&[plain, [true; 3]]).unwrap();
    let mut three = level(&[plain, [true; 3], [true, false, false], [false, false, true]]).unwrap();
    assert!(
        one.programs().0 > none.programs().0,
        "{:?} programs with the maps, {:?} without",
        one.programs(),
        none.programs()
    );
    let compiled = three.programs();
    assert_eq!(compiled, one.programs(), "three combinations, one");
    three.render(&Level::input(0.));
    assert_eq!(three.programs(), compiled, "the compiled first frame");
}

// An occlusion map packed in the metallic-roughness map (ORM) is read from
// that map's sample: its material draws with the pipelines of the same
// material without it. Plausible defect: packed occlusion taken for an
// occlusion map of its own (`MaterialMap::Occlusion`), or counted by
// `Variant::occlusion_specular`, which would sample the image again with
// pipelines of their own. The oracle is wgpu's count of the pipelines and
// shader modules a compiled level holds, beside a plain cube: a level that
// adds a cube with packed occlusion holds as many as one that adds the same
// cube with its metallic-roughness map alone.
#[test]
fn packed_occlusion_takes_no_variant() {
    use crate::asset::{Image, MaterialTexture};
    let level = |packed: bool| {
        let mut asset = test_support::cube();
        let mesh = asset.meshes[0].clone();
        let material = asset.materials[0].clone();
        asset.images = vec![Image::Rgba8(image::RgbaImage::from_pixel(
            2,
            2,
            image::Rgba([120, 200, 60, 255]),
        ))];
        asset.meshes = vec![
            mesh.clone(),
            crate::asset::CpuMesh {
                material: 1,
                ..mesh
            },
        ];
        asset.materials = vec![
            material.clone(),
            crate::asset::Material {
                mr_texture: Some(MaterialTexture::new(0)),
                occlusion_texture: packed.then(|| MaterialTexture::new(0)),
                ..material
            },
        ];
        let mut level = Level::with_opaque(asset)?;
        level.compile_all(&Level::input(0.));
        Some(level.programs())
    };
    let Some(packed) = level(true) else {
        return;
    };
    assert_eq!(packed, level(false).unwrap());
}
