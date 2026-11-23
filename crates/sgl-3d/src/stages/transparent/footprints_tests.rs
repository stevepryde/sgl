//! Instance footprints observed in real frames (the register's M-23): the
//! transparency and composition mask where a changed instance stood and
//! stands, and nowhere else, for one frame, under TAA and FSR. Every
//! expected pixel comes from the fixture's geometry projected by hand,
//! never from the shaders.
use crate::asset::{Asset, CpuMesh, Image, MaterialTexture, Vertex};
use crate::deformation::{Influence, MeshDeformation};
use crate::renderer::test_frame::TestRenderer;
use crate::settings::{self, Settings};
use crate::shading::gbuffer;
use crate::{
    AlphaMode, Camera, FrameInput, InstanceId, InstanceState, Mobility, ModelId, Scene,
    test_support,
};
use glam::{Mat4, Vec2, Vec3};

const SIZE: [u32; 2] = [64, 64];

/// Where the card stands, and the point of the view far from every card.
const LEFT: Vec3 = Vec3::new(-1.2, 0., -5.);
const RIGHT: Vec3 = Vec3::new(1.2, 0., -5.);
const FAR: Vec3 = Vec3::new(0., 1.6, -5.);

fn settings(antialiasing: settings::Antialiasing) -> Settings {
    Settings {
        antialiasing,
        fsr_quality: settings::FsrQuality::NativeAa,
        bloom: settings::Bloom::Off,
        atmosphere: false,
        ..Settings::default()
    }
}

/// The camera at the origin looking down -Z.
fn camera() -> Camera {
    Camera {
        view: Mat4::IDENTITY,
        projection: crate::perspective(1., 1., 0.1),
        eye: Vec3::ZERO,
    }
}

/// Where world point `p` lands in the camera's frame, in pixels from the
/// top left.
fn pixel(p: Vec3) -> Vec2 {
    let camera = camera();
    let clip = camera.projection * camera.view * p.extend(1.);
    let ndc = clip.truncate().truncate() / clip.w;
    Vec2::new(
        (ndc.x * 0.5 + 0.5) * SIZE[0] as f32,
        (0.5 - ndc.y * 0.5) * SIZE[1] as f32,
    )
}

/// A square facing +Z centred at `centre`, `half` metres to each side, its
/// U rising along +X, with the fixture material.
fn square(centre: Vec3, half: f32) -> CpuMesh {
    CpuMesh {
        vertices: [(-1., -1.), (1., -1.), (1., 1.), (-1., 1.)]
            .map(|(x, y)| Vertex {
                tangent: [1., 0., 0., 1.],
                lightmap_bounds: [0., 0., 1., 1.],
                lightmap_uv: [0.; 2],
                position: (centre + Vec3::new(x * half, y * half, 0.)).to_array(),
                normal: [0., 0., 1.],
                uv: [(x + 1.) / 2., (1. - y) / 2.],
                uv1: [0.0; 2],
                color: [1.; 4],
            })
            .to_vec(),
        indices: vec![0, 1, 2, 0, 2, 3],
        material: 0,
        deformation: MeshDeformation::default(),
    }
}

/// An asset of one square of `half` about `centre` in its model's space.
fn card(centre: Vec3, half: f32) -> Asset {
    let mut asset = test_support::cube();
    asset.meshes = vec![square(centre, half)];
    asset
}

/// A renderer running `antialiasing` at the scene size, or `None` where it
/// cannot.
fn renderer(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    antialiasing: settings::Antialiasing,
) -> Option<(TestRenderer, Settings, wgpu::TextureView)> {
    let settings = settings(antialiasing);
    let renderer = TestRenderer::for_test(device, queue, SIZE, &settings);
    if renderer.view.antialiasing_in_effect(&settings) != antialiasing {
        eprintln!("skipping {antialiasing:?}: {:?}", renderer.view.fsr_error());
        return None;
    }
    let output = crate::view::targets::target(device, "footprint frames", SIZE, gbuffer::COLOR);
    Some((renderer, settings, output))
}

