use super::*;
use crate::settings::ShadowQuality;
use glam::camera;

// Surfaces in a camera view leave specular image-based lighting to the
// reflection stages (as AMD's SSSR sample's scene input does) but keep
// diffuse image-based lighting and emission; capture faces shade with
// environment specular. A nonemissive pure metal has no diffuse term and,
// with the direct light behind it, positive radiance in the camera view would
// be leaked specular image-based lighting. The capture metal, dielectric and
// emissive controls keep a disconnected or black draw from passing.
#[test]
fn camera_surfaces_exclude_specular_ibl_and_keep_diffuse_and_emission() {
    use crate::asset::{CpuMesh, Vertex};
    use crate::settings::Settings;
    use crate::{Camera, DirectionalLight, FrameInput, HemisphereLight, Scene};
    use glam::Vec3;
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    let size = [960u32, 540];
    let mut world = crate::test_support::cube();
    world.meshes = vec![CpuMesh {
        vertices: [[-0.5, -0.5], [0.5, -0.5], [0.5, 0.5], [-0.5, 0.5]]
            .map(|[x, y]| Vertex {
                tangent: [0.; 4],
                lightmap_uv: [0.; 2],
                lightmap_bounds: [0., 0., 1., 1.],
                position: [x, y, 0.],
                normal: [0., 0., -1.],
                uv: [0.5; 2],
                color: [1.; 4],
            })
            .to_vec(),
        indices: vec![0, 2, 1, 0, 3, 2],
        material: 0,
        deformation: Default::default(),
    }];
    world.materials[0].base = [0.5, 0.5, 0.5, 1.];
    world.materials[0].roughness = 0.3;
    let mut scene = Scene::new(&device, &queue);
    let (world, _) = crate::test_support::add_static(&device, &queue, &mut scene, world);
    // A uniform sky of radiance 1 at every roughness (binary16 0x3c00).
    let environment = scene
        .add_environment(
            &device,
            &queue,
            &crate::test_support::environment([255; 4], &0x3c00u16.to_le_bytes()),
        )
        .unwrap();
    let view = camera::rh::view::look_at_mat4(Vec3::new(0., 0., -2.), Vec3::ZERO, Vec3::Y);
    let mut input = FrameInput::new(Camera {
        view,
        projection: camera::rh::proj::directx::orthographic(-1., 1., -1., 1., 10., 0.1),
        eye: Vec3::new(0., 0., -2.),
    });
    // The sun shines from +Z, behind the plane's -Z normal; a hemisphere
    // fill and the sky light it.
    input.directional_lights[0] = Some(DirectionalLight {
        direction: Vec3::new(-0.2, -0.5, -1.),
        color: [1., 0.8, 0.6],
        illuminance: 3.,
        shadow: None,
        ..Default::default()
    });
    input.hemisphere_light = HemisphereLight {
        sky_color: [0.5, 0.6, 0.8],
        ground_color: [0.2, 0.2, 0.25],
        intensity: 1.,
    };
    input.reflection_environment = input.diffuse_environment;
    input.environment = Some(environment);
    let settings = Settings::default();
    let mut renderer = Renderer::for_test(&device, &queue, size, &settings);
    let target = |label, format| crate::view::targets::target(&device, label, size, format);
    let mut observations = Vec::new();
    let mut metal = Vec::new();
    for (name, capture, metallic, emission) in [
        ("camera metal", false, 1., [0.; 3]),
        ("capture metal", true, 1., [0.; 3]),
        ("camera diffuse", false, 0., [0.; 3]),
        ("camera emission", false, 1., [0.25, 0.5, 1.]),
    ] {
        let mut values = scene.material(world.materials[0]).unwrap();
        values.metallic = metallic;
        values.emission[..3].copy_from_slice(&emission);
        scene
            .set_material(&queue, world.materials[0], values)
            .unwrap();
        let color = target("lit surface", crate::shading::gbuffer::COLOR);
        let motion = target("motion", crate::shading::gbuffer::MOTION);
        let depth = target("depth", crate::shading::gbuffer::DEPTH);
        renderer.prepare_test_frame(&device, &queue, &mut scene, &input, &settings);
        if capture {
            renderer.test_view_as_capture(&queue);
        }
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let attachment = crate::view::targets::attachment;
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("camera and capture surfaces"),
                color_attachments: &[attachment(&color), attachment(&motion)],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(0.),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                ..Default::default()
            });
            renderer.draw_static((&device, &queue), &scene, &mut pass);
        }
        queue.submit([encoder.finish()]);
        let bytes = crate::test_support::read(&device, &queue, color.texture(), 8);
        let half = crate::test_support::half;
        let mut mean = [0.; 3];
        for y in 238..302 {
            for x in 448..512 {
                let p = &bytes[(y * 960 + x) * 8..];
                assert_eq!(half(&p[6..]), 1., "fixture was not rasterized");
                for c in 0..3 {
                    let value = half(&p[c * 2..]);
                    assert!(value.is_finite() && value >= 0., "invalid {name} radiance");
                    match name {
                        "camera metal" => metal.push(value),
                        "camera emission" => {
                            // The same metal plus emission, at half precision.
                            let added = value - metal[((y - 238) * 64 + x - 448) * 3 + c];
                            assert!(
                                (added - emission[c]).abs() <= emission[c] / 512.,
                                "emission was altered: {added} != {}",
                                emission[c]
                            )
                        }
                        _ => assert!(value > 0., "missing {name} environment lighting"),
                    }
                    mean[c] += value / 4096.;
                }
            }
        }
        observations.push((name, mean));
    }
    // A metal keeps only multiple scattering here, which uses irradiance: at
    // roughness 0.3 its GGX directional albedo exceeds 0.9, so multiple
    // scattering is under a tenth of the diffuse control's 0.5 albedo.
    // Leaked single-scattering specular (F0 0.5) would match it.
    for c in 0..3 {
        assert!(
            observations[0].1[c] < 0.2 * observations[2].1[c],
            "specular IBL leaked into the camera's surfaces: {observations:?}"
        );
    }
}

