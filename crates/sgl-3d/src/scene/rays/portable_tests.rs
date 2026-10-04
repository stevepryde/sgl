//! Real feature-free GPU queries versus independent world-space plane/edge tests.
use super::tests::{Fixture, Pose, asset, triangle};
use super::*;
use crate::asset::Asset;
use glam::{DMat4, DVec3, Quat, Vec3};
use wgpu::util::DeviceExt;

fn query(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    scene: &mut Fixture,
    poses: &[Pose],
    rays: &[[f32; 8]],
) -> Vec<[u32; 8]> {
    let input = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("portable independent fixture rays"),
        contents: bytemuck::cast_slice(rays),
        usage: wgpu::BufferUsages::STORAGE,
    });
    let hits = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: input.size(),
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: input.size(),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let bindings = scene.query.ray_bind_group(device, &input, &hits);
    let mut encoder = device.create_command_encoder(&Default::default());
    scene.update(device, queue, poses);
    let group = scene.scene_group(device);
    scene
        .query
        .trace(&mut encoder, &group, &bindings, rays.len() as u32);
    encoder.copy_buffer_to_buffer(&hits, 0, &readback, 0, hits.size());
    // Drop application handles before completion: the encoded GPU work and bind
    // groups must retain the allocations actually used by the dispatch.
    drop(bindings);
    drop(input);
    drop(hits);
    queue.submit([encoder.finish()]);
    let (tx, rx) = std::sync::mpsc::channel();
    readback.map_async(wgpu::MapMode::Read, .., move |result| {
        tx.send(result).unwrap()
    });
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    rx.recv().unwrap().unwrap();
    let result = bytemuck::cast_slice(&readback.get_mapped_range(..)).to_vec();
    readback.unmap();
    result
}

#[test]
fn portable_scene_exact_intervals_parallel_axes_and_instance_removal() {
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    pollster::block_on(async {
        let asset = asset(vec![triangle(0., -2., false, 0)], true);
        let mut scene = Fixture::new(&device, &queue, &[&asset]);
        let mut poses = vec![Pose {
            model: 0,
            world: Mat4::IDENTITY,
            id: 50,
        }];
        let rays = [
            [0., 0., 0., 0., 0., 0., -1., 2.], // exact far endpoint
            [0., 0., 0., 2., 0., 0., -1., 3.], // exact near endpoint
            [0., 0., 0., 2., 0., 0., -1., 2.], // degenerate closed interval
            [
                0.,
                0.,
                0.,
                0.,
                0.,
                0.,
                -1.,
                f32::from_bits(2f32.to_bits() - 1),
            ],
            [
                0.,
                0.,
                0.,
                f32::from_bits(2f32.to_bits() + 1),
                0.,
                0.,
                -1.,
                3.,
            ],
            [0., 0., -2., 0., 0., 0., -1., 1.], // origin on plane
            [0., 0., -2., f32::EPSILON, 0., 0., -1., 1.],
            [0., 0., -2., 0., 1., 0., 0., 10.], // coplanar / parallel
            [0., 0., 0., 0., 0., 0., -2., 2.],  // non-unit ray t=1
            [2., 0., 0., 0., 0., 0., -1., 4.],  // parallel slab outside
            [0., 0., 0., 0., f32::MIN_POSITIVE, 0., -1., 3.],
            [f32::NAN, 0., 0., 0., 0., 0., -1., 3.],
            [0., 0., 0., 0., 0., 0., f32::INFINITY, 3.],
            [0., 0., 0., 3., 0., 0., -1., 2.],
            [0., 0., 0., 0., 0., 0., -1e-9, 3e9], // no determinant epsilon
            [-1., -1., 0., 0., 0., 0., -1., 3.],  // exact vertex
            [0., -1., 0., 0., 0., 0., -1., 3.],   // exact edge
        ];
        let results = query(&device, &queue, &mut scene, &poses, &rays);
        let expected = [
            Some(2.),
            Some(2.),
            Some(2.),
            None,
            None,
            Some(0.),
            None,
            None,
            Some(1.),
            None,
            Some(2.),
            None,
            None,
            None,
            Some(2e9),
            Some(2.),
            Some(2.),
        ];
        for (i, (actual, expected)) in results.iter().zip(expected).enumerate() {
            assert_eq!(actual[0] != 0, expected.is_some(), "ray {i}: {actual:?}");
            if let Some(t) = expected {
                if i == 14 {
                    // Near-parallel solve amplifies f32 arithmetic rounding;
                    // distance remains within two ULPs at this parameter scale.
                    assert!((f32::from_bits(actual[4]) - t).abs() <= 256., "ray {i}");
                } else {
                    assert_eq!(f32::from_bits(actual[4]), t, "ray {i}");
                }
            }
        }
        poses[0].world = Mat4::from_scale_rotation_translation(
            Vec3::new(-2., 0.5, 3.),
            Quat::IDENTITY,
            Vec3::new(0., 0., 1.),
        );
        let moved = query(
            &device,
            &queue,
            &mut scene,
            &poses,
            &[[0., 0., 0., 0., 0., 0., -1., 10.]],
        );
        assert!((f32::from_bits(moved[0][4]) - 5.).abs() < 1e-5);
        let removed = query(&device, &queue, &mut scene, &[], &rays);
        assert!(
            removed.iter().all(|hit| hit[0] == 0),
            "removed instances must not survive in unused allocation slots"
        );
    });
}