/// One frame of `scene`, finished, and its transparency and composition
/// mask, 0..=255 at each pixel; none where the frame held no masks.
fn frame(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    (renderer, settings, output): &mut (TestRenderer, Settings, wgpu::TextureView),
    scene: &mut Scene,
) -> Option<Vec<u8>> {
    let input = FrameInput::new(camera());
    let mut encoder = device.create_command_encoder(&Default::default());
    renderer.render(
        device,
        queue,
        &mut encoder,
        scene,
        &input,
        settings,
        output,
        None,
    );
    queue.submit([encoder.finish()]);
    renderer.finish_frame(scene);
    let masks = renderer.targets().temporal_masks.as_ref()?;
    Some(test_support::read(device, queue, masks[1].texture(), 1))
}

fn at(marks: &[u8], p: Vec3) -> u8 {
    let p = pixel(p);
    marks[p.y as usize * SIZE[0] as usize + p.x as usize]
}

fn place(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    scene: &mut Scene,
    model: ModelId,
    pose: Mat4,
    mobility: Mobility,
) -> InstanceId {
    let state = InstanceState {
        pose,
        ..InstanceState::new(model)
    };
    scene.add_instance(device, queue, state, mobility).unwrap()
}

/// An edit of a scene holding one card, and the points it marks.
type Edit = fn(&mut Scene, &wgpu::Device, &wgpu::Queue, &Fixture);

/// What an edit works on: the card at `LEFT` (its instance, model and
/// material), the model of a card at `RIGHT` in its model's space, and a
/// static card below `LEFT`.
struct Fixture {
    card: InstanceId,
    model: ModelId,
    material: crate::MaterialId,
    right: ModelId,
    fixed: InstanceId,
}

fn fixture(device: &wgpu::Device, queue: &wgpu::Queue, scene: &mut Scene) -> Fixture {
    let ids = scene.add_asset(device, queue, card(LEFT, 0.5)).unwrap();
    let (model, material) = (ids.model, ids.materials[0]);
    let right = scene
        .add_asset(device, queue, card(RIGHT, 0.5))
        .unwrap()
        .model;
    let card = place(
        device,
        queue,
        scene,
        model,
        Mat4::IDENTITY,
        Mobility::Moving,
    );
    // A static card below, which only the static move edits.
    let low = scene.add_asset(device, queue, card_below()).unwrap().model;
    let fixed = place(device, queue, scene, low, Mat4::IDENTITY, Mobility::Static);
    Fixture {
        card,
        model,
        material,
        right,
        fixed,
    }
}

/// The static card: below `LEFT`.
fn card_below() -> Asset {
    card(LEFT - Vec3::Y * 1.2, 0.4)
}