// Defects: a capture shades without the scene's lights (its list empty or
// unbound), or takes a baked light where an atlas chart's bake already holds
// it. The oracle is the captured radiance of a scene whose only light is the
// scene light: a black sky and no frame lights, so without the light the
// capture holds nothing but the black floor.
#[test]
fn captures_shade_scene_lights_by_the_ownership_rule() {
    use crate::asset::{CpuMesh, Vertex};
    use crate::baked_specular_probe::SpecularProbeTexels;
    use crate::settings::Settings;
    use crate::{Camera, FrameInput, Light, LightShape, Scene};
    use glam::{Mat4, Vec3};
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    let mut floor = crate::test_support::cube();
    floor.meshes = vec![CpuMesh {
        vertices: [[-2., -2.], [2., -2.], [2., 2.], [-2., 2.]]
            .map(|[x, z]| Vertex {
                tangent: [0.; 4],
                lightmap_uv: [0.5; 2],
                lightmap_bounds: [0., 0., 1., 1.],
                position: [x, -1., z],
                normal: [0., 1., 0.],
                uv: [0.5; 2],
                color: [1.; 4],
            })
            .to_vec(),
        indices: vec![0, 2, 1, 0, 3, 2],
        material: 0,
        deformation: Default::default(),
    }];
    floor.materials[0].metallic = 0.;
    let mut scene = Scene::new(&device, &queue);
    crate::test_support::add_static(&device, &queue, &mut scene, floor);
    // A black atlas whose chart covers the floor.
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
    let input = FrameInput::new(Camera {
        view: Mat4::IDENTITY,
        projection: Mat4::IDENTITY,
        eye: Vec3::ZERO,
    });
    let settings = Settings::default();
    let mut renderer = Renderer::for_test(&device, &queue, [64, 64], &settings);
    let mut captured = |scene: &mut Scene| -> f32 {
        let radiance = renderer
            .capture_specular_probe(&device, &queue, scene, &input, &settings, Vec3::ZERO, 64)
            .unwrap();
        let SpecularProbeTexels::Rgba16Float(texels) = radiance.texels else {
            unreachable!("captures return RGBA16F")
        };
        texels
            .chunks_exact(4)
            .map(|texel| {
                (0..3)
                    .map(|c| crate::test_support::half(&texel[c].to_le_bytes()))
                    .sum::<f32>()
            })
            .sum()
    };
    let dark = captured(&mut scene);
    let light = Light {
        position: Vec3::new(0., -0.5, 0.),
        shape: LightShape::Point {
            radius: LightShape::DEFAULT_RADIUS,
        },
        color: [1.; 3],
        intensity: 4.,
        range: 3.,
        baked: false,
        specular: 1.,
        casts_shadow: false,
        ..Default::default()
    };
    let id = scene.add_light(&device, &queue, light).unwrap();
    let live = captured(&mut scene);
    assert!(
        live > dark + 1.,
        "a live light did not reach the capture: {live} vs {dark}"
    );
    scene
        .set_light(
            &queue,
            id,
            Light {
                baked: true,
                ..light
            },
        )
        .unwrap();
    assert_eq!(
        captured(&mut scene),
        dark,
        "a baked light lit a capture's surface that a chart covers"
    );
}

