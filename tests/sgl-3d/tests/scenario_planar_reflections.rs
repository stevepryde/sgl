//! Planar reflections in the scenario harness (`specs/testing.md`, "SGL3D
//! scenario harness"): a planar reflector's picture on the surfaces whose
//! material names it (the architecture's Frame, "Planar reflections"),
//! judged by numbers read from the frame of the view that shows the mirror:
//! which of the scene's known colours its texels hold where the mirror's
//! geometry says an object's image lies.
//!
//! The suite covers the feature at rest, with an object moving across the
//! mirror, with the viewer translating and turning (the views of a mirror
//! from two poses, and the eye crossing the plane), an appearance and a
//! departure (a reflector entering and leaving the view), a camera cut
//! (which a reflected view has no history to lose by: it keeps none in
//! screen space, and its first frame is the one every test reads), a
//! resize, each level, both binding tiers, a screen-space method beside the
//! picture, a jittered viewer, two viewers, two mirrors in one view, two
//! facing mirrors, the roughness cutoff, the visibility mask and a moved
//! reflector, and fog (of a card and a sprite) and aerial perspective along a
//! reflected path (#887).
//! It does not cover a masked reflector's cut-out texels, a viewer that
//! itself draws a render texture, world-space rays beside a picture, nor a
//! reflector under shadows, probes or dynamic GI, which light a reflected
//! view as they light any view. Of the fog it does not cover a tilted
//! plane (water), whose bound on the froxels behind it is tested against a
//! brute-force model (`shading::fog_plane_tests`), fog under the viewer's or
//! the reflector's motion, nor the per-sample medium test within the froxel
//! the plane crosses: that moves the result by at most one slice's medium,
//! the order to which the integration's slice steps already place any
//! change of density, so no fixture separates the two (the viewer's own fog
//! at a surface takes medium from up to half a slice behind it). The
//! clipped projection's depth is covered where it is owned
//! (`shading::view_depth_tests`,
//! `probe_tiles_keep_their_probes_under_a_clipped_projection`).
#![cfg(not(target_arch = "wasm32"))]
mod scenario;

use scenario::geometry::{Pose, View};
use scenario::rig::{Gpu, Rig, floor_gpu, gpu};
use scenario::temporal::{DEPTH, Failures, VIEW};
use sgl_3d::asset::{Asset, CpuMesh as Mesh, Material, Vertex};
use sgl_3d::diagnostics::DiagnosticTarget;
use sgl_3d::glam::{Vec2, Vec3};
use sgl_3d::graphics_device::BindingTier;
use sgl_3d::settings::{Antialiasing, PlanarReflections, ReflectionMethod, ScreenSpaceReflections};
use sgl_3d::{
    Exposure, InstanceId, InstanceState, MaterialId, Mobility, PlanarReflector, PlanarReflectorId,
    SurfaceMaterial,
};
use std::f32::consts::PI;

/// The backdrop's grey, which a mirror shows where its picture holds
/// nothing.
const SKY: f32 = 0.05;
/// A mirror's size in metres: it spans 79 by 44 of the main view's pixels
/// `DEPTH` ahead.
const MIRROR: Vec2 = Vec2::new(3.2, 1.8);
/// An object's size in metres.
const OBJECT: Vec2 = Vec2::new(0.6, 0.6);
/// Where the mirror stands: `DEPTH` ahead of the origin, facing it.
const MIRROR_CENTRE: Vec3 = Vec3::new(0., 0., -DEPTH);

/// The scene's known colours, as the render-texture suite's: three in
/// which one channel is strong, then three in which one is weak.
const COLOURS: [[f32; 3]; 6] = [
    [0.6, 0.02, 0.02],
    [0.02, 0.6, 0.02],
    [0.02, 0.02, 0.6],
    [0.02, 0.6, 0.6],
    [0.6, 0.02, 0.6],
    [0.6, 0.6, 0.02],
];

/// Which of `COLOURS` a texel holds, by its channels' order alone, which a
/// mirror's response keeps: one channel at least 3 times both others, or
/// two at least 3 times the third. None for a grey and for black.
fn colour(texel: [f32; 4]) -> Option<usize> {
    let [r, g, b, _] = texel;
    let channels = [r, g, b];
    let strong = |c: usize| (0..3).all(|o| o == c || channels[c] >= 3. * channels[o]);
    let weak = |c: usize| (0..3).all(|o| o == c || channels[o] >= 3. * channels[c]);
    if channels.iter().all(|&c| c < 0.01) {
        return None;
    }
    (0..3)
        .find(|&c| strong(c))
        .or_else(|| (0..3).find(|&c| weak(c)).map(|c| c + 3))
}

/// A point's mirror image in the plane through `origin` with unit `normal`,
/// by construction: twice its foot on the plane less itself.
fn image(origin: Vec3, normal: Vec3, point: Vec3) -> Vec3 {
    point - 2. * normal * normal.dot(point - origin)
}

/// The radiance `view`'s last frame holds at pixel `pixel` (its
/// `Composite` target).
fn radiance(rig: &Rig, view: usize, pixel: Vec2) -> [f32; 4] {
    rig.read_view(view, DiagnosticTarget::Composite)
        .at([pixel.x as u32, pixel.y as u32])
}

/// Where the world point `point` lies in `view`, in pixels.
fn pixel(rig: &Rig, view: usize, point: Vec3) -> Vec2 {
    let lens = rig.views[view].lens;
    lens.project(&rig.views[view].input.camera, point)
        .expect("a point before the camera")
        .0
}

/// Makes `material` a perfect mirror of `reflector`: a lit metal of
/// roughness 0 that reflects all it receives, one-sided, emitting nothing.
fn mirror_material(rig: &mut Rig, material: MaterialId, reflector: PlanarReflectorId) {
    let values = rig.scene.material(material).expect("a mirror's material");
    rig.scene
        .set_material(
            &rig.gpu.queue,
            material,
            SurfaceMaterial {
                base: [1., 1., 1., 1.],
                emission: [0.; 3],
                metallic: 1.,
                roughness: 0.,
                unlit: false,
                double_sided: false,
                planar_reflector: Some(reflector),
                ..values
            },
        )
        .expect("a mirror");
}

/// Moves the emissive card `id` to `pose`.
fn place(rig: &mut Rig, id: InstanceId, pose: Pose) {
    let state = InstanceState {
        pose: pose.matrix(),
        ..*rig.scene.instance(id).expect("a card")
    };
    rig.scene
        .set_instance(&rig.gpu.queue, id, state)
        .expect("a card's pose");
}

/// Sets how many faces `material` shows.
fn one_sided(rig: &mut Rig, material: MaterialId) {
    let values = rig.scene.material(material).expect("a card's material");
    rig.scene
        .set_material(
            &rig.gpu.queue,
            material,
            SurfaceMaterial {
                double_sided: false,
                ..values
            },
        )
        .expect("a one-sided card");
}

