//! Which history the ray-traced shadow stage keeps when the camera or the
//! surfaces move along the view axis: AMD's denoiser (slot 0, the sun) and
//! the temporal blend (a local light in slot 4 and on) keep a surface's
//! history wherever its depth from the last frame's camera, its depth now
//! plus the G-buffer's depth motion, matches the stage's own last-frame
//! depth, and reject it where it does not (the register's S-11 and S-18).
//!
//! Two signals show history. The denoiser's temporal sample count, which a
//! rejected pixel restarts at 1 and a damper only lowers
//! (`FFX_DNSR_Shadows_TileClassification`), exceeds 1 only where a pixel
//! took its previous moments. The temporal blend's history shows as a
//! visibility between 0 and 1 once the watched light moves half a stripe
//! of the grate without a restart, while a texel that takes its trace holds
//! 0 or 1 alone (as `a_slot_whose_light_changed_and_a_camera_cut_restart_its_history`).
use super::*;

/// The camera's height above the floor, in metres, and how far it travels
/// along its view axis in a frame: a third of that height, far beyond
/// AMD's 1% for a surface facing the camera, and more than Wicked's metre
/// beyond the grate's 1.23 m, so that by Wicked's test every depth a floor
/// texel's history can hold, the floor's own or a bar's it was seen
/// through, is another surface.
const HEIGHT: f32 = 9.;
const TRAVEL: f32 = 3.;

/// The watched light over the grate at `z`, which moves its stripes by half
/// a stripe as `z` goes from -0.3 to 0.3, offset by `lift`.
fn watched(z: f32, lift: Vec3) -> Light {
    Light {
        position: Vec3::new(0., 6., z) + lift,
        shape: LightShape::Point { radius: 0. },
        intensity: 60.,
        range: 20.,
        casts_shadow: true,
        ..Light::default()
    }
}

/// The sun over the grate, a hard light whose stripes cross the floor.
fn sun() -> DirectionalLight {
    DirectionalLight {
        direction: Vec3::new(0.2, -1., 0.1),
        illuminance: 3.,
        shadow: Some(DirectionalShadow::DEFAULT),
        angular_diameter: 0.,
        ..DirectionalLight::default()
    }
}

/// A camera looking straight down from `height` over the origin, with
/// `projection`.
fn looking_down(height: f32, projection: Mat4) -> Camera {
    let eye = Vec3::new(0., height, 0.);
    Camera {
        view: glam::camera::rh::view::look_at_mat4(eye, Vec3::ZERO, Vec3::NEG_Z),
        projection,
        eye,
    }
}

/// How many tracing texels the denoiser's slot 0 took its previous moments
/// at in the last frame: those whose temporal sample count exceeds 1.
fn carried(device: &wgpu::Device, queue: &wgpu::Queue, renderer: &Renderer) -> usize {
    let moments = renderer
        .traced_shadow_moments()
        .expect("the ray-traced shadow stage ran");
    let size = moments.texture().size();
    let texels = (size.width * size.height) as usize;
    // Layer 0, slot 0's lane, comes first: mean, variance, count, 0 in
    // half floats.
    test_support::read(device, queue, moments.texture(), 8)[..texels * 8]
        .chunks_exact(8)
        .filter(|texel| test_support::half(&texel[4..6]) > 1.)
        .count()
}