// A probe capture has no camera: its directional shadow comes from cascades
// about its centre, each surface taking the first that holds it. Plausible
// defects: cascades fit to the frame's camera rather than the capture, a
// face selecting a cascade by its own view depth that does not hold the
// surface, or casters in front of a cascade clipped. The oracle is
// geometric (`floor_under_occluder`).
#[test]
fn a_capture_shadows_what_it_sees_from_cascades_about_its_centre() {
    let Some(gpu) = crate::test_support::device() else {
        return;
    };
    let (shadowed, lit) = floor_under_occluder(gpu, None, ShadowQuality::High);
    assert!(
        lit > 0.05 && shadowed < 0.01 * lit,
        "the floor below the capture is {shadowed} under the occluder and {lit} without"
    );
}

// Plausible defect: a capture at another shadow quality than the frames
// before it reallocates the frame's maps and draws its cascades into the
// new ones, while its lit groups bind the old ones, which hold the last
// frame's cascades about a camera far away. The oracle is geometric
// (`floor_under_occluder`), with the capture after a frame at the other
// quality, each way.
#[test]
fn a_capture_at_another_shadow_quality_shadows_from_its_own_maps() {
    for (frames, capture) in [
        (ShadowQuality::High, ShadowQuality::Low),
        (ShadowQuality::Low, ShadowQuality::High),
    ] {
        let Some(gpu) = crate::test_support::device() else {
            return;
        };
        let (shadowed, lit) = floor_under_occluder(gpu, Some(frames), capture);
        assert!(
            lit > 0.05 && shadowed < 0.01 * lit,
            "a {capture:?} capture after {frames:?} frames: the floor below it is {shadowed} \
             under the occluder and {lit} without"
        );
    }
}

/// The floor a capture records below it, under an occluder and without it,
/// at the shadow quality `capture`, after a frame at `frames` when given: a
/// capture looks down at a floor that an occluder high above it, beyond its
/// cascades' pancake, covers from a light shining down, while the frame's
/// camera is far away, so the floor it records should be dark under the
/// occluder and lit without it.
fn floor_under_occluder(
    (device, queue): (wgpu::Device, wgpu::Queue),
    frames: Option<ShadowQuality>,
    capture: ShadowQuality,
) -> (f32, f32) {
    use crate::asset::CpuMesh;
    use crate::settings::Settings;
    use crate::{Camera, DirectionalLight, DirectionalShadow, FrameInput, Scene};
    use glam::Vec3;
    // A white single-sided square facing +Y at height `y`.
    let square = |y: f32, half: f32| {
        let mut asset = crate::test_support::cube();
        let mut mesh: CpuMesh = asset.meshes.remove(0);
        // The cube's +Y face: vertices 12..16, indices 18..24.
        mesh.vertices = mesh.vertices[12..16]
            .iter()
            .map(|vertex| {
                let [x, _, z] = vertex.position;
                crate::asset::Vertex {
                    position: [x * 2. * half, y, z * 2. * half],
                    ..*vertex
                }
            })
            .collect();
        mesh.indices = vec![0, 1, 2, 0, 2, 3];
        asset.meshes = vec![mesh];
        asset.materials[0].base = [0.8, 0.8, 0.8, 1.];
        asset.materials[0].metallic = 0.;
        asset.materials[0].double_sided = false;
        asset
    };
    // The first cascade's cube reaches 2 m (Godot's first split, 0.1 of the
    // distance) from the capture's centre, 1 m up.
    let shadow = DirectionalShadow {
        distance: 20.,
        cascades: 3,
    };
    let mut scene = Scene::new(&device, &queue);
    crate::test_support::add_static(&device, &queue, &mut scene, square(0., 6.));
    // 10 m above the first cascade's near plane: its cube's top, 3 m up,
    // plus the pancake.
    let (_, occluder) = crate::test_support::add_static(
        &device,
        &queue,
        &mut scene,
        square(13. + crate::view::cascades::SHADOW_PANCAKE_SIZE, 3.),
    );
    let mut input = FrameInput::new(Camera {
        view: camera::rh::view::look_at_mat4(
            Vec3::new(1000., 0., 0.),
            Vec3::new(2000., 0., 0.),
            Vec3::Y,
        ),
        projection: crate::perspective(1., 1., 0.1),
        eye: Vec3::new(1000., 0., 0.),
    });
    input.directional_lights[0] = Some(DirectionalLight {
        direction: Vec3::NEG_Y,
        color: [1.; 3],
        illuminance: 1.,
        shadow: Some(shadow),
        ..Default::default()
    });
    let quality = |shadow_quality| Settings {
        shadow_quality,
        ..Settings::default()
    };
    let settings = quality(capture);
    let mut renderer = Renderer::for_test(
        &device,
        &queue,
        [64, 64],
        &quality(frames.unwrap_or(capture)),
    );
    if let Some(frames) = frames {
        let output = crate::view::targets::target(
            &device,
            "frame before the capture",
            [64, 64],
            crate::shading::gbuffer::COLOR,
        );
        let mut encoder = device.create_command_encoder(&Default::default());
        renderer.render(
            &device,
            &queue,
            &mut encoder,
            &mut scene,
            &input,
            &quality(frames),
            &output,
            None,
        );
        queue.submit([encoder.finish()]);
        renderer.finish_frame(&mut scene);
    }
    let face_size = 64;
    let mut floor = |scene: &mut Scene| {
        let radiance = renderer
            .capture_specular_probe(
                &device,
                &queue,
                scene,
                &input,
                &settings,
                Vec3::Y,
                face_size,
            )
            .unwrap();
        let crate::SpecularProbeTexels::Rgba16Float(texels) = radiance.texels else {
            unreachable!("captures return RGBA16F");
        };
        // The centre of the downward face (-Y, the fourth) at full detail.
        let size = face_size as usize;
        let texel = (3 * size * size + size / 2 * size + size / 2) * 4;
        crate::test_support::half(&texels[texel].to_le_bytes())
    };
    let shadowed = floor(&mut scene);
    let mut state = *scene.instance(occluder).unwrap();
    state.capture_visible = false;
    scene.set_instance(&queue, occluder, state).unwrap();
    (shadowed, floor(&mut scene))
}

