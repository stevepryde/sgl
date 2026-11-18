//! Views and the frame contract in the scenario harness (`specs/testing.md`,
//! "SGL3D scenario harness"): several views of one scene in a frame (the
//! architecture's Frame, "Views of one frame", and the register's F-1 and
//! F-12 in `specs/sgl3d-history.md`), judged by equivalence with each view
//! rendered alone and by the script's analytic motion; and what a frame
//! commits and when (Scene content, "Edits and frames", F-13 and M-8),
//! judged by the motion the next frame writes against the script's truth.
#![cfg(not(target_arch = "wasm32"))]
mod scenario;

use scenario::geometry::{Motion, Pose, View};
use scenario::image::Image;
use scenario::rig::{Gpu, Rig, Shape, gpu, ray_gpu};
use scenario::temporal::{
    DEPTH, Failures, STILL_DEPTH, STILL_UV, Scenario, VIEW, background, card, device,
    motion_follows_the_script, motion_follows_the_script_in, still,
};
use sgl_3d::asset::{Asset, CpuMesh, Material, Vertex};
use sgl_3d::diagnostics::{self, BuildStep, DiagnosticTarget};
use sgl_3d::glam::{Vec2, Vec3};
use sgl_3d::settings::{self, Antialiasing, Settings};
use sgl_3d::{
    DirectionalLight, DirectionalShadow, DynamicGiVolume, Fog, HemisphereLight, InstanceId,
    InstanceState, Light, MaterialShader, ShaderSource, SurfaceMaterial,
};

/// Where `image` and `oracle` first differ, texel by texel over every
/// channel, and by how much; none where they are identical.
fn apart(image: &Image, oracle: &Image) -> Option<([u32; 2], f32)> {
    assert_eq!(image.size, oracle.size, "frames of one view's size");
    image
        .texels
        .iter()
        .zip(&oracle.texels)
        .enumerate()
        .find(|(_, (a, b))| {
            a.iter()
                .zip(b.iter())
                .any(|(a, b)| a.to_bits() != b.to_bits())
        })
        .map(|(i, (a, b))| {
            let pixel = [i as u32 % image.size[0], i as u32 / image.size[0]];
            let most = a
                .iter()
                .zip(b.iter())
                .map(|(a, b)| (a - b).abs())
                .fold(
                    0f32,
                    |m, d| if d.is_nan() { f32::INFINITY } else { m.max(d) },
                );
            (pixel, most)
        })
}

/// View A's camera in the views cases: panning across the stage.
fn panning(frame: u32) -> Pose {
    VIEW.pan(0.37, DEPTH).pose(Pose::at(Vec3::ZERO), frame)
}

/// View B's camera: turning from a little right of and above A's start.
fn turning(frame: u32) -> Pose {
    Motion::yaw(0.004).pose(Pose::at(Vec3::new(0.2, 0.1, 0.)), frame)
}

/// Held still where `turning` is at `frame`, 0.3 m above, until then, and
/// on `turning` from it: view B with another past.
fn turning_after(frame: u32) -> impl Fn(u32) -> Pose {
    move |f| {
        if f < frame {
            Pose::at(turning(frame).position + Vec3::Y * 0.3)
        } else {
            turning(f)
        }
    }
}

/// The background and case 1's card moving across it.
fn across(frame: u32) -> Vec<Pose> {
    vec![
        background().1,
        Motion::translate(Vec3::new(0.02, 0., 0.))
            .pose(Pose::at(Vec3::new(-0.6, 0.2, -DEPTH)), frame),
    ]
}

/// The views cases' stage seen from `camera`: the background and a card
/// moving across it.
fn stage(camera: &dyn Fn(u32) -> Pose) -> Scenario<'_> {
    Scenario {
        shapes: vec![background().0, card()],
        camera,
        script: &across,
    }
}

/// A small card at the lower right that a morph target moves 0.2 m right
/// over `MORPH_FRAMES`: work for the deform pass every frame.
const MORPH_FRAMES: u32 = 8;

fn add_morph(rig: &mut Rig) -> InstanceId {
    rig.add_morphing(
        &Shape::card(Vec2::splat(0.3), 0., 0.3),
        Pose::at(Vec3::new(0.5, -0.45, -DEPTH)),
        Vec3::new(0.2, 0., 0.),
    )
}

fn morph(rig: &mut Rig, id: InstanceId, frame: u32) {
    rig.scene
        .set_instance_deformation(
            &rig.gpu.queue,
            id,
            &[],
            &[frame as f32 / MORPH_FRAMES as f32],
        )
        .expect("the morph weight");
}

/// A rig of `scenario` through `lens` with `method` and the morphing card.
fn rig(gpu: &Gpu, scenario: &Scenario, lens: View, method: Antialiasing) -> (Rig, InstanceId) {
    let mut rig = scenario.rig(gpu, lens, method);
    let morphing = add_morph(&mut rig);
    (rig, morphing)
}