fn level(rig: &mut Rig, view: usize, level: PlanarReflections) {
    rig.views[view].settings.planar_reflections = level;
}

/// A wall mirror `DEPTH` ahead of view 0, which stands at the origin
/// looking along -Z, and nothing else: a script adds what it reflects,
/// behind the viewer.
struct Hall {
    rig: Rig,
    reflector: PlanarReflectorId,
    mirror: InstanceId,
    material: MaterialId,
}

impl Hall {
    fn new(gpu: &Gpu) -> Hall {
        Hall::with_level(gpu, PlanarReflections::Full)
    }

    fn with_level(gpu: &Gpu, planar: PlanarReflections) -> Hall {
        Hall::with(gpu, planar, Antialiasing::Off)
    }

    fn with(gpu: &Gpu, planar: PlanarReflections, antialiasing: Antialiasing) -> Hall {
        let mut rig = Rig::new(gpu, VIEW, antialiasing, SKY);
        level(&mut rig, 0, planar);
        let reflector = rig
            .scene
            .add_planar_reflector(PlanarReflector::new(MIRROR_CENTRE, Vec3::Z))
            .expect("a reflector");
        let (mirror, material) = rig.add_emissive(MIRROR, [0.; 3], Pose::at(MIRROR_CENTRE));
        mirror_material(&mut rig, material, reflector);
        Hall {
            rig,
            reflector,
            mirror,
            material,
        }
    }

    /// Adds an object of `COLOURS[index]` at `at`, facing the mirror or,
    /// with `yaw`, turned from it; two-sided.
    fn object(&mut self, index: usize, at: Vec3) -> (InstanceId, MaterialId) {
        self.rig.add_emissive(OBJECT, COLOURS[index], Pose::at(at))
    }

    /// Where `point`'s mirror image lies in `view`, in pixels.
    fn image_pixel(&self, view: usize, point: Vec3) -> Vec2 {
        pixel(&self.rig, view, image(MIRROR_CENTRE, Vec3::Z, point))
    }

    /// Which colour `view`'s last frame holds where `point`'s image lies.
    fn shown(&self, view: usize, point: Vec3) -> Option<usize> {
        colour(radiance(&self.rig, view, self.image_pixel(view, point)))
    }

    /// The reflectors view `view` showed in its last frame, with each
    /// picture's size.
    fn pictures(&mut self, view: usize) -> Vec<(PlanarReflectorId, [u32; 2])> {
        self.rig.views[view]
            .view
            .planar_reflections(&self.rig.gpu.device)
            .into_iter()
            .map(|stats| (stats.reflector, stats.size))
            .collect()
    }
}

/// Where the moving object stands at `frame`: behind the viewer, outside
/// its frustum, 1 m further along +X each frame (about 10 px of its image
/// in the mirror, which is 6 px wide).
fn object_at(frame: u32) -> Vec3 {
    Vec3::new(-1.5 + frame as f32, 0.4, 2.)
}

/// Plausible defects: the mirror shows the frame before's picture; the
/// picture is not mirrored, or is flipped left to right; the reflected
/// camera is placed at the viewer's eye; the Basic binding tier does not
/// bind the picture. The fixture: `Hall` with an object behind the viewer,
/// outside the main frustum, moving 1 m a frame, on a device of the
/// adapter's limits or, with `floor`, of S3D-1's floor. The oracle is
/// analytic: in each frame the pixel where the mirror's geometry puts the
/// object's image (its mirror image, projected by the viewer's camera)
/// holds the object's colour, and the pixel mirrored about the view's
/// centre column, where an unflipped picture would put it, the sky. With
/// `method`, a screen-space method runs beside the picture, whose result
/// the frame's then starts from (plausible defects: the copy loses the
/// picture, or the method's result replaces it); with `antialiasing`, the
/// viewer is jittered (the lookup takes the jittered depth for the stable
/// one).
fn a_mirror_shows_what_is_behind_the_viewer(
    floor: bool,
    method: Option<ReflectionMethod>,
    antialiasing: Antialiasing,
) {
    let Some(gpu) = (if floor { floor_gpu() } else { gpu() }) else {
        return;
    };
    let mut hall = Hall::with(&gpu, PlanarReflections::Full, antialiasing);
    if let Some(method) = method {
        let settings = &mut hall.rig.views[0].settings;
        settings.screen_space_reflections = ScreenSpaceReflections::Full;
        settings.reflection_method = method;
    }
    let tier = hall.rig.renderer.binding_tier();
    assert!(
        !floor || tier == BindingTier::Basic,
        "a device at S3D-1's floor takes the Basic tier, not {tier:?}"
    );
    let (object, _) = hall.object(0, object_at(0));
    let mut failures = Failures::default();
    for frame in 0..4 {
        place(&mut hall.rig, object, Pose::at(object_at(frame)));
        hall.rig.render(frame);
        let at = hall.image_pixel(0, object_at(frame));
        let shown = colour(radiance(&hall.rig, 0, at));
        failures.check(shown == Some(0), || {
            format!("{tier:?}, frame {frame}: the mirror holds {shown:?} at {at}, where the object's image lies, not its colour")
        });
        let unflipped = Vec2::new(VIEW.size[0] as f32 - at.x, at.y);
        let other = radiance(&hall.rig, 0, unflipped);
        failures.check(colour(other).is_none(), || {
            format!("{tier:?}, frame {frame}: the mirror holds {other:?} at {unflipped}, where an unflipped picture would put the object")
        });
    }
    failures.assert();
}

mod a_mirror_shows_what_is_behind_the_viewer {
    crate::scenario::temporal::case_tests!(
        a_mirror_shows_what_is_behind_the_viewer;
        extended_tier: (false, None, crate::Antialiasing::Off),
        basic_tier: (true, None, crate::Antialiasing::Off),
        beside_velvet: (false, Some(crate::ReflectionMethod::Velvet), crate::Antialiasing::Off),
        beside_crystal: (false, Some(crate::ReflectionMethod::Crystal), crate::Antialiasing::Off),
        under_taa: (false, None, crate::Antialiasing::Taa),
    );
}

/// Plausible defects: the reflected view is not clipped at the plane, so
/// what stands behind the mirror hides the scene in front of it; the clip
/// is on the wrong side. The fixture: `Hall` with an object behind the
/// viewer and a dark card behind the mirror, between the reflected camera
/// (8 m behind the viewer's, through the mirror) and the plane, which
/// covers the reflected camera's whole view of the object. The oracle is
/// analytic: the object's image holds its colour, as without the card.
#[test]
fn what_stands_behind_the_plane_is_clipped() {
    let Some(gpu) = gpu() else { return };
    let mut hall = Hall::new(&gpu);
    let at = Vec3::new(0.8, 0.3, 2.);
    hall.object(1, at);
    // Halfway from the reflected eye, at (0, 0, -8), to the plane.
    hall.rig
        .add_emissive(MIRROR, COLOURS[4], Pose::at(Vec3::new(0., 0., -6.)));
    hall.rig.render(0);
    let shown = hall.shown(0, at);
    assert_eq!(
        shown,
        Some(1),
        "the mirror holds {shown:?} where the object's image lies: the card behind it is not clipped"
    );
}