// A probe capture shades with the scene's lights' shadows from the static
// layers it places itself: the frame's camera may never have seen those
// lights. Plausible defects: captures bound to no shadows or to the
// frame's atlas, a capture placing no lights, or a capture's static layer
// kept after a static edit removed its caster. The oracle is geometric: a
// capture looks down at a floor that an occluder between it and a casting
// point light covers, while the frame's camera is far away; the floor it
// records is dark under the occluder and lit once the occluder no longer
// casts.
#[test]
fn a_capture_shadows_scene_lights_from_static_layers_it_places() {
    use crate::asset::CpuMesh;
    use crate::settings::Settings;
    use crate::{Camera, FrameInput, Light, LightShape, Scene};
    use glam::Vec3;
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    // A white double-sided square facing +Y at height `y`.
    let square = |y: f32, half: f32| {
        let mut asset = crate::test_support::cube();
        let mut mesh: CpuMesh = asset.meshes.remove(0);
        // The cube's +Y face: vertices 12..16, indices 18..24.
        mesh.vertices = mesh.vertices[12..16]
            .iter()
            .map(|vertex| {
                let [x, _, z] = vertex.position;
                crate::asset::Vertex {
                    position: [x * 2. * half, y, z * 2. * half],
                    ..*vertex
                }
            })
            .collect();
        mesh.indices = vec![0, 1, 2, 0, 2, 3];
        asset.meshes = vec![mesh];
        asset.materials[0].base = [0.8, 0.8, 0.8, 1.];
        asset.materials[0].metallic = 0.;
        asset.materials[0].double_sided = true;
        asset
    };
    let mut scene = Scene::new(&device, &queue);
    crate::test_support::add_static(&device, &queue, &mut scene, square(0., 6.));
    let (_, occluder) =
        crate::test_support::add_static(&device, &queue, &mut scene, square(3., 1.));
    scene
        .add_light(
            &device,
            &queue,
            Light {
                position: Vec3::new(0., 5., 0.),
                shape: LightShape::Point {
                    radius: LightShape::DEFAULT_RADIUS,
                },
                color: [1.; 3],
                intensity: 20.,
                range: 10.,
                baked: false,
                specular: 1.,
                casts_shadow: true,
                ..Default::default()
            },
        )
        .unwrap();
    let input = FrameInput::new(Camera {
        view: camera::rh::view::look_at_mat4(
            Vec3::new(1000., 0., 0.),
            Vec3::new(2000., 0., 0.),
            Vec3::Y,
        ),
        projection: crate::perspective(1., 1., 0.1),
        eye: Vec3::new(1000., 0., 0.),
    });
    let settings = Settings::default();
    let mut renderer = Renderer::for_test(&device, &queue, [64, 64], &settings);
    let face_size = 64;
    let mut floor = |scene: &mut Scene| {
        let radiance = renderer
            .capture_specular_probe(
                &device,
                &queue,
                scene,
                &input,
                &settings,
                Vec3::Y,
                face_size,
            )
            .unwrap();
        let crate::SpecularProbeTexels::Rgba16Float(texels) = radiance.texels else {
            unreachable!("captures return RGBA16F");
        };
        // The centre of the downward face (-Y, the fourth) at full detail.
        let size = face_size as usize;
        let texel = (3 * size * size + size / 2 * size + size / 2) * 4;
        crate::test_support::half(&texels[texel].to_le_bytes())
    };
    let shadowed = floor(&mut scene);
    let mut state = *scene.instance(occluder).unwrap();
    state.capture_visible = false;
    scene.set_instance(&queue, occluder, state).unwrap();
    let lit = floor(&mut scene);
    assert!(
        lit > 0.05 && shadowed < 0.01 * lit,
        "the floor below the capture is {shadowed} under the occluder and {lit} without"
    );
}

