//! Planar reflections at the frame boundary: what a view holds for a
//! reflector while it shows it and after (the architecture's Frame,
//! "Planar reflections"), how a reflected view's history follows its
//! consumer's frames (the register's F-12) and what the consumer's
//! temporal composition mask holds over a reflector's surface (M-16).
use super::{Renderer, View, ViewFrame, ViewOutput};
use crate::asset::Asset;
use crate::settings::{Antialiasing, Bloom, PlanarReflections, Settings};
use crate::{
    Camera, FrameInput, InstanceState, Mobility, PlanarReflector, Scene, SurfaceMaterial,
    test_support,
};
use glam::{Mat4, Vec3};

const SIZE: [u32; 2] = [64, 64];

/// TAA, whose resolve reads the temporal masks.
fn settings() -> Settings {
    Settings {
        antialiasing: Antialiasing::Taa,
        bloom: Bloom::Off,
        atmosphere: false,
        ..Settings::default()
    }
}

/// A camera at the origin looking down -Z, or back along +Z.
fn input(back: bool) -> FrameInput {
    let view = if back {
        Mat4::from_rotation_y(std::f32::consts::PI)
    } else {
        Mat4::IDENTITY
    };
    FrameInput::new(Camera {
        view,
        projection: crate::perspective(1., 1., 0.1),
        eye: Vec3::ZERO,
    })
}

/// A metal square 0.8 m wide, 3 m before the origin, facing it: 15.6
/// texels about the centre of a 64-texel view from the origin.
fn mirror() -> Asset {
    let mut asset = test_support::cube();
    asset.meshes[0] = crate::asset::CpuMesh {
        vertices: [(-1., -1.), (1., -1.), (1., 1.), (-1., 1.)]
            .map(|(x, y): (f32, f32)| crate::asset::Vertex {
                uv1: [0.0; 2],
                tangent: [1., 0., 0., 1.],
                lightmap_bounds: [0., 0., 1., 1.],
                lightmap_uv: [0.; 2],
                position: [x * 0.4, y * 0.4, -3.],
                normal: [0., 0., 1.],
                uv: [0.; 2],
                color: [1.; 4],
            })
            .to_vec(),
        indices: vec![0, 1, 2, 0, 2, 3],
        material: 0,
        deformation: Default::default(),
    };
    asset.materials[0].base = [1.; 4];
    asset.materials[0].metallic = 1.;
    asset.materials[0].roughness = 0.;
    asset
}

/// A main view at the origin before a mirror; `reflects` names the
/// mirror's plane in its material.
struct Stage {
    device: wgpu::Device,
    queue: wgpu::Queue,
    scene: Scene,
    renderer: Renderer,
    main: View,
    output: wgpu::TextureView,
    /// The antialiasing the main view takes.
    antialiasing: Antialiasing,
    /// The reflector, where the mirror's material names it.
    reflector: Option<crate::PlanarReflectorId>,
    /// The mirror's material.
    material: crate::MaterialId,
    /// The instance that made the device, where its report is read.
    instance: Option<wgpu::Instance>,
}

impl Stage {
    /// None without a device, or where TAA does not run.
    fn new(reflects: bool) -> Option<Stage> {
        let (device, queue) = test_support::device()?;
        Stage::on((device, queue, None), reflects, Antialiasing::Taa)
    }

    /// On a device whose report counts what it holds, without
    /// antialiasing, so a frame's targets are the reflections' alone.
    fn counted() -> Option<Stage> {
        let (instance, device, queue) = test_support::counted_device()?;
        Stage::on((device, queue, Some(instance)), true, Antialiasing::Off)
    }

