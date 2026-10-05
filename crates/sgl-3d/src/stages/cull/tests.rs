//! The GPU draw lists' cull at its boundaries: what it appends to each
//! GPU-built view's sets, read back, against independent oracles.
use super::Cull;
use crate::asset::{Asset, CpuMesh, Vertex};
use crate::content::identity::Identity;
use crate::lod::MeshLod;
use crate::shading::culling::{CullStatistics, DrawCommand, words};
use crate::shading::uniforms::ViewUniform;
use crate::shading::vertex::DrawInstance;
use crate::view::culling::tests::{clipped_triangle, mesh};
use crate::view::draw_list::gpu::camera_cull;
use crate::view::draw_list::{DrawInstances, DrawList};
use crate::view::population::Population;
use crate::view::{FrameViews, View};
use crate::{InstanceId, InstanceState, Mobility, ModelId, Scene, test_support};
use bytemuck::Zeroable;
use glam::{Mat4, Vec3, camera};
use std::collections::BTreeSet;

/// What one cull appended to each GPU-built view, the camera's first: each
/// set's draw instances, in no order.
struct Culled {
    views: Vec<Vec<DrawInstance>>,
    camera_statistics: CullStatistics,
}

/// A camera view with `view`, `projection` and half-NDC `jitter`, as the
/// prepare stage uploads it.
fn camera_view(view: Mat4, projection: Mat4, jitter: [f32; 2]) -> View {
    let view_projection = projection * view;
    View::camera(ViewUniform {
        view: view.to_cols_array_2d(),
        projection: projection.to_cols_array_2d(),
        view_projection: view_projection.to_cols_array_2d(),
        jitter,
        ..ViewUniform::zeroed()
    })
}

/// Culls `scene` for `camera` at `size` pixels (with the frustum unless
/// `culling` is off) and for a cascade of each of `cascades`, under `mask`,
/// and reads back what each view appended.
fn cull(
    (device, queue): (&wgpu::Device, &wgpu::Queue),
    scene: &mut Scene,
    (camera, size): (View, [u32; 2]),
    cascades: &[Mat4],
    mask: u32,
    culling: bool,
) -> Culled {
    scene.prepare_frame(device, queue, Vec3::ZERO);
    let mut views = FrameViews::new(device);
    views.camera.set(queue, camera);
    views.camera.list.prepare(
        device,
        queue,
        scene,
        camera_cull(&camera, size, mask, culling),
        false,
    );
    crate::stages::prepare::set_cascades(
        (device, queue),
        scene,
        &mut views,
        mask,
        cascades.iter().copied(),
    );
    let mut encoder = device.create_command_encoder(&Default::default());
    Cull::new(device).encode(device, &mut encoder, scene, &views, None);
    queue.submit([encoder.finish()]);
    let read = |list: &crate::view::draw_list::gpu::GpuList| {
        let buffers = list.buffers();
        let draws = test_support::read_words(device, queue, buffers.draws);
        let regions = test_support::read_words(device, queue, buffers.regions);
        let mut appended = Vec::new();
        for (index, _, region) in scene.candidates.sets.iter() {
            let command = (words::<CullStatistics>() + index * words::<DrawCommand>()) as usize;
            let count: u32 = bytemuck::cast_slice::<u32, DrawCommand>(
                &draws[command..command + words::<DrawCommand>() as usize],
            )[0]
            .instance_count;
            assert!(
                count as usize <= region.len(),
                "a set's draw stays within its region"
            );
            let first = region.start as usize * words::<DrawInstance>() as usize;
            let entries =
                &regions[first..first + count as usize * words::<DrawInstance>() as usize];
            appended.extend_from_slice(bytemuck::cast_slice::<u32, DrawInstance>(entries));
        }
        (
            appended,
            *bytemuck::from_bytes::<CullStatistics>(bytemuck::cast_slice(&draws[..4])),
        )
    };
    let (camera_list, camera_statistics) = read(&views.camera.list);
    let mut culled = Culled {
        views: vec![camera_list],
        camera_statistics,
    };
    for slot in &views.cascades[..views.cascade_count] {
        culled.views.push(read(&slot.list).0);
    }
    culled
}