// Plausible defect: the expected previous depth taken from the camera's
// reprojection of the surface's current point (S-18, B-6), or the temporal
// blend's test comparing the depth now with the history's (Wicked's), which
// hold only for a surface at rest. The scene is a floor, the grate over it
// and every light moving with the camera, 3 m a frame down its view axis:
// what the camera sees is the same in every frame, so every receiver's
// history is its own. Its point now lies 12 m from the last frame's
// camera, where the floor stood 9 m from it, a difference of 25% that no
// tolerance of AMD's (1% to 10%) passes. The sun's moments must carry over
// and the watched light's history must show.
#[test]
fn a_surface_the_camera_follows_keeps_its_shadow_history() {
    let Some((device, queue)) = test_support::ray_tracing_device(|limits| limits) else {
        return;
    };
    let gpu = (&device, &queue);
    let size = [128, 128];
    let mut scene = Scene::new(&device, &queue);
    let floor = scene
        .add_asset(&device, &queue, quad(6., false))
        .unwrap()
        .model;
    let cube = scene
        .add_asset(&device, &queue, test_support::cube())
        .unwrap()
        .model;
    let mut parts = vec![(floor, Mat4::IDENTITY)];
    parts.extend(grate().iter().map(|block| {
        let pose =
            Mat4::from_scale_rotation_translation(block.half * 2., Quat::IDENTITY, block.centre);
        (cube, pose)
    }));
    let parts: Vec<_> = parts
        .into_iter()
        .map(|(model, pose)| {
            let state = InstanceState {
                pose,
                ..InstanceState::new(model)
            };
            let id = scene
                .add_instance(&device, &queue, state, Mobility::Moving)
                .unwrap();
            (id, state)
        })
        .collect();
    let lights = |lift: Vec3, z: f32| {
        let mut lights = decoys(Vec3::new(0., HEIGHT, 0.) + lift).to_vec();
        lights.push(watched(z, lift));
        lights
    };
    let ids: Vec<_> = lights(Vec3::ZERO, -0.3)
        .into_iter()
        .map(|light| scene.add_light(&device, &queue, light).unwrap())
        .collect();
    let key = ids[3].index() as u32;
    let projection = crate::perspective(0.9, 1., 0.1);
    // The camera in the floor's own frame, the same in every frame.
    let local = looking_down(HEIGHT, projection);
    let settings = settings(true);
    let mut renderer = Renderer::for_test(&device, &queue, size, &settings);
    let mut frame = |scene: &mut Scene, index: u32, z: f32| {
        let lift = Vec3::new(0., -TRAVEL * index as f32, 0.);
        for &(id, state) in &parts {
            let pose = Mat4::from_translation(lift) * state.pose;
            scene
                .set_instance(&queue, id, InstanceState { pose, ..state })
                .unwrap();
        }
        for (&id, light) in ids.iter().zip(lights(lift, z)) {
            scene.set_light(&queue, id, light).unwrap();
        }
        let mut camera = local;
        camera.view = local.view * Mat4::from_translation(-lift);
        camera.eye = local.eye + lift;
        let mut input = input(camera, Some(sun()));
        input.camera_cut = index == 0;
        render(gpu, &mut renderer, scene, &input, &settings)
    };
    for index in 0..3 {
        frame(&mut scene, index, -0.3);
    }
    let moved = frame(&mut scene, 3, 0.3);
    let moments = carried(&device, &queue, &renderer);
    assert!(
        moments > 100,
        "the followed floor's sun moments carried over at only {moments} texels"
    );
    let [shadowed, between, lit] = blended(&moved, &local, key);
    assert!(
        between > 100 && moved.slot(key) >= 4,
        "the followed floor's history shows at {between} of {shadowed} shadowed, {lit} lit texels in slot {}",
        moved.slot(key)
    );
}

// Plausible defects: Wicked's absolute metre of linear depth (S-11, B-7),
// which still scenery fails whenever the camera travels further along its
// view axis in a frame; the depth motion taken with the wrong sign; and,
// under an orthographic camera, whose depth motion is 0 whatever the
// camera's travel, that zero taken for the receiver's change of depth. The
// scene is still: the floor and the grate, the camera looking down from
// 12 m and then from 9 m. Its history must survive the 3 m, the sun's
// moments carrying over and the watched light's history showing, under
// either projection. By Wicked's metre no floor texel keeps any.
#[test]
fn still_scenery_keeps_its_shadow_history_as_the_camera_travels_toward_it() {
    let Some((device, queue)) = test_support::ray_tracing_device(|limits| limits) else {
        return;
    };
    let gpu = (&device, &queue);
    let size = [128, 128];
    let start = HEIGHT + TRAVEL;
    let projections = [
        ("perspective", crate::perspective(0.9, 1., 0.1)),
        (
            "orthographic",
            glam::camera::rh::proj::directx::orthographic(-4.5, 4.5, -4.5, 4.5, 100., 0.1),
        ),
    ];
    for (name, projection) in projections {
        let [one, two, three] = decoys(Vec3::new(0., start, 0.));
        let (mut scene, ids) = scene(
            (&device, &queue),
            6.,
            &grate(),
            &[one, two, three, watched(-0.3, Vec3::ZERO)],
        );
        let key = ids[3].index() as u32;
        let settings = settings(true);
        let mut renderer = Renderer::for_test(&device, &queue, size, &settings);
        for (index, height) in [start, start].into_iter().enumerate() {
            let mut input = input(looking_down(height, projection), Some(sun()));
            input.camera_cut = index == 0;
            render(gpu, &mut renderer, &mut scene, &input, &settings);
        }
        scene
            .set_light(&queue, ids[3], watched(0.3, Vec3::ZERO))
            .unwrap();
        let near = looking_down(HEIGHT, projection);
        let mut input = input(near, Some(sun()));
        input.camera_cut = false;
        let travelled = render(gpu, &mut renderer, &mut scene, &input, &settings);
        let moments = carried(&device, &queue, &renderer);
        assert!(
            moments > 100,
            "{name}: the still floor's sun moments carried over at only {moments} texels"
        );
        let [shadowed, between, lit] = blended(&travelled, &near, key);
        assert!(
            between > 100 && travelled.slot(key) >= 4,
            "{name}: the still floor's history shows at {between} of {shadowed} shadowed, {lit} lit texels in slot {}",
            travelled.slot(key)
        );
    }
}

