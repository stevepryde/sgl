//! Which edits leave instance footprints (the register's M-23), which raise
//! the view-wide signal instead (M-9), and how long each, and the words a
//! footprint names, last: observed through real scene edits and frames,
//! never by setting either directly.
use super::Scene;
use crate::content::identity::ModelId;
use crate::test_support;
use crate::{InstanceState, Mobility, SurfaceMaterial};
use glam::{Mat4, Vec3};

/// The footprints pending: stood states and stands.
fn pending(scene: &Scene) -> [usize; 2] {
    scene.footprints.pending()
}

fn shown(model: ModelId) -> InstanceState {
    InstanceState {
        model,
        pose: Mat4::IDENTITY,
        visible: true,
        capture_visible: true,
    }
}

fn at(x: f32) -> Mat4 {
    Mat4::from_translation(Vec3::new(x, 0., 0.))
}

// Plausible defects: a departure, a hiding or a geometry replacement left
// unmarked, so what it uncovers keeps the instance's history; marks retired
// before a frame begun after the edit is finished; repeated identical
// writes marking anything. The oracle is M-23: each change no motion
// vector describes leaves where the instance stood (a frame drew it) and
// where it stands (visible now) until a frame begun after it is finished,
// and raises nothing view-wide.
#[test]
fn footprints_track_departures_until_a_frame_is_finished_and_ignore_unchanged_poses() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    let model = scene
        .add_asset(&device, &queue, test_support::cube())
        .unwrap()
        .model;
    let mut state = shown(model);
    let id = scene
        .add_instance(&device, &queue, state, Mobility::Moving)
        .unwrap();
    assert_eq!(
        pending(&scene),
        [0, 1],
        "an appearance marks where it stands"
    );
    scene.test_frame(&device, &queue);
    assert_eq!(pending(&scene), [0, 0], "a finished frame retires it");
    scene.set_instance(&queue, id, state).unwrap();
    assert_eq!(pending(&scene), [0, 0], "a repeated pose marks nothing");
    scene
        .set_model(
            &device,
            &queue,
            model,
            crate::PreparedModel::new(Vec::new()).unwrap(),
        )
        .unwrap();
    assert_eq!(
        pending(&scene),
        [1, 0],
        "geometry removed under an unchanged pose leaves where it stood"
    );
    scene.test_frame(&device, &queue);
    assert_eq!(pending(&scene), [0, 0]);
    let model = scene
        .add_asset(&device, &queue, test_support::cube())
        .unwrap()
        .model;
    state = shown(model);
    scene.set_instance(&queue, id, state).unwrap();
    scene.test_frame(&device, &queue);
    state.visible = false;
    scene.set_instance(&queue, id, state).unwrap();
    assert_eq!(pending(&scene), [1, 0], "hiding marks where it stood");
    scene.test_frame(&device, &queue);
    state.pose = at(1.);
    scene.set_instance(&queue, id, state).unwrap();
    assert_eq!(
        pending(&scene),
        [0, 0],
        "a hidden instance moved shows nothing"
    );
    state.visible = true;
    scene.set_instance(&queue, id, state).unwrap();
    assert_eq!(pending(&scene), [0, 1], "showing marks where it stands");
    scene.test_frame(&device, &queue);
    scene.remove_instance(id).unwrap();
    assert_eq!(pending(&scene), [1, 0], "removal keeps where it stood");
    assert!(!scene.rigid_objects_changed(), "nothing view-wide");
    scene.begin_frame(&device, &queue);
    let retried = scene.begin_frame(&device, &queue);
    assert_eq!(
        pending(&scene),
        [1, 0],
        "an unfinished frame retires nothing"
    );
    assert!(scene.finish_frame(retried));
    assert_eq!(pending(&scene), [0, 0], "a finished frame retires it");
}

