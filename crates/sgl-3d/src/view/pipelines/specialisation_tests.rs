//! Which specialisations' pipelines the cache keeps as the scene's lit
//! constants change, and which variants it keeps as features stop.
use super::{GeometryPipelines, LayerConstants, LitConstants};
use crate::Scene;
use glam::Vec3;

fn pipelines(device: &wgpu::Device) -> GeometryPipelines {
    let tier = crate::shading::bind::BindingTier::of(&device.limits());
    GeometryPipelines::new(
        device,
        &crate::shading::bind::lit(device, tier),
        [
            &crate::shading::bind::shadow(device),
            &crate::shading::bind::scene(device),
            &crate::shading::bind::blended(device, tier),
            &crate::shading::bind::shadow_mask(device),
            &crate::shading::bind::screen_indirect(device),
            &crate::shading::bind::shadow_mask_screen_indirect(device),
            &crate::shading::bind::contact_shadow(device),
            &crate::shading::bind::contact_shadow_screen_indirect(device),
            &crate::shading::bind::caster_positions(device),
        ],
        LayerConstants::ALL,
    )
}

fn rectangle() -> crate::Light {
    crate::Light {
        position: Vec3::new(0., 4., 0.),
        shape: crate::LightShape::Rect {
            direction: Vec3::NEG_Y,
            width_axis: Vec3::X,
            width: 2.,
            height: 1.,
        },
        intensity: 10.,
        range: 10.,
        ..Default::default()
    }
}

// Plausible defects: every earlier specialisation's pipelines kept for the
// renderer's lifetime (a full lit set per distinct combination of lit
// constants, diagnostics layers and transmission), or only the current
// one's, so a constant that flips back (a decal expiring, a rectangle
// light switched off) recompiles the whole lit set inside `specialise`.
// The oracle is the cache's keys for each specialisation the scene's
// content selects: A (nothing), B (a rectangle light) and C (a rectangle
// light and a filmed material).
#[test]
fn pipelines_of_the_current_and_previous_specialisation_stay() {
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    let specialisation = |lit| (LayerConstants::ALL, lit, false);
    let a = specialisation(LitConstants::default());
    let b = specialisation(LitConstants {
        rect_lights: true,
        ..LitConstants::default()
    });
    let c = specialisation(LitConstants {
        rect_lights: true,
        films: true,
        ..LitConstants::default()
    });
    let mut scene = Scene::new(&device, &queue);
    let mut pipelines = pipelines(&device);
    let specialise = |pipelines: &mut GeometryPipelines, scene: &Scene| {
        pipelines.specialise(&device, LayerConstants::ALL, scene, (false, false, false));
    };

    specialise(&mut pipelines, &scene);
    let held = pipelines.specialised_pipelines(a);
    assert!(held > 0, "A is prepared");

    // A, B, A: B keeps every pipeline A held, so the return to A is a hit.
    let light = scene.add_light(&device, &queue, rectangle()).unwrap();
    specialise(&mut pipelines, &scene);
    assert_eq!(
        pipelines.specialised_pipelines(a),
        held,
        "B keeps the previous specialisation's pipelines"
    );
    assert!(pipelines.specialised_pipelines(b) > 0, "B is prepared");
    scene.remove_light(light).unwrap();
    specialise(&mut pipelines, &scene);

    // A, B, C: C drops A's and keeps B's.
    scene.add_light(&device, &queue, rectangle()).unwrap();
    specialise(&mut pipelines, &scene);
    scene
        .add_materials(
            &device,
            &queue,
            &[crate::asset::Material {
                iridescence: 1.,
                ..Default::default()
            }],
            &[],
        )
        .unwrap();
    specialise(&mut pipelines, &scene);
    assert!(pipelines.specialised_pipelines(c) > 0, "C is prepared");
    assert!(
        pipelines.specialised_pipelines(b) > 0,
        "C keeps the previous specialisation's pipelines"
    );
    assert_eq!(
        pipelines.specialised_pipelines(a),
        0,
        "C drops the pipelines of a specialisation before the previous"
    );
}

// Plausible defect: the fused and lighting pipelines that read screen-space
// indirect light, and their camera programs, outlive the feature, held
// while it is off and compiled again with each later specialisation. The
// oracle is the cache's pipelines of the specialisation and the default
// programs' modules before the feature ran, against theirs after it ran and
// stopped.
#[test]
fn screen_indirect_pipelines_go_with_the_feature() {
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    let specialisation = (LayerConstants::ALL, LitConstants::default(), false);
    let scene = Scene::new(&device, &queue);
    let mut pipelines = pipelines(&device);
    let mut specialise = |screen_indirect| {
        pipelines.specialise(
            &device,
            LayerConstants::ALL,
            &scene,
            (false, false, screen_indirect),
        );
        (
            pipelines.specialised_pipelines(specialisation),
            pipelines.default_programs(),
        )
    };
    let before = specialise(false);
    let running = specialise(true);
    assert!(
        running.0 > before.0 && running.1 > before.1,
        "the feature's pipelines and programs are prepared: {before:?} then {running:?}"
    );
    assert_eq!(
        specialise(false),
        before,
        "the feature's pipelines and programs outlive it"
    );
}
