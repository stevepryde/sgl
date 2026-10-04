//! Clusters judged by brute force, through the production assignment,
//! upload and shader lookup.
use super::*;
use crate::scene::buffer;
use crate::shading::{self, bind::group0, uniforms::ViewUniform};
use crate::{Decal, DecalImageId, Light, LightShape, Scene};
use glam::camera;
use glam::{Quat, Vec2, Vec3, Vec4Swizzles};

/// A small deterministic generator.
struct Random(u32);

impl Random {
    fn unit(&mut self) -> f32 {
        self.0 = self.0.wrapping_mul(1664525).wrapping_add(1013904223);
        (self.0 >> 8) as f32 / (1u32 << 24) as f32
    }
    fn range(&mut self, low: f32, high: f32) -> f32 {
        low + (high - low) * self.unit()
    }
    fn direction(&mut self) -> Vec3 {
        loop {
            let v = Vec3::new(
                self.range(-1., 1.),
                self.range(-1., 1.),
                self.range(-1., 1.),
            );
            if (0.01..=1.).contains(&v.length_squared()) {
                return v.normalize();
            }
        }
    }
}

/// A random light around `center`, within `extent` on each axis.
fn random_light(random: &mut Random, center: Vec3, extent: Vec3) -> Light {
    let kind = random.unit();
    let shape = if kind < 1. / 3. {
        LightShape::Point
    } else if kind < 2. / 3. {
        let outer_angle = random.range(0.05, 1.55);
        LightShape::Spot {
            direction: random.direction() * random.range(0.5, 3.),
            inner_angle: outer_angle * random.unit(),
            outer_angle,
        }
    } else {
        LightShape::Rect {
            direction: random.direction() * random.range(0.5, 3.),
            width_axis: random.direction(),
            width: random.range(0.1, 5.),
            height: random.range(0.1, 5.),
        }
    };
    Light {
        position: center
            + extent
                * Vec3::new(
                    random.range(-1., 1.),
                    random.range(-1., 1.),
                    random.range(-1., 1.),
                ),
        shape,
        color: [1.; 3],
        intensity: 1.,
        range: random.range(0.3, 30.),
        baked: random.unit() < 0.3,
        specular: 1.,
        casts_shadow: false,
    }
}

/// Whether `light` reaches `point`, with a margin that keeps floating-point
/// ties at the edge of its range or cone from deciding: inside its range
/// and, for a spot, inside its outer cone, where Filament's attenuation is
/// not zero, and for a rectangle, in front of its face, which Bevy's
/// `rect_light` lights.
fn reaches(light: &Light, point: Vec3) -> bool {
    let offset = point - light.position;
    if offset.length() >= light.range * 0.999 {
        return false;
    }
    match light.shape {
        LightShape::Point => true,
        LightShape::Spot {
            direction,
            outer_angle,
            ..
        } => {
            offset.length() > 1e-3
                && offset
                    .normalize()
                    .dot(direction.normalize())
                    .clamp(-1., 1.)
                    .acos()
                    < outer_angle - 1e-3
        }
        LightShape::Rect { direction, .. } => offset.dot(direction.normalize()) > 1e-3,
    }
}

/// A box of `size` at `position`, turned by `rotation`; `judge` gives it
/// its image.
fn decal(position: Vec3, rotation: Quat, size: Vec3) -> Decal {
    Decal {
        position,
        rotation,
        size,
        base_color: DecalImageId::issue(0, 0),
        normal: None,
        metallic_roughness: None,
        color: [1.; 4],
        base_color_mix: 1.,
        upper_fade: 0.,
        lower_fade: 0.,
        normal_fade: 0.,
    }
}

