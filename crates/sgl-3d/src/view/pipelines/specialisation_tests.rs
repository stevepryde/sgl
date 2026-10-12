//! Which specialisations' pipelines the cache keeps as the scene's lit
//! constants change.
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
        pipelines.specialise(&device, LayerConstants::ALL, scene, false);
    };

    specialise(&mut pipelines, &scene);
    let held = pipelines.specialised_pipelines(a);
    assert!(held > 0, "A is prepared");

    // A, B, A: the return to A keeps every pipeline A held.
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
    assert_eq!(pipelines.specialised_pipelines(a), held);

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