// Plausible defects: a moving instance's move, which its motion vectors
// describe, leaving footprints (so a scene with anything moving marks it
// every frame), or a change that writes no motion vector leaving none. The
// expected values are M-9's old cases, now M-23's: a pose change of a
// moving instance whose last submitted pose applies (visible in that frame
// and now, the same model) leaves none, wherever it moves; every change M-5
// and M-4 give no motion (another model, hidden, shown after a hidden frame,
// a hidden new instance shown and moved before its first submitted frame, a
// static instance moved) leaves them until a frame is submitted, and none
// raises the view-wide signal.
#[test]
fn footprints_leave_moves_with_motion_to_the_pixels() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    let mut add_model = || {
        scene
            .add_asset(&device, &queue, test_support::cube())
            .unwrap()
            .model
    };
    let (model, other) = (add_model(), add_model());
    let moving = scene
        .add_instance(&device, &queue, shown(model), Mobility::Moving)
        .unwrap();
    let fixed = scene
        .add_instance(&device, &queue, shown(model), Mobility::Static)
        .unwrap();
    let mut state = InstanceState {
        pose: at(0.5),
        ..shown(model)
    };
    scene.test_frame(&device, &queue);
    let expect = |scene: &mut Scene, change: InstanceState, id, marks: bool, what: &str| {
        scene.set_instance(&queue, id, change).unwrap();
        assert_eq!(pending(scene) != [0, 0], marks, "{what}");
        assert!(!scene.rigid_objects_changed(), "{what}: nothing view-wide");
        scene.test_frame(&device, &queue);
        assert_eq!(
            pending(scene),
            [0, 0],
            "{what}: a finished frame retires it"
        );
    };
    for (x, what) in [
        (0.5, "a moving instance moved from its submitted pose"),
        (1000., "a moving instance moved out of view"),
        (0.75, "a moving instance moved back into view"),
    ] {
        state.pose = at(x);
        expect(&mut scene, state, moving, false, what);
    }
    state.model = other;
    expect(
        &mut scene,
        state,
        moving,
        true,
        "a moving instance given another model",
    );
    state.pose = at(1.);
    expect(
        &mut scene,
        state,
        moving,
        false,
        "the next move of the other model",
    );
    state.visible = false;
    expect(&mut scene, state, moving, true, "a moving instance hidden");
    state = InstanceState {
        pose: at(2.),
        ..shown(other)
    };
    expect(
        &mut scene,
        state,
        moving,
        true,
        "a moving instance shown elsewhere after a hidden frame",
    );
    expect(
        &mut scene,
        InstanceState {
            pose: at(0.25),
            ..shown(model)
        },
        fixed,
        true,
        "a static instance moved",
    );
    let fresh = scene
        .add_instance(
            &device,
            &queue,
            InstanceState {
                visible: false,
                ..shown(model)
            },
            Mobility::Moving,
        )
        .unwrap();
    assert_eq!(
        pending(&scene),
        [0, 0],
        "a hidden instance added shows nothing"
    );
    scene
        .set_instance(
            &queue,
            fresh,
            InstanceState {
                pose: at(3.),
                ..shown(model)
            },
        )
        .unwrap();
    assert_eq!(
        pending(&scene),
        [0, 1],
        "an instance with no submitted frame shown and moved stands, and stood nowhere"
    );
}

