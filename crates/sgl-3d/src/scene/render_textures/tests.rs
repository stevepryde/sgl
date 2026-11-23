//! A scene's render textures: what its operations accept and refuse, and
//! in which frames a material that names one counts as changing.
use crate::lod::MeshLod;
use crate::scene::shaders::ShadingChange;
use crate::{
    InstanceState, Mobility, RenderTexture, Scene, SceneError, SurfaceMaterial, test_support,
};
use glam::{Mat4, Vec3};

/// A scene with the cube's material, and that material.
fn scene_with_material(device: &wgpu::Device, queue: &wgpu::Queue) -> (Scene, crate::MaterialId) {
    let mut scene = Scene::new(device, queue);
    let ids = scene
        .add_asset(device, queue, test_support::cube())
        .expect("the cube");
    (scene, ids.materials[0])
}

fn name(
    scene: &mut Scene,
    queue: &wgpu::Queue,
    material: crate::MaterialId,
    texture: Option<crate::RenderTextureId>,
) -> Result<(), SceneError> {
    let values = scene.material(material).expect("the material");
    scene.set_material(
        queue,
        material,
        SurfaceMaterial {
            emission_render_texture: texture,
            ..values
        },
    )
}

// Plausible defects: a size the device cannot hold reaches wgpu, which
// panics; a render texture is removed while a material's group 2 binds it,
// or stays in use after the material lets go of it or is removed; a
// material names a render texture of another scene or a removed one. The
// oracles are the operations' documented results.
#[test]
fn render_textures_are_added_named_and_removed_by_the_rules() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let (mut scene, material) = scene_with_material(&device, &queue);
    let side = device.limits().max_texture_dimension_2d;
    assert!(matches!(
        scene.add_render_texture(&device, RenderTexture::new([0, 4])),
        Err(SceneError::EmptyImage)
    ));
    assert!(matches!(
        scene.add_render_texture(&device, RenderTexture::new([side + 1, 4])),
        Err(SceneError::DeviceLimit)
    ));
    let texture = scene
        .add_render_texture(&device, RenderTexture::new([8, 4]))
        .expect("a render texture");
    assert_eq!(
        scene.render_texture(texture).expect("its size").size,
        [8, 4]
    );
    // A size the device cannot hold leaves it as it was.
    assert!(matches!(
        scene.set_render_texture(&device, texture, RenderTexture::new([side + 1, 4])),
        Err(SceneError::DeviceLimit)
    ));
    assert_eq!(
        scene.render_texture(texture).expect("its size").size,
        [8, 4]
    );
    name(&mut scene, &queue, material, Some(texture)).expect("a live render texture");
    assert!(matches!(
        scene.remove_render_texture(texture),
        Err(SceneError::RenderTextureInUse)
    ));
    // A resize while a material names it keeps its use.
    scene
        .set_render_texture(&device, texture, RenderTexture::new([16, 8]))
        .expect("a resize");
    assert!(matches!(
        scene.remove_render_texture(texture),
        Err(SceneError::RenderTextureInUse)
    ));
    name(&mut scene, &queue, material, None).expect("no render texture");
    scene
        .remove_render_texture(texture)
        .expect("no material names it");
    assert!(matches!(
        name(&mut scene, &queue, material, Some(texture)),
        Err(SceneError::UnknownRenderTexture)
    ));
    assert!(matches!(
        scene.render_texture(texture),
        Err(SceneError::UnknownRenderTexture)
    ));
    // A removed material lets go of the render texture it named.
    let second = scene
        .add_render_texture(&device, RenderTexture::new([8, 4]))
        .expect("a render texture");
    let (mut other, _) = scene_with_material(&device, &queue);
    assert!(matches!(
        name(&mut scene, &queue, material, Some(second)).and_then(|()| {
            let foreign = other
                .add_render_texture(&device, RenderTexture::new([8, 4]))
                .expect("another scene's render texture");
            name(&mut scene, &queue, material, Some(foreign))
        }),
        Err(SceneError::UnknownRenderTexture)
    ));
    // The refused edit left the material naming `second`.
    assert_eq!(
        scene
            .material(material)
            .expect("the material")
            .emission_render_texture,
        Some(second)
    );
}

