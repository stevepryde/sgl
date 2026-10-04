//! The blended population's order, against view distances measured
//! independently of the view matrix.
use super::{DrawInstances, DrawList, Geometry};
use crate::content::identity::Identity;
use crate::shading::uniforms::ViewUniform;
use crate::view::View;
use crate::view::population::Population;
use crate::{AlphaMode, InstanceState, Mobility, Scene, test_support};
use bytemuck::Zeroable;
use glam::camera;
use glam::{Mat4, Quat, Vec3};

// Plausible defects: blended draws left in instance order, sorted front to
// back, keyed by the instance's origin or its model's bounds rather than
// each drawn mesh's bounds centre at its pose, or by a depth taken in the
// wrong space; opaque meshes drawn with them; draws merged across another
// draw between them, which reorders them, or consecutive equal draws left
// apart. The oracle measures each
// blended mesh's world bounds centre, found from its vertices, along the
// camera's forward direction from its eye, as the camera was constructed
// rather than through its view matrix, and orders the farthest first.
#[test]
fn blended_draws_are_sorted_back_to_front_by_mesh_bounds_centre() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut asset = test_support::cube();
    let cube = asset.meshes[0].clone();
    asset.materials.push(asset.materials[0].clone());
    asset.materials[0].alpha = AlphaMode::Blend {
        receives_screen_space_reflections: false,
    };
    // Two blended meshes whose bounds centres lie away from the model's
    // origin, and an opaque one at it.
    let offsets = [Vec3::new(0., 0., 3.), Vec3::new(2., 1., -4.)];
    asset.meshes = offsets
        .iter()
        .map(|&offset| {
            let mut mesh = cube.clone();
            for vertex in &mut mesh.vertices {
                vertex.position = (Vec3::from(vertex.position) + offset).to_array();
            }
            mesh
        })
        .collect();
    let mut opaque = cube;
    opaque.material = 1;
    asset.meshes.push(opaque);
    let centres: Vec<Vec3> = asset.meshes[..2]
        .iter()
        .map(|mesh| {
            let (min, max) = mesh.vertices.iter().fold(
                (Vec3::INFINITY, Vec3::NEG_INFINITY),
                |(min, max), vertex| {
                    let p = Vec3::from(vertex.position);
                    (min.min(p), max.max(p))
                },
            );
            (min + max) / 2.
        })
        .collect();
    let mut scene = Scene::new(&device, &queue);
    let model = scene.add_asset(&device, &queue, asset).unwrap().model;
    // Instances turned, scaled and spread by a fixed sequence.
    let mut seed = 0x2545_f491_u32;
    let mut random = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed as f32 / u32::MAX as f32
    };
    let mut expected = Vec::new();
    let eye = Vec3::new(1., 2., 12.);
    let target = Vec3::new(0., 0., -2.);
    let forward = (target - eye).normalize();
    for _ in 0..12 {
        let pose = Mat4::from_scale_rotation_translation(
            Vec3::splat(0.5 + random()),
            Quat::from_rotation_y(random() * std::f32::consts::TAU),
            Vec3::new(
                random() * 16. - 8.,
                random() * 4. - 2.,
                random() * 16. - 12.,
            ),
        );
        let state = InstanceState {
            model,
            pose,
            visible: true,
            capture_visible: true,
        };
        let instance = scene
            .add_instance(&device, &queue, state, Mobility::Moving)
            .unwrap();
        for (mesh, centre) in centres.iter().enumerate() {
            let distance = (pose.transform_point3(*centre) - eye).dot(forward);
            expected.push((distance, instance.index(), mesh));
        }
    }
    expected.sort_by(|a, b| b.0.total_cmp(&a.0));
    assert!(
        expected.windows(2).all(|pair| pair[0].0 > pair[1].0),
        "the fixture's distances must differ"
    );
    let view = camera::rh::view::look_at_mat4(eye, target, Vec3::Y);
    let projection = crate::perspective(1., 1., 0.1);
    let camera = View::camera(ViewUniform {
        view: view.to_cols_array_2d(),
        projection: projection.to_cols_array_2d(),
        view_projection: (projection * view).to_cols_array_2d(),
        eye: eye.to_array(),
        ..ViewUniform::zeroed()
    });
    let mut list = DrawList::default();
    let population = Population::Blended {
        lod: None,
        cull: false,
    };
    list.build(
        &mut DrawInstances::default(),
        &scene,
        &camera,
        None,
        population,
    );
    let drawn: Vec<_> = list
        .batches
        .iter()
        .flat_map(|batch| {
            let Geometry::Mesh { mesh, .. } = batch.key.geometry else {
                panic!("these blended batches draw whole rigid meshes")
            };
            list.instances_of(batch)
                .iter()
                .map(move |drawn| (drawn.object as usize, mesh))
        })
        .collect();
    let order: Vec<_> = expected
        .iter()
        .map(|&(_, instance, mesh)| (instance, mesh))
        .collect();
    assert_eq!(drawn, order);
    // Draws merge only where they follow one another in that order, and do
    // there: every instance is a moving, unmirrored instance of one model,
    // so each run of one mesh is one draw.
    let runs = 1 + order
        .windows(2)
        .filter(|pair| pair[0].1 != pair[1].1)
        .count();
    assert!(runs < order.len(), "the fixture's order must have runs");
    assert_eq!(list.batches.len(), runs);
}