/// Adds `asset`'s model and an instance of it at `pose`.
fn place(
    (device, queue): (&wgpu::Device, &wgpu::Queue),
    scene: &mut Scene,
    model: ModelId,
    pose: Mat4,
    (visible, capture_visible, mobility): (bool, bool, Mobility),
) -> InstanceId {
    let state = InstanceState {
        model,
        pose,
        visible,
        capture_visible,
    };
    scene.add_instance(device, queue, state, mobility).unwrap()
}

/// An asset of `meshes`, each with its own copy of the cube's material.
fn asset(meshes: Vec<CpuMesh>) -> Asset {
    let mut asset = test_support::cube();
    let material = asset.materials[0].clone();
    asset.materials = vec![material; meshes.len()];
    asset.meshes = meshes
        .into_iter()
        .enumerate()
        .map(|(index, mesh)| CpuMesh {
            material: index,
            ..mesh
        })
        .collect();
    asset
}

// Plausible defects: the GPU's pose applied to the view's planes the wrong
// way round, so a section with a visible triangle is culled; a section's
// bounds, first index or triangle count read from the wrong words; a
// mirrored or translated pose losing sections near the view's edges. The
// oracle clips each actual triangle against WebGPU's clip volume in double
// precision, through the camera's view, projection and jitter, as the CPU
// culling test does: every triangle it keeps must lie in a section the
// camera's list appended for its instance. Culling must also drop sections:
// at each location part of the grid lies outside the view.
#[test]
fn gpu_frustum_never_drops_a_triangle_the_clip_oracle_keeps() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let gpu = (&device, &queue);
    let grid = mesh((-20..20).flat_map(|z| {
        (-20..20).map(move |x| {
            let p = Vec3::new(x as f32 * 3., 0., z as f32 * 3.);
            [p, p + Vec3::new(2., 0., 0.), p + Vec3::new(0., 2., -1.)]
        })
    }));
    let triangles: Vec<[Vec3; 3]> = grid
        .vertices
        .chunks_exact(3)
        .map(|triangle| std::array::from_fn(|corner| Vec3::from_array(triangle[corner].position)))
        .collect();
    let projection = crate::perspective(1.1, 1.3, 0.5);
    let jitter = [0.013, -0.007];
    for location in [Vec3::ZERO, Vec3::new(1_000_000., 100., -300_000.)] {
        let mut scene = Scene::new(&device, &queue);
        let model = scene
            .add_asset(&device, &queue, asset(vec![grid.clone()]))
            .unwrap()
            .model;
        let mut poses = Vec::new();
        for scale in [Vec3::ONE, Vec3::new(-2., 0.3, 1.4)] {
            for yaw in [0., 0.7, 1.9] {
                let pose = Mat4::from_translation(location)
                    * Mat4::from_rotation_y(yaw)
                    * Mat4::from_scale(scale);
                let instance = place(gpu, &mut scene, model, pose, (true, true, Mobility::Static));
                poses.push((instance, pose));
            }
        }
        let view = camera::rh::view::look_at_mat4(
            location + Vec3::new(1., 3., 4.),
            location - Vec3::Z * 10.,
            Vec3::Y,
        );
        let culled = cull(
            gpu,
            &mut scene,
            (camera_view(view, projection, jitter), [1920, 1080]),
            &[],
            u32::MAX,
            true,
        );
        let appended = &culled.views[0];
        let sections = (triangles.len() as u32).div_ceil(128) as usize;
        assert!(
            appended.len() < poses.len() * sections,
            "the cull drops sections outside the view: {} of {}",
            appended.len(),
            poses.len() * sections
        );
        for (instance, pose) in poses {
            let shown: Vec<_> = appended
                .iter()
                .filter(|drawn| drawn.object == instance.index() as u32)
                .collect();
            for (primitive, triangle) in triangles.iter().enumerate() {
                let clip = triangle
                    .iter()
                    .map(|p| {
                        let p = p.as_dvec3().extend(1.);
                        let mut p =
                            projection.as_dmat4() * (view.as_dmat4() * (pose.as_dmat4() * p));
                        p.x += 2. * f64::from(jitter[0]) * p.w;
                        p.y += 2. * f64::from(jitter[1]) * p.w;
                        p
                    })
                    .collect();
                if clipped_triangle(clip) {
                    let index = primitive as u32 * 3;
                    assert!(
                        shown.iter().any(|drawn| {
                            (drawn.first_index..drawn.first_index + drawn.triangles * 3)
                                .contains(&index)
                        }),
                        "instance at {location} dropped visible primitive {primitive}"
                    );
                }
            }
        }
    }
}