/// Each frame of a view rendered alone: `scenario` in a rig of its own.
fn solo(gpu: &Gpu, scenario: &Scenario, method: Antialiasing, frames: u32) -> Vec<Image> {
    let (mut rig, morphing) = rig(gpu, scenario, VIEW, method);
    (0..frames)
        .map(|frame| {
            scenario.pose(&mut rig, frame);
            morph(&mut rig, morphing, frame);
            rig.render(frame);
            rig.read(DiagnosticTarget::Antialiased)
        })
        .collect()
}

/// The frames the views cases render, and the one whose motion they judge.
const FRAMES: u32 = 6;
const MOTION_FRAME: u32 = 3;

/// How the views of a frame are listed: A is view 0, B view 1.
#[derive(Clone, Copy, Debug)]
enum Order {
    Ab,
    Ba,
    /// AB in even frames, BA in odd ones.
    Alternating,
}

impl Order {
    fn of(self, frame: u32) -> [usize; 2] {
        match self {
            Order::Ab => [0, 1],
            Order::Ba => [1, 0],
            Order::Alternating if frame.is_multiple_of(2) => [0, 1],
            Order::Alternating => [1, 0],
        }
    }
}

/// F-12, D-57 (guard 1). Plausible defects: a view reprojects through
/// another view's camera (one camera history for the renderer); a second
/// view sees zero object motion (the scene's motion committed by a view
/// rather than by the frame); the scene's work skipped, repeated or run
/// after the first view drew (the deform pass in the wrong view's order);
/// or anything a view writes leaking into another, so that the order of
/// the views matters. The fixture: two views of one stage, A panning and B
/// turning, a card moving across the background and a morph target moving
/// a small card, listed AB, BA and alternating. The oracles: equivalence,
/// every frame's `Antialiased` target of each view equal, texel for texel,
/// to the same view rendered alone in a rig of its own; and analytic, each
/// view's motion at frame 3 is the script's through that view's cameras
/// (case 1's `motion_follows_the_script`).
fn views_match_their_solo_runs(method: Antialiasing) {
    let Some(gpu) = device(method) else { return };
    let turning_camera = turning;
    let [a, b] = [&panning as &dyn Fn(u32) -> Pose, &turning_camera].map(stage);
    let alone = [&a, &b].map(|scenario| solo(&gpu, scenario, method, FRAMES));
    let mut failures = Failures::default();
    for order in [Order::Ab, Order::Ba, Order::Alternating] {
        let (mut rig, morphing) = rig(&gpu, &a, VIEW, method);
        rig.add_view(VIEW, method);
        for frame in 0..FRAMES {
            a.pose(&mut rig, frame);
            rig.look_view(1, turning(frame));
            morph(&mut rig, morphing, frame);
            rig.render_order(frame, &order.of(frame));
            for (view, name) in [(0, "A"), (1, "B")] {
                let image = rig.read_view(view, DiagnosticTarget::Antialiased);
                let differs = apart(&image, &alone[view][frame as usize]);
                failures.check(differs.is_none(), || {
                    let (pixel, by) = differs.unwrap();
                    format!(
                        "{method:?} {order:?}: view {name}'s frame {frame} differs from it alone at {pixel:?} by {by}"
                    )
                });
            }
            if frame == MOTION_FRAME {
                for (view, scenario, name) in [(0, &a, "A"), (1, &b, "B")] {
                    let label = format!("{method:?} {order:?} view {name}");
                    match motion_follows_the_script_in(&rig, view, scenario, frame, &label) {
                        Ok(worst) => eprintln!(
                            "{label}: largest motion error {} px, {} m",
                            worst[0] * VIEW.size[0] as f32,
                            worst[1]
                        ),
                        Err(failure) => failures.check(false, || failure),
                    }
                }
            }
        }
    }
    failures.assert();
}

mod views_match_their_solo_runs {
    use sgl_3d::settings::Antialiasing::{Fsr2, Fsr3, Taa};
    crate::scenario::temporal::case_tests!(views_match_their_solo_runs; taa: (Taa), fsr2: (Fsr2), fsr3: (Fsr3));
}

/// F-12 (guard 2). Plausible defects: creating, resizing or dropping a view
/// disturbs another's history (state held by the renderer that a new view
/// replaces, a resize that restarts every view, a dropped view's targets
/// still bound). The fixture: view A of the stage panning throughout; view
/// B created at frame 2, its targets resized at frame 4 and dropped at
/// frame 6. The oracle is equivalence: each of A's frames equals, texel for
/// texel, A rendered alone.
fn a_view_keeps_its_history_while_another_appears_resizes_and_leaves(method: Antialiasing) {
    let Some(gpu) = device(method) else { return };
    let a = stage(&panning);
    let frames = 9;
    let alone = solo(&gpu, &a, method, frames);
    let (mut rig, morphing) = rig(&gpu, &a, VIEW, method);
    let resized = View {
        size: [160, 90],
        ..VIEW
    };
    let mut failures = Failures::default();
    for frame in 0..frames {
        match frame {
            2 => {
                rig.add_view(VIEW, method);
            }
            4 => rig.views[1].lens = resized,
            6 => rig.remove_view(1),
            _ => {}
        }
        a.pose(&mut rig, frame);
        if rig.views.len() > 1 {
            rig.look_view(1, turning(frame));
        }
        morph(&mut rig, morphing, frame);
        rig.render(frame);
        let differs = apart(
            &rig.read(DiagnosticTarget::Antialiased),
            &alone[frame as usize],
        );
        failures.check(differs.is_none(), || {
            let (pixel, by) = differs.unwrap();
            format!("{method:?}: A's frame {frame} differs from it alone at {pixel:?} by {by}")
        });
    }
    failures.assert();
}