// Plausible defect: a material edit that adds or removes what its users
// draw, with nothing moving, left unmarked, so the camera at rest fades the
// change in over many frames. The expected values are M-23's: an edit
// leaves footprints, until a submitted frame, exactly when it changes what
// a visible instance's meshes draw; shading alone, or a material nothing
// visible draws with, leaves none; none raises the view-wide signal.
#[test]
fn material_edits_that_change_what_is_drawn_leave_footprints_until_a_frame_is_finished() {
    use crate::{AlphaMode, MaterialShader};
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    let shader = test_support::add_test_shader(&mut scene);
    let mut material = |visible: bool| {
        let ids = scene
            .add_asset(&device, &queue, test_support::cube())
            .unwrap();
        let state = InstanceState {
            visible,
            ..InstanceState::new(ids.model)
        };
        scene
            .add_instance(&device, &queue, state, Mobility::Moving)
            .unwrap();
        ids.materials[0]
    };
    let (visible, hidden) = (material(true), material(false));
    let base = scene.material(visible).unwrap();
    let masked = |cutoff| SurfaceMaterial {
        alpha: AlphaMode::Mask { cutoff },
        ..base
    };
    let blended = |receives_screen_space_reflections, keeps_specular| SurfaceMaterial {
        alpha: AlphaMode::Blend {
            receives_screen_space_reflections,
            keeps_specular,
        },
        ..base
    };
    let edits = [
        (
            "visibility group",
            base,
            SurfaceMaterial {
                visibility_group: base.visibility_group ^ 2,
                ..base
            },
            true,
        ),
        ("opaque to masked", base, masked(0.5), true),
        ("mask cutoff", masked(0.5), masked(0.75), true),
        ("opaque to blended", base, blended(false, false), true),
        (
            "double-sided",
            base,
            SurfaceMaterial {
                double_sided: !base.double_sided,
                ..base
            },
            true,
        ),
        (
            "a shader that reads the time",
            base,
            SurfaceMaterial {
                shader: Some(MaterialShader {
                    shader,
                    displacement_bound: 0.5,
                }),
                ..base
            },
            true,
        ),
        (
            "colour",
            base,
            SurfaceMaterial {
                base: [0.25, 0.5, 0.75, base.base[3]],
                ..base
            },
            false,
        ),
        (
            "blended specular",
            blended(false, false),
            blended(false, true),
            false,
        ),
    ];
    for (name, before, after, draws) in edits {
        for (id, users) in [(visible, "a visible instance"), (hidden, "nothing visible")] {
            scene.set_material(&queue, id, before).unwrap();
            scene.test_frame(&device, &queue);
            scene.set_material(&queue, id, after).unwrap();
            let expected = draws && id == visible;
            assert_eq!(
                pending(&scene) != [0, 0],
                expected,
                "{name} edit of a material {users} draws with"
            );
            assert!(!scene.rigid_objects_changed(), "{name}: nothing view-wide");
            scene.test_frame(&device, &queue);
            assert_eq!(
                pending(&scene),
                [0, 0],
                "{name}: retired by a finished frame"
            );
        }
    }
}

/// A shader that moves each vertex along +Y by its parameters' `lift`,
/// reading no time: its displacement stands still.
const LIFTING_SHADER: &str = r#"
struct ShaderParams {
 lift:f32,
 unused:vec3<f32>,
}
fn material_vertex(v:MaterialVertex,ctx:VertexContext,params:ShaderParams)->MaterialVertex {
 var out=v;
 out.position.y+=params.lift;
 return out;
}
fn material_surface(s:MaterialSurface,ctx:SurfaceContext,params:ShaderParams)->MaterialSurface {
 return s;
}
"#;

