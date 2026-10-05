//! A streamed and edited world's retained resources: chunks added,
//! replaced and removed over many cycles keep the scene's buffers bounded.
use crate::asset::{CpuMesh, Vertex};
use crate::*;
use glam::{Mat4, Vec3};

/// A chunk of `quads` unit quads, scattered by `seed` over a 16 m cube.
fn chunk(quads: usize, seed: u64, material: MaterialId) -> PreparedModel {
    let mut mesh = CpuMesh {
        vertices: Vec::with_capacity(quads * 4),
        indices: Vec::with_capacity(quads * 6),
        material: 0,
        deformation: Default::default(),
    };
    for quad in 0..quads as u64 {
        let cell =
            (seed ^ quad.wrapping_mul(0x9e37_79b9_7f4a_7c15)).wrapping_mul(0xff51_afd7_ed55_8ccd);
        let at = Vec3::new(
            (cell % 16) as f32,
            ((cell >> 8) % 16) as f32,
            ((cell >> 16) % 16) as f32,
        );
        let start = mesh.vertices.len() as u32;
        for corner in [Vec3::ZERO, Vec3::X, Vec3::new(1., 0., 1.), Vec3::Z] {
            mesh.vertices.push(Vertex {
                tangent: [1., 0., 0., 1.],
                lightmap_bounds: [0., 0., 1., 1.],
                lightmap_uv: [0.; 2],
                position: (at + corner).to_array(),
                normal: [0., 1., 0.],
                uv: [corner.x, corner.z],
                color: [1.; 4],
            });
        }
        mesh.indices
            .extend([0, 2, 1, 0, 3, 2].map(|index| start + index));
    }
    PreparedModel::new(vec![ModelMesh {
        vertices: mesh.vertices,
        indices: mesh.indices,
        material,
        deformation: Default::default(),
    }])
    .unwrap()
}

/// The sizes of the buffers content grows: the ray source, the object
/// records, the ray entries and the geometry buffers.
fn retained(scene: &Scene) -> [u64; 4] {
    [
        scene.rays.source().size(),
        scene.instances.objects.buffer().size(),
        scene.ray_instances.buffer().size(),
        scene.geometry.sizes()[0],
    ]
}

/// The slots chunks stream through.
const SLOTS: usize = 48;
/// The most words the ray source holds for a quad of a chunk: its four
/// vertices, six indices, and for each of its two triangles at most a BVH
/// node (12 words) and a leaf record (2).
const QUAD_WORDS: usize = 4 * std::mem::size_of::<Vertex>() / 4 + 6 + 2 * (12 + 2);
/// The most words a slot's chunk holds besides its quads: its model's mesh
/// record (4), and its instance's share of the instance BVHs, whose ranges
/// keep room for twice their instances, at most a node and a leaf record
/// for each.
const SLOT_WORDS: usize = 4 + 2 * (12 + 2);

// Plausible defects: removed or replaced content's ranges or records never
// freed or reused, so each insert grows a buffer; or freed ranges left so
// fragmented that a steady stream of chunks keeps growing the ray source.
// The oracle is the requirement that bounded content keeps bounded
// resources, with the content counted by the test itself. Over 2,400 insert,
// replace and remove cycles of at most 48 chunks of up to 600 quads, each
// submitted as a frame:
// - the words the ray source's allocator reports holding stay within what
//   the live chunks can need (their quads at `QUAD_WORDS`, `SLOT_WORDS` a
//   slot) above what the scene held before the first chunk;
// - the buffers grow only while the test's content reaches a new peak of
//   quads or instances: they end the size they had at the last peak;
// - the ray source's last word in use stays within a fragmentation
//   allowance of twice the most words the allocator held, through the run.
#[test]
fn a_bounded_stream_of_chunks_keeps_bounded_buffers() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    let material = scene
        .add_materials(&device, &queue, &[asset::Material::default()], &[])
        .unwrap()[0];
    let (_, before) = scene.rays.words_in_use();
    let mut seed = 0x5eed_u64;
    let mut random = move || {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        seed >> 33
    };
    // Each slot's chunk and its quads.
    let mut slots: Vec<Option<(ModelId, InstanceId, usize)>> = vec![None; SLOTS];
    // The most quads and instances the test held, the buffers' sizes when it
    // last reached either, the most words the allocator held, and its last
    // word in use at its highest.
    let (mut peak_quads, mut peak_instances) = (0, 0);
    let mut at_peak = retained(&scene);
    let (mut most_live, mut highest_end) = (0, 0);
    for cycle in 0..2_400 {
        let slot = random() as usize % SLOTS;
        let quads = match random() % 4 {
            0 => 0,
            1 => random() as usize % 51,
            _ => 300 + random() as usize % 301,
        };
        match slots[slot] {
            None if quads > 0 => {
                let model = scene
                    .add_model(&device, &queue, chunk(quads, random(), material))
                    .unwrap();
                let pose = Mat4::from_translation(Vec3::new(slot as f32 * 16., 0., 0.));
                let instance = scene
                    .add_instance(
                        &device,
                        &queue,
                        InstanceState {
                            pose,
                            ..InstanceState::new(model)
                        },
                        Mobility::Static,
                    )
                    .unwrap();
                slots[slot] = Some((model, instance, quads));
            }
            None => {}
            Some((model, instance, _)) if random() % 3 == 0 => {
                scene.remove_instance(instance).unwrap();
                scene.remove_model(model).unwrap();
                slots[slot] = None;
            }
            Some((model, instance, _)) => {
                scene
                    .set_model(&device, &queue, model, chunk(quads, random(), material))
                    .unwrap();
                slots[slot] = Some((model, instance, quads));
            }
        }
        scene.update_rays(&device, &queue, !0);
        queue.submit([]);
        scene.finish_frame();
        let live_quads: usize = slots.iter().flatten().map(|(_, _, quads)| quads).sum();
        let instances = slots.iter().flatten().count();
        let (end, live) = scene.rays.words_in_use();
        let bound = before + (live_quads * QUAD_WORDS + SLOTS * SLOT_WORDS) as u64;
        assert!(
            live <= bound,
            "cycle {cycle}: the ray source holds {live} words for {live_quads} live quads, at most {bound}"
        );
        most_live = most_live.max(live);
        highest_end = highest_end.max(end);
        if live_quads > peak_quads || instances > peak_instances {
            peak_quads = peak_quads.max(live_quads);
            peak_instances = peak_instances.max(instances);
            at_peak = retained(&scene);
        }
    }
    assert_eq!(
        retained(&scene),
        at_peak,
        "buffers grew after content last reached its peak of {peak_quads} quads and {peak_instances} instances"
    );
    assert!(
        highest_end <= 2 * most_live,
        "the ray source's last word in use reached {highest_end} for at most {most_live} words of content"
    );
}