// Plausible defect: blended draws of several index ranges merged into one
// instanced draw, which draws each range for every instance in turn, so a
// farther instance's later range blends over a nearer instance's earlier
// one. The fixture: a blended mesh of three leaves, the middle one behind
// the camera, so culling cuts each of two instances into two ranges; the
// oracle is the instances' authored depths. Each draw call rasterizes its
// instances in order, so the draws the list issues, expanded per instance,
// are the order the GPU blends them in.
#[test]
fn culled_blended_draws_keep_each_instance_whole_and_back_to_front() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut asset = test_support::cube();
    asset.materials[0].alpha = AlphaMode::Blend {
        receives_screen_space_reflections: false,
    };
    asset.meshes = vec![test_support::leaf_clusters(&[
        Vec3::new(-2., 0., -10.),
        Vec3::new(0., 0., 20.),
        Vec3::new(2., 0., -10.),
    ])];
    let mut scene = Scene::new(&device, &queue);
    let model = scene.add_asset(&device, &queue, asset).unwrap().model;
    // Added near first, so a list left in instance order fails.
    let [near, far] = [0., -6.].map(|z| {
        let state = InstanceState {
            model,
            pose: Mat4::from_translation(Vec3::new(0., 0., z)),
            visible: true,
            capture_visible: true,
        };
        scene
            .add_instance(&device, &queue, state, Mobility::Moving)
            .unwrap()
            .index()
    });
    let projection = crate::perspective(1., 1., 0.1);
    let camera = View::camera(ViewUniform {
        view: Mat4::IDENTITY.to_cols_array_2d(),
        projection: projection.to_cols_array_2d(),
        view_projection: projection.to_cols_array_2d(),
        ..ViewUniform::zeroed()
    });
    let mut list = DrawList::default();
    let population = Population::Blended {
        lod: None,
        cull: true,
    };
    list.build(
        &mut DrawInstances::default(),
        &scene,
        &camera,
        None,
        population,
    );
    let drawn: Vec<_> = list
        .calls()
        .flat_map(|(batch, range)| {
            list.instances_of(batch)
                .iter()
                .map(move |drawn| (drawn.object as usize, range.clone()))
        })
        .collect();
    let leaf = crate::scene::mesh_ranges::INDICES_PER_LEAF as u32;
    let ranges = [0..leaf, 2 * leaf..3 * leaf];
    let expected: Vec<_> = [far, near]
        .into_iter()
        .flat_map(|instance| ranges.iter().map(move |range| (instance, range.clone())))
        .collect();
    assert_eq!(drawn, expected);
}
