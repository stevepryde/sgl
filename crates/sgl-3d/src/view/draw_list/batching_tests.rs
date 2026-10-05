//! Which draws merge into one instanced draw, against what each instance
//! and mesh was authored with.
use super::{DrawInstances, DrawList, Geometry};
use crate::content::identity::Identity;
use crate::shading::uniforms::ViewUniform;
use crate::view::View;
use crate::view::population::Population;
use crate::{AlphaMode, InstanceState, Mobility, Scene, test_support};
use bytemuck::Zeroable;
use glam::{Mat4, Vec3};
use std::collections::{HashMap, HashSet};

// Plausible defects: a batch that takes instances drawn with another
// material, another pipeline (a mirrored pose's opposite face culling, a
// masked material's discard), another model's geometry or another
// mobility; a draw lost or drawn twice; equal draws left unmerged; an
// instance's meshes drawn out of their authored order. The oracle is what
// each mesh and instance was authored with: its model, mesh, material,
// alpha mode, whether its pose mirrors and its mobility.
#[test]
fn merged_draws_share_material_pipeline_and_mobility() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut asset = test_support::cube();
    // Two meshes of the same geometry: single-sided opaque, then masked.
    asset.materials[0].double_sided = false;
    let mut masked = asset.materials[0].clone();
    masked.alpha = AlphaMode::Mask { cutoff: 0.5 };
    masked.base = [0.2, 0.4, 0.6, 1.];
    asset.materials.push(masked);
    let mut second = asset.meshes[0].clone();
    second.material = 1;
    asset.meshes.push(second);
    let mut scene = Scene::new(&device, &queue);
    let a = scene.add_asset(&device, &queue, asset.clone()).unwrap();
    // Another model of the same geometry and its own copies of the materials.
    let b = scene.add_asset(&device, &queue, asset).unwrap();
    // (model, materials, mirrored, mobility) of each instance, in the order
    // added.
    let placed = [
        (&a, false, Mobility::Moving),
        (&b, false, Mobility::Moving),
        (&a, true, Mobility::Moving),
        (&a, false, Mobility::Static),
        (&a, false, Mobility::Moving),
        (&b, true, Mobility::Static),
        (&a, true, Mobility::Moving),
        (&a, false, Mobility::Static),
        (&b, false, Mobility::Moving),
    ];
    let mut authored = HashMap::new();
    for (index, &(ids, mirrored, mobility)) in placed.iter().enumerate() {
        let x = index as f32 * 2.;
        let scale = if mirrored {
            Vec3::new(-1., 1., 1.)
        } else {
            Vec3::ONE
        };
        let state = InstanceState {
            model: ids.model,
            pose: Mat4::from_scale_rotation_translation(
                scale,
                glam::Quat::IDENTITY,
                Vec3::new(x, 0., -10.),
            ),
            visible: true,
            capture_visible: true,
        };
        let instance = scene
            .add_instance(&device, &queue, state, mobility)
            .unwrap();
        for mesh in 0..2 {
            authored.insert(
                (instance.index(), mesh),
                (ids.model, mesh, ids.materials[mesh], mirrored, mobility),
            );
        }
    }
    let camera = View::camera(ViewUniform {
        view: Mat4::IDENTITY.to_cols_array_2d(),
        projection: crate::perspective(1., 1., 0.1).to_cols_array_2d(),
        ..ViewUniform::zeroed()
    });
    let mut list = DrawList::default();
    let population = Population::Camera {
        lod: None,
        cull: false,
        hidden: None,
    };
    list.build(
        &mut DrawInstances::default(),
        &scene,
        &camera,
        None,
        population,
    );
    let mut drawn = HashSet::new();
    // Each instance's meshes, in the order drawn.
    let mut order: HashMap<usize, Vec<usize>> = HashMap::new();
    for batch in &list.batches {
        let Geometry::Mesh { mesh, .. } = batch.key.geometry else {
            panic!("rigid meshes draw their own geometry");
        };
        let instances = list.instances_of(batch);
        let shared: HashSet<_> = instances
            .iter()
            .map(|drawn| authored[&(drawn.object as usize, mesh)])
            .collect();
        assert_eq!(
            shared.len(),
            1,
            "one draw holds instances authored differently: {:?}",
            shared
                .iter()
                .map(|tuple| (tuple.3, tuple.4))
                .collect::<Vec<_>>()
        );
        for instance in instances {
            let instance = instance.object as usize;
            assert!(
                drawn.insert((instance, mesh)),
                "mesh {mesh} of {instance} drawn twice"
            );
            order.entry(instance).or_default().push(mesh);
        }
    }
    assert_eq!(
        drawn.len(),
        authored.len(),
        "every mesh of every instance draws"
    );
    let distinct: HashSet<_> = authored.values().collect();
    assert_eq!(
        list.batches.len(),
        distinct.len(),
        "each set of equally authored draws is one draw"
    );
    for (instance, meshes) in order {
        assert_eq!(
            meshes,
            [0, 1],
            "instance {instance}'s meshes in authored order"
        );
    }
}

