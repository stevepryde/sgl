//! Which specialisations' pipelines the cache keeps as the scene's lit
//! constants change.
use super::{Alpha, Cull, GeometryPass, GeometryPipelines, LayerConstants, Variant};
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

// The GPU handles expose recompilation on a return to A. Cache occupancy
// catches speculative creation before demand and failure to evict A at C.
#[test]
fn pipelines_of_the_current_and_previous_specialisation_stay() {
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    let mut pipelines = pipelines(&device);
    let request = |pipelines: &mut GeometryPipelines, scene: &Scene| {
        pipelines.with_pipeline(
            scene,
            GeometryPass::Forward,
            Variant {
                cull: Cull::Back,
                alpha: Alpha::Opaque,
                deformed: false,
            },
            None,
            Clone::clone,
        )
    };

    pipelines.specialise(LayerConstants::ALL, &scene);
    assert!(pipelines.cache.is_empty(), "no geometry requested");
    let a = request(&mut pipelines, &scene);
    let light = scene.add_light(&device, &queue, rectangle()).unwrap();
    pipelines.specialise(LayerConstants::ALL, &scene);
    assert_eq!(pipelines.cache.len(), 1, "B has no draws yet");
    let b = request(&mut pipelines, &scene);
    assert_ne!(a, b, "rectangle lights require different lit shading");

    scene.remove_light(light).unwrap();
    pipelines.specialise(LayerConstants::ALL, &scene);
    assert_eq!(
        request(&mut pipelines, &scene),
        a,
        "return to A reuses its pipeline"
    );
    scene.add_light(&device, &queue, rectangle()).unwrap();
    pipelines.specialise(LayerConstants::ALL, &scene);
    assert_eq!(request(&mut pipelines, &scene), b);

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
    pipelines.specialise(LayerConstants::ALL, &scene);
    assert_eq!(pipelines.cache.len(), 1, "C evicts A before its first draw");
    assert!(
        pipelines.cache.values().any(|pipeline| pipeline == &b),
        "B stays"
    );
    let c = request(&mut pipelines, &scene);
    assert_ne!(c, b, "films require different lit shading");
}