// Plausible defect: the jitter left out of the planes, so a triangle the
// jittered raster shows at the view's edge is culled. It also runs a million
// metres out, but does not target the f32 tolerance: these steps do not
// happen to round across the plane, so a cull without it passes here, and
// the tolerance rests on its derivation (`view::culling::Frustum::planes`,
// culling.wgsl's `cull_reaches`). The oracle clips each
// triangle in double precision through the jittered camera, as above: tiny
// triangles step across the view's left plane, at the origin and a million
// metres from it, and every one it keeps must be appended; at the origin,
// some must be culled.
#[test]
fn gpu_frustum_keeps_triangles_a_rounding_or_a_jitter_inside_its_planes() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let gpu = (&device, &queue);
    let triangle = [
        Vec3::new(-0.002, -0.002, 0.),
        Vec3::new(0.002, -0.002, 0.),
        Vec3::new(0., 0.002, 0.),
    ];
    let projection = crate::perspective(1., 1.5, 0.1);
    let jitter = [0.011, 0.];
    // The left plane at 10 m: x = -10 * aspect * tan(fovy / 2).
    let edge = -10. * 1.5 * 0.5_f32.tan();
    for location in [Vec3::ZERO, Vec3::new(1_000_000., 0., -1_000_000.)] {
        let mut scene = Scene::new(&device, &queue);
        let model = scene
            .add_asset(&device, &queue, asset(vec![mesh([triangle])]))
            .unwrap()
            .model;
        let placed: Vec<_> = (0..400)
            .map(|step| {
                let x = edge - 0.4 + step as f32 * 0.002;
                let pose = Mat4::from_translation(location + Vec3::new(x, 0., -10.));
                (
                    place(gpu, &mut scene, model, pose, (true, true, Mobility::Static)),
                    pose,
                )
            })
            .collect();
        let view = Mat4::from_translation(-location);
        let culled = cull(
            gpu,
            &mut scene,
            (camera_view(view, projection, jitter), [1920, 1080]),
            &[],
            u32::MAX,
            true,
        );
        let appended: BTreeSet<u32> = culled.views[0].iter().map(|drawn| drawn.object).collect();
        let mut kept = 0;
        for (instance, pose) in placed {
            let clip = triangle
                .iter()
                .map(|p| {
                    let p = p.as_dvec3().extend(1.);
                    let mut p = projection.as_dmat4() * (view.as_dmat4() * (pose.as_dmat4() * p));
                    p.x += 2. * f64::from(jitter[0]) * p.w;
                    p
                })
                .collect();
            if clipped_triangle(clip) {
                kept += 1;
                assert!(
                    appended.contains(&(instance.index() as u32)),
                    "at {location}, culled a visible triangle at {:?}",
                    pose.w_axis
                );
            }
        }
        assert!(kept > 0, "the steps reach into the view");
        // A million metres out, the tolerance covers f32 positions' 6 cm
        // steps and more; at the origin the cull drops what lies outside.
        if location == Vec3::ZERO {
            assert!(appended.len() < 400, "the steps cross the plane");
        }
    }
}

/// A square of two triangles `size` metres across in the XY plane.
fn square(size: f32) -> CpuMesh {
    let corner = |x: f32, y: f32| Vertex {
        position: [x * size, y * size, 0.],
        normal: [0., 0., 1.],
        tangent: [1., 0., 0., 1.],
        lightmap_bounds: [0., 0., 1., 1.],
        color: [1.; 4],
        ..Zeroable::zeroed()
    };
    CpuMesh {
        vertices: vec![
            corner(-0.5, -0.5),
            corner(0.5, -0.5),
            corner(0.5, 0.5),
            corner(-0.5, 0.5),
        ],
        indices: vec![0, 1, 2, 0, 2, 3],
        material: 0,
        deformation: Default::default(),
    }
}