/// A random decal box around `center`, within `extent` on each axis, any
/// way round: from a thin marking to a deep box.
fn random_decal(random: &mut Random, center: Vec3, extent: Vec3) -> Decal {
    decal(
        center
            + extent
                * Vec3::new(
                    random.range(-1., 1.),
                    random.range(-1., 1.),
                    random.range(-1., 1.),
                ),
        Quat::from_axis_angle(random.direction(), random.range(0., 6.3)),
        Vec3::new(
            random.range(0.1, 12.),
            random.range(0.05, 6.),
            random.range(0.1, 12.),
        ),
    )
}

/// A point inside `decal`'s box, uniformly.
fn point_in(random: &mut Random, decal: &Decal) -> Vec3 {
    let local = Vec3::new(
        random.range(-0.5, 0.5),
        random.range(-0.5, 0.5),
        random.range(-0.5, 0.5),
    ) * decal.size;
    decal.position + decal.rotation * local
}

/// Whether `point` is inside `decal`'s box, short of its faces by a margin
/// that keeps floating-point ties there from deciding.
fn inside(decal: &Decal, point: Vec3) -> bool {
    let local = decal.rotation.inverse() * (point - decal.position);
    local.abs().cmplt(decal.size * 0.5 * 0.999).all()
}

/// One camera to judge: its view and projection at `screen` pixels.
struct Camera {
    label: &'static str,
    view: Mat4,
    projection: Mat4,
    screen: [u32; 2],
}

/// A point the camera sees and the pixel it lands on, or None.
fn seen(camera: &Camera, point: Vec3) -> Option<[f32; 2]> {
    let view = camera.view.transform_point3(point);
    let orthographic = camera.projection.w_axis.w == 1.;
    let clip = camera.projection * view.extend(1.);
    let ndc = clip.xyz() / clip.w;
    let inside = clip.w > 0.
        && ndc.x.abs() < 1.
        && ndc.y.abs() < 1.
        && if orthographic {
            (0.0..1.0).contains(&ndc.z)
        } else {
            ndc.z > 0. && ndc.z < 1.
        };
    inside.then(|| {
        [
            (ndc.x * 0.5 + 0.5) * camera.screen[0] as f32,
            (0.5 - ndc.y * 0.5) * camera.screen[1] as f32,
        ]
    })
}

/// Points the camera sees: some along random pixels' rays, most near the
/// lights and inside the decals, where the test has teeth.
fn samples(
    random: &mut Random,
    camera: &Camera,
    lights: &[Light],
    decals: &[Decal],
) -> Vec<(Vec3, [f32; 2])> {
    let inverse = (camera.projection * camera.view).inverse();
    let orthographic = camera.projection.w_axis.w == 1.;
    let mut samples = Vec::new();
    while samples.len() < 512 {
        let ndc = Vec2::new(random.range(-1., 1.), random.range(-1., 1.));
        // Reversed Z: a depth spread over many orders of magnitude.
        let depth = if orthographic {
            random.unit()
        } else {
            10f32.powf(random.range(-4.5, 0.))
        };
        let point = inverse.project_point3(ndc.extend(depth));
        if let Some(pixel) = seen(camera, point) {
            samples.push((point, pixel));
        }
    }
    let mut attempts = 0;
    while samples.len() < 4096 && attempts < 200_000 {
        attempts += 1;
        let pick =
            |random: &mut Random, count: usize| (random.unit() * count as f32) as usize % count;
        let point = if decals.is_empty() || (!lights.is_empty() && random.unit() < 0.5) {
            let light = &lights[pick(random, lights.len())];
            light.position + random.direction() * light.range * random.unit().sqrt()
        } else {
            let decal = decals[pick(random, decals.len())];
            point_in(random, &decal)
        };
        if let Some(pixel) = seen(camera, point) {
            samples.push((point, pixel));
        }
    }
    samples
}