    fn on(
        (device, queue, instance): (wgpu::Device, wgpu::Queue, Option<wgpu::Instance>),
        reflects: bool,
        antialiasing: Antialiasing,
    ) -> Option<Stage> {
        let mut scene = Scene::new(&device, &queue);
        let ids = scene.add_asset(&device, &queue, mirror()).unwrap();
        scene
            .add_instance(
                &device,
                &queue,
                InstanceState::new(ids.model),
                Mobility::Static,
            )
            .unwrap();
        let mut named = None;
        if reflects {
            let reflector = scene
                .add_planar_reflector(PlanarReflector::new(Vec3::new(0., 0., -3.), Vec3::Z))
                .unwrap();
            let values = scene.material(ids.materials[0]).unwrap();
            scene
                .set_material(
                    &queue,
                    ids.materials[0],
                    SurfaceMaterial {
                        planar_reflector: Some(reflector),
                        ..values
                    },
                )
                .unwrap();
            named = Some(reflector);
        }
        let renderer = Renderer::new(&device);
        let format = crate::shading::gbuffer::COLOR;
        let settings = Settings {
            antialiasing,
            ..settings()
        };
        let main = View::new(&renderer, &device, &queue, format, SIZE, 1., &settings).unwrap();
        if main.antialiasing_in_effect(&settings) != antialiasing {
            eprintln!("skipping: {antialiasing:?} does not run here");
            return None;
        }
        Some(Stage {
            output: crate::view::targets::target(&device, "planar reflection frame", SIZE, format),
            main,
            renderer,
            scene,
            device,
            queue,
            antialiasing,
            reflector: named,
            material: ids.materials[0],
            instance,
        })
    }

    /// What the device holds of the programs a view is made of: render
    /// and compute pipelines, pipeline and bind group layouts, and shader
    /// modules; and of its targets: textures and texture views.
    fn held(&self) -> ([usize; 5], [usize; 2]) {
        let report = self.instance.as_ref().unwrap().generate_report().unwrap();
        let hub = report.hub_report();
        (
            [
                hub.render_pipelines.num_allocated,
                hub.compute_pipelines.num_allocated,
                hub.pipeline_layouts.num_allocated,
                hub.bind_group_layouts.num_allocated,
                hub.shader_modules.num_allocated,
            ],
            [hub.textures.num_allocated, hub.texture_views.num_allocated],
        )
    }

    /// The texels of the largest texture the main view's reflected view
    /// keeps for its picture (its composite and depth) and for its
    /// directional shadows (a cascade's side), where it holds one.
    fn kept_texels(&self) -> Option<(u32, u32)> {
        let reflected = &self.main.reflected.first()?.view;
        let side = |view: &wgpu::TextureView| view.texture().width() * view.texture().height();
        Some((
            side(&reflected.targets.composite).max(side(&reflected.targets.depth)),
            reflected.shadows.directional.array.texture().width(),
        ))
    }

    /// Renders, submits and finishes a frame of the main view looking at
    /// the mirror, or `back`, away from it, at `level`.
    fn frame(&mut self, back: bool, level: PlanarReflections) {
        self.frame_seen(&input(back), level);
    }

