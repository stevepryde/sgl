//! Authored primary LOD choices under actual world-space reflection settings.
//! Geometry supplies the oracle: an outward ray from a convex sphere cannot
//! reflect that sphere; a distant plane of the same instance can reflect,
//! and a separate plane only five centimetres away must still occlude.
use super::{Function, Query};
use crate::asset::{CpuMesh, Vertex};
use crate::settings::{self, Settings};
use crate::shading::RayQueryForm;
use crate::{Camera, FrameInput, InstanceState, Mobility, Renderer, Scene, test_support};
use glam::{Mat4, Vec3};
use wgpu::util::DeviceExt;

const SIZE: [u32; 2] = [64, 64];

fn sphere(latitude: u32, longitude: u32) -> crate::asset::Asset {
    let mut asset = test_support::cube();
    let mut vertices = Vec::new();
    for y in 0..=latitude {
        let phi = std::f32::consts::PI * y as f32 / latitude as f32;
        for x in 0..=longitude {
            // No vertex lies on the camera-facing axis: the coarse surface
            // is measurably behind the base sphere at the central pixels.
            let theta = std::f32::consts::TAU * (x as f32 + 0.5) / longitude as f32;
            let p = Vec3::new(phi.sin() * theta.cos(), phi.cos(), phi.sin() * theta.sin());
            vertices.push(Vertex {
                position: p.to_array(),
                normal: p.to_array(),
                tangent: [0.; 4],
                uv: [0.; 2],
                color: [1.; 4],
                lightmap_uv: [0.; 2],
                lightmap_bounds: [0., 0., 1., 1.],
            });
        }
    }
    let mut indices = Vec::new();
    for y in 0..latitude {
        for x in 0..longitude {
            let a = y * (longitude + 1) + x;
            let b = a + longitude + 1;
            if y > 0 {
                indices.extend([a, a + 1, b]);
            }
            if y + 1 < latitude {
                indices.extend([a + 1, b + 1, b]);
            }
        }
    }
    asset.meshes = vec![CpuMesh {
        vertices,
        indices,
        material: 0,
        deformation: Default::default(),
    }];
    asset.materials[0].double_sided = true;
    asset.materials[0].metallic = 1.;
    asset.materials[0].roughness = 0.05;
    asset
}

fn trace(
    (device, queue): (&wgpu::Device, &wgpu::Queue),
    scene: &mut Scene,
    form: Option<RayQueryForm>,
    function: Function,
    receiver: [u32; 2],
    rays: &[[f32; 8]],
) -> Vec<[u32; 8]> {
    scene.prepare_frame(device, queue, Vec3::ZERO);
    if let Some(form) = form {
        scene
            .prepare_acceleration_structures(device, queue, Vec3::ZERO, form)
            .unwrap();
    }
    scene.update_rays(device, queue, u32::MAX, form.is_some());
    let query = Query::with_path(device, form);
    let input = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("LOD receiver geometric rays"),
        contents: bytemuck::cast_slice(rays),
        usage: wgpu::BufferUsages::STORAGE,
    });
    let output = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("LOD receiver geometric hits"),
        size: input.size(),
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    if form.is_some() {
        scene.encode_acceleration_structures(&mut encoder);
    }
    let tlas = form.map(|_| scene.acceleration_structures().unwrap().tlas().0);
    let bindings = query.bind(device, (&input, &output), (function, 0, receiver), tlas);
    query.trace(
        &mut encoder,
        &scene.scene_group,
        &bindings,
        rays.len() as u32,
    );
    queue.submit([encoder.finish()]);
    scene.finish_frame();
    test_support::read_words(device, queue, &output)
        .chunks_exact(8)
        .map(|words| words.try_into().unwrap())
        .collect()
}

