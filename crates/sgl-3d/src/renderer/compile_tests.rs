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
}

impl Level {
    fn new() -> Option<Self> {
        Self::with_opaque(test_support::cube())
    }

    /// The level with `opaque` as its opaque cube.
    fn with_opaque(opaque: crate::asset::Asset) -> Option<Self> {
        let (instance, device, queue) = test_support::counted_device()?;
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
        let description = crate::RenderTexture::new([16, 16]);
        let texture = level
            .scene
            .add_render_texture(&level.device, description)
            .unwrap();
        let material = level.opaque;
        let values = level.scene.material(material).unwrap();
        level
            .scene
            .set_material(
                &level.queue,
                material,
                crate::SurfaceMaterial {
                    emission_render_texture: Some(texture),
                    ..values
                },
            )
            .unwrap();
        let feed = View::new(
            &level.renderer,
            &level.device,
            &level.queue,
            description.format(),
            [16, 16],
            1.,
            &level.settings,
        )
        .unwrap();
        level.feed = Some((feed, texture));
        Some(level)
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