/// Plausible defects: the reflected view's winding is not the viewer's, so
/// it culls front faces and draws back faces. The fixture: `Hall` with two
/// one-sided cards behind the viewer, one facing the mirror and one facing
/// away from it. The oracle is analytic: the mirror shows the one that
/// faces it and, where the other's image lies, the sky.
#[test]
fn front_faces_show_in_the_mirror() {
    let Some(gpu) = gpu() else { return };
    let mut hall = Hall::new(&gpu);
    let facing = Vec3::new(-0.9, 0.3, 2.);
    let turned = Vec3::new(0.9, 0.3, 2.);
    // A card faces +Z as placed: toward the mirror when turned half round.
    let (card, material) = hall.object(0, facing);
    place(
        &mut hall.rig,
        card,
        Pose {
            position: facing,
            yaw: PI,
        },
    );
    one_sided(&mut hall.rig, material);
    let (_, material) = hall.object(1, turned);
    one_sided(&mut hall.rig, material);
    hall.rig.render(0);
    let shown = hall.shown(0, facing);
    assert_eq!(
        shown,
        Some(0),
        "the mirror holds {shown:?} where the card that faces it lies"
    );
    let hidden = radiance(&hall.rig, 0, hall.image_pixel(0, turned));
    assert!(
        colour(hidden).is_none(),
        "the mirror holds {hidden:?} where the card that faces away from it lies"
    );
}

/// Plausible defects: the reflected view draws its own reflector's
/// surfaces, which then hide the scene from it wherever they stand in
/// front of the plane (a two-sided mirror, water displaced above its
/// plane). The fixture: a two-sided mirror standing 2 cm proud of its
/// reflector's plane, on the reflecting side, so that its back faces the
/// reflected camera in front of the near plane, and an object behind the
/// viewer. The oracle is analytic: the object's image in the plane holds
/// its colour.
#[test]
fn a_mirror_is_not_in_its_own_picture() {
    let Some(gpu) = gpu() else { return };
    let mut rig = Rig::new(&gpu, VIEW, Antialiasing::Off, SKY);
    level(&mut rig, 0, PlanarReflections::Full);
    let plane = MIRROR_CENTRE - Vec3::Z * 0.02;
    let reflector = rig
        .scene
        .add_planar_reflector(PlanarReflector::new(plane, Vec3::Z))
        .unwrap();
    let (_, material) = rig.add_emissive(MIRROR, [0.; 3], Pose::at(MIRROR_CENTRE));
    mirror_material(&mut rig, material, reflector);
    let values = rig.scene.material(material).unwrap();
    rig.scene
        .set_material(
            &rig.gpu.queue,
            material,
            SurfaceMaterial {
                double_sided: true,
                ..values
            },
        )
        .unwrap();
    let at = Vec3::new(0.8, 0.3, 2.);
    rig.add_emissive(OBJECT, COLOURS[3], Pose::at(at));
    rig.render(0);
    let shown = colour(radiance(&rig, 0, pixel(&rig, 0, image(plane, Vec3::Z, at))));
    assert_eq!(
        shown,
        Some(3),
        "the mirror holds {shown:?} where the object's image lies: it shows in its own picture"
    );
}

/// Plausible defects: a view behind the mirror renders a reflection (of
/// the wrong side), or keeps its reflected view; the picture does not
/// return in the frame the eye comes back. The fixture: `Hall` with an
/// object behind the viewer; the viewer moves through the mirror's plane
/// to stand behind it, facing its back, then returns. The oracle: behind
/// the plane the view shows no reflector and holds no reflected view; in
/// the frame it returns, the object's image holds its colour.
#[test]
fn the_eye_crosses_the_plane() {
    let Some(gpu) = gpu() else { return };
    let mut hall = Hall::new(&gpu);
    let at = Vec3::new(0.8, 0.3, 2.);
    hall.object(2, at);
    hall.rig.render(0);
    assert_eq!(hall.shown(0, at), Some(2), "before crossing");
    assert_eq!(hall.pictures(0).len(), 1);
    // Behind the mirror, looking back at it.
    hall.rig.look(Pose {
        position: Vec3::new(0., 0., -DEPTH - 3.),
        yaw: PI,
    });
    hall.rig.render(1);
    assert!(
        hall.pictures(0).is_empty(),
        "a view behind the plane shows no reflector"
    );
    hall.rig.look(Pose::at(Vec3::ZERO));
    hall.rig.render(2);
    assert_eq!(hall.shown(0, at), Some(2), "the frame the eye returns");
    assert_eq!(hall.pictures(0).len(), 1);
}

/// Plausible defects: the reflected view keeps its size, or its camera its
/// aspect, when its viewer is resized; the picture's size ignores the
/// level. The fixture: `Hall` with an object behind the viewer, rendered at
/// each level at the suite's size and then at another aspect. The oracle is
/// analytic: the object's image holds its colour at every size and level,
/// and the picture holds the mirror's bounds on screen at the level's
/// scale (the level's texels per render pixel, a side) and no more than a
/// step beyond them: an eighth of the level's full side, plus the texels
/// of padding and rounding (4).
#[test]
fn the_picture_follows_the_level_and_a_resize() {
    let Some(gpu) = gpu() else { return };
    let at = Vec3::new(0.8, 0.3, 2.);
    let mut failures = Failures::default();
    for (planar, scale) in [
        (PlanarReflections::Full, 1.),
        (PlanarReflections::Half, 0.5),
        (PlanarReflections::Quarter, 0.25),
    ] {
        let mut hall = Hall::with_level(&gpu, planar);
        hall.object(3, at);
        for (frame, size) in [VIEW.size, [160, 144]].into_iter().enumerate() {
            hall.rig.views[0].lens = View { size, ..VIEW };
            hall.rig.render(frame as u32);
            let shown = hall.shown(0, at);
            failures.check(shown == Some(3), || {
                format!("{planar:?} at {size:?}: the mirror holds {shown:?} where the object's image lies")
            });
            // The mirror's bounds in the view, in pixels.
            let corner = |x: f32, y: f32| {
                pixel(
                    &hall.rig,
                    0,
                    MIRROR_CENTRE + Vec3::new(x * MIRROR.x * 0.5, y * MIRROR.y * 0.5, 0.),
                )
            };
            let span = (corner(1., -1.) - corner(-1., 1.)) * scale;
            let pictures = hall.pictures(0);
            let picture = pictures[0].1;
            let full = size.map(|side| (side as f32 * scale).ceil() as u32);
            failures.check(
                (0..2).all(|axis| {
                    let step = full[axis].div_ceil(8) as f32;
                    picture[axis] as f32 >= span[axis]
                        && picture[axis] as f32 <= span[axis] + step + 4.
                        && picture[axis] <= full[axis]
                }),
                || {
                    format!("{planar:?} at {size:?}: a picture of {picture:?} for a mirror spanning {span} of {full:?}")
                },
            );
        }
    }
    failures.assert();
}