    /// Renders, submits and finishes a frame of the main view seeing
    /// `input` at `level`.
    fn frame_seen(&mut self, input: &FrameInput, level: PlanarReflections) {
        let settings = Settings {
            planar_reflections: level,
            antialiasing: self.antialiasing,
            ..settings()
        };
        self.main
            .resize(&self.device, self.main.sizes.output, 1., &settings);
        let mut views = [ViewFrame {
            view: &mut self.main,
            input,
            settings: &settings,
            output: ViewOutput::Texture(&self.output),
            timing: None,
        }];
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

    /// What the main view holds for a planar reflection: its reflected
    /// views, its reflections' planar method with its targets, and the
    /// temporal masks a shown reflector's surface is marked in under TAA.
    fn holds(&self) -> (usize, bool, bool) {
        (
            self.main.reflected.len(),
            self.main.reflections.holds_planar(),
            self.main.targets.temporal_masks.is_some(),
        )
    }
}

// The cost of an absent reflection. Plausible defects: a view makes a
// reflected view, or the planar method, for a reflector it has not shown;
// it keeps one at a level of `Off`, or for a reflector the scene no longer
// holds, or across a resize that changes its picture's size; it reports
// one it only keeps as shown; a scene that holds a reflector keeps a TAA
// view's temporal masks, and so its composition pass, in frames that show
// no picture. The fixture: a view under TAA, turned away from a mirror,
// then before it, away, before it, at `Off`; then shown and kept across a
// resize; then shown, kept while its material stops naming the reflector,
// and the reflector removed. The oracle is the contract: a view makes a
// reflected view and the planar method only for a reflector it shows,
// keeps them unshown while the reflector is away within the idle period,
// and releases them at once at `Off`, for a removed reflector and across a
// resize; the masks follow what it shows; a scene without a reflector
// holds none of them.
#[test]
fn a_view_holds_a_reflected_view_only_while_it_shows_the_reflector() {
    let Some(mut stage) = Stage::new(true) else {
        return;
    };
    let level = PlanarReflections::Half;
    let shown = |stage: &mut Stage| stage.main.planar_reflections(&stage.device).len();
    for (frame, (back, level, held, showing)) in [
        (true, level, (0, false, false), 0),
        (false, level, (1, true, true), 1),
        (true, level, (1, true, false), 0),
        (false, level, (1, true, true), 1),
        (false, PlanarReflections::Off, (0, false, false), 0),
    ]
    .into_iter()
    .enumerate()
    {
        stage.frame(back, level);
        assert_eq!(stage.holds(), held, "frame {frame}");
        assert_eq!(shown(&mut stage), showing, "frame {frame}");
    }
    // Kept, then a resize that changes the picture's size.
    stage.frame(false, level);
    stage.frame(true, level);
    assert_eq!(stage.holds().0, 1, "kept while away");
    stage.main.resize(&stage.device, [48, 48], 1., &settings());
    stage.frame(true, level);
    assert_eq!(stage.holds(), (0, false, false), "released across a resize");
    // Kept, then the reflector removed.
    stage.frame(false, level);
    let values = stage.scene.material(stage.material).unwrap();
    stage
        .scene
        .set_material(
            &stage.queue,
            stage.material,
            SurfaceMaterial {
                planar_reflector: None,
                ..values
            },
        )
        .unwrap();
    stage.frame(false, level);
    assert_eq!(stage.holds().0, 1, "kept while no material names it");
    stage
        .scene
        .remove_planar_reflector(stage.reflector.unwrap())
        .unwrap();
    stage.frame(false, level);
    assert_eq!(
        stage.holds(),
        (0, false, false),
        "released with the reflector"
    );
    let Some(mut plain) = Stage::new(false) else {
        return;
    };
    plain.frame(false, level);
    assert_eq!(plain.holds(), (0, false, false));
}

// F-12, for a reflected view. Plausible defects: a reflected view kept
// while its reflector is away continues, when it returns, a history it
// does not have. The fixture: a view before a mirror for two frames,
// turned away for one, and back for two. The oracle is the contract's: a
// reflected view's first frame restarts, its second continues, it renders
// nothing while kept, and after a frame that did not show the reflector
// its first frame restarts again, while the consumer, rendered throughout,
// continues.
#[test]
fn a_reflected_view_restarts_when_its_reflector_returns() {
    let Some(mut stage) = Stage::new(true) else {
        return;
    };
    let level = PlanarReflections::Half;
    let continued = |view: &View| view.rendered.as_ref().unwrap().history.valid;
    for (frame, (back, reflected)) in [
        (false, Some(false)),
        (false, Some(true)),
        (true, None),
        (false, Some(false)),
        (false, Some(true)),
    ]
    .into_iter()
    .enumerate()
    {
        stage.frame(back, level);
        let held = stage
            .main
            .reflected
            .first()
            .filter(|held| held.idle == 0)
            .map(|held| continued(&held.view));
        assert_eq!(held, reflected, "frame {frame}: the reflected view");
        assert_eq!(
            continued(&stage.main),
            frame > 0,
            "frame {frame}: the consumer"
        );
    }
}

// M-16. Plausible defect: a reflector's surface, whose picture moves while
// its own motion vectors stand still, is not marked for TAA and FSR, so the
// reflection ghosts. The fixture: a view under TAA before a mirror, and
// the same mirror without its reflector. The oracle is the register's
// rule: the mirror's texels are marked transparency and composition 1 in
// every frame the view shows its picture, a texel beside it is never
// marked, and without the reflector the scene holds no masks at all.
#[test]
fn a_reflectors_surface_is_marked_while_its_picture_shows() {
    let Some(mut stage) = Stage::new(true) else {
        return;
    };
    for frame in 0..3 {
        stage.frame(false, PlanarReflections::Half);
        let mirror = stage.composition((32, 32));
        assert!(
            (mirror - 1.).abs() <= 1. / 255.,
            "frame {frame}: the mirror's composition mask is {mirror}"
        );
        let beside = stage.composition((4, 4));
        assert!(
            beside <= 1. / 255.,
            "frame {frame}: the composition mask beside the mirror is {beside}"
        );
    }
    let Some(mut plain) = Stage::new(false) else {
        return;
    };
    plain.frame(false, PlanarReflections::Half);
    assert!(plain.main.targets.temporal_masks.is_none());
}

// Plausible defect: a view whose projection is not a perspective one shows
// a reflector, whose picture's lookup and clip then take view rays from an
// eye the view does not have. The fixture: the view before the mirror with
// an orthographic camera that holds it. The oracle is the contract: an
// orthographic view shows no reflector and holds nothing for one.
#[test]
fn an_orthographic_view_shows_no_reflector() {
    let Some(mut stage) = Stage::new(true) else {
        return;
    };
    let orthographic = FrameInput::new(Camera {
        view: Mat4::IDENTITY,
        // Reversed Z over 0.1..100 m, 4 m wide: the mirror, 0.8 m wide and
        // 3 m ahead, is in it.
        projection: glam::camera::rh::proj::directx::orthographic(-2., 2., -2., 2., 100., 0.1),
        eye: Vec3::ZERO,
    });
    stage.frame_seen(&orthographic, PlanarReflections::Half);
    assert_eq!(stage.holds().0, 0);
    // The same view through a perspective camera shows it.
    stage.frame(false, PlanarReflections::Half);
    assert_eq!(stage.holds().0, 1);
}

// #888. Plausible defects: a view releases a reflector's reflected view, or
// its planar method, the frame the reflector leaves it, and makes them
// again, with every stage's programs, the frame it returns, which costs
// that frame milliseconds of CPU; it keeps them and makes a program all the
// same on return; or, kept, it holds its picture's targets and shadow maps
// while it shows nothing. The fixture: a view without antialiasing (whose
// targets are then the reflections' alone) before a mirror, turned away
// for 30 frames, well within the idle period, and back. The oracles: the
// device's own report holds the same pipelines, layouts and shader modules
// while the reflector is away and in the frame it returns as while it
// showed it, and the same textures again once it returns; while away, the
// kept view's picture targets hold one texel and its cascades one texel a
// side, where showing they held the picture and the `Low` cascades (1024).
#[test]
fn a_reflector_that_returns_within_the_idle_period_makes_nothing() {
    let Some(mut stage) = Stage::counted() else {
        return;
    };
    let level = PlanarReflections::Half;
    for _ in 0..3 {
        stage.frame(false, level);
    }
    let (programs, targets) = stage.held();
    let (picture, cascade) = stage.kept_texels().unwrap();
    assert!(picture > 1 && cascade == 1024, "{picture} {cascade}");
    for frame in 0..30 {
        stage.frame(true, level);
        assert_eq!(stage.held().0, programs, "away, frame {frame}");
        assert_eq!(stage.kept_texels(), Some((1, 1)), "away, frame {frame}");
    }
    assert_eq!(stage.main.planar_reflections(&stage.device).len(), 0);
    stage.frame(false, level);
    assert_eq!(stage.held(), (programs, targets), "the frame it returns");
    assert_eq!(stage.kept_texels(), Some((picture, cascade)));
    assert_eq!(stage.main.planar_reflections(&stage.device).len(), 1);
}

// #888. Plausible defect: a reflector left behind pins its reflected view's
// programs and the planar method for good. The fixture: the view before a
// mirror, then turned away for one frame more than the idle period. The
// oracle is the contract: the view then holds no reflected view and no
// planar method, and the device holds fewer pipelines and textures than
// while it showed it.
#[test]
fn a_reflector_away_past_the_idle_period_is_released() {
    let Some(mut stage) = Stage::counted() else {
        return;
    };
    let level = PlanarReflections::Half;
    for _ in 0..3 {
        stage.frame(false, level);
    }
    let showing = stage.held();
    for _ in 0..super::reflected::REFLECTED_VIEW_IDLE_FRAMES {
        stage.frame(true, level);
    }
    assert_eq!(stage.holds().0, 1, "kept for the idle period");
    stage.frame(true, level);
    assert_eq!(stage.holds(), (0, false, false), "released after it");
    let away = stage.held();
    assert!(
        away.0[0] < showing.0[0] && away.1[0] < showing.1[0],
        "the device holds {away:?} after the release, {showing:?} while showing it"
    );
}

// Plausible defects: sprites left out of a reflected view, which runs the
// transparent stage; faced to the consumer's camera rather than the
// reflected one, or placed through the reflected view's oblique, cropped
// projection wrongly, so a mirror shows them on the wrong side or not at
// all. The fixture: a masked red sprite 0.6 m wide 1 m behind the camera,
// which the camera does not see, 0.2 m to its right, before the mirror
// 3 m ahead. The oracle is the mirror's geometry: the sprite's mirror
// image stands 7 m ahead, 0.2 m to the right, so the view sees it red in
// the mirror along (0.2, 0, -7) and not along (-0.2, 0, -7), where the
// mirror shows the empty sky.
#[test]
fn a_sprite_shows_in_a_mirror_where_its_mirror_image_stands() {
    use crate::sprite::{Sprite, SpriteBatch, SpriteBlend};
    let Some(mut stage) = Stage::counted() else {
        return;
    };
    let red = crate::asset::Image::Rgba8(image::RgbaImage::from_pixel(
        1,
        1,
        image::Rgba([255, 0, 0, 255]),
    ));
    let image = stage
        .scene
        .add_sprite_image(&stage.device, &stage.queue, &red)
        .unwrap();
    let batch = stage
        .scene
        .add_sprite_batch(&SpriteBatch {
            blend: SpriteBlend::Masked { cutoff: 0.5 },
            ..SpriteBatch::new(image)
        })
        .unwrap();
    let sprite = Sprite::new(Vec3::new(0.2, 0., 1.), glam::Vec2::splat(0.6));
    stage
        .scene
        .set_sprites(&stage.device, &stage.queue, batch, &[sprite])
        .unwrap();
    stage.frame(false, PlanarReflections::Full);
    let composite = test_support::read(
        &stage.device,
        &stage.queue,
        stage.main.targets.composite.texture(),
        8,
    );
    // The texel along a direction from the camera at the origin.
    let texel = |direction: Vec3| {
        let x = direction.x / -direction.z / 0.5f32.tan();
        let column = ((x * 0.5 + 0.5) * SIZE[0] as f32) as u32;
        let row = SIZE[1] / 2;
        let at = ((row * SIZE[0] + column) * 8) as usize;
        [0, 1].map(|c| test_support::half(&composite[at + c * 2..at + c * 2 + 2]))
    };
    let [shown, shown_green] = texel(Vec3::new(0.2, 0., -7.));
    let [opposite, _] = texel(Vec3::new(-0.2, 0., -7.));
    assert!(
        shown > 0.5 && shown_green < 0.1,
        "the mirror image: {shown}, {shown_green}"
    );
    assert!(opposite < 0.1, "the other side: {opposite}");
}

// A reflected view shades the scene's materials through the same owner as
// every view, so a map's UV set and transform hold in a mirror (Material
// map coordinates). Plausible defect: the reflected view's pipelines drawn
// without the material's second set or its transform. The fixture: an
// unlit, double-sided, masked red quad 1.2 m wide, 1 m behind the camera,
// which the camera does not see; its base map (`half_cut_out`: its left
// half cut out) is on the second set, which runs against world X, through
// a transform that mirrors it back (u' = 1 - u), so its +X half is kept;
// its first set lies in the map's kept half. The oracle is the mirror's
// geometry, as for the sprite above: the view sees the quad red in the
// mirror along (0.2, 0, -7) and the sky along (-0.2, 0, -7); read from
// the first set both show red, and from the second untransformed the two
// swap.
#[test]
fn a_map_on_the_second_set_through_a_transform_holds_in_a_mirror() {
    use crate::asset::{UvSet, UvTransform};
    let Some(mut stage) = Stage::counted() else {
        return;
    };
    let mut quad = test_support::masked(test_support::cube(), 0.5);
    quad.meshes[0] = crate::asset::CpuMesh {
        vertices: [(-1., -1.), (1., -1.), (1., 1.), (-1., 1.)]
            .map(|(x, y): (f32, f32)| crate::asset::Vertex {
                position: [x * 0.6, y * 0.6, 1.],
                normal: [0., 0., -1.],
                uv: [0.9, 0.5],
                uv1: [(1. - x) * 0.5, 0.5],
                color: [1.; 4],
                lightmap_bounds: [0., 0., 1., 1.],
                ..bytemuck::Zeroable::zeroed()
            })
            .to_vec(),
        indices: vec![0, 1, 2, 0, 2, 3],
        material: 0,
        deformation: Default::default(),
    };
    let material = &mut quad.materials[0];
    material.base = [1., 0., 0., 1.];
    material.unlit = true;
    material.double_sided = true;
    let base = material.base_texture.as_mut().unwrap();
    base.uv_set = UvSet::Uv1;
    base.transform = UvTransform {
        offset: [1., 0.],
        rotation: 0.,
        scale: [-1., 1.],
    };
    let ids = stage
        .scene
        .add_asset(&stage.device, &stage.queue, quad)
        .unwrap();
    stage
        .scene
        .add_instance(
            &stage.device,
            &stage.queue,
            InstanceState::new(ids.model),
            Mobility::Static,
        )
        .unwrap();
    stage.frame(false, PlanarReflections::Full);
    let composite = test_support::read(
        &stage.device,
        &stage.queue,
        stage.main.targets.composite.texture(),
        8,
    );
    // The texel along a direction from the camera at the origin.
    let texel = |direction: Vec3| {
        let x = direction.x / -direction.z / 0.5f32.tan();
        let column = ((x * 0.5 + 0.5) * SIZE[0] as f32) as u32;
        let row = SIZE[1] / 2;
        let at = ((row * SIZE[0] + column) * 8) as usize;
        [0, 1].map(|c| test_support::half(&composite[at + c * 2..at + c * 2 + 2]))
    };
    let [kept, kept_green] = texel(Vec3::new(0.2, 0., -7.));
    let [cut, _] = texel(Vec3::new(-0.2, 0., -7.));
    assert!(
        kept > 0.5 && kept_green < 0.1,
        "the kept half in the mirror: {kept}, {kept_green}"
    );
    assert!(cut < 0.1, "the cut-out half in the mirror: {cut}");
}