// Plausible defects: the GPU's projected error bound smaller than the
// CPU's, from its f32 composition of a translated pose with the view or a
// margin left out, so the camera draws an alternative coarser than the
// bound admits; the chain walked finest first, or its levels' errors or
// bounds read from the wrong record. The oracle is the CPU builder's
// selection (`view::lod::LodSelector`), which bounds the displacement in
// double precision: for every instance, the level the GPU drew is never
// coarser than the one it chose, and is the same where no level's CPU
// bound lies within 0.1% of half a pixel, beyond the GPU's margins.
// Instances recede from a metre to a kilometre, near the origin and a
// million metres from it, every other one rotated and scaled nonuniformly,
// so the GPU composes the pose's every element, and the GPU must also
// choose coarser levels where they are admissible.
#[test]
fn gpu_lod_is_never_coarser_than_the_cpu_admits() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let gpu = (&device, &queue);
    let projection = crate::perspective(1., 16. / 9., 0.1);
    let size = [1920, 1080];
    for origin in [Vec3::ZERO, Vec3::new(1_000_000., 0., 0.)] {
        let mut scene = Scene::new(&device, &queue);
        let base = scene
            .add_asset(&device, &queue, asset(vec![square(1.)]))
            .unwrap()
            .model;
        let errors = [0.0005, 0.005, 0.05];
        let mut alternatives = Vec::new();
        for _ in errors {
            let model = scene
                .add_asset(&device, &queue, asset(vec![square(1.)]))
                .unwrap()
                .model;
            alternatives.push(model);
        }
        let lods: Vec<_> = alternatives
            .iter()
            .zip(errors)
            .map(|(&model, max_error)| MeshLod {
                model,
                mesh: 0,
                max_error,
            })
            .collect();
        scene.set_mesh_lods(base, 0, lods).unwrap();
        // Each level's mesh record, finest first.
        let words: Vec<u32> = std::iter::once(base)
            .chain(alternatives.iter().copied())
            .map(|model| scene.drawn_model(model).ray.mesh_word(0))
            .collect();
        let mut placed = Vec::new();
        for step in 0..48 {
            let distance = 1.5 * 1.15_f32.powi(step);
            let mut pose = Mat4::from_translation(origin + Vec3::new(0.1, 0.05, -distance));
            if step % 2 == 1 {
                pose = pose
                    * Mat4::from_rotation_y(0.4)
                    * Mat4::from_rotation_x(0.3)
                    * Mat4::from_scale(Vec3::new(1.5, 0.7, 1.2));
            }
            placed.push((
                place(gpu, &mut scene, base, pose, (true, true, Mobility::Static)),
                pose,
            ));
        }
        let view = Mat4::from_translation(-origin);
        let culled = cull(
            gpu,
            &mut scene,
            (camera_view(view, projection, [0.; 2]), size),
            &[],
            u32::MAX,
            true,
        );
        let selector = crate::view::lod::LodSelector::new(view, projection, size).unwrap();
        let base_mesh = &scene.drawn_model(base).meshes[0];
        let mut coarser = 0;
        for (instance, pose) in placed {
            let drawn: BTreeSet<u32> = culled.views[0]
                .iter()
                .filter(|drawn| drawn.object == instance.index() as u32)
                .map(|drawn| drawn.mesh)
                .collect();
            assert_eq!(drawn.len(), 1, "one level of each instance");
            let gpu_level = words
                .iter()
                .position(|word| drawn.contains(word))
                .expect("a level's mesh");
            let cpu_level = selector
                .select(base_mesh, &scene.models, pose)
                .map_or(0, |lod| {
                    1 + alternatives.iter().position(|&m| m == lod.model).unwrap()
                });
            assert!(
                gpu_level <= cpu_level,
                "at {pose:?} the GPU drew level {gpu_level}, coarser than the CPU's {cpu_level}"
            );
            let marginal = alternatives.iter().zip(errors).any(|(&model, error)| {
                let other = scene.drawn_model(model).meshes[0].ranges.bounds().unwrap();
                let bounds = base_mesh.ranges.bounds().unwrap();
                let union = [bounds[0].min(other[0]), bounds[1].max(other[1])];
                let pixels = crate::shading::lod::projected_error(
                    [pose, view, projection],
                    union,
                    error,
                    size,
                );
                (pixels / 0.5 - 1.).abs() < 1e-3
            });
            if !marginal {
                assert_eq!(gpu_level, cpu_level, "at {pose:?}, away from the threshold");
            }
            coarser += usize::from(gpu_level > 0);
        }
        // A million metres out, f32 cannot bound centimetres: both keep
        // full detail there.
        if origin == Vec3::ZERO {
            assert!(
                coarser > 0,
                "the GPU chooses coarser levels where they are admissible"
            );
        }
    }
}

