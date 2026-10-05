//! Scene lights observed in the lit colour of real frames.
use crate::asset::{CpuMesh, Vertex};
use crate::renderer::Renderer;
use crate::settings::{self, Settings};
use crate::{
    Camera, FrameInput, InstanceState, Light, LightShape, Mobility, Scene, SceneError, test_support,
};
use glam::{Mat4, Vec3};

const SIZE: [u32; 2] = [64, 32];

/// A quad facing +Z at `center`, 1.2 by 1 metres, charted at the middle of
/// a 1×1 atlas.
fn quad(center: Vec3) -> CpuMesh {
    CpuMesh {
        vertices: [(-0.6, -0.5), (0.6, -0.5), (0.6, 0.5), (-0.6, 0.5)]
            .map(|(x, y)| Vertex {
                tangent: [0.; 4],
                lightmap_bounds: [0., 0., 1., 1.],
                lightmap_uv: [0.5; 2],
                position: (center + Vec3::new(x, y, 0.)).to_array(),
                normal: [0., 0., 1.],
                uv: [0.; 2],
                color: [1.; 4],
            })
            .to_vec(),
        indices: vec![0, 1, 2, 0, 2, 3],
        material: 0,
        deformation: Default::default(),
    }
}

fn light(position: Vec3) -> Light {
    Light {
        position,
        shape: LightShape::Point {
            radius: LightShape::DEFAULT_RADIUS,
        },
        color: [1.; 3],
        intensity: 2.,
        range: 3.,
        baked: false,
        specular: 1.,
        casts_shadow: false,
        ..Default::default()
    }
}