/// Plausible defects: a reflector's picture is shared between the views
/// that show it, so one of them sees the other's reflection. The fixture:
/// `Hall` with an object behind both viewers and a second view to the side
/// of the first, turned toward the mirror. The oracle is analytic: each
/// view holds the object's colour where its own camera puts the object's
/// image, and those pixels differ by more than the object's width.
#[test]
fn two_views_of_one_mirror_each_see_their_own_reflection() {
    let Some(gpu) = gpu() else { return };
    let mut hall = Hall::new(&gpu);
    let side = hall.rig.add_view(VIEW, Antialiasing::Off);
    level(&mut hall.rig, side, PlanarReflections::Full);
    hall.rig.look_view(
        side,
        Pose {
            position: Vec3::new(2., 0., 0.5),
            yaw: 0.35,
        },
    );
    let at = Vec3::new(0.2, 0.3, 2.);
    hall.object(5, at);
    hall.rig.render(0);
    let pixels = [0, side].map(|view| hall.image_pixel(view, at));
    assert!(
        (pixels[0] - pixels[1]).length() > 12.,
        "the fixture separates the two views' images: {pixels:?}"
    );
    for view in [0, side] {
        let shown = hall.shown(view, at);
        assert_eq!(
            shown,
            Some(5),
            "view {view} holds {shown:?} where its own image of the object lies"
        );
        assert_eq!(hall.pictures(view).len(), 1, "view {view}'s own picture");
    }
}

/// Plausible defects: a reflector out of the view, or a level of `Off`,
/// still renders a reflected view or keeps one; a reflector that returns
/// shows a stale picture. The fixture: `Hall` with an object behind the
/// viewer, which turns away from the mirror and back, then sets the level
/// `Off`. The oracle: the view shows no reflector while the mirror is out
/// of its frustum or the level is `Off` (and the mirror then holds no
/// colour, only its fallback), and the object's image holds its colour in
/// the frame the mirror returns.
#[test]
fn a_reflector_out_of_view_has_no_reflected_view() {
    let Some(gpu) = gpu() else { return };
    let mut hall = Hall::new(&gpu);
    let at = Vec3::new(0.8, 0.3, 2.);
    let (object, _) = hall.object(4, at);
    hall.rig.render(0);
    assert_eq!(hall.pictures(0).len(), 1);
    // Looking the other way: the mirror is behind the viewer.
    hall.rig.look(Pose {
        position: Vec3::new(0., 0., -1.),
        yaw: PI,
    });
    hall.rig.render(1);
    assert!(
        hall.pictures(0).is_empty(),
        "a mirror out of the frustum is not shown"
    );
    // It returns, with the object elsewhere: this frame's picture.
    let moved = Vec3::new(-0.8, 0.3, 2.);
    place(&mut hall.rig, object, Pose::at(moved));
    hall.rig.look(Pose::at(Vec3::ZERO));
    hall.rig.render(2);
    assert_eq!(
        hall.shown(0, moved),
        Some(4),
        "the frame the mirror returns"
    );
    assert!(colour(radiance(&hall.rig, 0, hall.image_pixel(0, at))).is_none());
    level(&mut hall.rig, 0, PlanarReflections::Off);
    hall.rig.render(3);
    assert!(hall.pictures(0).is_empty(), "a level of Off shows none");
    let fallback = radiance(&hall.rig, 0, hall.image_pixel(0, moved));
    assert!(
        colour(fallback).is_none(),
        "a mirror without a picture holds {fallback:?}, its fallback"
    );
}

/// Plausible defects: a mirror seen in a mirror renders its own reflected
/// view (unbounded for two that face each other), or shows a picture made
/// for another eye. The fixture: `Hall` with a second mirror behind the
/// viewer, facing the first, and an object beside the viewer, outside its
/// frustum. The first mirror shows the object's image and, beyond it, the
/// second mirror, in which the object's second image would lie. The oracle:
/// the viewer shows one reflector (the second mirror is out of its
/// frustum), the object's first image holds its colour, and where its
/// second image would lie, inside the second mirror's image, the first
/// mirror holds the second's fallback, no colour: one level deep.
#[test]
fn a_mirror_in_a_mirror_shows_its_fallback() {
    let Some(gpu) = gpu() else { return };
    let mut hall = Hall::new(&gpu);
    let behind = Vec3::new(0., 0., DEPTH);
    let second = hall
        .rig
        .scene
        .add_planar_reflector(PlanarReflector::new(behind, Vec3::NEG_Z))
        .expect("a second reflector");
    let (card, material) = hall.rig.add_emissive(MIRROR, [0.; 3], Pose::at(behind));
    place(
        &mut hall.rig,
        card,
        Pose {
            position: behind,
            yaw: PI,
        },
    );
    mirror_material(&mut hall.rig, material, second);
    let at = Vec3::new(0.8, 0., 0.);
    let (_, object) = hall
        .rig
        .add_emissive(Vec2::splat(0.4), COLOURS[1], Pose::at(at));
    let _ = object;
    hall.rig.render(0);
    let pictures = hall.pictures(0);
    assert_eq!(
        pictures.iter().map(|picture| picture.0).collect::<Vec<_>>(),
        [hall.reflector],
        "the viewer shows the mirror before it alone"
    );
    assert_eq!(hall.shown(0, at), Some(1), "the object's first image");
    // The object's image in the second mirror, seen in the first.
    let twice = image(MIRROR_CENTRE, Vec3::Z, image(behind, Vec3::NEG_Z, at));
    let deeper = radiance(&hall.rig, 0, pixel(&hall.rig, 0, twice));
    assert!(
        colour(deeper).is_none() && deeper[..3].iter().all(|&c| c < 0.01),
        "the mirror in the mirror holds {deeper:?}, not its fallback"
    );
}