// Plausible defects: a change past the footprints' cap, or of an instance
// whose shader displaces it where a footprint drawn without the shader
// would not mark it, left unmarked; or one that a footprint marks raising
// the view-wide signal too. The oracle is M-9 with M-23: past 1024 pending
// footprint draws, one for each marked mesh (a model of two meshes stands
// in two, so 512 of them fill it), or the triangle cap, and for an
// instance drawn with a shader
// whose `displacement_bound` is positive and that reads no time, the
// change raises the view-wide signal and adds no footprint; a shader that
// reads the time is marked every frame (M-16) and needs neither.
#[test]
fn past_the_cap_or_under_a_displacing_shader_a_change_rearms_the_view() {
    use super::footprints::{MAX_FOOTPRINT_DRAWS, MAX_FOOTPRINT_TRIANGLES};
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    // A model of two meshes: each stand is two draws.
    let mut pair = test_support::cube();
    pair.meshes.push(pair.meshes[0].clone());
    let cube = scene.add_asset(&device, &queue, pair).unwrap().model;
    for _ in 0..MAX_FOOTPRINT_DRAWS / 2 {
        scene
            .add_instance(&device, &queue, shown(cube), Mobility::Moving)
            .unwrap();
    }
    assert_eq!(pending(&scene), [0, MAX_FOOTPRINT_DRAWS / 2]);
    assert!(!scene.rigid_objects_changed(), "up to the cap");
    scene
        .add_instance(&device, &queue, shown(cube), Mobility::Moving)
        .unwrap();
    assert_eq!(
        pending(&scene),
        [0, MAX_FOOTPRINT_DRAWS / 2],
        "past it, none added"
    );
    assert!(scene.rigid_objects_changed(), "past the cap");
    scene.test_frame(&device, &queue);
    assert!(!scene.rigid_objects_changed());

    // A model of more triangles than the cap admits.
    let mut big = test_support::cube();
    let quads = (MAX_FOOTPRINT_TRIANGLES / 2 + 1) as u32;
    let mesh = &mut big.meshes[0];
    let (vertices, indices) = (mesh.vertices.clone(), mesh.indices[..6].to_vec());
    mesh.vertices = vertices[..4].to_vec();
    mesh.indices = (0..quads).flat_map(|_| indices.iter().copied()).collect();
    let big = scene.add_asset(&device, &queue, big).unwrap().model;
    scene
        .add_instance(&device, &queue, shown(big), Mobility::Moving)
        .unwrap();
    assert_eq!(pending(&scene), [0, 0], "too many triangles");
    assert!(scene.rigid_objects_changed(), "past the triangle cap");
    scene.test_frame(&device, &queue);

    let lifting = scene
        .add_shader(crate::ShaderSource {
            wgsl: LIFTING_SHADER.into(),
            label: "lifting".into(),
        })
        .unwrap();
    let timed = test_support::add_test_shader(&mut scene);
    for (shader, view_wide, what) in [
        (lifting, true, "a displacing shader that reads no time"),
        (timed, false, "a displacing shader that reads the time"),
    ] {
        let ids = scene
            .add_asset(&device, &queue, test_support::cube())
            .unwrap();
        test_support::set_shader(&mut scene, &queue, ids.materials[0], (shader, 0.5));
        let id = scene
            .add_instance(&device, &queue, shown(ids.model), Mobility::Moving)
            .unwrap();
        assert_eq!(scene.rigid_objects_changed(), view_wide, "{what}: added");
        assert_eq!(pending(&scene), [0, 0], "{what}: no footprint");
        scene.test_frame(&device, &queue);
        scene.remove_instance(id).unwrap();
        assert_eq!(scene.rigid_objects_changed(), view_wide, "{what}: removed");
        assert_eq!(pending(&scene), [0, 0], "{what}: no footprint");
        scene.test_frame(&device, &queue);
    }
}

// Plausible defect: a removed instance's model, or a deforming instance's
// positions, freed at once, so a model placed in the same window reuses
// their words and the footprint of where the instance stood draws another
// model's triangles (`footprints_tests` in the transparent stage shows
// that in the frame); or kept for ever. The oracle is M-23: while a
// footprint is pending the words wait, and a frame begun after the removal
// and finished frees them; without one pending they are freed at once.
#[test]
fn a_removed_models_words_wait_while_a_footprint_may_name_them() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    let model = scene
        .add_asset(&device, &queue, test_support::cube())
        .unwrap()
        .model;
    let id = scene
        .add_instance(&device, &queue, shown(model), Mobility::Moving)
        .unwrap();
    scene.test_frame(&device, &queue);
    scene.remove_instance(id).unwrap();
    scene.remove_model(model).unwrap();
    assert_eq!(scene.footprints.retiring(), 1, "kept while pending");
    scene.test_frame(&device, &queue);
    assert_eq!(scene.footprints.retiring(), 0, "freed once retired");
    let other = scene
        .add_asset(&device, &queue, test_support::cube())
        .unwrap()
        .model;
    scene.remove_model(other).unwrap();
    assert_eq!(
        scene.footprints.retiring(),
        0,
        "nothing pending, freed at once"
    );
}