mod a_view_keeps_its_history_while_another_appears_resizes_and_leaves {
    use sgl_3d::settings::Antialiasing::{Fsr2, Fsr3, Taa};
    crate::scenario::temporal::case_tests!(
        a_view_keeps_its_history_while_another_appears_resizes_and_leaves;
        taa: (Taa), fsr2: (Fsr2), fsr3: (Fsr3)
    );
}

/// F-1, F-12 (guard 3). Plausible defect: a view left out of a finished
/// frame of its scene continues its history over the gap, mixing a
/// two-frame camera delta with one frame of the scene's motion. The
/// fixture: views A and B of the stage, B left out of frame 4. The
/// oracles: the restart suite's equivalence (case 10), B's frame 5 renders
/// exactly alike in a run where B turned before and one where it was held
/// still elsewhere until then, which holds only when it restarted; and A,
/// rendered in every frame, equals A alone throughout.
fn a_view_that_skips_a_frame_restarts(method: Antialiasing) {
    let Some(gpu) = device(method) else { return };
    let a = stage(&panning);
    let skipped = 4;
    let frames = skipped + 2;
    let alone = solo(&gpu, &a, method, frames);
    let held = turning_after(skipped + 1);
    let mut failures = Failures::default();
    let mut restarted = Vec::new();
    for past in [&turning as &dyn Fn(u32) -> Pose, &held] {
        let (mut rig, morphing) = rig(&gpu, &a, VIEW, method);
        rig.add_view(VIEW, method);
        for frame in 0..frames {
            a.pose(&mut rig, frame);
            rig.look_view(1, past(frame));
            morph(&mut rig, morphing, frame);
            if frame == skipped {
                rig.render_order(frame, &[0]);
            } else {
                rig.render(frame);
            }
            let differs = apart(
                &rig.read(DiagnosticTarget::Antialiased),
                &alone[frame as usize],
            );
            failures.check(differs.is_none(), || {
                let (pixel, by) = differs.unwrap();
                format!("{method:?}: A's frame {frame} differs from it alone at {pixel:?} by {by}")
            });
        }
        restarted.push(rig.read_view(1, DiagnosticTarget::Antialiased));
    }
    let differs = apart(&restarted[0], &restarted[1]);
    eprintln!("{method:?}: B's frame after the gap, runs with different pasts: {differs:?}");
    failures.check(differs.is_none(), || {
        let (pixel, by) = differs.unwrap();
        format!(
            "{method:?}: B's frame after the frame that left it out depends on its past, at {pixel:?} by {by}"
        )
    });
    failures.assert();
}

mod a_view_that_skips_a_frame_restarts {
    use sgl_3d::settings::Antialiasing::{Fsr2, Fsr3, Taa};
    crate::scenario::temporal::case_tests!(a_view_that_skips_a_frame_restarts; taa: (Taa), fsr2: (Fsr2), fsr3: (Fsr3));
}

/// F-13. Plausible defect: `finish_frame` commits the scene as it is when it
/// runs, not as the frame drew it, so a loop that submits a frame, poses the
/// scene for the next and then finishes the frame it submitted makes every
/// moving instance's next frame measure its motion from its new pose: zero
/// motion. The fixture: case 1's card moving across before the background
/// while the camera pans, each frame's poses set after the previous frame
/// is submitted and before it is finished (`Rig::render_then`). The oracle
/// is case 1's, analytic (`motion_follows_the_script`): the last frame's
/// motion is the script's from the previous frame's poses to its own.
#[test]
fn edits_after_submission_belong_to_the_next_frame() {
    let Some(gpu) = gpu() else { return };
    let (back, back_pose) = background();
    let card_start = Pose::at(Vec3::new(-0.6, 0.2, -DEPTH));
    let across = Motion::translate(Vec3::new(0.02, 0., 0.));
    let pan = VIEW.pan(0.37, DEPTH);
    let camera = |f: u32| pan.pose(Pose::at(Vec3::ZERO), f);
    let script = |f: u32| vec![back_pose, across.pose(card_start, f)];
    let scenario = Scenario {
        shapes: vec![back, card()],
        camera: &camera,
        script: &script,
    };
    let mut rig = scenario.rig(&gpu, VIEW, Antialiasing::Taa);
    let frame = 3;
    scenario.pose(&mut rig, 0);
    for f in 0..frame {
        rig.render_then(f, |rig| scenario.pose(rig, f + 1));
    }
    rig.render(frame);
    let worst = motion_follows_the_script(&rig, &scenario, frame, "posed after submission")
        .unwrap_or_else(|failure| panic!("{failure}"));
    eprintln!(
        "posed after submission: largest error {} px, {} m",
        worst[0] * VIEW.size[0] as f32,
        worst[1]
    );
}