/// Plausible defects: a view that draws a render texture is left out of
/// the frame, or rendered after the mirror's picture, when its screen is
/// seen only in a mirror, so the mirror shows a black or a stale screen.
/// The fixture: `Hall` with an in-world screen behind the viewer, facing
/// the mirror, that shows the picture a feed view draws of a source whose
/// colour changes every frame; no view has the screen in its own frustum.
/// The oracle is analytic: in every frame the screen's image in the mirror
/// holds that frame's colour.
#[test]
fn a_screen_seen_only_in_a_mirror_shows_this_frames_picture() {
    let Some(gpu) = gpu() else { return };
    let mut hall = Hall::new(&gpu);
    let feed_lens = View {
        size: [64, 36],
        ..VIEW
    };
    let texture = hall
        .rig
        .scene
        .add_render_texture(&gpu.device, sgl_3d::RenderTexture::new(feed_lens.size))
        .expect("a render texture");
    let feed = hall.rig.add_producer(feed_lens, Antialiasing::Off, texture);
    let far = Vec3::new(100., 0., 0.);
    hall.rig.look_view(feed, Pose::at(far));
    let (_, source) = hall.rig.add_emissive(
        Vec2::new(24., 14.),
        COLOURS[0],
        Pose::at(far - Vec3::Z * 8.),
    );
    let at = Vec3::new(0.6, 0.2, 2.);
    let (_, screen) = hall
        .rig
        .add_emissive(Vec2::new(1.6, 0.9), [1.; 3], Pose::at(at));
    let values = hall.rig.scene.material(screen).unwrap();
    hall.rig
        .scene
        .set_material(
            &hall.rig.gpu.queue,
            screen,
            SurfaceMaterial {
                emission_render_texture: Some(texture),
                ..values
            },
        )
        .expect("a screen");
    let mut failures = Failures::default();
    for frame in 0..4 {
        let index = frame as usize % 3;
        let values = hall.rig.scene.material(source).unwrap();
        hall.rig
            .scene
            .set_material(
                &hall.rig.gpu.queue,
                source,
                SurfaceMaterial {
                    emission: COLOURS[index],
                    ..values
                },
            )
            .unwrap();
        hall.rig.render(frame);
        let shown = hall.shown(0, at);
        failures.check(shown == Some(index), || {
            format!("frame {frame}: the screen in the mirror holds {shown:?}, not this frame's colour {index}")
        });
    }
    failures.assert();
}

/// Plausible defects: a view that shows two reflectors gives both the
/// first one's picture, or the second picture's pass clears the first
/// (each writes the frame's one result). The fixture: `Hall` with a second
/// mirror on the wall to the viewer's right, in another plane, and an
/// object for each: one behind the viewer, which only the first mirror
/// shows, and one behind the first mirror's plane, which that mirror clips
/// and only the second shows. The oracle is analytic: the view shows two
/// reflectors, and each object's image in its own mirror holds its colour.
#[test]
fn two_mirrors_in_one_view_each_show_their_own_picture() {
    let Some(gpu) = gpu() else { return };
    let mut hall = Hall::new(&gpu);
    let wall = Vec3::new(3., 0., -DEPTH);
    let second = hall
        .rig
        .scene
        .add_planar_reflector(PlanarReflector::new(wall, Vec3::NEG_X))
        .expect("a second reflector");
    let (card, material) = hall.rig.add_emissive(MIRROR, [0.; 3], Pose::at(wall));
    // A card faces +Z as placed: toward -X a quarter turn on.
    place(
        &mut hall.rig,
        card,
        Pose {
            position: wall,
            yaw: -PI / 2.,
        },
    );
    mirror_material(&mut hall.rig, material, second);
    let behind = Vec3::new(-0.5, 0.3, 2.);
    hall.object(0, behind);
    // Beyond the first mirror, which hides it from the viewer and clips it
    // from its own picture.
    let beyond = Vec3::new(-3., 0.3, -10.);
    hall.object(1, beyond);
    hall.rig.render(0);
    assert_eq!(hall.pictures(0).len(), 2, "the view shows both mirrors");
    assert_eq!(hall.shown(0, behind), Some(0), "the first mirror's object");
    let in_second = pixel(&hall.rig, 0, image(wall, Vec3::NEG_X, beyond));
    let shown = colour(radiance(&hall.rig, 0, in_second));
    assert_eq!(
        shown,
        Some(1),
        "the second mirror holds {shown:?} at {in_second}, where its object's image lies"
    );
}

/// Plausible defects: a rough surface shows the sharp picture at full
/// strength; the cutoff differs with screen-space reflections on and off.
/// The fixture: `Hall` with an object behind the viewer and the mirror's
/// roughness raised to 0.9, above both reflection methods' cutoffs (0.2
/// and 0.7). The oracle is the contract: a lobe at or above the selected
/// method's cutoff takes no method's result, so the object's image holds
/// no colour; at roughness 0 it does, in the same frame's fixture.
#[test]
fn a_rough_reflector_keeps_its_fallback() {
    let Some(gpu) = gpu() else { return };
    let mut hall = Hall::new(&gpu);
    let at = Vec3::new(0.8, 0.3, 2.);
    hall.object(2, at);
    hall.rig.render(0);
    assert_eq!(hall.shown(0, at), Some(2), "a mirror of roughness 0");
    let values = hall.rig.scene.material(hall.material).unwrap();
    hall.rig
        .scene
        .set_material(
            &hall.rig.gpu.queue,
            hall.material,
            SurfaceMaterial {
                roughness: 0.9,
                ..values
            },
        )
        .unwrap();
    hall.rig.render(1);
    let rough = radiance(&hall.rig, 0, hall.image_pixel(0, at));
    assert!(
        colour(rough).is_none(),
        "a reflector of roughness 0.9 holds {rough:?} where the object's image lies"
    );
}

/// Plausible defects: a reflector's surface hidden by the view's
/// visibility mask still makes its reflected view render; a reflector
/// moved with `Scene::set_planar_reflector` keeps reflecting about its old
/// plane. The fixture: `Hall` with an object behind the viewer; the
/// mirror's material is put in a visibility group the view then disables
/// and enables; then the mirror and its reflector move 1.5 m nearer. The
/// oracle: no reflector is shown while the mask hides the mirror, and
/// after the move the object's image in the moved plane holds its colour
/// where the old plane's image, more than 8 px from it, does not.
#[test]
fn a_hidden_mirror_shows_nothing_and_a_moved_one_follows() {
    let Some(gpu) = gpu() else { return };
    let mut hall = Hall::new(&gpu);
    let at = Vec3::new(1.5, 0.3, 0.5);
    hall.object(5, at);
    let group = 1 << 5;
    let values = hall.rig.scene.material(hall.material).unwrap();
    hall.rig
        .scene
        .set_material(
            &hall.rig.gpu.queue,
            hall.material,
            SurfaceMaterial {
                visibility_group: group,
                ..values
            },
        )
        .unwrap();
    hall.rig.render(0);
    assert_eq!(hall.pictures(0).len(), 1, "the mirror's group is enabled");
    hall.rig.views[0].input.visibility_mask &= !group;
    hall.rig.render(1);
    assert!(hall.pictures(0).is_empty(), "the mask hides the mirror");
    hall.rig.views[0].input.visibility_mask |= group;
    // The mirror and its plane, 1.5 m nearer the viewer.
    let moved = MIRROR_CENTRE + Vec3::Z * 1.5;
    place(&mut hall.rig, hall.mirror, Pose::at(moved));
    hall.rig
        .scene
        .set_planar_reflector(hall.reflector, PlanarReflector::new(moved, Vec3::Z))
        .unwrap();
    hall.rig.render(2);
    let new = pixel(&hall.rig, 0, image(moved, Vec3::Z, at));
    let old = hall.image_pixel(0, at);
    assert!(
        (new - old).length() > 8.,
        "the fixture separates the images: {new} {old}"
    );
    let shown = colour(radiance(&hall.rig, 0, new));
    assert_eq!(shown, Some(5), "the moved mirror holds {shown:?} at {new}");
    let stale = radiance(&hall.rig, 0, old);
    assert!(
        colour(stale).is_none(),
        "the moved mirror holds {stale:?} where the old plane put the image"
    );
}

