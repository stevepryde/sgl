//! Decals through the real opaque stage, which writes what reflections
//! read, and through a real ray hit.
use crate::asset::{CpuMesh, Image, Vertex};
use crate::renderer::Renderer;
use crate::settings::{AmbientOcclusionQuality, ScreenSpaceReflections, Settings};
use crate::test_support;
use crate::{Camera, Decal, FrameInput, Scene, SceneError, perspective};
use glam::{Mat4, Quat, Vec3};

const SIZE: [u32; 2] = [64, 64];

/// A square facing +Z about `center`, `half` metres from it to each side.
fn square(center: Vec3, half: f32) -> CpuMesh {
    CpuMesh {
        vertices: [(-1., -1.), (1., -1.), (1., 1.), (-1., 1.)]
            .map(|(x, y)| Vertex {
                position: (center + Vec3::new(x * half, y * half, 0.)).to_array(),
                normal: [0., 0., 1.],
                uv: [(x + 1.) / 2., (1. - y) / 2.],
                color: [1.; 4],
                lightmap_uv: [0.; 2],
                lightmap_bounds: [0., 0., 1., 1.],
                tangent: [0.; 4],
            })
            .to_vec(),
        indices: vec![0, 1, 2, 0, 2, 3],
        material: 0,
        deformation: Default::default(),
    }
}

/// An image of `texel` throughout.
fn flat(texel: [u8; 4]) -> Image {
    Image::Rgba8(image::RgbaImage::from_pixel(32, 32, image::Rgba(texel)))
}

/// A unit vector's octahedral coordinates (Cigolle et al. 2014, "A Survey of
/// Efficient Representations for Independent Unit Vectors", section 3.1),
/// signed, as the G-buffer stores normals.
fn octahedral(v: Vec3) -> [f32; 2] {
    let n = v / v.abs().element_sum();
    if n.z >= 0. {
        [n.x, n.y]
    } else {
        [
            (1. - n.y.abs()) * n.x.signum(),
            (1. - n.x.abs()) * n.y.signum(),
        ]
    }
}

/// What the decal or the material makes of one surface point.
#[derive(Clone, Copy, Debug)]
struct Expected {
    base: Vec3,
    metallic: f32,
    roughness: f32,
    normal: Vec3,
}

/// An image of four colours, a quadrant each: red at its top left, green
/// at its top right, blue at its bottom left, yellow at its bottom right.
fn quadrants() -> Image {
    Image::Rgba8(image::RgbaImage::from_fn(64, 64, |x, y| {
        image::Rgba(match (x < 32, y < 32) {
            (true, true) => [255, 0, 0, 255],
            (false, true) => [0, 255, 0, 255],
            (true, false) => [0, 0, 255, 255],
            (false, false) => [255, 255, 0, 255],
        })
    }))
}