/// A game's shader whose vertex function moves every vertex by its
/// parameters' `offset` metres, whatever the time: its motion changes only
/// when the game sets other parameters.
const OFFSET_SHADER: &str = r#"
struct ShaderParams {
 offset:vec3<f32>,
 unused:f32,
}
fn material_vertex(v:MaterialVertex,ctx:VertexContext,params:ShaderParams)->MaterialVertex {
 var out=v;
 out.position+=params.offset;
 return out;
}
fn material_surface(s:MaterialSurface,ctx:SurfaceContext,params:ShaderParams)->MaterialSurface {
 return s;
}
"#;

/// M-8 (B-8 of the 2026-11-18 history audit). Plausible defect: a material
/// that gains a shader gets a parameter block whose previous copy is zero,
/// so a frame that draws it with parameters set before its first frame
/// measures the vertex function's motion from zero parameters. The
/// fixture: a still card before a still camera, drawn plainly for a few
/// frames, that then gains `OFFSET_SHADER` with a 0.1 m offset (about
/// 2.5 px at `DEPTH`), set before the frame that first draws it. The oracle
/// is analytic: nothing moved between the two frames' evaluations, so the
/// card's interior writes zero motion within case 1's `STILL_UV` and
/// `STILL_DEPTH`, in the frame that gains the shader and the one after.
#[test]
fn a_material_that_gains_a_shader_writes_no_parameter_motion_in_its_first_frame() {
    let Some(gpu) = gpu() else { return };
    let mut rig = Rig::new(&gpu, VIEW, Antialiasing::Taa, scenario::temporal::SKY);
    let (id, materials) = rig.add_editable(&card(), Pose::at(Vec3::new(0., 0., -DEPTH)), 0);
    rig.look(still(0));
    let gained = 3;
    for frame in 0..gained {
        rig.render(frame);
    }
    let shader = rig
        .scene
        .add_shader(ShaderSource {
            wgsl: OFFSET_SHADER.into(),
            label: "offset".into(),
        })
        .expect("the offset shader");
    let offset = 0.1f32;
    for &material in &materials {
        let values = rig.scene.material(material).expect("a fixture material");
        let shader = Some(MaterialShader {
            shader,
            displacement_bound: offset,
        });
        rig.scene
            .set_material(
                &rig.gpu.queue,
                material,
                SurfaceMaterial { shader, ..values },
            )
            .expect("a shaded card");
        rig.scene
            .set_shader_parameters(
                &rig.gpu.queue,
                material,
                bytemuck::cast_slice(&[offset, 0., 0., 0.]),
            )
            .expect("the offset");
    }
    for frame in gained..gained + 2 {
        rig.render(frame);
        let motion = rig.read(DiagnosticTarget::Motion);
        let interior = rig
            .read(DiagnosticTarget::SourceId)
            .region_of(diagnostics::source_id(id))
            .eroded(1);
        assert!(
            !interior.is_empty(),
            "frame {frame}: the card drew no interior"
        );
        let mut largest = [0f32; 2];
        for pixel in interior.iter() {
            let [r, g, b, _] = motion.at(pixel);
            largest = [
                largest[0].max(r.abs().max(g.abs())),
                largest[1].max(b.abs()),
            ];
        }
        eprintln!(
            "frame {frame}: largest motion {} px, depth motion {} m",
            largest[0] * VIEW.size[0] as f32,
            largest[1]
        );
        assert!(
            largest[0] <= STILL_UV && largest[1] <= STILL_DEPTH,
            "frame {frame}, {} after the card gained its shader: its still interior wrote {} px and {} m of motion",
            frame - gained,
            largest[0] * VIEW.size[0] as f32,
            largest[1]
        );
    }
}

/// A lit quad of `base` colour with `metallic` and `roughness`, facing
/// `normal`, from `corners`: content the lit stages (reflections, ambient
/// occlusion, fog, dynamic GI) change.
fn lit(corners: [Vec3; 4], normal: Vec3, base: f32, metallic: f32, roughness: f32) -> Asset {
    Asset {
        meshes: vec![CpuMesh {
            vertices: corners
                .map(|position| Vertex {
                    position: position.to_array(),
                    normal: normal.to_array(),
                    uv: [0.; 2],
                    color: [1.; 4],
                    tangent: [1., 0., 0., 1.],
                    lightmap_bounds: [0., 0., 1., 1.],
                    lightmap_uv: [0.; 2],
                })
                .to_vec(),
            indices: vec![0, 1, 2, 0, 2, 3],
            material: 0,
            deformation: Default::default(),
        }],
        materials: vec![Material {
            name: "views fixture".into(),
            base: [base, base * 0.8, base * 0.6, 1.],
            metallic,
            roughness,
            double_sided: true,
            ..Material::default()
        }],
        images: Vec::new(),
        rig: Default::default(),
        ignored: Vec::new(),
    }
}