// A probe capture draws its scene lights' static layers at its own time:
// a static caster whose shader reads the time casts where it stands at the
// capture's `elapsed_seconds`, as the capture's surfaces are drawn, not
// where it stood in the last frame. Plausible defect: the layers drawn
// with the frame's uniform, at the last frame's time. The oracle is
// geometric: the shader lifts an occluder by amplitude × sin(π/2 × time)
// from below the floor, beyond the light's reach, to between a casting
// point light and the floor the capture looks down at. After a frame at
// t = 0, a capture at t = 1 records the floor dark, and one at t = 2,
// where the occluder is back at rest, lit.
#[test]
fn a_capture_shadows_scene_lights_from_casters_at_its_own_time() {
    use crate::asset::CpuMesh;
    use crate::settings::Settings;
    use crate::{Camera, FrameInput, Light, LightShape, Scene};
    use glam::Vec3;
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    // A white double-sided square facing +Y at height `y`.
    let square = |y: f32, half: f32| {
        let mut asset = crate::test_support::cube();
        let mut mesh: CpuMesh = asset.meshes.remove(0);
        // The cube's +Y face: vertices 12..16, indices 18..24.
        mesh.vertices = mesh.vertices[12..16]
            .iter()
            .map(|vertex| {
                let [x, _, z] = vertex.position;
                crate::asset::Vertex {
                    position: [x * 2. * half, y, z * 2. * half],
                    ..*vertex
                }
            })
            .collect();
        mesh.indices = vec![0, 1, 2, 0, 2, 3];
        asset.meshes = vec![mesh];
        asset.materials[0].base = [0.8, 0.8, 0.8, 1.];
        asset.materials[0].metallic = 0.;
        asset.materials[0].double_sided = true;
        asset
    };
    // Lifted by `amplitude`, the occluder rests at 3 m, between the light
    // and the floor; at rest it lies 15 m from the light, beyond its 10 m
    // reach.
    let amplitude = 13.;
    let mut scene = Scene::new(&device, &queue);
    let shader = crate::test_support::add_test_shader(&mut scene);
    crate::test_support::add_static(&device, &queue, &mut scene, square(0., 6.));
    let (occluder, _) =
        crate::test_support::add_static(&device, &queue, &mut scene, square(3. - amplitude, 1.));
    let material = occluder.materials[0];
    crate::test_support::set_shader(&mut scene, &queue, material, (shader, amplitude));
    let params = crate::test_support::TestShaderParams {
        direction: [0., 1., 0.],
        amplitude,
        frequency: std::f32::consts::FRAC_PI_2,
        ..Default::default()
    };
    crate::test_support::set_test_params(&mut scene, &queue, material, params);
    scene
        .add_light(
            &device,
            &queue,
            Light {
                position: Vec3::new(0., 5., 0.),
                shape: LightShape::Point {
                    radius: LightShape::DEFAULT_RADIUS,
                },
                color: [1.; 3],
                intensity: 20.,
                range: 10.,
                baked: false,
                specular: 1.,
                casts_shadow: true,
                ..Default::default()
            },
        )
        .unwrap();
    let at = |elapsed_seconds| FrameInput {
        elapsed_seconds,
        ..FrameInput::new(Camera {
            view: camera::rh::view::look_at_mat4(
                Vec3::new(1000., 0., 0.),
                Vec3::new(2000., 0., 0.),
                Vec3::Y,
            ),
            projection: crate::perspective(1., 1., 0.1),
            eye: Vec3::new(1000., 0., 0.),
        })
    };
    let settings = Settings::default();
    let mut renderer = Renderer::for_test(&device, &queue, [64, 64], &settings);
    let output = crate::view::targets::target(
        &device,
        "frame before the capture",
        [64, 64],
        crate::shading::gbuffer::COLOR,
    );
    let mut encoder = device.create_command_encoder(&Default::default());
    renderer.render(
        &device,
        &queue,
        &mut encoder,
        &mut scene,
        &at(0.),
        &settings,
        &output,
        None,
    );
    queue.submit([encoder.finish()]);
    renderer.finish_frame(&mut scene);
    let face_size = 64;
    let mut floor = |scene: &mut Scene, seconds| {
        let radiance = renderer
            .capture_specular_probe(
                &device,
                &queue,
                scene,
                &at(seconds),
                &settings,
                Vec3::Y,
                face_size,
            )
            .unwrap();
        let crate::SpecularProbeTexels::Rgba16Float(texels) = radiance.texels else {
            unreachable!("captures return RGBA16F");
        };
        // The centre of the downward face (-Y, the fourth) at full detail.
        let size = face_size as usize;
        let texel = (3 * size * size + size / 2 * size + size / 2) * 4;
        crate::test_support::half(&texels[texel].to_le_bytes())
    };
    let shadowed = floor(&mut scene, 1.);
    let lit = floor(&mut scene, 2.);
    assert!(
        lit > 0.05 && shadowed < 0.01 * lit,
        "the floor below the capture is {shadowed} with the occluder lifted at the capture's \
         time and {lit} with it at rest"
    );
}