/// The production shader's cluster range for each sample.
fn look_up(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    camera: &Camera,
    clusters: &Clusters,
    samples: &[(Vec3, [f32; 2])],
) -> Vec<[u32; 4]> {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("cluster lookup"),
        source: wgpu::ShaderSource::Wgsl(
            format!(
                r#"{}
@group(0) @binding(3) var<storage,read> samples:array<vec4<f32>>;
@group(0) @binding(4) var<storage,read_write> ranges:array<vec4<u32>>;
@compute @workgroup_size(64) fn observe(@builtin(global_invocation_id) id:vec3<u32>) {{
 if id.x<arrayLength(&ranges) {{
  let range=cluster_range(samples[id.x*2u].xyz,samples[id.x*2u+1u].xy);
  ranges[id.x]=vec4(range.first,range.live,range.baked,range.decals);
 }}
}}
"#,
                shading::compose(&[&shading::BIND_LIT, &shading::CLUSTERS])
            )
            .into(),
        ),
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: None,
        layout: None,
        module: &shader,
        entry_point: Some("observe"),
        compilation_options: Default::default(),
        cache: None,
    });
    let view = ViewUniform {
        view: camera.view.to_cols_array_2d(),
        ..bytemuck::Zeroable::zeroed()
    };
    let view = buffer(
        device,
        "lookup view",
        bytemuck::bytes_of(&view),
        wgpu::BufferUsages::UNIFORM,
    );
    let points: Vec<[f32; 4]> = samples
        .iter()
        .flat_map(|(point, pixel)| [point.extend(1.).to_array(), [pixel[0], pixel[1], 0., 0.]])
        .collect();
    let points = buffer(
        device,
        "lookup samples",
        bytemuck::cast_slice(&points),
        wgpu::BufferUsages::STORAGE,
    );
    let bytes = (samples.len() * 16) as u64;
    let ranges = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: bytes,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: bytes,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[
            wgpu::BindGroupEntry {
                binding: group0::VIEW,
                resource: view.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: group0::CLUSTERS,
                resource: clusters.buffer().as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: points.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 4,
                resource: ranges.as_entire_binding(),
            },
        ],
    });
    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_compute_pass(&Default::default());
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &group, &[]);
        pass.dispatch_workgroups((samples.len() as u32).div_ceil(64), 1, 1);
    }
    encoder.copy_buffer_to_buffer(&ranges, 0, &readback, 0, bytes);
    queue.submit([encoder.finish()]);
    let (send, receive) = std::sync::mpsc::channel();
    readback
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            send.send(result).unwrap()
        });
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    receive.recv().unwrap().unwrap();
    bytemuck::cast_slice(&readback.slice(..).get_mapped_range()).to_vec()
}

/// Clusters `lights` and `decals` for `camera` as a frame does and checks
/// every sample.
fn judge(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    random: &mut Random,
    camera: &Camera,
    lights: &[Light],
    decals: &[Decal],
) {
    let mut scene = Scene::new(device, queue);
    let ids: Vec<_> = lights
        .iter()
        .map(|light| scene.add_light(device, queue, *light).unwrap())
        .collect();
    let blank = scene
        .add_decal_image(crate::asset::Image::Rgba8(image::RgbaImage::new(1, 1)))
        .unwrap();
    let decal_ids: Vec<_> = decals
        .iter()
        .map(|decal| {
            let decal = Decal {
                base_color: blank,
                ..*decal
            };
            scene.add_decal(device, queue, decal).unwrap()
        })
        .collect();
    let mut clusters = Clusters::new(device, "judged clusters");
    clusters.cluster(
        device,
        queue,
        &scene.lights,
        true,
        &scene.decals,
        camera.view,
        camera.projection,
        camera.screen,
        CAMERA_CLUSTERS,
    );
    let samples = samples(random, camera, lights, decals);
    let ranges = look_up(device, queue, camera, &clusters, &samples);
    let (mut reached, mut covered) = (0, 0);
    for ((point, pixel), [first, live, baked, decal_count]) in samples.iter().zip(ranges) {
        let first = first as usize;
        let live_end = first + live as usize;
        let baked_end = live_end + baked as usize;
        let live_lights = &clusters.data[first..live_end];
        let baked_lights = &clusters.data[live_end..baked_end];
        let listed_decals = &clusters.data[baked_end..baked_end + decal_count as usize];
        for (light, id) in lights.iter().zip(&ids) {
            if !reaches(light, *point) {
                continue;
            }
            reached += 1;
            let listed = if light.baked {
                baked_lights
            } else {
                live_lights
            };
            assert!(
                listed.contains(&(id.index() as u32)),
                "{}: {light:?} reaches {point} (pixel {pixel:?}) but is not in its cluster's {} lights",
                camera.label,
                if light.baked { "baked" } else { "live" },
            );
        }
        for (decal, id) in decals.iter().zip(&decal_ids) {
            if !inside(decal, *point) {
                continue;
            }
            covered += 1;
            assert!(
                listed_decals.contains(&(id.index() as u32)),
                "{}: {decal:?} holds {point} (pixel {pixel:?}) but is not in its cluster's decals",
                camera.label,
            );
        }
    }
    // Most samples were drawn near lights and inside decals, so the checks
    // above are not vacuous.
    if !lights.is_empty() {
        assert!(
            reached >= 256,
            "{}: only {reached} light-point pairs",
            camera.label
        );
    }
    if !decals.is_empty() {
        assert!(
            covered >= 256,
            "{}: only {covered} decal-point pairs",
            camera.label
        );
    }
}