// Plausible defects: the camera's or a cascade's population read from the
// wrong bits (`visible` for a cascade, `capture_visible` for the camera),
// a material's group or casting ignored, a blended mesh drawn, a free
// candidate slot or a removed instance's candidates culled, an instance's
// candidates naming another mesh. The oracle is the CPU builder, which
// probe captures keep: for a frame whose views hold every instance (only
// the frustum culls), a cascade's (instance, mesh) pairs are its probe
// capture's cascade population's (`Population::CaptureShadow`, static
// casters of an enabled group), and the camera's are the instances it
// shows by the spec's rules (`visible`, an enabled group, not blended),
// placed in front of it, less the one placed behind it.
#[test]
fn gpu_lists_hold_the_cpu_builders_instances_and_meshes() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let gpu = (&device, &queue);
    let mut scene = Scene::new(&device, &queue);
    // Meshes: opaque; blended; in group 2, which the mask leaves out; not
    // casting; masked and double-sided.
    let mut assets = asset(vec![
        square(1.),
        square(1.),
        square(1.),
        square(1.),
        square(1.),
    ]);
    assets.materials[1].alpha = crate::AlphaMode::Blend {
        receives_screen_space_reflections: false,
    };
    assets.materials[2].visibility_group = 2;
    assets.materials[3].casts_directional_shadow = false;
    assets.materials[4].alpha = crate::AlphaMode::Mask { cutoff: 0.5 };
    assets.materials[4].double_sided = true;
    let model = scene.add_asset(&device, &queue, assets).unwrap().model;
    let mut instances = Vec::new();
    for (index, flags) in [
        (true, true, Mobility::Static),
        (false, true, Mobility::Static),
        (true, false, Mobility::Static),
        (true, true, Mobility::Static),
        (true, true, Mobility::Static),
    ]
    .into_iter()
    .enumerate()
    {
        // In front of the camera, one mirrored.
        let scale = if index == 3 {
            Vec3::new(-1., 1., 1.)
        } else {
            Vec3::ONE
        };
        let pose = Mat4::from_scale_rotation_translation(
            scale,
            glam::Quat::IDENTITY,
            Vec3::new(index as f32 * 1.5 - 3., 0., -8.),
        );
        instances.push(place(gpu, &mut scene, model, pose, flags));
    }
    // Behind the camera, which the camera culls and the cascade holds.
    let behind = Mat4::from_translation(Vec3::new(0., 0., 8.));
    instances.push(place(
        gpu,
        &mut scene,
        model,
        behind,
        (true, true, Mobility::Static),
    ));
    // An instance removed in front of the camera: its candidates do not
    // survive it.
    let ahead = Mat4::from_translation(Vec3::new(0., 3., -8.));
    let removed = place(
        gpu,
        &mut scene,
        model,
        ahead,
        (true, true, Mobility::Static),
    );
    scene.remove_instance(removed).unwrap();
    let mask = !2;
    // A cascade that holds every instance: x, y and depth across 40 m.
    let cascade = camera::rh::proj::directx::orthographic(-20., 20., -20., 20., 20., -20.);
    let projection = crate::perspective(1., 1., 0.1);
    let culled = cull(
        gpu,
        &mut scene,
        (camera_view(Mat4::IDENTITY, projection, [0.; 2]), [256, 256]),
        &[cascade],
        mask,
        true,
    );
    let pairs = |drawn: &[DrawInstance]| -> BTreeSet<(u32, u32)> {
        drawn
            .iter()
            .map(|drawn| (drawn.object, drawn.mesh))
            .collect()
    };
    let words: Vec<u32> = (0..5)
        .map(|mesh| scene.drawn_model(model).ray.mesh_word(mesh))
        .collect();
    let mut camera = BTreeSet::new();
    for (index, instance) in instances.iter().enumerate().take(5) {
        let visible = index != 1;
        for (mesh, &word) in words.iter().enumerate() {
            // Not blended, and in a group the mask enables.
            if visible && mesh != 1 && mesh != 2 {
                camera.insert((instance.index() as u32, word));
            }
        }
    }
    assert_eq!(
        pairs(&culled.views[0]),
        camera,
        "the camera's instances and meshes"
    );
    // Every square is one section of two triangles, every instance static.
    let sections = culled.views[0].len() as u32;
    assert_eq!(
        [
            culled.camera_statistics.static_sections,
            culled.camera_statistics.static_triangles,
            culled.camera_statistics.moving_sections,
        ],
        [sections, sections * 2, 0],
        "the camera's statistics count what it appended"
    );
    let mut captured = DrawList::default();
    let mut drawn = DrawInstances::default();
    captured.build(
        &mut drawn,
        &scene,
        &View::shadow_cascade(cascade),
        Some(mask),
        Population::CaptureShadow,
    );
    let cpu = pairs(drawn.entries());
    assert!(!cpu.is_empty());
    assert_eq!(
        pairs(&culled.views[1]),
        cpu,
        "the cascade's instances and meshes"
    );
}