/// A floor lit by the hemisphere fill alone under a black sky, packing an
/// occlusion map of red `red` in its metallic-roughness image, white and
/// `metallic` at perceptual `roughness`: the radiance a probe capture at
/// the origin holds of it at each occlusion strength of `strengths`.
fn captured_floor<const N: usize>(
    red: u8,
    metallic: f32,
    roughness: f32,
    strengths: [f32; N],
) -> Option<[f32; N]> {
    use crate::asset::{CpuMesh, Image, Vertex};
    use crate::baked_specular_probe::SpecularProbeTexels;
    use crate::settings::Settings;
    use crate::{Camera, FrameInput, HemisphereLight, Scene};
    use glam::{Mat4, Vec3};
    let (device, queue) = crate::test_support::device()?;
    let mut floor = crate::test_support::cube();
    floor.meshes = vec![CpuMesh {
        vertices: [[-2., -2.], [2., -2.], [2., 2.], [-2., 2.]]
            .map(|[x, z]| Vertex {
                tangent: [0.; 4],
                lightmap_uv: [0.; 2],
                lightmap_bounds: [0., 0., 1., 1.],
                position: [x, -1., z],
                normal: [0., 1., 0.],
                uv: [0.5; 2],
                color: [1.; 4],
            })
            .to_vec(),
        indices: vec![0, 2, 1, 0, 3, 2],
        material: 0,
        deformation: Default::default(),
    }];
    floor.images = vec![Image::Rgba8(image::RgbaImage::from_pixel(
        4,
        4,
        image::Rgba([red, 255, 255, 255]),
    ))];
    if metallic > 0. {
        floor.materials[0].base = [1.; 4];
    }
    floor.materials[0].metallic = metallic;
    floor.materials[0].roughness = roughness;
    floor.materials[0].mr_texture = Some(0);
    floor.materials[0].occlusion_texture = Some(0);
    let mut scene = Scene::new(&device, &queue);
    let (ids, _) = crate::test_support::add_static(&device, &queue, &mut scene, floor);
    let mut input = FrameInput::new(Camera {
        view: Mat4::IDENTITY,
        projection: Mat4::IDENTITY,
        eye: Vec3::ZERO,
    });
    input.hemisphere_light = HemisphereLight {
        sky_color: [1.; 3],
        ground_color: [1.; 3],
        intensity: 1.,
    };
    let settings = Settings::default();
    let mut renderer = Renderer::for_test(&device, &queue, [64, 64], &settings);
    Some(strengths.map(|strength| {
        let mut values = scene.material(ids.materials[0]).unwrap();
        values.occlusion_strength = strength;
        scene
            .set_material(&queue, ids.materials[0], values)
            .unwrap();
        let radiance = renderer
            .capture_specular_probe(
                &device,
                &queue,
                &mut scene,
                &input,
                &settings,
                Vec3::ZERO,
                64,
            )
            .unwrap();
        let SpecularProbeTexels::Rgba16Float(texels) = radiance.texels else {
            unreachable!("captures return RGBA16F")
        };
        texels
            .chunks_exact(4)
            .map(|texel| {
                (0..3)
                    .map(|c| crate::test_support::half(&texel[c].to_le_bytes()))
                    .sum::<f32>()
            })
            .sum()
    }))
}