// Different numerical algorithm from WGSL: intersect each WORLD triangle's plane,
// then test oriented edges at the hit, with no inverse ray, BVH or MT bary solve.
fn oracle(assets: &[Asset], poses: &[Pose], ray: [f32; 8]) -> Option<(f64, usize, u32, u32)> {
    let origin = DVec3::new(ray[0] as f64, ray[1] as f64, ray[2] as f64);
    let direction = DVec3::new(ray[4] as f64, ray[5] as f64, ray[6] as f64);
    let mut closest = None;
    let mut maximum = ray[7] as f64;
    for (slot, instance) in poses.iter().enumerate() {
        let world = DMat4::from_cols_array(&instance.world.to_cols_array().map(f64::from));
        for (mesh_id, mesh) in assets[instance.model].meshes.iter().enumerate() {
            for (primitive, indices) in mesh.indices.chunks_exact(3).enumerate() {
                let mut p = [0, 1, 2].map(|i| {
                    world.transform_point3(DVec3::from_array(
                        mesh.vertices[indices[i] as usize].position.map(f64::from),
                    ))
                });
                // Bake the pose with the loader's authored-side convention:
                // mirrored geometry reverses indices. The oracle still solves
                // world-space planes/edges independently of object-space MT.
                if world.determinant() < 0. {
                    p.swap(1, 2);
                }
                let normal = (p[1] - p[0]).cross(p[2] - p[0]);
                let denom = normal.dot(direction);
                if denom == 0.
                    || (denom >= 0.
                        && !assets[instance.model].materials[mesh.material].double_sided)
                {
                    continue;
                }
                let t = normal.dot(p[0] - origin) / denom;
                if t < ray[3] as f64 || t > maximum {
                    continue;
                }
                let point = origin + direction * t;
                if (0..3).all(|i| (p[(i + 1) % 3] - p[i]).cross(point - p[i]).dot(normal) >= 0.) {
                    maximum = t;
                    closest = Some((t, slot, mesh_id as u32, primitive as u32));
                }
            }
        }
    }
    closest
}