// M-16. Plausible defects: a screen is never marked for the temporal
// composition, so antialiasing smears a changing picture, or is marked in
// every frame, so a still picture never rests; the scene does not hold the
// masks for a material that names a render texture. The oracle is the
// register's rule: a material that names a render texture changes in the
// frames a render drew into the texture or replaced it since the last
// finished frame, and in the frame after, whose consumers may have sampled
// the picture before its producer drew; not otherwise.
#[test]
fn a_screen_counts_as_changing_while_its_picture_changes() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let (mut scene, material) = scene_with_material(&device, &queue);
    let change = |scene: &Scene| scene.shading_changes().of(scene, material);
    assert!(!scene.materials.holds_moving_surfaces());
    assert_eq!(change(&scene), ShadingChange::None);
    let texture = scene
        .add_render_texture(&device, RenderTexture::new([8, 4]))
        .expect("a render texture");
    name(&mut scene, &queue, material, Some(texture)).expect("a screen");
    assert!(scene.materials.holds_moving_surfaces());
    // New: black replaces whatever the material emitted before.
    assert_eq!(change(&scene), ShadingChange::Continuous);
    scene.test_frame(&device, &queue);
    scene.test_frame(&device, &queue);
    assert_eq!(change(&scene), ShadingChange::None);
    // A frame draws into it: this frame and the next.
    scene.render_textures.drawn(texture);
    assert_eq!(change(&scene), ShadingChange::Continuous);
    scene.test_frame(&device, &queue);
    assert_eq!(change(&scene), ShadingChange::Continuous);
    scene.test_frame(&device, &queue);
    assert_eq!(change(&scene), ShadingChange::None);
    // A replacement is a change of picture too.
    scene
        .set_render_texture(&device, texture, RenderTexture::new([16, 8]))
        .expect("a resize");
    assert_eq!(change(&scene), ShadingChange::Continuous);
    name(&mut scene, &queue, material, None).expect("no screen");
    assert!(!scene.materials.holds_moving_surfaces());
    assert_eq!(change(&scene), ShadingChange::None);
}

// The frame's test of whether a view shows a screen (the architecture's
// Frame, "Render textures"). Plausible defects, each of which leaves a
// producer out of a frame that shows its picture, a stale screen: a hidden
// instance counted or a shown one missed; bounds that leave out what a
// shader may move a vertex, or a level of detail the cull may draw in the
// mesh's place, which reaches beyond it. The oracle is the fixture cube's
// own vertices (its half side, `half`) and the contract: a screen is each mesh of a visible
// instance drawn with a material that names the texture, at the instance's
// pose, its bounds the mesh's and its levels' own, grown by the
// displacement bound.
#[test]
fn screens_are_the_shown_meshes_that_name_a_render_texture_with_all_they_may_draw() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    // The cube is centred on its origin: its half side from its vertices.
    let half = test_support::cube().meshes[0]
        .vertices
        .iter()
        .flat_map(|vertex| vertex.position)
        .fold(0f32, f32::max);
    let cube = scene
        .add_asset(&device, &queue, test_support::cube())
        .expect("a cube");
    let pose = Mat4::from_translation(Vec3::new(5., 0., 0.));
    let state = InstanceState {
        pose,
        ..InstanceState::new(cube.model)
    };
    let instance = scene
        .add_instance(&device, &queue, state, Mobility::Moving)
        .expect("an instance");
    assert!(
        scene.screens().is_empty(),
        "no material names a render texture"
    );
    let texture = scene
        .add_render_texture(&device, RenderTexture::new([8, 4]))
        .expect("a render texture");
    name(&mut scene, &queue, cube.materials[0], Some(texture)).expect("a screen");
    let screens = scene.screens();
    assert_eq!(screens.len(), 1, "one mesh of one shown instance");
    assert_eq!(screens[0].texture, texture);
    assert_eq!(screens[0].pose, pose);
    assert_eq!(screens[0].bounds, [Vec3::splat(-half), Vec3::splat(half)]);
    // What a shader may move a vertex grows the bounds.
    let shader = test_support::add_test_shader(&mut scene);
    test_support::set_shader(&mut scene, &queue, cube.materials[0], (shader, 0.25));
    assert_eq!(
        scene.screens()[0].bounds,
        [Vec3::splat(-half - 0.25), Vec3::splat(half + 0.25)]
    );
    // A level of detail three times the size: the cull may draw it instead.
    let mut large = test_support::cube();
    for vertex in &mut large.meshes[0].vertices {
        vertex.position = vertex.position.map(|c| c * 3.);
    }
    let large = scene.add_asset(&device, &queue, large).expect("a level");
    scene
        .set_mesh_lods(
            cube.model,
            0,
            vec![MeshLod {
                model: large.model,
                mesh: 0,
                max_error: 0.1,
            }],
        )
        .expect("a level of detail");
    assert_eq!(
        scene.screens()[0].bounds,
        [
            Vec3::splat(-3. * half - 0.25),
            Vec3::splat(3. * half + 0.25)
        ]
    );
    // A hidden instance shows nothing.
    scene
        .set_instance(
            &queue,
            instance,
            InstanceState {
                visible: false,
                ..state
            },
        )
        .expect("a hidden instance");
    assert!(scene.screens().is_empty(), "a hidden instance is no screen");
}