/// A lit stage: a glossy floor below the cameras, a wall `DEPTH` ahead and a
/// lit card moving across before it, under a directional light and a
/// hemisphere fill. Returns the card.
fn light_the_stage(rig: &mut Rig) -> InstanceId {
    let floor =
        [(-4., -0.5), (4., -0.5), (4., -9.), (-4., -9.)].map(|(x, z)| Vec3::new(x, -0.6, z));
    rig.add_model(lit(floor, Vec3::Y, 0.6, 1., 0.12), Pose::at(Vec3::ZERO));
    let wall = [(-4., -0.6), (4., -0.6), (4., 2.5), (-4., 2.5)]
        .map(|(x, y)| Vec3::new(x, y, -DEPTH - 0.5));
    rig.add_model(lit(wall, Vec3::Z, 0.5, 0., 0.7), Pose::at(Vec3::ZERO));
    let card =
        [(-0.3, -0.3), (0.3, -0.3), (0.3, 0.3), (-0.3, 0.3)].map(|(x, y)| Vec3::new(x, y, 0.));
    let id = rig.add_model(lit(card, Vec3::Z, 0.9, 0., 0.4), lit_card(0));
    for view in &mut rig.views {
        view.input.directional_lights[0] = Some(DirectionalLight {
            direction: Vec3::new(-0.3, -0.8, -0.5).normalize(),
            illuminance: 3.,
            ..Default::default()
        });
        view.input.hemisphere_light = HemisphereLight {
            sky_color: [0.4, 0.45, 0.5],
            ground_color: [0.2; 3],
            intensity: 0.5,
        };
    }
    id
}

/// The lit card's pose at `frame`: moving across, a little above the floor.
fn lit_card(frame: u32) -> Pose {
    Motion::translate(Vec3::new(0.03, 0., 0.))
        .pose(Pose::at(Vec3::new(-0.5, -0.1, -DEPTH + 1.)), frame)
}

fn place_lit_card(rig: &mut Rig, id: InstanceId, frame: u32) {
    let state = InstanceState {
        pose: lit_card(frame).matrix(),
        ..*rig.scene.instance(id).expect("the lit card")
    };
    rig.scene
        .set_instance(&rig.gpu.queue, id, state)
        .expect("the lit card's pose");
}

/// A dynamic GI volume of 18 probes over the lit stage. Few enough that
/// the frame's ray budget refuses none: a budget that refuses some admits
/// probes first-come through atomics (`allocate.wgsl`, as Wicked's surfel
/// budget), which two identical runs need not order alike, so their frames
/// would differ whatever the views.
fn add_volume(rig: &mut Rig) {
    rig.scene
        .set_dynamic_gi_volume(
            &rig.gpu.device,
            Some(DynamicGiVolume {
                origin: Vec3::new(-3., -0.4, -7.),
                spacing: Vec3::splat(3.),
                probes: [3, 2, 3],
            }),
        )
        .expect("a dynamic GI volume");
}

/// A feature guard 7 runs on the secondary view.
#[derive(Clone, Copy, Debug)]
enum Feature {
    Crystal,
    Velvet,
    WorldRays,
    AmbientOcclusion,
    Fog,
    DynamicGi,
}

impl Feature {
    /// `settings` with the feature on.
    fn on(self, settings: &mut Settings) {
        match self {
            Feature::Crystal | Feature::Velvet => {
                settings.screen_space_reflections = settings::ScreenSpaceReflections::Full;
                settings.reflection_method = match self {
                    Feature::Crystal => settings::ReflectionMethod::Crystal,
                    _ => settings::ReflectionMethod::Velvet,
                };
            }
            Feature::WorldRays => {
                settings.screen_space_reflections = settings::ScreenSpaceReflections::Full;
                settings.world_space_reflections = settings::WorldSpaceReflections::Moving;
            }
            Feature::AmbientOcclusion => {
                settings.ambient_occlusion = settings::AmbientOcclusionQuality::High;
            }
            Feature::Fog => settings.atmosphere = true,
            Feature::DynamicGi => settings.dynamic_gi = settings::DynamicGiQuality::Low,
        }
    }
}

/// The frames guard 7 renders.
const FEATURE_FRAMES: u32 = 5;