// Plausible defects: a change no motion vector describes leaving its old
// pixels or its new ones unmarked (TAA then fades the change at rest, FSR
// keeps its locks there), marking pixels far from it, keeping the mark a
// frame longer, or raising the view-wide signal as before. Each edit is
// made at rest on a scene holding no other mask writer. The oracle is
// M-23: in the frame after the edit the transparency and composition mask
// is 255 where the card stood and where it stands, hand-projected, and 0
// far from both; the scene raises nothing view-wide; a frame later nothing
// is marked or pending and, under TAA, the masks are released, as in every
// frame without a change. FSR, which holds the masks every frame, runs the
// removal and the addition where the device has it.
#[test]
fn a_changed_instance_marks_where_it_stood_and_stands_and_nothing_else() {
    let Some((device, queue)) = test_support::fsr_device() else {
        return;
    };
    let edits: [(&str, Edit, &[Vec3]); 7] = [
        (
            "removed",
            |scene, _, _, f| scene.remove_instance(f.card).unwrap(),
            &[LEFT],
        ),
        (
            "hidden",
            |scene, _, queue, f| {
                let state = InstanceState {
                    visible: false,
                    ..*scene.instance(f.card).unwrap()
                };
                scene.set_instance(queue, f.card, state).unwrap();
            },
            &[LEFT],
        ),
        (
            "added",
            |scene, device, queue, f| {
                place(
                    device,
                    queue,
                    scene,
                    f.right,
                    Mat4::IDENTITY,
                    Mobility::Moving,
                );
            },
            &[RIGHT],
        ),
        (
            "given another model",
            |scene, _, queue, f| {
                let state = InstanceState {
                    model: f.right,
                    ..*scene.instance(f.card).unwrap()
                };
                scene.set_instance(queue, f.card, state).unwrap();
            },
            &[LEFT, RIGHT],
        ),
        (
            "a static card moved",
            |scene, _, queue, f| {
                let state = InstanceState {
                    pose: Mat4::from_translation(RIGHT - LEFT),
                    ..*scene.instance(f.fixed).unwrap()
                };
                scene.set_instance(queue, f.fixed, state).unwrap();
            },
            &[Vec3::new(-1.2, -1.2, -5.), Vec3::new(1.2, -1.2, -5.)],
        ),
        (
            "its geometry replaced",
            |scene, device, queue, f| {
                let mesh = square(RIGHT, 0.5);
                let prepared = crate::PreparedModel::new(vec![crate::ModelMesh {
                    vertices: mesh.vertices,
                    indices: mesh.indices,
                    material: f.material,
                    deformation: MeshDeformation::default(),
                }])
                .unwrap();
                scene.set_model(device, queue, f.model, prepared).unwrap();
            },
            &[LEFT, RIGHT],
        ),
        (
            "removed with its model, and another placed",
            |scene, device, queue, f| {
                scene.remove_instance(f.card).unwrap();
                scene.remove_model(f.model).unwrap();
                // Its words free now would take the removed card's, and
                // where the card stood would mark this card's at `RIGHT`.
                let other = scene
                    .add_asset(device, queue, card(RIGHT, 0.5))
                    .unwrap()
                    .model;
                place(
                    device,
                    queue,
                    scene,
                    other,
                    Mat4::from_translation(Vec3::Y * 9.),
                    Mobility::Moving,
                );
            },
            &[LEFT],
        ),
    ];
    let methods = [settings::Antialiasing::Taa, settings::Antialiasing::Fsr2];
    for method in methods {
        for (name, edit, marked) in edits {
            if method != settings::Antialiasing::Taa && !matches!(name, "removed" | "added") {
                continue;
            }
            let Some(mut frames) = renderer(&device, &queue, method) else {
                continue;
            };
            let mut scene = Scene::new(&device, &queue);
            let f = fixture(&device, &queue, &mut scene);
            for _ in 0..2 {
                frame(&device, &queue, &mut frames, &mut scene);
            }
            if method == settings::Antialiasing::Taa {
                assert!(
                    frame(&device, &queue, &mut frames, &mut scene).is_none(),
                    "{name}: a TAA frame without a change holds no masks"
                );
            }
            edit(&mut scene, &device, &queue, &f);
            assert!(
                !scene.rigid_objects_changed(),
                "{method:?} {name}: nothing view-wide"
            );
            let marks = frame(&device, &queue, &mut frames, &mut scene)
                .unwrap_or_else(|| panic!("{method:?} {name}: the frame holds the masks"));
            for &p in marked {
                assert_eq!(at(&marks, p), 255, "{method:?} {name}: {p}");
            }
            assert_eq!(at(&marks, FAR), 0, "{method:?} {name}: far from it");
            let after = frame(&device, &queue, &mut frames, &mut scene);
            assert_eq!(
                scene.footprints.pending(),
                [0, 0],
                "{method:?} {name}: retired"
            );
            match after {
                None => assert_eq!(method, settings::Antialiasing::Taa),
                Some(marks) => {
                    for &p in marked {
                        assert_eq!(at(&marks, p), 0, "{method:?} {name}: a frame later, {p}");
                    }
                }
            }
        }
    }
}