// Defects: a probe capture's surfaces leave their material's occlusion off,
// as the camera's opaque surfaces do while shading, leaving it to source
// completion, which captures do not run. The oracle is glTF 2.0's
// lerp(1, red, strength): a floor packing an occlusion map of red 64 in its
// metallic-roughness image, lit by the hemisphere fill alone under a black
// sky, so all its radiance is ambient diffuse, captures 64/255 of the
// radiance it captures at strength 0.
#[test]
fn a_capture_occludes_ambient_diffuse_by_the_materials_occlusion() {
    const RED: u8 = 64;
    let Some([unoccluded, occluded]) = captured_floor(RED, 0., 0.35, [0., 1.]) else {
        return;
    };
    let expected = unoccluded * f32::from(RED) / 255.;
    assert!(
        unoccluded > 1. && (occluded - expected).abs() <= expected * 0.01,
        "the capture holds {occluded} of the floor's {unoccluded}, expected {expected}"
    );
}

// Defects: a probe capture's surfaces occlude their specular multiple
// scattering by their material's occlusion linearly, as their diffuse
// share, or not at all. The oracle is Filament's occlusion of
// energy-compensated specular by its specular occlusion and multi-bounce
// (Lagarde and de Rousiers 2014; Jimenez et al. 2016): a rough white metal
// floor, whose light under the hemisphere fill is all multiple scattering,
// packing an occlusion map of red 0, keeps more than 0.7 of it at strength
// 0.5 (visibility 0.5, where the diffuse visibility keeps 0.5 and the
// multi-bounce on F0 1 about 0.9) and none at strength 1.
#[test]
fn a_capture_occludes_multiple_scattering_by_specular_occlusion() {
    let Some([full, half_visible, none]) = captured_floor(0, 1., 0.8, [0., 0.5, 1.]) else {
        return;
    };
    assert!(full > 1., "the metal floor must reflect: {full}");
    let kept = half_visible / full;
    eprintln!("capture: keeps {kept} of its light at visibility 0.5");
    assert!(
        kept > 0.7 && kept <= 1.,
        "the capture keeps {kept} at visibility 0.5"
    );
    assert!(
        none <= full * 1e-3,
        "the capture holds {none} of {full} at visibility 0"
    );
}

/// The solid angle, seen from the origin, of the rectangle `[x0, x1]` x
/// `[y0, y1]` in a plane at unit distance: the signed sum of its corners'
/// quadrant areas, atan(x y / sqrt(1 + x^2 + y^2)) (Filament's
/// `sphereQuadrantArea`). A cube texel's solid angle is the same over its
/// corners on the face.
fn rectangle_solid_angle([x0, x1]: [f64; 2], [y0, y1]: [f64; 2]) -> f64 {
    let quadrant = |x: f64, y: f64| (x * y / (1. + x * x + y * y).sqrt()).atan();
    quadrant(x1, y1) - quadrant(x0, y1) - quadrant(x1, y0) + quadrant(x0, y0)
}