// Plausible defect: a tolerance that lets one surface take another's
// history, as Wicked's metre does wherever two surfaces lie less than a
// metre apart along the view. The scene is a lid 0.5 m over the floor,
// both under the grate, seen from 9 m: the lid moves away, below the
// floor, as the watched light moves half a stripe, so the floor it
// reveals, 0.5 m deeper (6% of its depth), has no history of its own
// there. Every revealed floor texel must take its trace, 0 or 1.
#[test]
fn a_surface_revealed_behind_a_near_one_takes_no_history() {
    let Some((device, queue)) = test_support::ray_tracing_device(|limits| limits) else {
        return;
    };
    let gpu = (&device, &queue);
    let size = [128, 128];
    let [one, two, three] = decoys(Vec3::new(0., HEIGHT, 0.));
    let (mut scene, ids) = scene(
        gpu,
        6.,
        &grate(),
        &[one, two, three, watched(-0.3, Vec3::ZERO)],
    );
    let key = ids[3].index() as u32;
    // The lid, a slab 2 cm thick whose top stands 0.51 m over the floor.
    let cube = scene
        .add_asset(&device, &queue, test_support::cube())
        .unwrap()
        .model;
    let lid = |y: f32| InstanceState {
        pose: Mat4::from_scale_rotation_translation(
            Vec3::new(12., 0.02, 12.),
            Quat::IDENTITY,
            Vec3::new(0., y, 0.),
        ),
        ..InstanceState::new(cube)
    };
    let lid_id = scene
        .add_instance(&device, &queue, lid(0.5), Mobility::Moving)
        .unwrap();
    let camera = looking_down(HEIGHT, crate::perspective(0.9, 1., 0.1));
    let settings = settings(true);
    let mut renderer = Renderer::for_test(&device, &queue, size, &settings);
    let mut input = input(camera, None);
    let mut frame = |scene: &mut Scene, cut: bool| {
        input.camera_cut = cut;
        render(gpu, &mut renderer, scene, &input, &settings)
    };
    for index in 0..2 {
        scene.set_instance(&queue, lid_id, lid(0.5)).unwrap();
        frame(&mut scene, index == 0);
    }
    scene.set_instance(&queue, lid_id, lid(-50.)).unwrap();
    scene
        .set_light(&queue, ids[3], watched(0.3, Vec3::ZERO))
        .unwrap();
    let revealed = frame(&mut scene, false);
    let [shadowed, between, lit] = blended(&revealed, &camera, key);
    assert!(
        between == 0 && shadowed > 100 && lit > 100,
        "the revealed floor holds {shadowed} shadowed, {between} blended, {lit} lit texels"
    );
}

// Plausible defect: a light that leaves the atlas's ranking for a frame
// keeping its slot's history when it returns (S-6), which then holds what
// the slot held while it was away. A light leaves the ranking when its
// range no longer reaches the view, so no pixel it lights on its return
// was on screen within its range the frame before: its history holds
// nothing of the surfaces it lights, and its slot restarts. The oracle is
// the camera cut, which restarts every slot: the slot it returns to holds
// what it holds in a cut frame after. The light lights a grate of its own
// 10 m along the floor, which the camera, looking down from 9 m, leaves
// for a frame by panning 12 m and returns to; the sun keeps the stage
// running throughout.
#[test]
fn a_light_that_leaves_the_view_restarts_its_slot_when_it_returns() {
    let Some((device, queue)) = test_support::ray_tracing_device(|limits| limits) else {
        return;
    };
    let gpu = (&device, &queue);
    let size = [128, 128];
    let away = Vec3::new(-10., 0., 0.);
    let blocks: Vec<Block> = grate()
        .into_iter()
        .map(|block| Block::new(block.centre + away, block.half, block.mobility))
        .collect();
    let light = Light {
        range: 7.,
        ..watched(-0.3, away)
    };
    let (mut scene, ids) = scene(gpu, 15., &blocks, &[light]);
    let key = ids[0].index() as u32;
    let over = |x: f32| {
        let eye = Vec3::new(x, HEIGHT, 0.);
        Camera {
            view: glam::camera::rh::view::look_at_mat4(eye, Vec3::new(x, 0., 0.), Vec3::NEG_Z),
            projection: crate::perspective(0.9, 1., 0.1),
            eye,
        }
    };
    let settings = settings(true);
    let mut renderer = Renderer::for_test(&device, &queue, size, &settings);
    let mut frame = |x: f32, cut: bool| {
        let mut input = input(over(x), Some(sun()));
        input.camera_cut = cut;
        render(gpu, &mut renderer, &mut scene, &input, &settings)
    };
    let held = frame(-6., true);
    let slot = held.slot(key);
    frame(-6., false);
    let left = frame(6., false);
    let (_, table) = left.mask.as_ref().expect("the sun keeps the stage running");
    assert!(
        !table.lights.iter().flatten().any(|&held| held == key),
        "the light leaves the ranking"
    );
    let returned = frame(-6., false);
    assert_eq!(returned.slot(key), slot, "the light returns to its slot");
    let differing = returned.differing(slot, &frame(-6., true), slot);
    assert_eq!(
        differing, 0,
        "after its light returned, slot {slot} differs from a camera cut at {differing} pixels"
    );
}
