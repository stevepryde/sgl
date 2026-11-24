//! Which specialisations' pipelines the cache keeps as the scene's lit
//! constants change, and which it keeps as a feature stops.
use super::{Alpha, CameraShadows, Cull, GeometryPass, GeometryPipelines, LayerConstants, Variant};
use crate::Scene;
use glam::Vec3;

fn pipelines(device: &wgpu::Device) -> GeometryPipelines {
    let tier = crate::shading::bind::BindingTier::of(&device.limits());
    GeometryPipelines::new(
        device,
        &crate::view::compile::Compiler::new(device),
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
                uv1: 0,
                transformed: 0,
                occlusion_specular: false,
                cull: Cull::Back,
                alpha: Alpha::Opaque,
                deformed: false,
                shader_sampled: 0,
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
        pipelines
            .cache
            .values()
            .any(|pipeline| pipeline.ready() == b.as_ref()),
        "B stays"
    );
    let c = request(&mut pipelines, &scene);
    assert_ne!(c, b, "films require different lit shading");
}

// Plausible defect: the fused and lighting pipelines that read screen-space
// indirect light, and their camera programs, outlive the feature, held
// while it is off. Oracle: the cache's pipelines and the default programs'
// modules before the feature's were requested, against theirs after the
// first frame that does not run it; while it runs they stay.
#[test]
fn screen_indirect_pipelines_go_with_the_feature() {
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    let scene = Scene::new(&device, &queue);
    let mut pipelines = pipelines(&device);
    pipelines.specialise(LayerConstants::ALL, &scene);
    let request = |pipelines: &mut GeometryPipelines, pass| {
        let variant = Variant {
            uv1: 0,
            transformed: 0,
            cull: Cull::Back,
            alpha: Alpha::Opaque,
            deformed: false,
            shader_sampled: 0,
            occlusion_specular: false,
        };
        pipelines.with_pipeline(&scene, pass, variant, None, |_| ());
    };
    let lighting = |shadows, screen_indirect| GeometryPass::Lighting {
        shadows,
        screen_indirect,
    };
    request(&mut pipelines, GeometryPass::GBuffer);
    request(&mut pipelines, lighting(CameraShadows::Maps, false));
    let held =
        |pipelines: &GeometryPipelines| (pipelines.cache.len(), pipelines.programs.modules());
    let before = held(&pipelines);
    request(&mut pipelines, lighting(CameraShadows::Maps, true));
    request(&mut pipelines, lighting(CameraShadows::ShadowMask, true));
    let running = held(&pipelines);
    assert!(
        running.0 > before.0 && running.1 > before.1,
        "the feature's pipelines and programs are made: {before:?} then {running:?}"
    );
    pipelines.screen_indirect_runs(true);
    assert_eq!(held(&pipelines), running, "they stay while it runs");
    pipelines.screen_indirect_runs(false);
    assert_eq!(
        held(&pipelines),
        before,
        "the feature's pipelines and programs outlive it"
    );
}

// A pass whose programs sample none of an occlusion map of its own and the
// specular maps keys none of their variants, so materials that differ only
// in them share that pass's pipelines. Plausible defects: the key keeping
// `Variant::occlusion_specular`, or those maps' second UV set, transform or
// shader-sampled state, for the receivers, the temporal composition mask,
// the anisotropy target or a volume layer. The oracle is that sharing: for
// each such pass the key of a material whose own occlusion map takes all
// three equals that of the same material without the map; and the G-buffer
// pass, which records the occlusion, keys it.
#[test]
fn passes_that_sample_none_of_these_maps_key_none_of_them() {
    use super::{LitConstants, PipelineKey};
    use crate::shading::bind::group2;
    let slot = 1 << group2::map_slot(group2::OCCLUSION_MAP);
    let plain = Variant {
        cull: Cull::Back,
        alpha: Alpha::Opaque,
        deformed: false,
        shader_sampled: 0,
        uv1: 0,
        transformed: 0,
        occlusion_specular: false,
    };
    let mapped = Variant {
        shader_sampled: slot,
        uv1: slot,
        transformed: slot,
        occlusion_specular: true,
        ..plain
    };
    let key = |pass, variant| {
        PipelineKey::new(
            pass,
            variant,
            None,
            (LayerConstants::ALL, LitConstants::default()),
            false,
        )
    };
    for pass in [
        GeometryPass::Receivers,
        GeometryPass::TemporalComposition { footprint: false },
        GeometryPass::GBufferAnisotropy,
        GeometryPass::VolumeEntry,
    ] {
        assert_eq!(key(pass, mapped), key(pass, plain), "{pass:?}");
    }
    assert_ne!(
        key(GeometryPass::GBuffer, mapped),
        key(GeometryPass::GBuffer, plain)
    );
}