// Plausible defects: the box's decal space mirrored, turned (an inverted
// rotation) or scaled wrongly, so the decal lands elsewhere, shows another
// part of its image, or covers points beyond its faces; an image read from
// another place or size in the atlas, or in the wrong colour space; a
// normal map read in another convention or framed by the wrong axes;
// roughness or metallic taken from the wrong channel; an atlas pack that
// misses a decal added after another, or leaves its record naming an old
// place; the G-buffer or a ray hit evaluating the surface without its
// decals, so reflections and SSR do not see them, or one opaque form
// applying them differently. The oracle is the decal's documented meaning:
// a floor whose material is known, behind which nothing else lies, under a
// box turned about the floor's normal that projects down its +Y (the
// floor's normal) a four-colour image across its X (U) and Z (V, top at
// −Z), with uniform normal and metallic-roughness maps. At each quadrant's
// centre, the G-buffer targets reflections read and a ray hit hold that
// quadrant's colour (seen through f0 at metallic 1), the map's roughness
// and the map's normal, its +Y toward the image's top; a second decal,
// added on its own, shows its own image; outside the boxes, and on a square
// in front of the floor within a box's footprint but beyond its faces, they
// hold the material's. Adding the decals packs nothing until the frame.
#[test]
fn decals_change_base_colour_normal_and_roughness_inside_their_box_alone() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    let mut asset = test_support::cube();
    let front = Vec3::new(-1., -0.3, -4.);
    asset.meshes = vec![square(Vec3::new(0., 0., -6.), 8.), square(front, 0.3)];
    let material = &mut asset.materials[0];
    material.base = [0.2, 0.4, 0.6, 1.];
    material.metallic = 0.;
    material.roughness = 0.8;
    test_support::add_static(&device, &queue, &mut scene, asset);
    let base_color = scene.add_decal_image(quadrants()).unwrap();
    // x = 2·166/255 − 1, toward the image's top 2·204/255 − 1 = 0.6.
    let normal = scene.add_decal_image(flat([166, 204, 255, 255])).unwrap();
    // Roughness 51/255 = 0.2 in green, metallic 1 in blue.
    let metallic_roughness = scene.add_decal_image(flat([0, 51, 255, 255])).unwrap();
    let white = scene.add_decal_image(flat([255; 4])).unwrap();
    // Its +Y along the floor's normal (+Z), then turned about it.
    let rotation = Quat::from_rotation_z(0.4) * Quat::from_rotation_x(std::f32::consts::FRAC_PI_2);
    let center = Vec3::new(0., 0., -6.);
    let decal = Decal {
        position: center,
        rotation,
        size: Vec3::new(4., 1., 4.),
        base_color,
        normal: Some(normal),
        metallic_roughness: Some(metallic_roughness),
        color: [1.; 4],
        base_color_mix: 1.,
        upper_fade: 0.,
        lower_fade: 0.,
        normal_fade: 0.,
    };
    scene.add_decal(&device, &queue, decal).unwrap();
    let corner = Vec3::new(-2.6, -2.6, -6.);
    scene
        .add_decal(
            &device,
            &queue,
            Decal {
                position: corner,
                size: Vec3::new(0.8, 1., 0.8),
                base_color: white,
                normal: None,
                ..decal
            },
        )
        .unwrap();
    assert!(
        scene.decals.packing(),
        "the decals' images wait for the frame's upload"
    );
    // The box's axes, as the rotation turns them.
    let [x_axis, y_axis, z_axis] = [Vec3::X, Vec3::Y, Vec3::Z].map(|axis| rotation * axis);
    let tangent_x: f32 = 2. * 166. / 255. - 1.;
    let tangent_y: f32 = 2. * 204. / 255. - 1.;
    let tangent_z = (1. - tangent_x * tangent_x - tangent_y * tangent_y).sqrt();
    let decaled = |base: Vec3| Expected {
        base,
        metallic: 1.,
        roughness: 0.2,
        normal: x_axis * tangent_x - z_axis * tangent_y + y_axis * tangent_z,
    };
    let material = Expected {
        base: Vec3::new(0.2, 0.4, 0.6),
        metallic: 0.,
        roughness: 0.8,
        normal: Vec3::Z,
    };
    // Each quadrant's centre, a metre along the box's X and Z from its
    // centre; the second decal's centre; the floor outside the boxes; the
    // front square, over floor the first box covers.
    let points = [
        (center - x_axis - z_axis, decaled(Vec3::X)),
        (center + x_axis - z_axis, decaled(Vec3::Y)),
        (center - x_axis + z_axis, decaled(Vec3::Z)),
        (center + x_axis + z_axis, decaled(Vec3::new(1., 1., 0.))),
        (
            corner,
            Expected {
                normal: Vec3::Z,
                ..decaled(Vec3::ONE)
            },
        ),
        (Vec3::new(3.1, 0., -6.), material),
        (front, material),
    ];

    let projection = perspective(1., 1., 0.1);
    let input = FrameInput::new(Camera {
        view: Mat4::IDENTITY,
        projection,
        eye: Vec3::ZERO,
    });
    let settings = Settings {
        ambient_occlusion: AmbientOcclusionQuality::Off,
        // World-space rays need a screen-space method to fill in.
        screen_space_reflections: ScreenSpaceReflections::Half,
        world_space_reflections: true,
        ..Settings::default()
    };
    let mut renderer = Renderer::for_test(&device, &queue, SIZE, &settings);
    let mut frame = renderer.prepare_test_frame(&device, &queue, &mut scene, &input, &settings);
    assert!(!scene.decals.packing(), "the frame uploaded the atlas");
    let forms = if renderer.test_fused_supported() {
        vec![true, false]
    } else {
        vec![false]
    };
    for fused in forms {
        let mut encoder = device.create_command_encoder(&Default::default());
        renderer.encode_test_opaque(&device, &queue, &mut encoder, &scene, &mut frame, fused);
        queue.submit([encoder.finish()]);
        let targets = renderer.targets();
        let normals = test_support::read(&device, &queue, targets.normal.texture(), 8);
        let materials = test_support::read(&device, &queue, targets.material.texture(), 8);
        let f0 = test_support::read(&device, &queue, targets.f0.texture(), 4);
        for (point, expected) in &points {
            let ndc = projection.project_point3(*point);
            let [x, y] =
                [ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5].map(|t| (t * SIZE[0] as f32) as u32);
            let at = (y * SIZE[0] + x) as usize;
            let normal = [0, 1].map(|c| test_support::half(&normals[at * 8 + c * 2..]));
            let roughness = test_support::half(&materials[at * 8 + 2..]);
            let f0: Vec<f32> = f0[at * 4..at * 4 + 3]
                .iter()
                .map(|&v| f32::from(v) / 255.)
                .collect();
            let expected_f0 = Vec3::splat(0.04).lerp(expected.base, expected.metallic);
            let expected_normal = octahedral(expected.normal.normalize());
            let label = format!("fused {fused}, {point} at pixel ({x}, {y})");
            for c in 0..2 {
                assert!(
                    (normal[c] - expected_normal[c]).abs() < 3e-3,
                    "{label}: normal {normal:?}, expected {expected_normal:?} ({expected:?})"
                );
            }
            assert!(
                (roughness - expected.roughness).abs() < 2e-3,
                "{label}: roughness {roughness}, expected {expected:?}"
            );
            for c in 0..3 {
                assert!(
                    (f0[c] - expected_f0[c]).abs() <= 1. / 255.,
                    "{label}: f0 {f0:?}, expected {expected_f0} ({expected:?})"
                );
            }
        }
    }

    // A ray hit from the camera at each point.
    let observation = format!(
        r#"
@group(3) @binding(0) var<storage,read_write> output:array<vec4<f32>>;
@compute @workgroup_size(1) fn observe() {{
 let directions=array<vec3<f32>,{count}>({directions});
 for (var i=0u;i<{count}u;i++) {{
  let ray=SceneRay(vec4(0.,0.,0.,0.),vec4(directions[i],100.));
  let hit=scene_decode_hit(scene_trace_nearest(ray),ray.origin.xyz,ray.direction.xyz);
  let material=scene_material(hit.material_word);
  let s=ray_surface(hit,material,ray_base_color(hit,material),vec3(0.),-normalize(directions[i]),cluster_range(hit.position,vec2(0.)));
  output[i*2u]=vec4(s.base.rgb,s.metallic);
  output[i*2u+1u]=vec4(s.normal,s.roughness);
 }}
}}
"#,
        count = points.len(),
        directions = points
            .iter()
            .map(|(d, _)| format!("vec3({},{},{})", d.x, d.y, d.z))
            .collect::<Vec<_>>()
            .join(","),
    );
    let observed = observe_ray_hits(
        &device,
        &queue,
        &mut renderer,
        &mut scene,
        &input,
        &settings,
        &observation,
        points.len() * 2,
    );
    for (index, (point, expected)) in points.iter().enumerate() {
        let [r, g, b, metallic] = observed[index * 2];
        let [x, y, z, roughness] = observed[index * 2 + 1];
        let label = format!("ray hit at {point}");
        assert!(
            Vec3::new(r, g, b).distance(expected.base) < 1e-3,
            "{label}: base {:?}, expected {expected:?}",
            [r, g, b]
        );
        assert!(
            (metallic - expected.metallic).abs() < 1e-3,
            "{label}: metallic {metallic}"
        );
        assert!(
            (roughness - expected.roughness).abs() < 1e-3,
            "{label}: roughness {roughness}"
        );
        assert!(
            Vec3::new(x, y, z).distance(expected.normal.normalize()) < 2e-3,
            "{label}: normal {:?}, expected {expected:?}",
            [x, y, z]
        );
    }
}