/// B's composite frames, with `feature` on, rendered as the secondary view
/// after A or, with `with_a` false, alone. Dynamic GI is the scene's, which
/// the primary view steers (its frustum, eye, lights and time, D-57): there
/// both views look through one camera, B at half A's size, and A's
/// settings turn dynamic GI on, so A steers it as B alone does.
fn secondary_composites(gpu: &Gpu, feature: Feature, with_a: bool) -> Vec<Image> {
    let gi = matches!(feature, Feature::DynamicGi);
    let small = View {
        size: [VIEW.size[0] / 2, VIEW.size[1] / 2],
        ..VIEW
    };
    let b_lens = if gi { small } else { VIEW };
    let b_camera = |frame: u32| if gi { panning(frame) } else { turning(frame) };
    let mut rig = Rig::new(
        gpu,
        if with_a { VIEW } else { b_lens },
        Antialiasing::Taa,
        0.05,
    );
    let b = if with_a {
        rig.add_view(b_lens, Antialiasing::Taa)
    } else {
        0
    };
    let card = light_the_stage(&mut rig);
    if gi {
        add_volume(&mut rig);
        for view in &mut rig.views {
            Feature::DynamicGi.on(&mut view.settings);
        }
    }
    feature.on(&mut rig.views[b].settings);
    if matches!(feature, Feature::Fog) {
        rig.views[b].input.atmosphere = true;
        rig.views[b].input.fog = Fog {
            density: 0.08,
            length: 20.,
            ..Default::default()
        };
    }
    (0..FEATURE_FRAMES)
        .map(|frame| {
            if with_a {
                rig.look(panning(frame));
            }
            rig.look_view(b, b_camera(frame));
            place_lit_card(&mut rig, card, frame);
            rig.render(frame);
            rig.read_view(b, DiagnosticTarget::Composite)
        })
        .collect()
}

/// D-57 (guard 7). Plausible defects: a feature's state or targets shared
/// between views, or written by the primary view's frame where a secondary
/// reads it (a reflection or occlusion history, the fog's froxels, the
/// world rays' completion, the dynamic GI probes bound by one view only, or
/// updated more than once a frame). The fixture: a lit stage, view B with
/// `feature` on listed after a view A that pans. The oracle is equivalence:
/// every frame's `Composite` of B equals, texel for texel, B rendered alone.
fn views_match_their_solo_runs_with(feature: Feature) {
    let Some(gpu) = gpu() else { return };
    let together = secondary_composites(&gpu, feature, true);
    let alone = secondary_composites(&gpu, feature, false);
    let mut failures = Failures::default();
    for (frame, (with_a, alone)) in together.iter().zip(&alone).enumerate() {
        let differs = apart(with_a, alone);
        failures.check(differs.is_none(), || {
            let (pixel, by) = differs.unwrap();
            format!("{feature:?}: B's composite {frame} differs from B alone at {pixel:?} by {by}")
        });
    }
    failures.assert();
}

mod views_match_their_solo_runs_with {
    use super::Feature::*;
    crate::scenario::temporal::case_tests!(
        views_match_their_solo_runs_with;
        crystal: (Crystal),
        velvet: (Velvet),
        world_rays: (WorldRays),
        ambient_occlusion: (AmbientOcclusion),
        fog: (Fog),
        dynamic_gi: (DynamicGi),
    );
}

/// What a run of `frames` frames counted and reported, from the first.
struct Shared {
    counted: diagnostics::Counters,
    reports: Vec<diagnostics::DynamicGiReport>,
    ray_queries: bool,
}

/// `frames` frames of the lit stage with dynamic GI and the morphing card,
/// seen by view A and, with `two`, by view B too, with `hardware` ray
/// tracing (where the device has it).
fn shared_work(gpu: &Gpu, two: bool, frames: u32, hardware: bool) -> Shared {
    let mut rig = Rig::new(gpu, VIEW, Antialiasing::Taa, 0.05);
    if two {
        rig.add_view(VIEW, Antialiasing::Taa);
    }
    let card = light_the_stage(&mut rig);
    let morphing = add_morph(&mut rig);
    add_volume(&mut rig);
    for view in &mut rig.views {
        view.settings.dynamic_gi = settings::DynamicGiQuality::Low;
        view.settings.hardware_ray_tracing = hardware;
        view.settings.diagnostics.dynamic_gi = true;
    }
    let before = diagnostics::counters();
    let mut reports = Vec::new();
    for frame in 0..frames {
        rig.look(panning(frame));
        if two {
            rig.look_view(1, turning(frame));
        }
        place_lit_card(&mut rig, card, frame);
        morph(&mut rig, morphing, frame);
        rig.render(frame);
        gpu.device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        reports.extend(rig.renderer.take_dynamic_gi_reports(&gpu.device));
    }
    let counted = diagnostics::counters().since(&before);
    let ray_queries = rig
        .renderer
        .ray_tracing_in_effect(&rig.views[0].settings, &rig.scene);
    Shared {
        counted,
        reports,
        ray_queries,
    }
}