// Plausible defects: a plane, slice or tile that the CPU assignment and the
// shader's lookup place differently; a sphere refinement or cone test that
// drops a cluster a light reaches; a decal's bound that misses a corner of
// its box; a far slice that ends before a light or decal; live lights,
// baked lights and decals in each other's range. The oracle is a
// brute-force distance and angle test of every light, and a containment
// test of every decal's box, against points the camera sees.
#[test]
fn every_light_and_decal_reaching_a_point_is_in_its_cluster() {
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    let mut random = Random(0x2545_f491);
    for (index, screen) in [[1920, 1080], [1280, 720], [333, 777], [64, 48], [1, 1]]
        .into_iter()
        .enumerate()
    {
        for _ in 0..3 {
            let eye = Vec3::new(
                random.range(-50., 50.),
                random.range(-5., 20.),
                random.range(-50., 50.),
            );
            let target = eye + random.direction();
            let up = if random.direction().y.abs() > 0.99 {
                Vec3::X
            } else {
                Vec3::Y
            };
            let aspect = screen[0] as f32 / screen[1] as f32;
            let camera = Camera {
                label: ["1920x1080", "1280x720", "333x777", "64x48", "1x1"][index],
                view: camera::rh::view::look_at_mat4(eye, target, up),
                projection: crate::perspective(
                    random.range(0.3, 2.),
                    aspect,
                    random.range(0.05, 1.),
                ),
                screen,
            };
            let lights: Vec<_> = (0..200)
                .map(|_| random_light(&mut random, eye, Vec3::splat(60.)))
                .collect();
            let decals: Vec<_> = (0..100)
                .map(|_| random_decal(&mut random, eye, Vec3::splat(60.)))
                .collect();
            judge(&device, &queue, &mut random, &camera, &lights, &decals);
        }
    }
    // Down a tunnel 3 km long: fixtures on its ceiling and walls every 8 m,
    // spots and strips shining down and inward, and a craft's light near the
    // camera.
    let mut lights = Vec::new();
    for station in 0..375 {
        let z = -(station as f32) * 8.;
        for (position, direction) in [
            (Vec3::new(0., 8.5, z), Vec3::NEG_Y),
            (Vec3::new(-6., 4.5, z), Vec3::X),
            (Vec3::new(6., 4.5, z), Vec3::NEG_X),
        ] {
            lights.push(Light {
                position,
                shape: match station % 3 {
                    0 => LightShape::Point,
                    1 => LightShape::Spot {
                        direction,
                        inner_angle: 0.3,
                        outer_angle: 1.4,
                    },
                    _ => LightShape::Rect {
                        direction,
                        width_axis: Vec3::Z,
                        width: 5.8,
                        height: 0.3,
                    },
                },
                color: [1.; 3],
                intensity: 1.,
                range: random.range(8., 25.),
                baked: station % 3 != 0,
                specular: 1.,
                casts_shadow: false,
            });
        }
    }
    lights.push(Light {
        position: Vec3::new(0., 0.6, 2.9),
        shape: LightShape::Point,
        color: [1.; 3],
        intensity: 1.,
        range: 9.,
        baked: false,
        specular: 1.,
        casts_shadow: true,
    });
    // Markings on its road, a few degrees off true, and panels on its walls.
    let mut decals = Vec::new();
    for station in 0..375 {
        let z = -(station as f32) * 8. - 4.;
        decals.push(decal(
            Vec3::new(random.range(-3., 3.), 0., z),
            Quat::from_rotation_y(random.range(-0.1, 0.1)),
            Vec3::new(0.4, 0.3, 3.),
        ));
        for side in [-1., 1.] {
            decals.push(decal(
                Vec3::new(side * 6., 2.5, z),
                Quat::from_rotation_z(side * std::f32::consts::FRAC_PI_2),
                Vec3::new(2., 0.5, 2.5),
            ));
        }
    }
    for (label, eye, target) in [
        ("tunnel", Vec3::new(0., 2.5, 6.), Vec3::new(0., 2., -100.)),
        (
            "tunnel, toward a wall",
            Vec3::new(-2., 2.5, -300.),
            Vec3::new(5., 3., -340.),
        ),
    ] {
        let camera = Camera {
            label,
            view: camera::rh::view::look_at_mat4(eye, target, Vec3::Y),
            projection: crate::perspective(1.1, 16. / 9., 0.1),
            screen: [1920, 1080],
        };
        judge(&device, &queue, &mut random, &camera, &lights, &decals);
    }
    // Every light ends within the first depth slice (5 m), so the slicing
    // has no farther light to reach.
    let camera = Camera {
        label: "lights within the first slice",
        view: camera::rh::view::look_at_mat4(Vec3::ZERO, Vec3::NEG_Z, Vec3::Y),
        projection: crate::perspective(1.2, 16. / 9., 0.1),
        screen: [1280, 720],
    };
    let lights: Vec<_> = (0..100)
        .map(|_| {
            let mut light = random_light(&mut random, Vec3::new(0., 0., -2.), Vec3::splat(1.5));
            light.range = random.range(0.3, 1.4);
            light
        })
        .collect();
    judge(&device, &queue, &mut random, &camera, &lights, &[]);
    // Decals alone: the slicing reaches as far as the farthest decal.
    let decals: Vec<_> = (0..100)
        .map(|_| random_decal(&mut random, Vec3::new(0., 0., -40.), Vec3::splat(30.)))
        .collect();
    let camera = Camera {
        label: "decals alone",
        ..camera
    };
    judge(&device, &queue, &mut random, &camera, &[], &decals);
    // Orthographic, reversed Z (near 1, far 0).
    let eye = Vec3::new(10., 30., 10.);
    let camera = Camera {
        label: "orthographic",
        view: camera::rh::view::look_at_mat4(eye, Vec3::ZERO, Vec3::Y),
        projection: camera::rh::proj::directx::orthographic(-40., 40., -25., 25., 200., 0.5),
        screen: [1600, 1000],
    };
    let lights: Vec<_> = (0..200)
        .map(|_| random_light(&mut random, Vec3::ZERO, Vec3::splat(40.)))
        .collect();
    let decals: Vec<_> = (0..100)
        .map(|_| random_decal(&mut random, Vec3::ZERO, Vec3::splat(40.)))
        .collect();
    judge(&device, &queue, &mut random, &camera, &lights, &decals);
}