// Plausible defects: where an instance stood marked through what lies in
// front of it, or where one stands left unmarked where the camera's cull
// draws a coarser level whose surface lies behind the base level's; a
// masked mesh's cut-out texels left unmarked though the frame before showed
// what lay there; a deforming instance marked at its bind pose rather than
// its deformed positions. The oracle is M-23's rule: where it stood is
// drawn at or in front of the opaque depth, so a card behind a wall marks
// nothing there; where it stands with no depth test, so a card appearing
// behind the wall marks the wall's pixels it projects to; cut-out texels
// are marked; a skinned card translated by its one joint marks where the
// joint put it, and not where its bind pose lies.
#[test]
fn footprints_follow_depth_cut_outs_and_deformation_as_drawn() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let Some(mut frames) = renderer(&device, &queue, settings::Antialiasing::Taa) else {
        return;
    };
    // A wall at 3 m before `LEFT`, covering it.
    let mut scene = Scene::new(&device, &queue);
    let wall = scene
        .add_asset(&device, &queue, card(Vec3::new(-1.2, 0., -3.), 0.8))
        .unwrap()
        .model;
    place(
        &device,
        &queue,
        &mut scene,
        wall,
        Mat4::IDENTITY,
        Mobility::Static,
    );
    let behind = scene
        .add_asset(&device, &queue, card(LEFT, 0.3))
        .unwrap()
        .model;
    let hidden = place(
        &device,
        &queue,
        &mut scene,
        behind,
        Mat4::IDENTITY,
        Mobility::Moving,
    );
    for _ in 0..3 {
        frame(&device, &queue, &mut frames, &mut scene);
    }
    scene.remove_instance(hidden).unwrap();
    let marks = frame(&device, &queue, &mut frames, &mut scene).unwrap();
    assert_eq!(at(&marks, LEFT), 0, "where a card stood behind the wall");
    for _ in 0..2 {
        frame(&device, &queue, &mut frames, &mut scene);
    }
    place(
        &device,
        &queue,
        &mut scene,
        behind,
        Mat4::IDENTITY,
        Mobility::Moving,
    );
    let marks = frame(&device, &queue, &mut frames, &mut scene).unwrap();
    assert_eq!(at(&marks, LEFT), 255, "where a card stands behind the wall");

    // A masked card whose left half is cut out, removed.
    let mut scene = Scene::new(&device, &queue);
    let mut masked = card(RIGHT, 0.5);
    masked.images = vec![Image::Rgba8(test_support::half_cut_out())];
    masked.materials[0].base_texture = Some(MaterialTexture::new(0));
    masked.materials[0].alpha = AlphaMode::Mask { cutoff: 0.5 };
    let masked = scene.add_asset(&device, &queue, masked).unwrap().model;
    let id = place(
        &device,
        &queue,
        &mut scene,
        masked,
        Mat4::IDENTITY,
        Mobility::Moving,
    );
    for _ in 0..3 {
        frame(&device, &queue, &mut frames, &mut scene);
    }
    scene.remove_instance(id).unwrap();
    let marks = frame(&device, &queue, &mut frames, &mut scene).unwrap();
    let cut_out = RIGHT - Vec3::X * 0.25;
    assert_eq!(at(&marks, cut_out), 255, "a cut-out texel where it stood");

    // A skinned card at `LEFT` whose joint carries it to `RIGHT`, removed.
    let mut scene = Scene::new(&device, &queue);
    let mut skinned = card(LEFT, 0.5);
    skinned.meshes[0].deformation = MeshDeformation {
        influences: vec![
            Influence {
                joints: [0; 4],
                weights: [1., 0., 0., 0.],
            };
            4
        ],
        morph_targets: Vec::new(),
    };
    let skinned = scene.add_asset(&device, &queue, skinned).unwrap().model;
    let id = place(
        &device,
        &queue,
        &mut scene,
        skinned,
        Mat4::IDENTITY,
        Mobility::Moving,
    );
    let joint = [Mat4::from_translation(RIGHT - LEFT)];
    scene
        .set_instance_deformation(&queue, id, &joint, &[])
        .unwrap();
    for _ in 0..3 {
        frame(&device, &queue, &mut frames, &mut scene);
    }
    scene.remove_instance(id).unwrap();
    let marks = frame(&device, &queue, &mut frames, &mut scene).unwrap();
    assert_eq!(at(&marks, RIGHT), 255, "where the joint put it");
    assert_eq!(at(&marks, LEFT), 0, "its bind pose");
}