// Plausible defects: a frame rendered and abandoned leaving its readback to
// be mapped by the next submitted frame, although its copy never ran, or
// its counts adding to the next frame's; statistics reported before any
// frame completes. The oracle is the scene: a submitted frame's camera
// holds two squares, a section of two triangles each, one static and one
// moving.
#[test]
fn an_abandoned_frame_reports_nothing_and_the_next_reports_its_own() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let gpu = (&device, &queue);
    let mut scene = Scene::new(&device, &queue);
    let model = scene
        .add_asset(&device, &queue, asset(vec![square(1.)]))
        .unwrap()
        .model;
    for (x, mobility) in [(-1., Mobility::Static), (1., Mobility::Moving)] {
        let pose = Mat4::from_translation(Vec3::new(x, 0., -5.));
        place(gpu, &mut scene, model, pose, (true, true, mobility));
    }
    let settings = crate::settings::Settings {
        antialiasing: crate::settings::Antialiasing::Off,
        ..Default::default()
    };
    let mut renderer = crate::Renderer::for_test(&device, &queue, [64; 2], &settings);
    let input = crate::FrameInput::new(crate::Camera {
        view: Mat4::IDENTITY,
        projection: crate::perspective(1., 1., 0.1),
        eye: Vec3::ZERO,
    });
    let output =
        crate::view::targets::target(&device, "output", [64; 2], crate::shading::gbuffer::COLOR);
    let render = |renderer: &mut crate::Renderer, scene: &mut Scene| {
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
        encoder
    };
    // Abandoned: never submitted, never finished.
    drop(render(&mut renderer, &mut scene));
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    assert_eq!(renderer.geometry_stats(&device), None, "no frame completed");
    let encoder = render(&mut renderer, &mut scene);
    queue.submit([encoder.finish()]);
    renderer.finish_frame(&mut scene);
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    let stats = renderer
        .geometry_stats(&device)
        .expect("the submitted frame completed");
    assert_eq!(stats.static_instances, (1, 2));
    assert_eq!(stats.moving_instances, (1, 2));
}

// Plausible defects: a mesh past the most sections a cull strides, or a
// chain past the most levels its walk takes, accepted, so the GPU skips
// sections or levels it should draw; or the limits off by one. The oracle
// is the caps the spec names: 65,536 sections of 128 triangles a mesh, and
// 8 alternatives a mesh.
#[test]
fn meshes_and_chains_past_the_culls_caps_are_refused() {
    let vertices = square(1.).vertices[..3].to_vec();
    let triangles = 65_536 * 128 + 1;
    let refused = crate::PreparedModel::new(vec![crate::ModelMesh {
        material: crate::MaterialId::issue(0, 0),
        vertices,
        indices: (0..triangles * 3).map(|index| index % 3).collect(),
        deformation: Default::default(),
    }]);
    assert!(matches!(refused, Err(crate::SceneError::TooManySections)));
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    let base = scene
        .add_asset(&device, &queue, asset(vec![square(1.)]))
        .unwrap()
        .model;
    let alternative = scene
        .add_asset(&device, &queue, asset(vec![square(1.)]))
        .unwrap()
        .model;
    let lods = |count| {
        vec![
            MeshLod {
                model: alternative,
                mesh: 0,
                max_error: 0.01,
            };
            count
        ]
    };
    assert!(matches!(
        scene.set_mesh_lods(base, 0, lods(9)),
        Err(crate::SceneError::TooManyLods)
    ));
    scene.set_mesh_lods(base, 0, lods(8)).unwrap();
}

// A mesh's section count at the cap's edge: 65,536 full sections pass, one
// more triangle does not. Pure arithmetic of the refusal, at the triangle
// counts the spec gives, without preparing eight million triangles.
#[test]
fn the_section_cap_holds_exactly_its_triangles() {
    assert!(crate::scene::prepared::fits_sections(65_536 * 128));
    assert!(!crate::scene::prepared::fits_sections(65_536 * 128 + 1));
}