// Plausible defects: the lit pipelines of a scene without decals, which
// compile the decal path out, shading a surface otherwise than those with
// it, such as losing its mapped normal, roughness or metallic with the
// decals, or losing another constant with theirs so the rectangle light
// drops out. The oracle is a decal's documented reach, the surfaces inside
// its box: a scene whose only decal lies behind the camera renders, pixel
// for pixel, the frame of the same scene without it, the first with the
// decal path compiled in and the second without; so does the scene once the
// decal is removed again.
#[test]
fn a_decal_out_of_view_leaves_the_frame_as_without_decals() {
    use crate::settings::{Antialiasing, Bloom};
    use crate::{Light, LightShape};
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    let mut asset = test_support::cube();
    asset.meshes = vec![square(Vec3::new(0., 0., -6.), 8.)];
    asset.images = vec![flat([166, 204, 255, 255])];
    asset.materials[0].normal_texture = Some(0);
    test_support::add_static(&device, &queue, &mut scene, asset);
    let light = Light {
        position: Vec3::new(1., 1., -4.),
        range: 10.,
        ..Light::default()
    };
    scene.add_light(&device, &queue, light).unwrap();
    let panel = LightShape::Rect {
        direction: -Vec3::Z,
        width_axis: Vec3::X,
        width: 1.,
        height: 0.5,
    };
    scene
        .add_light(
            &device,
            &queue,
            Light {
                position: Vec3::new(-2., 0., -5.),
                shape: panel,
                ..light
            },
        )
        .unwrap();
    let image = scene.add_decal_image(flat([255, 0, 0, 255])).unwrap();
    let settings = Settings {
        antialiasing: Antialiasing::Off,
        bloom: Bloom::Off,
        ..Settings::default()
    };
    let mut renderer = Renderer::for_test(&device, &queue, SIZE, &settings);
    let input = FrameInput::new(Camera {
        view: Mat4::IDENTITY,
        projection: perspective(1., 1., 0.1),
        eye: Vec3::ZERO,
    });
    let output = crate::view::targets::target(
        &device,
        "decal frames",
        SIZE,
        crate::shading::gbuffer::COLOR,
    );
    let mut frame = |scene: &mut Scene| {
        let mut encoder = device.create_command_encoder(&Default::default());
        renderer.render(
            &device,
            &queue,
            &mut encoder,
            scene,
            &input,
            &settings,
            &output,
            None,
        );
        queue.submit([encoder.finish()]);
        renderer.finish_frame(scene);
        test_support::read(&device, &queue, output.texture(), 8)
    };
    let without = frame(&mut scene);
    assert!(
        without.chunks(8).any(|pixel| pixel != &without[..8]),
        "the lights shade the floor across the frame"
    );
    // The camera looks down −Z.
    let behind = Decal {
        position: Vec3::new(0., 0., 10.),
        size: Vec3::ONE,
        ..Decal::new(image)
    };
    let decal = scene.add_decal(&device, &queue, behind).unwrap();
    assert!(
        frame(&mut scene) == without,
        "a decal behind the camera changed the frame"
    );
    scene.remove_decal(decal).unwrap();
    assert!(
        frame(&mut scene) == without,
        "removing the decal changed the frame"
    );
}