#[test]
fn portable_scene_randomized_hierarchy_against_world_f64_oracle() {
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    pollster::block_on(async {
        let meshes = (0..513)
            .map(|i| {
                let mut mesh = triangle(
                    (i % 19) as f32 * 1.3 - 12.,
                    -(i % 7) as f32 * 0.8 - 1.,
                    i % 3 == 0,
                    0,
                );
                for vertex in &mut mesh.vertices {
                    vertex.position[1] += (i / 19) as f32 * 0.9 - 12.;
                }
                mesh
            })
            .collect();
        let assets = vec![
            asset(meshes, false),
            asset(vec![triangle(0., -2., false, 0)], true),
        ];
        let mut scene = Fixture::new(&device, &queue, &assets.iter().collect::<Vec<_>>());
        let mut poses = vec![
            Pose {
                model: 0,
                world: Mat4::from_scale_rotation_translation(
                    Vec3::new(1.3, 0.7, 2.),
                    Quat::from_rotation_y(0.23),
                    Vec3::new(2., 1., -5.),
                ),
                id: 77,
            },
            Pose {
                model: 0,
                world: Mat4::from_scale_rotation_translation(
                    Vec3::new(-0.8, 1.2, 0.4),
                    Quat::from_rotation_x(-0.13),
                    Vec3::new(-3., 0., -7.),
                ),
                id: 88,
            },
            Pose {
                model: 1,
                world: Mat4::from_translation(Vec3::new(0., 0., -1.)),
                id: 99,
            },
        ];
        // Fixed PRNG solely chooses physical cases; it does not derive expected hits.
        let mut state = 0x2a97_8413u32;
        let mut random = || {
            state = state.wrapping_mul(1664525).wrapping_add(1013904223);
            (state >> 8) as f32 / 16_777_216.
        };
        let rays: Vec<_> = (0..2048)
            .map(|_| {
                [
                    random() * 44. - 22.,
                    random() * 34. - 17.,
                    3.,
                    random() * 0.1,
                    (random() - 0.5) * 0.25,
                    (random() - 0.5) * 0.25,
                    -0.5 - random() * 2.,
                    random() * 30. + 0.5,
                ]
            })
            .collect();
        for frame in 0..3 {
            if frame == 1 {
                poses[0].world = Mat4::from_translation(Vec3::new(4., -1., 2.)) * poses[0].world;
            }
            if frame == 2 {
                poses.swap(0, 1);
                poses.pop();
            }
            let results = query(&device, &queue, &mut scene, &poses, &rays);
            let mut hits = 0;
            for (index, (&ray, actual)) in rays.iter().zip(&results).enumerate() {
                let expected = oracle(&assets, &poses, ray);
                assert_eq!(
                    actual[0] != 0,
                    expected.is_some(),
                    "frame={frame}, ray={index}, actual={actual:?}, expected={expected:?}"
                );
                if let Some((t, slot, mesh, primitive)) = expected {
                    hits += 1;
                    assert!(
                        (f32::from_bits(actual[4]) as f64 - t).abs() < 0.0001,
                        "frame={frame}, ray={index}: GPU t={}, oracle {t}",
                        f32::from_bits(actual[4])
                    );
                    assert_eq!(
                        &actual[1..4],
                        &[slot as u32, mesh, primitive],
                        "frame={frame}, ray={index}"
                    );
                }
            }
            eprintln!(
                "frame {frame}: {hits} independently confirmed hits / {} rays",
                rays.len()
            );
        }
    });
}