// Plausible defect: draws whose culled ranges share their first range but
// differ after it merged into one bin, so an instance draws another's
// sections. The fixture: a mesh of four leaves, the second behind the
// camera and the outer two either side of the view, so each instance's pose
// decides which of them it shows. The oracle is that authored layout: which
// leaves each instance's pose places in the camera's view.
#[test]
fn culled_draws_merge_only_with_the_same_sections() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut asset = test_support::cube();
    // Leaves 0 to 3: ahead, behind the camera, to the right, to the left.
    asset.meshes = vec![test_support::leaf_clusters(&[
        Vec3::new(0., 0., -10.),
        Vec3::new(0., 0., 20.),
        Vec3::new(6., 0., -10.),
        Vec3::new(-6., 0., -10.),
    ])];
    let mut scene = Scene::new(&device, &queue);
    let model = scene.add_asset(&device, &queue, asset).unwrap().model;
    // The view is ±5.46 m wide 10 m ahead: moved left, an instance shows
    // leaves 0 and 2; moved right, leaves 0 and 3.
    let placed = [(-2., [0, 2]), (2., [0, 3]), (-2.5, [0, 2])];
    let instances: Vec<_> = placed
        .iter()
        .map(|&(x, _)| {
            let state = InstanceState {
                model,
                pose: Mat4::from_translation(Vec3::new(x, 0., 0.)),
                visible: true,
                capture_visible: true,
            };
            scene
                .add_instance(&device, &queue, state, Mobility::Static)
                .unwrap()
                .index()
        })
        .collect();
    let projection = crate::perspective(1., 1., 0.1);
    let camera = View::camera(ViewUniform {
        view: Mat4::IDENTITY.to_cols_array_2d(),
        projection: projection.to_cols_array_2d(),
        view_projection: projection.to_cols_array_2d(),
        ..ViewUniform::zeroed()
    });
    let mut list = DrawList::default();
    let population = Population::Camera {
        lod: None,
        cull: true,
        hidden: None,
    };
    list.build(
        &mut DrawInstances::default(),
        &scene,
        &camera,
        None,
        population,
    );
    let leaf = crate::scene::mesh_ranges::INDICES_PER_LEAF as u32;
    // Each instance's drawn leaves, and the instances each draw holds.
    let mut leaves: HashMap<usize, Vec<u32>> = HashMap::new();
    let mut together = Vec::new();
    for batch in &list.batches {
        let held: Vec<_> = list
            .instances_of(batch)
            .iter()
            .map(|drawn| drawn.object as usize)
            .collect();
        for range in &list.ranges[batch.ranges.clone()] {
            for &instance in &held {
                leaves
                    .entry(instance)
                    .or_default()
                    .extend((range.start / leaf)..(range.end / leaf));
            }
        }
        together.push(held);
    }
    for (instance, (_, expected)) in instances.iter().zip(&placed) {
        assert_eq!(leaves[instance], expected, "instance {instance}'s leaves");
    }
    together.sort();
    assert_eq!(
        together,
        [vec![instances[0], instances[2]], vec![instances[1]]],
        "the instances that show the same leaves share one draw"
    );
}