// Defect: a probe face stores the sample at each texel's centre, so an
// emitter thinner than a texel is stored either not at all or across the
// whole texel at full radiance (#263). The oracle is geometric: an emissive
// strip 0.6 texels wide and 8 tall at face size 128, facing the probe at
// unit distance off the texel and sample grids, holds radiance L over its
// analytic solid angle, so mip 0 must store L times that angle, summed as
// each texel's radiance times its solid angle, less what the probe stores
// with the strip dark. Each face renders at 2048 texels a side
// (sgl3d-architecture.md, "A probe capture"), so each edge errs by at most
// half a sample, 1/2048 of the face's half-width: the stored energy lies
// between the strip shrunk and grown by that much on every side, about
// +-10 %, with 1 % more for half precision and the uniform box's equal
// weights within a texel. One sample per texel stores the strip 0 or 1
// texel wide, 0 or 1.67 times its energy.
#[test]
fn a_capture_stores_a_sub_texel_emitter_at_its_solid_angle() {
    use crate::asset::{CpuMesh, Vertex};
    use crate::settings::Settings;
    use crate::{Camera, FrameInput, Scene};
    use glam::Mat4;
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    const FACE_SIZE: u32 = 128;
    const RADIANCE: f32 = 4.;
    // The face's half-width is 1 at unit distance.
    let texel_width = 2. / f64::from(FACE_SIZE);
    let x = [0.0213, 0.0213 + 0.6 * texel_width];
    let y = [-0.0517, -0.0517 + 8. * texel_width];
    let mut strip = crate::test_support::cube();
    strip.meshes = vec![CpuMesh {
        vertices: [[x[0], y[0]], [x[1], y[0]], [x[1], y[1]], [x[0], y[1]]]
            .map(|[x, y]| Vertex {
                tangent: [0.; 4],
                lightmap_uv: [0.; 2],
                lightmap_bounds: [0., 0., 1., 1.],
                position: [x as f32, y as f32, -1.],
                normal: [0., 0., 1.],
                uv: [0.5; 2],
                color: [1.; 4],
            })
            .to_vec(),
        indices: vec![0, 1, 2, 0, 2, 3],
        material: 0,
        deformation: Default::default(),
    }];
    let material = &mut strip.materials[0];
    material.base = [0., 0., 0., 1.];
    material.metallic = 0.;
    material.roughness = 1.;
    material.double_sided = true;
    let mut scene = Scene::new(&device, &queue);
    let (ids, _) = crate::test_support::add_static(&device, &queue, &mut scene, strip);
    let input = FrameInput::new(Camera {
        view: Mat4::IDENTITY,
        projection: Mat4::IDENTITY,
        eye: Vec3::ZERO,
    });
    let settings = Settings::default();
    let mut renderer = Renderer::for_test(&device, &queue, [64, 64], &settings);
    // Each channel's stored energy, sum of radiance x solid angle over mip 0.
    let mut energy = |emission: f32| -> [f64; 3] {
        let mut values = scene.material(ids.materials[0]).unwrap();
        values.emission = [emission; 3];
        scene
            .set_material(&queue, ids.materials[0], values)
            .unwrap();
        let radiance = renderer
            .capture_specular_probe(
                &device,
                &queue,
                &mut scene,
                &input,
                &settings,
                Vec3::ZERO,
                FACE_SIZE,
            )
            .unwrap();
        let SpecularProbeTexels::Rgba16Float(texels) = radiance.texels else {
            unreachable!("captures return RGBA16F")
        };
        let size = FACE_SIZE as usize;
        let mut sums = [0.; 3];
        for (index, texel) in texels[..6 * size * size * 4].chunks_exact(4).enumerate() {
            let (column, row) = ((index % size) as f64, (index / size % size) as f64);
            let solid_angle = rectangle_solid_angle(
                [
                    -1. + column * texel_width,
                    -1. + (column + 1.) * texel_width,
                ],
                [-1. + row * texel_width, -1. + (row + 1.) * texel_width],
            );
            for (sum, value) in sums.iter_mut().zip(texel) {
                *sum += f64::from(crate::test_support::half(&value.to_le_bytes())) * solid_angle;
            }
        }
        sums
    };
    let dark = energy(0.);
    let lit = energy(RADIANCE);
    let half_sample = 1. / 2048.;
    let bound = |grow: f64| {
        f64::from(RADIANCE)
            * rectangle_solid_angle([x[0] - grow, x[1] + grow], [y[0] - grow, y[1] + grow])
    };
    let (least, most) = (bound(-half_sample) * 0.99, bound(half_sample) * 1.01);
    for c in 0..3 {
        let stored = lit[c] - dark[c];
        eprintln!(
            "capture: channel {c} stores {} of the strip's energy",
            stored / bound(0.)
        );
        assert!(
            (least..=most).contains(&stored),
            "channel {c}: the probe stores energy {stored}, expected {least}..{most} \
             (exactly {})",
            bound(0.)
        );
    }
}