/// Runs `observation` in compute under the frame's ray-hit lit group 0 and
/// the scene's group 1, and reads back its `words` vec4 outputs.
#[allow(clippy::too_many_arguments)]
fn observe_ray_hits(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    renderer: &mut Renderer,
    scene: &mut Scene,
    input: &FrameInput,
    settings: &Settings,
    observation: &str,
    outputs: usize,
) -> Vec<[f32; 4]> {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("decal ray hits"),
        source: wgpu::ShaderSource::Wgsl(
            format!("{}\n{}", crate::shading::lit_compute_library(), observation).into(),
        ),
    });
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: None,
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only: false },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: None,
        layout: Some(
            &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: None,
                bind_group_layouts: &[
                    Some(renderer.test_lit_layout()),
                    Some(scene.scene_layout()),
                    None,
                    Some(&layout),
                ],
                immediate_size: 0,
            }),
        ),
        module: &shader,
        entry_point: Some("observe"),
        compilation_options: Default::default(),
        cache: None,
    });
    let bytes = (outputs * 16) as u64;
    let output = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: bytes,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: output.as_entire_binding(),
        }],
    });
    renderer.prepare_test_frame(device, queue, scene, input, settings);
    scene.update_rays(device, queue, input.visibility_mask);
    let mut encoder = device.create_command_encoder(&Default::default());
    {
        let mut pass = encoder.begin_compute_pass(&Default::default());
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, renderer.test_ray_hit_lit(), &[]);
        pass.set_bind_group(1, &scene.scene_group, &[]);
        pass.set_bind_group(3, &group, &[]);
        pass.dispatch_workgroups(1, 1, 1);
    }
    queue.submit([encoder.finish()]);
    scene.finish_frame();
    let words = test_support::read_words(device, queue, &output);
    bytemuck::cast_slice::<u32, [f32; 4]>(&words).to_vec()
}