// Plausible defects: a baked light lights static receivers whose chart's
// bake already holds it, including the black outside a cropped chart's
// bounds, or misses moving ones or static ones without baked lighting (no
// chart assigned, or baked lighting off), as when the live and baked ranges
// are swapped or the rule follows mobility or chart bounds alone; the
// specular scale is not applied, or applied to
// diffuse light; a replaced, removed or refused light keeps lighting; the
// light buffer's growth loses records. The oracle is each receiver's lit
// colour against the same frame without the light: black receivers, a
// black environment and no other light, so a light that does not reach a
// receiver leaves it exactly as it was.
#[test]
fn scene_lights_light_what_they_own() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let diffuse = [Vec3::new(-0.8, 0.6, -3.), Vec3::new(0.8, 0.6, -3.)];
    let mirror = Vec3::new(0., -0.7, -3.);
    // A static receiver without a chart (the unassigned UV (0,0)), and one
    // outside its cropped chart's bounds.
    let uncharted = Vec3::new(1.5, -0.7, -3.);
    let cropped = Vec3::new(-1.5, -0.7, -3.);
    let asset = |center, metallic: f32| {
        let mut asset = test_support::cube();
        asset.meshes = vec![quad(center)];
        asset.materials[0].base = [0.8, 0.8, 0.8, 1.];
        asset.materials[0].metallic = metallic;
        asset.materials[0].roughness = 0.5;
        asset
    };
    let mut scene = Scene::new(&device, &queue);
    let place = |scene: &mut Scene, asset, mobility| {
        let model = scene.add_asset(&device, &queue, asset).unwrap().model;
        let state = InstanceState {
            model,
            pose: Mat4::IDENTITY,
            visible: true,
            capture_visible: true,
        };
        scene
            .add_instance(&device, &queue, state, mobility)
            .unwrap();
    };
    place(&mut scene, asset(diffuse[0], 0.), Mobility::Static);
    place(&mut scene, asset(diffuse[1], 0.), Mobility::Moving);
    place(&mut scene, asset(mirror, 1.), Mobility::Static);
    let mut unassigned = asset(uncharted, 0.);
    for vertex in &mut unassigned.meshes[0].vertices {
        vertex.lightmap_uv = [0.; 2];
    }
    place(&mut scene, unassigned, Mobility::Static);
    let mut outside = asset(cropped, 0.);
    for vertex in &mut outside.meshes[0].vertices {
        vertex.lightmap_bounds = [0.75, 0.75, 1., 1.];
    }
    place(&mut scene, outside, Mobility::Static);
    // A black atlas: a chart holds every static receiver but `unassigned`.
    scene
        .set_static_irradiance_atlas(
            &device,
            &queue,
            &crate::static_lighting::IrradianceAtlas {
                size: [1, 1],
                irradiance: vec![[0.; 3]],
                back_irradiance: vec![[0.; 3]],
                directionality: Vec::new(),
                back_directionality: Vec::new(),
            },
        )
        .unwrap();
    let camera = Camera {
        view: Mat4::IDENTITY,
        projection: crate::perspective(1., SIZE[0] as f32 / SIZE[1] as f32, 0.1),
        eye: Vec3::ZERO,
    };
    let mut input = FrameInput::new(camera);
    input.backdrop = crate::Backdrop::Color([0.; 3]);
    let settings = Settings {
        antialiasing: settings::Antialiasing::Off,
        bloom: settings::Bloom::Off,
        atmosphere: false,
        ..Settings::default()
    };
    let mut renderer = Renderer::for_test(&device, &queue, SIZE, &settings);
    let output = crate::view::targets::target(
        &device,
        "scene light output",
        SIZE,
        crate::shading::gbuffer::COLOR,
    );
    // The pixel each receiver's center lands on.
    let pixel = |point: Vec3| {
        let clip = camera.projection * camera.view * point.extend(1.);
        let x = (clip.x / clip.w * 0.5 + 0.5) * SIZE[0] as f32;
        let y = (0.5 - clip.y / clip.w * 0.5) * SIZE[1] as f32;
        (y as usize * SIZE[0] as usize + x as usize) * 8
    };
    let receivers = [diffuse[0], diffuse[1], mirror, uncharted, cropped].map(pixel);
    // The lit colour's red channel at the static, moving, mirror, uncharted
    // and cropped static receivers.
    let mut observe = |scene: &mut Scene, input: &FrameInput| -> [f32; 5] {
        let mut encoder = device.create_command_encoder(&Default::default());
        renderer.render(
            &device,
            &queue,
            &mut encoder,
            scene,
            input,
            &settings,
            &output,
            None,
        );
        queue.submit([encoder.finish()]);
        renderer.finish_frame(scene);
        let color = test_support::read(&device, &queue, renderer.targets().color.texture(), 8);
        receivers.map(|at| test_support::half(&color[at..at + 2]))
    };
    let dark = observe(&mut scene, &input);
    let lit = |now: f32, before: f32| now > before + 0.01;

    // A live light reaches every receiver; a baked one only the moving one.
    let above = Vec3::new(0., 0.6, -2.);
    let id = scene.add_light(&device, &queue, light(above)).unwrap();
    let live = observe(&mut scene, &input);
    assert!(
        [0, 1, 3, 4].iter().all(|&at| lit(live[at], dark[at])),
        "{live:?}"
    );
    let baked = Light {
        baked: true,
        ..light(above)
    };
    scene.set_light(&queue, id, baked).unwrap();
    assert_eq!(scene.light(id).unwrap(), &baked);
    let owned = observe(&mut scene, &input);
    assert_eq!(
        owned[0], dark[0],
        "a baked light lit a charted static receiver"
    );
    assert!(
        lit(owned[1], dark[1]),
        "a baked light missed a moving receiver"
    );
    assert!(
        lit(owned[3], dark[3]),
        "a baked light missed a static receiver without a chart"
    );
    assert_eq!(
        owned[4], dark[4],
        "a baked light lit the black outside a cropped chart"
    );
    let unbaked = FrameInput {
        baked_lighting: false,
        ..input
    };
    assert!(
        lit(observe(&mut scene, &unbaked)[0], dark[0]),
        "a baked light missed a static receiver while baked lighting is off"
    );

    // A metal has no diffuse colour: a diffuse-only light leaves it dark.
    let ahead = Vec3::new(0., -0.7, -1.8);
    scene
        .set_light(
            &queue,
            id,
            Light {
                specular: 0.,
                ..light(ahead)
            },
        )
        .unwrap();
    let diffuse_only = observe(&mut scene, &input);
    assert_eq!(diffuse_only[2], dark[2], "a diffuse-only light lit a metal");
    scene.set_light(&queue, id, light(ahead)).unwrap();
    let highlight = observe(&mut scene, &input)[2];
    assert!(lit(highlight, dark[2]), "{highlight}");

    // A rectangle facing the receivers lights them; turned away, it lights
    // none. The lit pipelines shade rectangles only while the scene holds
    // one, so these frames also follow that specialisation each way.
    let panel = |direction| Light {
        shape: LightShape::Rect {
            direction,
            width_axis: Vec3::X,
            width: 2.,
            height: 0.5,
        },
        ..light(Vec3::new(0., 0., -2.))
    };
    scene.set_light(&queue, id, panel(Vec3::NEG_Z)).unwrap();
    let facing = observe(&mut scene, &input);
    assert!(
        [0, 1, 3, 4].iter().all(|&at| lit(facing[at], dark[at])),
        "{facing:?}"
    );
    scene.set_light(&queue, id, panel(Vec3::Z)).unwrap();
    assert_eq!(
        observe(&mut scene, &input),
        dark,
        "a rectangle lit what is behind it"
    );
    scene.set_light(&queue, id, light(ahead)).unwrap();
    assert_eq!(observe(&mut scene, &input)[2], highlight);

    // Refused input changes nothing; a removed light lights nothing, and
    // its identity names nothing.
    for invalid in [
        Light {
            range: 0.,
            ..light(ahead)
        },
        Light {
            shape: LightShape::Spot {
                direction: Vec3::ZERO,
                inner_angle: 0.,
                outer_angle: 0.5,
                radius: LightShape::DEFAULT_RADIUS,
            },
            ..light(ahead)
        },
        Light {
            shape: LightShape::Spot {
                direction: Vec3::Z,
                inner_angle: 0.6,
                outer_angle: 0.5,
                radius: LightShape::DEFAULT_RADIUS,
            },
            ..light(ahead)
        },
        Light {
            intensity: f32::NAN,
            ..light(ahead)
        },
        Light {
            fog_energy: -1.,
            ..light(ahead)
        },
        Light {
            shadow_opacity: -0.5,
            ..light(ahead)
        },
        Light {
            shadow_opacity: 1.5,
            ..light(ahead)
        },
        Light {
            shadow_opacity: f32::NAN,
            ..light(ahead)
        },
        Light {
            shape: LightShape::Rect {
                direction: Vec3::Z,
                width_axis: Vec3::NEG_Z,
                width: 1.,
                height: 1.,
            },
            ..light(ahead)
        },
        Light {
            shape: LightShape::Rect {
                direction: Vec3::Z,
                width_axis: Vec3::X,
                width: 0.,
                height: 1.,
            },
            ..light(ahead)
        },
    ] {
        assert!(matches!(
            scene.set_light(&queue, id, invalid),
            Err(SceneError::InvalidLight)
        ));
        assert!(matches!(
            scene.add_light(&device, &queue, invalid),
            Err(SceneError::InvalidLight)
        ));
    }
    assert_eq!(observe(&mut scene, &input)[2], highlight);
    scene.remove_light(id).unwrap();
    assert_eq!(observe(&mut scene, &input), dark);
    assert!(matches!(
        scene.set_light(&queue, id, light(ahead)),
        Err(SceneError::UnknownLight)
    ));
    assert!(matches!(
        scene.remove_light(id),
        Err(SceneError::UnknownLight)
    ));
    assert!(matches!(scene.light(id), Err(SceneError::UnknownLight)));

    // A light added first keeps its record when many more grow the buffer.
    let first = scene.add_light(&device, &queue, light(above)).unwrap();
    let before = observe(&mut scene, &input);
    for index in 0..300 {
        scene
            .add_light(&device, &queue, light(Vec3::new(index as f32, 1000., 0.)))
            .unwrap();
    }
    assert_eq!(observe(&mut scene, &input), before);
    assert!(lit(before[1], dark[1]));
    scene.remove_light(first).unwrap();
}