/// D-57 (guard 5). Plausible defect: the scene's frame work runs per view
/// rather than once a frame. Both views have the same settings, so this
/// does not judge work only a secondary view asks for (guard 5b does). The
/// fixture: the lit stage with a dynamic GI volume
/// and a morphing card, three frames seen by two views and by one, on the
/// portable ray path and, where the device has ray queries, the hardware
/// one. The oracles are the contract's: the moving instance BVH is built
/// once a frame on the portable path, and the TLAS on the hardware one; the
/// deform pass writes as many deformations as the single-view run's; and
/// the dynamic GI stage reports one frame per frame, tracing as many rays
/// as in the single-view run (reported on the portable path alone,
/// `Diagnostics::dynamic_gi`).
#[test]
fn shared_work_runs_once_a_frame() {
    let Some(gpu) = ray_gpu() else { return };
    let frames = 3;
    let mut failures = Failures::default();
    let [one, two] = [false, true].map(|two| shared_work(&gpu, two, frames, false));
    let bvhs = two.counted.step(BuildStep::MovingInstanceBvh).calls;
    eprintln!(
        "portable, two views: {bvhs} moving instance BVHs, {} deformations ({} alone), {} dynamic GI reports",
        two.counted.deformations,
        one.counted.deformations,
        two.reports.len()
    );
    failures.check(bvhs == u64::from(frames), || {
        format!("the moving instance BVH was built {bvhs} times in {frames} frames")
    });
    failures.check(
        two.counted.deformations == one.counted.deformations && one.counted.deformations > 0,
        || {
            format!(
                "two views dispatched {} deformations where one did {}",
                two.counted.deformations, one.counted.deformations
            )
        },
    );
    failures.check(two.reports.len() == frames as usize, || {
        format!(
            "dynamic GI reported {} frames in {frames}",
            two.reports.len()
        )
    });
    let rays = |shared: &Shared| -> Vec<u32> { shared.reports.iter().map(|r| r.rays).collect() };
    failures.check(rays(&two) == rays(&one), || {
        format!(
            "dynamic GI traced {:?} rays a frame with two views, {:?} with one",
            rays(&two),
            rays(&one)
        )
    });
    let [one, two] = [false, true].map(|two| shared_work(&gpu, two, frames, true));
    if two.ray_queries {
        eprintln!(
            "hardware, two views: {} TLASes, {} deformations ({} alone)",
            two.counted.tlas_builds, two.counted.deformations, one.counted.deformations
        );
        failures.check(two.counted.tlas_builds == u64::from(frames), || {
            format!(
                "the TLAS was built {} times in {frames} frames",
                two.counted.tlas_builds
            )
        });
        failures.check(two.counted.deformations == one.counted.deformations, || {
            format!(
                "two views dispatched {} deformations where one did {} (hardware path)",
                two.counted.deformations, one.counted.deformations
            )
        });
    } else {
        eprintln!("unsupported: the device has no ray queries; TLAS builds not judged");
    }
    failures.assert();
}

/// What guard 5b turns on in view B alone.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Traced {
    /// World-space reflection rays.
    WorldRays,
    /// Ray-traced shadows, which trace in hardware only.
    Shadows,
}

/// B's composites over three frames of the lit stage with `traced` on in B
/// alone, with `hardware` ray tracing, B listed after a view A that traces
/// nothing (or, with `with_a` false, alone), and the frames' counters.
fn traced_by_b(
    gpu: &Gpu,
    traced: Traced,
    hardware: bool,
    with_a: bool,
) -> (Vec<Image>, diagnostics::Counters) {
    let mut rig = Rig::new(gpu, VIEW, Antialiasing::Taa, 0.05);
    let b = if with_a {
        rig.add_view(VIEW, Antialiasing::Taa)
    } else {
        0
    };
    let card = light_the_stage(&mut rig);
    for view in &mut rig.views {
        view.settings.hardware_ray_tracing = hardware;
    }
    let settings = &mut rig.views[b].settings;
    match traced {
        Traced::WorldRays => Feature::WorldRays.on(settings),
        Traced::Shadows => {
            // Its slot 0 holds the directional light, which casts a shadow.
            settings.ray_traced_shadows = true;
            for view in &mut rig.views {
                if let Some(light) = &mut view.input.directional_lights[0] {
                    light.shadow = Some(DirectionalShadow {
                        distance: 20.,
                        cascades: 2,
                    });
                }
            }
        }
    }
    let before = diagnostics::counters();
    let images = (0..3)
        .map(|frame| {
            if with_a {
                rig.look(panning(frame));
            }
            rig.look_view(b, turning(frame));
            place_lit_card(&mut rig, card, frame);
            rig.render(frame);
            rig.read_view(b, DiagnosticTarget::Composite)
        })
        .collect();
    (images, diagnostics::counters().since(&before))
}