/// Plausible defects: the picture is tone mapped or exposed before the
/// mirror samples it, and again by the viewer; it is stored encoded. The
/// fixture: `Hall` with an object behind the viewer, and a second view
/// turned to see the object directly; both at an exposure of 2 stops,
/// which a picture exposed before the mirror samples it would hold 4
/// times over. The oracle is an equivalence with a bound: a perfect mirror (a metal of
/// roughness 0 with a white base) reflects all it receives, so the image's
/// radiance in the mirror is the object's as the second view sees it
/// directly, no more (energy is not created; half-float rounding) and no
/// less than 0.9 of it (the split-sum table's resolution at roughness 0).
#[test]
fn a_mirror_reflects_radiance_once() {
    let Some(gpu) = gpu() else { return };
    let mut hall = Hall::new(&gpu);
    let at = Vec3::new(0.5, 0.2, 2.);
    hall.object(0, at);
    let direct = hall.rig.add_view(VIEW, Antialiasing::Off);
    hall.rig.look_view(
        direct,
        Pose {
            position: Vec3::ZERO,
            yaw: PI,
        },
    );
    for view in [0, direct] {
        hall.rig.views[view].input.exposure = Exposure {
            stops: 2.,
            automatic: None,
        };
    }
    hall.rig.render(0);
    let seen = radiance(&hall.rig, direct, pixel(&hall.rig, direct, at));
    let mirrored = radiance(&hall.rig, 0, hall.image_pixel(0, at));
    assert!(seen[0] > 0.5, "the direct view sees the object: {seen:?}");
    for channel in 0..3 {
        assert!(
            mirrored[channel] <= seen[channel] * 1.01 + 1e-3
                && mirrored[channel] >= seen[channel] * 0.9 - 1e-3,
            "channel {channel}: the mirror holds {mirrored:?} of the object's {seen:?}"
        );
    }
}

/// A card in the mirror's plane whose vertex normals lean by `tilt`
/// radians about +X from the plane's (+Z toward +Y), with the mirror's
/// material: a surface whose shading normal is not the plane's.
fn tilted_mirror(rig: &mut Rig, reflector: PlanarReflectorId, tilt: f32) -> InstanceId {
    let [low, high] = [-MIRROR * 0.5, MIRROR * 0.5];
    let vertex = |x: f32, y: f32| Vertex {
        position: [x, y, 0.],
        normal: [0., tilt.sin(), tilt.cos()],
        uv: [0.; 2],
        color: [1.; 4],
        tangent: [1., 0., 0., 1.],
        lightmap_bounds: [0., 0., 1., 1.],
        lightmap_uv: [0.; 2],
    };
    let asset = Asset {
        meshes: vec![Mesh {
            vertices: vec![
                vertex(low.x, low.y),
                vertex(high.x, low.y),
                vertex(high.x, high.y),
                vertex(low.x, high.y),
            ],
            indices: vec![0, 1, 2, 0, 2, 3],
            material: 0,
            deformation: Default::default(),
        }],
        materials: vec![Material {
            name: "tilted mirror".into(),
            base: [1., 1., 1., 1.],
            metallic: 1.,
            roughness: 0.,
            ..Material::default()
        }],
        images: Vec::new(),
        rig: Default::default(),
        ignored: Vec::new(),
        lights: Default::default(),
        instanced: Vec::new(),
    };
    let ids = rig
        .scene
        .add_asset(&rig.gpu.device, &rig.gpu.queue, asset)
        .expect("a tilted mirror's model");
    let values = rig.scene.material(ids.materials[0]).unwrap();
    rig.scene
        .set_material(
            &rig.gpu.queue,
            ids.materials[0],
            SurfaceMaterial {
                planar_reflector: Some(reflector),
                ..values
            },
        )
        .expect("a tilted mirror's material");
    rig.scene
        .add_instance(
            &rig.gpu.device,
            &rig.gpu.queue,
            InstanceState {
                pose: Pose::at(MIRROR_CENTRE).matrix(),
                ..InstanceState::new(ids.model)
            },
            Mobility::Moving,
        )
        .expect("a tilted mirror")
}

/// Plausible defects: a surface's own normal does not bend its reflection,
/// or bends it by an amount unrelated to the angle and to how far the
/// reflected scene is. The fixture: a mirror whose shading normal leans
/// 0.03 rad upward from its plane's, and a wall 6 m behind the viewer with
/// a marker on it. The oracle is analytic: the ray from the viewer's eye
/// to the mirror's centre, reflected about the leaning normal, meets the
/// wall at a point computed here; a marker there shows at the mirror's
/// centre, and a marker where the plane's own normal would send the ray
/// does not. The lookup takes the distance of what the mirror direction
/// reaches, which here differs from the leaning ray's by under 1 part in
/// 500, a twentieth of the marker's half width.
#[test]
fn a_leaning_normal_bends_the_reflection() {
    let Some(gpu) = gpu() else { return };
    let tilt = 0.03_f32;
    let wall = 6.;
    // The ray to the mirror's centre runs along -Z; it leaves along the
    // leaning normal turned twice from +Z, and meets the wall at:
    let direction = Vec3::new(0., (2. * tilt).sin(), (2. * tilt).cos());
    let reach = (wall - MIRROR_CENTRE.z) / direction.z;
    let leaning = MIRROR_CENTRE + direction * reach;
    let straight = Vec3::new(0., 0., wall);
    assert!(
        (leaning - straight).length() > 0.5,
        "the fixture separates the two rays' ends: {leaning}"
    );
    for (marker, shows) in [(leaning, true), (straight, false)] {
        let mut rig = Rig::new(&gpu, VIEW, Antialiasing::Off, SKY);
        level(&mut rig, 0, PlanarReflections::Full);
        let reflector = rig
            .scene
            .add_planar_reflector(PlanarReflector::new(MIRROR_CENTRE, Vec3::Z))
            .unwrap();
        tilted_mirror(&mut rig, reflector, tilt);
        // The wall, dark, and the marker just before it.
        rig.add_emissive(
            Vec2::splat(12.),
            [0.02; 3],
            Pose::at(Vec3::new(0., 0., wall + 0.01)),
        );
        rig.add_emissive(Vec2::splat(0.4), COLOURS[0], Pose::at(marker));
        rig.render(0);
        let centre = radiance(&rig, 0, pixel(&rig, 0, MIRROR_CENTRE));
        assert_eq!(
            colour(centre) == Some(0),
            shows,
            "a marker at {marker}: the mirror's centre holds {centre:?}"
        );
    }
}