fn plane(centre: Vec3, direction: Vec3, inverse: Mat4) -> CpuMesh {
    let u = direction.any_orthonormal_vector();
    let v = direction.cross(u);
    CpuMesh {
        vertices: [(-2., -2.), (2., -2.), (2., 2.), (-2., 2.)]
            .map(|(x, y)| Vertex {
                position: inverse.transform_point3(centre + u * x + v * y).to_array(),
                normal: [0., 0., 1.],
                tangent: [0.; 4],
                uv: [0.; 2],
                color: [1.; 4],
                lightmap_uv: [0.; 2],
                lightmap_bounds: [0., 0., 1., 1.],
            })
            .to_vec(),
        indices: vec![0, 1, 2, 0, 2, 3],
        material: 0,
        deformation: Default::default(),
    }
}

fn convex_receiver(gpu: (&wgpu::Device, &wgpu::Queue), form: Option<RayQueryForm>) {
    let (device, queue) = gpu;
    let mut scene = Scene::new(device, queue);
    let base_asset = sphere(32, 64);
    let base_triangles = base_asset.meshes[0].indices.len() as u64 / 3;
    let coarse_asset = sphere(8, 16);
    let coarse_triangles = coarse_asset.meshes[0].indices.len() as u64 / 3;
    let base = scene
        .add_asset(device, queue, base_asset.clone())
        .unwrap()
        .model;
    let coarse = scene.add_asset(device, queue, coarse_asset).unwrap().model;
    // Every coarse face plane is within 0.037363 m of the unit sphere
    // (its geometric chord sagitta); 0.04 bounds the authored alternative.
    let alternatives = || {
        vec![crate::lod::MeshLod {
            model: coarse,
            mesh: 0,
            max_error: 0.04,
        }]
    };
    scene.set_mesh_lods(base, 0, alternatives()).unwrap();
    let centre = Vec3::new(0., 0., -150.);
    let pose = Mat4::from_translation(centre) * Mat4::from_scale(Vec3::new(10., 7., 8.));
    let instance = scene
        .add_instance(
            device,
            queue,
            InstanceState {
                pose,
                ..InstanceState::new(base)
            },
            Mobility::Static,
        )
        .unwrap();
    // Isolate CPU blended counts in the moving population; its coarse
    // geometry is selected by a different builder from the opaque sphere.
    let mut blended_asset = base_asset.clone();
    blended_asset.materials[0].alpha = crate::AlphaMode::Blend {
        receives_screen_space_reflections: true,
        keeps_specular: false,
    };
    let blended = scene.add_asset(device, queue, blended_asset).unwrap().model;
    scene.set_mesh_lods(blended, 0, alternatives()).unwrap();
    scene
        .add_instance(
            device,
            queue,
            InstanceState {
                pose: Mat4::from_translation(Vec3::new(30., 0., 0.)) * pose,
                ..InstanceState::new(blended)
            },
            Mobility::Moving,
        )
        .unwrap();
    let mut settings = Settings {
        antialiasing: settings::Antialiasing::Off,
        atmosphere: false,
        bloom: settings::Bloom::Off,
        occlusion_culling: false,
        hardware_ray_tracing: form.is_some(),
        screen_space_reflections: settings::ScreenSpaceReflections::Half,
        world_space_reflections: settings::WorldSpaceReflections::Off,
        ..Settings::default()
    };
    let mut renderer = Renderer::for_test(device, queue, SIZE, &settings);
    let projection = crate::perspective(1., 1., 0.1);
    let input = FrameInput::new(Camera {
        view: Mat4::IDENTITY,
        projection,
        eye: Vec3::ZERO,
    });
    let output = crate::view::targets::target(
        device,
        "LOD ray receiver",
        SIZE,
        crate::shading::gbuffer::COLOR,
    );
    // The final case asks for world rays, but disables their prerequisite:
    // effective configuration must preserve primary LOD while no rays run.
    for (reach, screen, active) in [
        (
            settings::WorldSpaceReflections::Off,
            settings::ScreenSpaceReflections::Half,
            false,
        ),
        (
            settings::WorldSpaceReflections::All,
            settings::ScreenSpaceReflections::Half,
            true,
        ),
        (
            settings::WorldSpaceReflections::Moving,
            settings::ScreenSpaceReflections::Half,
            true,
        ),
        (
            settings::WorldSpaceReflections::All,
            settings::ScreenSpaceReflections::Off,
            false,
        ),
    ] {
        settings.world_space_reflections = reach;
        settings.screen_space_reflections = screen;
        let mut encoder = device.create_command_encoder(&Default::default());
        renderer.render(
            device,
            queue,
            &mut encoder,
            &mut scene,
            &input,
            &settings,
            &output,
            None,
        );
        queue.submit([encoder.finish()]);
        renderer.finish_frame(&mut scene);
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        let stats = renderer
            .geometry_stats(device)
            .expect("completed GPU/CPU draw counts");
        let expected = if active {
            base_triangles
        } else {
            coarse_triangles
        };
        assert_eq!(
            stats.static_instances.1, expected,
            "{reach:?}/{screen:?}: GPU opaque geometry must match the active ray representation"
        );
        assert_eq!(
            stats.moving_instances.1, expected,
            "{reach:?}/{screen:?}: CPU blended geometry follows the same primary decision"
        );
        let ids = test_support::read(device, queue, renderer.targets().source_id.texture(), 8);
        let depths = test_support::read(device, queue, renderer.targets().depth.texture(), 4);
        let normals = test_support::read(device, queue, renderer.targets().normal.texture(), 8);
        let mut sphere_rays = Vec::new();
        let mut receivers = Vec::new();
        let mut least_cosine = 1f32;
        for (pixel, id) in bytemuck::cast_slice::<u8, [u32; 2]>(&ids)
            .iter()
            .enumerate()
        {
            if id[0] != crate::diagnostics::source_id(instance) {
                continue;
            }
            let x = (pixel % SIZE[0] as usize) as f32 + 0.5;
            let y = (pixel / SIZE[0] as usize) as f32 + 0.5;
            let depth = f32::from_le_bytes(depths[pixel * 4..pixel * 4 + 4].try_into().unwrap());
            let point = projection.inverse().project_point3(Vec3::new(
                x / 64. * 2. - 1.,
                1. - y / 64. * 2.,
                depth,
            ));
            let packed = &normals[pixel * 8..pixel * 8 + 4];
            let ox = test_support::half(&packed[..2]);
            let oy = test_support::half(&packed[2..]);
            // Standard octahedral coordinates of the actual G-buffer
            // normal, including its half-float quantisation. The oracle
            // is convex geometry, not this normal reconstruction.
            let mut normal = Vec3::new(ox, oy, 1. - ox.abs() - oy.abs());
            if normal.z < 0. {
                normal.x = (1. - oy.abs()) * ox.signum();
                normal.y = (1. - ox.abs()) * oy.signum();
            }
            normal = normal.normalize();
            let incoming = point.normalize();
            let direction = incoming - 2. * incoming.dot(normal) * normal;
            let outward = ((point - centre) / Vec3::new(100., 49., 64.)).normalize();
            let cosine = direction.dot(outward);
            assert!(
                cosine > 0.,
                "the reflection leaves the analytic convex receiver at pixel {pixel}"
            );
            least_cosine = least_cosine.min(cosine);
            sphere_rays.push([
                point.x,
                point.y,
                point.z,
                0.01,
                direction.x,
                direction.y,
                direction.z,
                20.,
            ]);
            receivers.push(*id);
        }
        assert!(
            sphere_rays.len() >= 30,
            "exercise a useful sphere receiver region: {} pixels",
            sphere_rays.len()
        );
        assert!(
            least_cosine < 0.4,
            "include grazing reflected rays, least cosine {least_cosine}"
        );
        // Every base-surface ray is independently expected to miss: the
        // reflected directions leave a convex surface. t_min handles the
        // depth/normal reconstruction's roundoff; exact-triangle exclusion
        // remains intact because raster and the base BVH/BLAS now match.
        let mut self_hits = 0;
        for (&ray, &receiver) in sphere_rays.iter().zip(&receivers) {
            let hit = trace(
                gpu,
                &mut scene,
                form,
                Function::NearestExceptReceiver,
                receiver,
                &[ray],
            )[0];
            self_hits += usize::from(hit[0] != 0);
            if active {
                assert_eq!(
                    hit[0],
                    0,
                    "{form:?}/{reach:?}: outward sphere ray self-hit at {}",
                    f32::from_bits(hit[4])
                );
                let visible = trace(
                    gpu,
                    &mut scene,
                    form,
                    Function::StaticVisible,
                    receiver,
                    &[ray],
                )[0];
                assert_ne!(
                    visible[0], 0,
                    "Moving reach must not be blocked by its own base receiver"
                );
            }
        }
        if !active {
            assert!(
                self_hits > 0,
                "the coarse surface must reproduce the original base-shell self-hit"
            );
        } else if reach == settings::WorldSpaceReflections::All {
            let ray = sphere_rays[sphere_rays.len() / 2];
            let receiver = receivers[receivers.len() / 2];
            let p = Vec3::new(ray[0], ray[1], ray[2]);
            let direction = Vec3::new(ray[4], ray[5], ray[6]);
            let mut other = test_support::cube();
            other.materials[0].double_sided = true;
            other.meshes = vec![plane(p + direction * 0.05, direction, Mat4::IDENTITY)];
            let other_model = scene.add_asset(device, queue, other).unwrap().model;
            let other_instance = scene
                .add_instance(
                    device,
                    queue,
                    InstanceState::new(other_model),
                    Mobility::Static,
                )
                .unwrap();
            let hit = trace(
                gpu,
                &mut scene,
                form,
                Function::NearestExceptReceiver,
                receiver,
                &[ray],
            )[0];
            assert_eq!(
                hit[1] + 1,
                crate::diagnostics::source_id(other_instance),
                "a nearby other surface still reflects"
            );
            assert_ne!(hit[0], 0);
            assert!(
                (f32::from_bits(hit[4]) - 0.05).abs() < 0.001,
                "the independently placed nearby plane is the hit"
            );
            let visibility = trace(
                gpu,
                &mut scene,
                form,
                Function::StaticVisible,
                receiver,
                &[ray],
            )[0];
            assert_eq!(
                visibility[0], 0,
                "the nearby static plane must block visibility"
            );
            scene.remove_instance(other_instance).unwrap();
            let mut concave = base_asset.clone();
            concave
                .meshes
                .push(plane(p + direction * 4., direction, pose.inverse()));
            let concave_model = scene.add_asset(device, queue, concave).unwrap().model;
            scene
                .set_mesh_lods(concave_model, 0, alternatives())
                .unwrap();
            scene
                .set_instance(
                    queue,
                    instance,
                    InstanceState {
                        pose,
                        ..InstanceState::new(concave_model)
                    },
                )
                .unwrap();
            let distant = trace(
                gpu,
                &mut scene,
                form,
                Function::NearestExceptReceiver,
                receiver,
                &[ray],
            )[0];
            assert_ne!(distant[0], 0);
            assert_eq!(
                (distant[1] + 1, distant[2]),
                (receiver[0], 1),
                "a distant part of the same instance still reflects"
            );
            assert!(
                (f32::from_bits(distant[4]) - 4.).abs() < 0.001,
                "the independently placed distant plane is the hit"
            );
            scene
                .set_instance(
                    queue,
                    instance,
                    InstanceState {
                        pose,
                        ..InstanceState::new(base)
                    },
                )
                .unwrap();
        }
    }
}

#[test]
fn portable_primary_lod_matches_world_reflection_geometry() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    convex_receiver((&device, &queue), None);
}

#[test]
fn hardware_primary_lod_matches_world_reflection_geometry() {
    let Some((device, queue)) = test_support::ray_tracing_device(|limits| limits) else {
        return;
    };
    for form in [RayQueryForm::Baseline, RayQueryForm::Candidates] {
        convex_receiver((&device, &queue), Some(form));
    }
}