// Plausible defects: an image a decal uses removed from under it, or one no
// decal uses any longer kept from removal; a decal or image identity that
// outlives its removal. The oracle is the scene contract.
#[test]
fn decal_images_stay_while_a_decal_uses_them() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    let shared = scene.add_decal_image(flat([255; 4])).unwrap();
    let other = scene.add_decal_image(flat([0, 128, 255, 255])).unwrap();
    let decal = Decal {
        position: Vec3::ZERO,
        rotation: Quat::IDENTITY,
        size: Vec3::ONE,
        base_color: shared,
        normal: Some(shared),
        metallic_roughness: None,
        color: [1.; 4],
        base_color_mix: 1.,
        upper_fade: 0.3,
        lower_fade: 0.3,
        normal_fade: 0.,
    };
    let first = scene.add_decal(&device, &queue, decal).unwrap();
    let second = scene.add_decal(&device, &queue, decal).unwrap();
    assert!(matches!(
        scene.remove_decal_image(shared),
        Err(SceneError::DecalImageInUse)
    ));
    scene.remove_decal(first).unwrap();
    assert!(matches!(
        scene.remove_decal_image(shared),
        Err(SceneError::DecalImageInUse)
    ));
    // The second decal turns to the other image for its base colour.
    let moved = Decal {
        base_color: other,
        ..decal
    };
    scene.set_decal(&device, &queue, second, moved).unwrap();
    assert_eq!(scene.decal(second).unwrap(), &moved);
    assert!(matches!(
        scene.remove_decal_image(shared),
        Err(SceneError::DecalImageInUse)
    ));
    scene
        .set_decal(
            &device,
            &queue,
            second,
            Decal {
                normal: None,
                ..moved
            },
        )
        .unwrap();
    scene.remove_decal_image(shared).unwrap();
    assert!(matches!(
        scene.add_decal(&device, &queue, decal),
        Err(SceneError::UnknownDecalImage)
    ));
    assert!(matches!(scene.decal(first), Err(SceneError::UnknownDecal)));
    assert!(matches!(
        scene.remove_decal_image(other),
        Err(SceneError::DecalImageInUse)
    ));
    scene.remove_decal(second).unwrap();
    scene.remove_decal_image(other).unwrap();
}
