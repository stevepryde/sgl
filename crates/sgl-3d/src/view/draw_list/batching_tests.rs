//! Which draws merge into one instanced draw, against what each instance
//! and mesh was authored with.
use super::batching::{BatchKey, Batcher, InstanceDraw};
use super::{DrawInstances, DrawList, Geometry};
use crate::content::identity::{Identity, MaterialId, ModelId};
use crate::shading::uniforms::ViewUniform;
use crate::shading::vertex::DrawInstance;
use crate::view::View;
use crate::view::pipelines::{Alpha, Cull, Variant};
use crate::view::population::Population;
use crate::{AlphaMode, InstanceState, Mobility, Scene, test_support};
use bytemuck::Zeroable;
use glam::{Mat4, Vec3};
use std::collections::{HashMap, HashSet};

// Plausible defects: a batch that takes instances drawn with another
// material, another pipeline (a mirrored pose's opposite face culling, a
// masked material's discard) or another model's geometry; a draw lost or
// drawn twice; equal draws left unmerged; an instance's meshes drawn out of
// their authored order. The oracle is what each mesh and instance was
// authored with: its model, mesh, material, alpha mode and whether its pose
// mirrors. A probe capture's cascades bin their casters, static ones.
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
    // (model, materials, mirrored) of each instance, in the order added.
    let placed = [
        (&a, false),
        (&b, false),
        (&a, true),
        (&a, false),
        (&a, false),
        (&b, true),
        (&a, true),
        (&a, false),
        (&b, false),
    ];
    let mut authored = HashMap::new();
    for (index, &(ids, mirrored)) in placed.iter().enumerate() {
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
            .add_instance(&device, &queue, state, Mobility::Static)
            .unwrap();
        for mesh in 0..2 {
            authored.insert(
                (instance.index(), mesh),
                (ids.model, mesh, ids.materials[mesh], mirrored),
            );
        }
    }
    let camera = View::camera(ViewUniform {
        view: Mat4::IDENTITY.to_cols_array_2d(),
        projection: crate::perspective(1., 1., 0.1).to_cols_array_2d(),
        ..ViewUniform::zeroed()
    });
    let mut list = DrawList::default();
    list.build(
        &mut DrawInstances::default(),
        &scene,
        &camera,
        None,
        Population::CaptureShadow,
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
            shared.iter().map(|tuple| tuple.3).collect::<Vec<_>>()
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
// sections (a local-light face's casters bin by the cluster ranges each
// instance's pose reaches). The oracle is each draw's authored ranges: the
// batches must hold together exactly the draws authored with the same
// ranges, each with its own ranges. CPU only.
#[test]
fn draws_merge_only_with_the_same_ranges() {
    let key = BatchKey {
        variant: Variant {
            cull: Cull::Back,
            alpha: Alpha::Opaque,
            deformed: false,
        },
        material: MaterialId::issue(0, 1),
        geometry: Geometry::Clusters {
            model: ModelId::issue(0, 1),
            mesh: 0,
        },
        mobility: Mobility::Static,
    };
    // Each draw's ranges: the first and third share theirs; the second
    // shares only their first range.
    let authored = [
        vec![0..384, 768..1152],
        vec![0..384, 1152..1536],
        vec![0..384, 768..1152],
    ];
    let mut ranges = Vec::new();
    let mut batcher = Batcher::default();
    for (object, drawn) in authored.iter().enumerate() {
        let start = ranges.len();
        ranges.extend(drawn.iter().cloned());
        batcher.push(InstanceDraw {
            key,
            ranges: start..ranges.len(),
            instance: DrawInstance {
                object: object as u32,
                ..Zeroable::zeroed()
            },
            order: (0, 0),
        });
    }
    let (mut batches, mut instances) = (Vec::new(), Vec::new());
    batcher.bin((&ranges, &mut batches, &mut instances));
    let mut together: Vec<Vec<u32>> = batches
        .iter()
        .map(|batch| {
            let held: Vec<u32> = instances
                [batch.instances.start as usize..batch.instances.end as usize]
                .iter()
                .map(|drawn| drawn.object)
                .collect();
            for &object in &held {
                assert_eq!(ranges[batch.ranges.clone()], authored[object as usize][..]);
            }
            held
        })
        .collect();
    together.sort();
    assert_eq!(together, [vec![0, 2], vec![1]]);
}