// Plausible defects: the frame being rendered's state, where it is the last
// finished frame's, counted as held by a footprint that does not hold it,
// so a later edit keeps it again (a second record and draw), or, after the
// model it was drawn with is removed in the submit-to-finish window, reads
// that model and panics; or counted as held by a footprint kept before the
// frame began, which retires when the frame is finished, so a removal in
// the window leaves nothing where the next frame's predecessor drew the
// instance. The oracle is M-23 with F-13: each state the next frame may
// follow is kept once, in a footprint that lasts until a frame begun after
// it is finished.
#[test]
fn a_state_the_window_keeps_is_kept_once_and_outlives_its_model() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    let [a, b] = [(); 2].map(|_| {
        scene
            .add_asset(&device, &queue, test_support::cube())
            .unwrap()
            .model
    });
    let id = scene
        .add_instance(&device, &queue, shown(a), Mobility::Static)
        .unwrap();
    scene.test_frame(&device, &queue);
    // Frame f draws it as frame f-1 did; in its window it takes model B
    // and model A goes.
    let attempt = scene.begin_frame(&device, &queue);
    scene.set_instance(&queue, id, shown(b)).unwrap();
    assert_eq!(pending(&scene), [1, 1], "f-1's state, which is f's, and B");
    scene.remove_model(a).unwrap();
    assert!(scene.finish_frame(attempt));
    assert_eq!(pending(&scene), [1, 1], "kept for the frame after f");
    scene.remove_instance(id).unwrap();
    assert_eq!(pending(&scene), [1, 1], "f's state is kept once");
    scene.test_frame(&device, &queue);
    assert_eq!(pending(&scene), [0, 0]);

    // An edit before frame g keeps g-1's state, which retires with g; g
    // draws it the same, and a removal in g's window keeps g's state.
    let id = scene
        .add_instance(&device, &queue, shown(b), Mobility::Static)
        .unwrap();
    scene.test_frame(&device, &queue);
    let hidden = InstanceState {
        visible: false,
        ..shown(b)
    };
    scene.set_instance(&queue, id, hidden).unwrap();
    scene.set_instance(&queue, id, shown(b)).unwrap();
    let attempt = scene.begin_frame(&device, &queue);
    scene.remove_instance(id).unwrap();
    assert!(scene.finish_frame(attempt));
    assert_eq!(
        pending(&scene),
        [1, 0],
        "where frame g drew it, for the frame after g"
    );
}

// Plausible defect: an instance added drawn with a material marked every
// frame needs no footprint, but is then counted as marked for the frame
// window, so an edit in the same window that makes its material need one
// (the time-reading shader removed) marks nothing and the frame fades the
// new surface in. The oracle is M-23: after that edit the instance stands
// marked until a frame is finished, and nothing is raised view-wide.
#[test]
fn an_instance_needing_no_footprint_is_marked_once_an_edit_makes_it_need_one() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    let timed = test_support::add_test_shader(&mut scene);
    let ids = scene
        .add_asset(&device, &queue, test_support::cube())
        .unwrap();
    let material = ids.materials[0];
    test_support::set_shader(&mut scene, &queue, material, (timed, 0.));
    scene
        .add_instance(&device, &queue, shown(ids.model), Mobility::Moving)
        .unwrap();
    assert_eq!(pending(&scene), [0, 0], "marked every frame by its shader");
    let plain = SurfaceMaterial {
        shader: None,
        ..scene.material(material).unwrap()
    };
    scene.set_material(&queue, material, plain).unwrap();
    assert_eq!(
        pending(&scene),
        [0, 1],
        "a plain surface now: where it stands"
    );
    assert!(!scene.rigid_objects_changed());
}