#[test]
fn portable_scene_fragment_segment_visibility() {
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    pollster::block_on(async {
        let asset = asset(vec![triangle(0., -2., false, 0)], false);
        let mut scene = Fixture::new(&device, &queue, &[&asset]);
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("fragment-stage physical segment fixture"),
            source: wgpu::ShaderSource::Wgsl(format!("{}\n{}",crate::shading::compose(&[&crate::shading::SCENE_RAYS_PORTABLE]),r#"
@vertex fn vertex(@builtin(vertex_index) index:u32)->@builtin(position) vec4<f32> {
 let points=array<vec2<f32>,3>(vec2(-1.,-1.),vec2(3.,-1.),vec2(-1.,3.));
 return vec4(points[index],0.,1.);
}
@fragment fn fragment(@builtin(position) position:vec4<f32>)->@location(0) vec4<f32> {
 let pixel=u32(position.x);
 let origin=vec3(select(0.,2.,pixel==1u),0.,0.);
 let maximum=select(select(3.,1.,pixel==2u),2.,pixel==3u);
 let direction=vec3(0.,0.,-1.);
 let visible=scene_segment_visible(origin,direction,0.,maximum);
 let hit=scene_decode_hit(scene_trace_nearest(SceneRay(vec4(origin,0.),vec4(direction,maximum))),origin,direction);
 return vec4(select(0.,1.,visible),f32(hit.instance_id),hit.distance,select(0.,1.,hit.hit));
}
"#).into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[None, Some(&crate::shading::bind::scene(&device))],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: None,
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vertex"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fragment"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba32Float,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: 4,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 256,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        scene.update(
            &device,
            &queue,
            &[Pose {
                model: 0,
                world: Mat4::IDENTITY,
                id: 51,
            }],
        );
        let group = scene.scene_group(&device);
        {
            let view = texture.create_view(&Default::default());
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(1, &group, &[]);
            pass.draw(0..3, 0..1);
        }
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(256),
                    rows_per_image: Some(1),
                },
            },
            texture.size(),
        );
        queue.submit([encoder.finish()]);
        let (tx, rx) = std::sync::mpsc::channel();
        readback.map_async(wgpu::MapMode::Read, .., move |r| tx.send(r).unwrap());
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        rx.recv().unwrap().unwrap();
        let mapped = readback.get_mapped_range(..);
        let pixels: &[[f32; 4]] = bytemuck::cast_slice(&mapped[..64]);
        assert_eq!(
            pixels,
            &[
                [0., 51., 2., 1.],
                [1., 0., 0., 0.],
                [1., 0., 0., 0.],
                [0., 51., 2., 1.]
            ]
        );
    });
}

#[test]
fn portable_scene_material_edits_restore_near_and_far_occluders() {
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    pollster::block_on(async {
        // The near plane presents its authored back face; the farther one its front.
        let mut asset = asset(
            vec![triangle(0., -2., true, 0), triangle(0., -4., false, 1)],
            false,
        );
        asset.materials.push(asset.materials[0].clone());
        let mut scene = Fixture::new(&device, &queue, &[&asset]);
        let poses = [Pose {
            model: 0,
            world: Mat4::IDENTITY,
            id: 11,
        }];
        let mut materials: Vec<_> = asset
            .materials
            .iter()
            .map(crate::SurfaceMaterial::authored)
            .collect();
        for (double_sided, group, enabled, distance) in [
            (false, 0, 0, 4.),
            (true, 0, 0, 2.),
            (true, 0, 0, 2.),
            (true, 1, 0, 4.),
            (true, 1, 1, 2.),
            (false, 0, 0, 4.),
            (true, 0, 0, 2.),
        ] {
            materials[0].double_sided = double_sided;
            materials[0].visibility_group = group;
            for (&word, material) in scene.materials[0].iter().zip(&materials) {
                let uniform = MaterialUniform::new(material, Default::default());
                scene.rays.write_material(&queue, word, &uniform);
            }
            scene.rays.set_visibility_mask(&queue, enabled);
            let hits = query(
                &device,
                &queue,
                &mut scene,
                &poses,
                &[[0., 0., 0., 0.001, 0., 0., -1., 8.]],
            );
            assert_ne!(
                hits[0][0], 0,
                "at least the far front face must remain visible"
            );
            assert!(
                (f32::from_bits(hits[0][4]) - distance).abs() < 0.00001,
                "wrong accepted plane after sidedness={double_sided}, group={group}, mask={enabled}"
            );
        }
    });
}