/// D-57 (guard 5b). Plausible defects: the scene's frame work is steered by
/// the primary view alone where the contract takes the union over the
/// frame's views, so a secondary view that traces while the primary does
/// not reads acceleration structures that were not built that frame, or
/// draws ray-traced shadows with pipelines specialised without the shadow
/// mask. The fixture: the lit stage with its card moving, view A tracing
/// nothing and view B turning with world-space rays (on the portable path,
/// and on the hardware path where the device has ray queries) or with
/// ray-traced shadows (hardware only). The oracles: the contract's, the
/// moving instance BVH built once a frame on the portable path and the TLAS
/// on the hardware one; and equivalence, each of B's composites equal,
/// texel for texel, to B rendered alone.
fn scene_work_follows_any_view_that_traces(traced: Traced, hardware: bool) {
    let Some(gpu) = ray_gpu() else { return };
    if hardware {
        let rig = Rig::new(&gpu, VIEW, Antialiasing::Taa, 0.05);
        let mut settings = rig.views[0].settings;
        settings.hardware_ray_tracing = true;
        if !rig.renderer.ray_tracing_in_effect(&settings, &rig.scene) {
            eprintln!(
                "unsupported: the device has no ray queries; the hardware path is not judged"
            );
            return;
        }
    }
    let mut failures = Failures::default();
    let (together, counted) = traced_by_b(&gpu, traced, hardware, true);
    let (alone, _) = traced_by_b(&gpu, traced, hardware, false);
    let builds = if hardware {
        counted.tlas_builds
    } else {
        counted.step(BuildStep::MovingInstanceBvh).calls
    };
    eprintln!("{traced:?}, hardware {hardware}: {builds} builds in 3 frames");
    failures.check(builds == 3, || {
        format!("{traced:?}, hardware {hardware}: built {builds} times in 3 frames")
    });
    for (frame, (together, alone)) in together.iter().zip(&alone).enumerate() {
        let differs = apart(together, alone);
        failures.check(differs.is_none(), || {
            let (pixel, by) = differs.unwrap();
            format!(
                "{traced:?}, hardware {hardware}: B's composite {frame} differs from B alone at {pixel:?} by {by}"
            )
        });
    }
    failures.assert();
}

mod scene_work_follows_any_view_that_traces {
    use super::Traced::*;
    crate::scenario::temporal::case_tests!(
        scene_work_follows_any_view_that_traces;
        portable_world_rays: (WorldRays, false),
        hardware_world_rays: (WorldRays, true),
        hardware_shadows: (Shadows, true),
    );
}

/// D-57, D9 (guard 6). Plausible defect: the local-light shadow atlas, its
/// static layers or its cache are shared between views, so a light's place
/// follows whichever view placed it last and the faces another view drew
/// are redrawn. Two nearby cameras place one light alike, so the fixture
/// gives the views different placements: view A at the origin and view B
/// 10 m behind it, both still, looking at the stage; light 1 just before
/// the card, whose screen coverage wants the atlas's largest slot from A
/// and a quarter of it from B; light 2 behind A's camera, which only B's
/// view reaches. Godot's atlas keeps a light's slots for 30 of its plans
/// before moving it to another size (`atlas::REALLOCATION_FRAMES`), so a
/// shared atlas, planned twice a frame, would move light 1 to B's size by
/// frame 15 and redraw its faces; the run is 18 frames. The oracles are the
/// rules: each view shadows the lights whose range reaches its frustum (A
/// one, B two, by construction), and a frame in which nothing a face shows
/// changed draws nothing (the architecture's local-light shadows), so from
/// the second frame on neither view draws a face or a static layer.
#[test]
fn each_view_draws_only_its_own_shadow_faces() {
    let Some(gpu) = gpu() else { return };
    let at_rest = |_| across(0);
    let a = Scenario {
        script: &at_rest,
        ..stage(&still)
    };
    let mut rig = a.rig(&gpu, VIEW, Antialiasing::Taa);
    rig.add_view(VIEW, Antialiasing::Taa);
    rig.look_view(1, Pose::at(Vec3::new(0.3, 0.2, 10.)));
    for (position, range) in [
        (Vec3::new(0.2, 0.3, -DEPTH + 0.8), 1.2),
        (Vec3::new(0., 0., 6.), 1.),
    ] {
        rig.scene
            .add_light(
                &gpu.device,
                &gpu.queue,
                Light {
                    position,
                    range,
                    casts_shadow: true,
                    ..Default::default()
                },
            )
            .expect("a shadowed light");
    }
    let mut failures = Failures::default();
    for frame in 0..18 {
        a.pose(&mut rig, frame);
        rig.render(frame);
        for (view, name, lights) in [(0, "A", 1), (1, "B", 2)] {
            let stats = rig.views[view].view.local_shadow_stats();
            if frame < 2 {
                eprintln!("frame {frame}, view {name}: {stats:?}");
            }
            failures.check(stats.shadowed == lights && stats.unshadowed == 0, || {
                format!("frame {frame}: view {name} shadowed {stats:?}, not its {lights} lights")
            });
            if frame > 0 {
                failures.check(stats.faces_drawn == 0 && stats.layers_drawn == 0, || {
                    format!("frame {frame}: view {name} drew shadow faces at rest: {stats:?}")
                });
            }
        }
    }
    failures.assert();
}