/// Plausible defects: a reflector that a moving camera brings to another
/// part of the screen keeps the crop of the frame before; a floor that
/// passes under the camera (its bounds cross the near plane) is cropped to
/// nothing or turned inside out. The fixture: a floor mirror 1.6 m below
/// the viewer, from behind it to far ahead, and an object above the floor
/// ahead of the viewer; the viewer walks forward. The oracle is analytic:
/// each frame the object's image in the floor holds its colour.
#[test]
fn a_floor_under_a_moving_viewer_reflects() {
    let Some(gpu) = gpu() else { return };
    let mut rig = Rig::new(&gpu, VIEW, Antialiasing::Off, SKY);
    level(&mut rig, 0, PlanarReflections::Full);
    let height = -1.6;
    let origin = Vec3::new(0., height, 0.);
    let reflector = rig
        .scene
        .add_planar_reflector(PlanarReflector::new(origin, Vec3::Y))
        .unwrap();
    // A floor: a mesh in the plane y = `height`, facing up.
    let vertex = |x: f32, z: f32| Vertex {
        position: [x, 0., z],
        normal: [0., 1., 0.],
        uv: [0.; 2],
        color: [1.; 4],
        tangent: [1., 0., 0., 1.],
        lightmap_bounds: [0., 0., 1., 1.],
        lightmap_uv: [0.; 2],
    };
    let asset = Asset {
        meshes: vec![Mesh {
            vertices: vec![
                vertex(-20., 20.),
                vertex(20., 20.),
                vertex(20., -40.),
                vertex(-20., -40.),
            ],
            indices: vec![0, 1, 2, 0, 2, 3],
            material: 0,
            deformation: Default::default(),
        }],
        materials: vec![Material {
            name: "floor mirror".into(),
            base: [1., 1., 1., 1.],
            metallic: 1.,
            roughness: 0.,
            ..Material::default()
        }],
        images: Vec::new(),
        rig: Default::default(),
        ignored: Vec::new(),
        lights: Default::default(),
        instanced: Vec::new(),
    };
    let ids = rig
        .scene
        .add_asset(&rig.gpu.device, &rig.gpu.queue, asset)
        .unwrap();
    let values = rig.scene.material(ids.materials[0]).unwrap();
    rig.scene
        .set_material(
            &rig.gpu.queue,
            ids.materials[0],
            SurfaceMaterial {
                planar_reflector: Some(reflector),
                ..values
            },
        )
        .unwrap();
    rig.scene
        .add_instance(
            &rig.gpu.device,
            &rig.gpu.queue,
            InstanceState {
                pose: Pose::at(origin).matrix(),
                ..InstanceState::new(ids.model)
            },
            Mobility::Static,
        )
        .unwrap();
    // Above the floor, ahead: its image lies as far below it.
    let at = Vec3::new(0.6, -0.4, -9.);
    rig.add_emissive(OBJECT, COLOURS[2], Pose::at(at));
    let mut failures = Failures::default();
    for frame in 0..4 {
        rig.look(Pose::at(Vec3::new(0., 0., -(frame as f32))));
        rig.render(frame);
        let under = pixel(&rig, 0, image(origin, Vec3::Y, at));
        let shown = colour(radiance(&rig, 0, under));
        failures.check(shown == Some(2), || {
            format!(
                "frame {frame}: the floor holds {shown:?} at {under}, where the object's image lies"
            )
        });
    }
    failures.assert();
}

/// The viewer's and the reflected views' fog: a uniform medium of
/// `density` that absorbs and scatters nothing (albedo 0, no ambient), so
/// light through it falls by its transmittance alone, over `length` metres
/// of 64 slices; or, with no density, the medium of the scene's fog
/// volumes alone.
fn fog(rig: &mut Rig, density: f32) {
    let view = &mut rig.views[0];
    view.settings.atmosphere = true;
    view.input.atmosphere = true;
    view.input.fog = sgl_3d::Fog {
        density,
        albedo: [0.; 3],
        ambient: 0.,
        length: FOG_LENGTH,
        ..sgl_3d::Fog::default()
    };
}

/// The fog's reach in metres: its 64 slices spread by the square over it.
const FOG_LENGTH: f32 = 32.;

/// The depth of the fog's slice at `depth` metres: 64 slices over
/// `FOG_LENGTH`, slice `i` ending at `FOG_LENGTH × (i / 64)²`
/// (shading/fog.wgsl `fog_slice_depth`, spread 2), so one is
/// 2 × FOG_LENGTH × u / 64 deep where u = √(depth / FOG_LENGTH).
fn fog_slice(depth: f32) -> f32 {
    2. * FOG_LENGTH * (depth / FOG_LENGTH).sqrt() / 64.
}

/// #887. Plausible defect: the reflected view fogs the reflected path from
/// the mirrored eye, which the viewer then fogs again from its eye to the
/// mirror, so that stretch counts twice (three.js r187's and Wicked
/// Engine's practice), or the viewer's stretch is skipped. The fixture:
/// `Hall` with an emitter behind the viewer, outside its frustum, in a
/// uniform medium that only absorbs. The oracle is Beer–Lambert over the
/// path the light takes, computed here from the placement: eye to mirror
/// to object, the eye's distance to the object's mirror image (4 m to the
/// mirror's plane and about 6 m back to the object, 10 m in all). The
/// object's image in the mirror holds, fogged over unfogged, the
/// transmittance of that path, within the slices' depth at the three
/// places a stretch ends (the mirror for the viewer, the plane and the
/// object for the reflected view), each at most a slice of absorption;
/// counting the eye-to-mirror stretch twice falls 4 m of absorption short.
#[test]
fn a_reflection_is_fogged_once_along_its_path() {
    fogged_once(false);
}

/// The case above with the emitter a sprite (a blended sprite of the
/// card's size and colour, alpha 1), which takes the frame's fog in the
/// transparent stage rather than in shading (plausible defect besides the
/// above: the reflected view's sprites sample its fog at a depth its
/// clipped projection skews).
#[test]
fn a_sprite_in_a_reflection_is_fogged_once_along_its_path() {
    fogged_once(true);
}

fn fogged_once(sprite: bool) {
    let Some(gpu) = gpu() else { return };
    let density = 0.08;
    let at = Vec3::new(0.6, 0.3, 2.);
    let radiance = |fogged: bool| {
        let mut hall = Hall::new(&gpu);
        if sprite {
            let (device, queue) = (hall.rig.gpu.device.clone(), hall.rig.gpu.queue.clone());
            let white = sgl_3d::asset::Image::Rgba8(image::RgbaImage::from_pixel(
                1,
                1,
                image::Rgba([255; 4]),
            ));
            let image = hall
                .rig
                .scene
                .add_sprite_image(&device, &queue, &white)
                .unwrap();
            let batch = hall
                .rig
                .scene
                .add_sprite_batch(&sgl_3d::sprite::SpriteBatch::new(image))
                .unwrap();
            let [r, g, b] = COLOURS[0];
            let card = sgl_3d::sprite::Sprite {
                tint: [r, g, b, 1.],
                ..sgl_3d::sprite::Sprite::new(at, OBJECT)
            };
            hall.rig
                .scene
                .set_sprites(&device, &queue, batch, &[card])
                .unwrap();
        } else {
            hall.object(0, at);
        }
        fog(&mut hall.rig, if fogged { density } else { 0. });
        if !fogged {
            hall.rig.views[0].input.atmosphere = false;
        }
        // The medium's temporal blend settles over its history.
        for frame in 0..24 {
            hall.rig.render(frame);
        }
        let pixel = hall.image_pixel(0, at);
        radiance(&hall.rig, 0, pixel)[0]
    };
    let clear = radiance(false);
    let fogged = radiance(true);
    assert!(clear > 0.3, "the mirror shows the object: {clear}");
    let eye = Vec3::ZERO;
    let path = (image(MIRROR_CENTRE, Vec3::Z, at) - eye).length();
    let to_mirror = path * DEPTH / (DEPTH + at.z + DEPTH);
    let slack = density * (fog_slice(DEPTH) + fog_slice(DEPTH) + fog_slice(path));
    let measured = (fogged / clear).ln();
    let expected = -density * path;
    assert!(
        (measured - expected).abs() <= slack,
        "the reflection's fog is {measured} of optical depth, not the path's {expected} (± {slack}); \
         counting the {to_mirror} m to the mirror twice gives {}",
        expected - density * to_mirror
    );
}

/// #887. Plausible defect: the reflected view's fog takes the medium of a
/// fog volume that lies behind the mirror's plane, between the mirrored eye
/// and the plane, where no reflected light passes. The fixture: `Hall`
/// with an emitter behind the viewer, and a dense fog volume (extinction
/// 1 per metre, absorbing alone) 2.7 m deep wholly behind the mirror's
/// plane, which the mirror hides from the viewer; the frame's own medium
/// has no density. The oracle is the contract: the object's image in the
/// mirror holds the radiance it holds without the volume, to within a
/// hundredth (the equality the contract states, with margin for the
/// temporal blend's settling and rounding); through the volume it would
/// hold e^-2.7, under a tenth.
#[test]
fn a_fog_volume_behind_the_mirror_does_not_fog_its_reflection() {
    let Some(gpu) = gpu() else { return };
    let at = Vec3::new(0.6, 0.3, 2.);
    let radiance = |volume: bool| {
        let mut hall = Hall::new(&gpu);
        hall.object(1, at);
        fog(&mut hall.rig, 0.);
        let volumes = if volume {
            vec![sgl_3d::FogVolume {
                center: Vec3::new(0., 0., -DEPTH - 2.15),
                size: Vec3::new(20., 20., 2.7),
                density: 1.,
                albedo: [0.; 3],
                edge_fade: 0.,
                ..Default::default()
            }]
        } else {
            Vec::new()
        };
        hall.rig
            .scene
            .update_fog_volumes(&hall.rig.gpu.device, &hall.rig.gpu.queue, &volumes)
            .expect("the fog volume");
        for frame in 0..12 {
            hall.rig.render(frame);
        }
        radiance(&hall.rig, 0, hall.image_pixel(0, at))[1]
    };
    let without = radiance(false);
    let with = radiance(true);
    assert!(without > 0.3, "the mirror shows the object: {without}");
    assert!(
        (with - without).abs() <= without * 0.01,
        "the reflection holds {with} with a fog volume behind the mirror, {without} without"
    );
}

/// What one view without antialiasing holds, under a procedural sky's
/// aerial perspective with no sun (so the air only absorbs) or under no
/// sky, at the pixel where the eye sees `seen`: the blue channel, which the
/// air dims most, of a white emitter at `emitter` (a 600 m card), with a
/// one-sided perfect mirror 3 km wide facing the eye 2 km ahead where
/// `mirrored`.
fn kilometres(gpu: &Gpu, air: bool, mirrored: bool, emitter: Vec3, seen: Vec3) -> f32 {
    let mut rig = Rig::new(gpu, VIEW, Antialiasing::Off, SKY);
    level(&mut rig, 0, PlanarReflections::Full);
    if air {
        rig.views[0].input.environment = sgl_3d::FrameEnvironment::Sky(sgl_3d::Sky::default());
    }
    let mirror_at = Vec3::new(0., 0., -2000.);
    if mirrored {
        let reflector = rig
            .scene
            .add_planar_reflector(PlanarReflector::new(mirror_at, Vec3::Z))
            .unwrap();
        let (_, material) = rig.add_emissive(Vec2::new(3000., 2000.), [0.; 3], Pose::at(mirror_at));
        mirror_material(&mut rig, material, reflector);
    }
    rig.add_emissive(Vec2::splat(600.), [0.6; 3], Pose::at(emitter));
    for frame in 0..4 {
        rig.render(frame);
    }
    radiance(&rig, 0, pixel(&rig, 0, seen))[2]
}

/// #887, for the air. Plausible defect: the reflected view takes the sky's
/// aerial perspective from the mirrored eye, so the air between the eye
/// and the mirror dims the reflection twice. The fixture: an emitter 1 km
/// behind the viewer, a mirror 2 km ahead, under a procedural sky with no
/// sun, whose air then only absorbs. The oracle is an equivalence: light
/// reflected along a path is dimmed by the air as light seen directly
/// along a path of the same length and height is, so the reflection's
/// air (with over without) matches that of the same emitter seen directly
/// at its mirror image, 5 km out. The threshold is comparative: a third
/// of the air of the 2 km from the eye to the mirror, measured as that of
/// an emitter seen directly at the mirror's distance, which the doubled
/// stretch would add.
#[test]
fn a_reflection_takes_the_air_once_along_its_path() {
    let Some(gpu) = gpu() else { return };
    let behind = Vec3::new(300., 0., 1000.);
    let image = image(Vec3::new(0., 0., -2000.), Vec3::Z, behind);
    let air = |emitter: Vec3, seen: Vec3, mirrored: bool| {
        let with = kilometres(&gpu, true, mirrored, emitter, seen);
        let without = kilometres(&gpu, false, mirrored, emitter, seen);
        assert!(without > 0.3, "the emitter shows: {without}");
        (with / without).ln()
    };
    let reflected = air(behind, image, true);
    let direct = air(image, image, false);
    let to_mirror = air(
        Vec3::new(120., 0., -2000.),
        Vec3::new(120., 0., -2000.),
        false,
    );
    assert!(
        to_mirror < -0.01,
        "the fixture's air dims 2 km: {to_mirror}"
    );
    assert!(
        (reflected - direct).abs() <= to_mirror.abs() / 3.,
        "the reflection takes {reflected} of air, the same path seen directly {direct}; \
         the 2 km to the mirror twice would add {to_mirror}"
    );
}
