//! Screen-space indirect light (`Settings::screen_space_indirect`,
//! `FrameInput::screen_space_indirect_radius`) in the scenario harness
//! (`specs/testing.md`, "SGL3D scenario harness"): an emissive wall, red
//! only, beside a white diffuse floor, under black sky with no lights and
//! no GI volume, so whatever reaches the floor is the wall's bounce. Every
//! oracle is the contract's: the floor is exactly black without the pass
//! and in the frames it keeps nothing for (SI-1, SI-2); the bounce keeps
//! the emitter's colour and stays below its radiance times the floor's
//! albedo; nothing beyond the radius takes any (SI-3, SI-4); and a removed
//! source's bounce fades at the accumulation's capped rate (SI-5).
//! Antialiasing is off, so each frame's composite is its own.
//!
//! Coverage: both levels at rest, the first frame, a camera cut, a resize,
//! camera translation with rotation, a moving lit box uncovering what lies
//! behind it, and the source removed. Not covered yet: an object the camera
//! follows, and an object moving along the view.
#![cfg(not(target_arch = "wasm32"))]
mod scenario;

use scenario::geometry::{Pose, View};
use scenario::image::{Image, Region, centre};
use scenario::rig::{Dump, HALF_STEP, Rig, gpu};
use sgl_3d::asset::{Asset, CpuMesh, Material, Vertex};
use sgl_3d::diagnostics::{self, DiagnosticTarget};
use sgl_3d::glam::Vec3;
use sgl_3d::settings::{Antialiasing, ScreenSpaceIndirect};
use sgl_3d::{InstanceId, InstanceState};

/// A vertical field of view whose half-angle's tangent is 1/2: the floor 3
/// m ahead spans 4 by 3 m.
const VIEW: View = View {
    size: [128, 96],
    fov_y: 0.927_295_2,
    near: 0.05,
};
/// The floor's plane, facing the camera, and the wall's, facing -x.
const FLOOR_Z: f32 = -3.;
const WALL_X: f32 = 0.6;
/// The wall's radiance, red only, and the floor's albedo.
const WALL_RADIANCE: f32 = 1.;
const FLOOR_ALBEDO: f32 = 1.;

/// A quad about `center` spanning `u` and `v` from it each way, facing
/// u × v.
fn face(center: Vec3, u: Vec3, v: Vec3) -> CpuMesh {
    let normal = u.cross(v).normalize();
    CpuMesh {
        vertices: [(-1., -1.), (1., -1.), (1., 1.), (-1., 1.)]
            .map(|(a, b): (f32, f32)| Vertex {
                position: (center + u * a + v * b).to_array(),
                normal: normal.to_array(),
                uv: [(a + 1.) / 2., (1. - b) / 2.],
                color: [1.; 4],
                tangent: u.normalize().extend(1.).to_array(),
                lightmap_bounds: [0., 0., 1., 1.],
                lightmap_uv: [0.; 2],
            })
            .to_vec(),
        indices: vec![0, 1, 2, 0, 2, 3],
        material: 0,
        deformation: Default::default(),
    }
}

/// The six faces of a box about the origin, `half` metres to each side.
fn cube(half: f32) -> Vec<CpuMesh> {
    let [x, y, z] = [Vec3::X, Vec3::Y, Vec3::Z].map(|axis| axis * half);
    vec![
        face(x, -z, y),
        face(-x, z, y),
        face(y, x, -z),
        face(-y, x, z),
        face(z, x, y),
        face(-z, -x, y),
    ]
}

/// `meshes` of a fully rough dielectric of `base` colour emitting
/// `emissive`.
fn model(meshes: Vec<CpuMesh>, base: [f32; 3], emissive: [f32; 3]) -> Asset {
    Asset {
        meshes,
        materials: vec![Material {
            name: "screen-space indirect fixture".into(),
            base: [base[0], base[1], base[2], 1.],
            emissive,
            metallic: 0.,
            roughness: 1.,
            ..Material::default()
        }],
        images: Vec::new(),
        rig: Default::default(),
        ignored: Vec::new(),
        lights: Default::default(),
    }
}

/// The floor, 4.6 by 6 m, from x = -4 m to the wall.
fn floor() -> Asset {
    model(
        vec![face(
            Vec3::new(-1.7, 0., FLOOR_Z),
            Vec3::X * 2.3,
            Vec3::Y * 3.,
        )],
        [FLOOR_ALBEDO; 3],
        [0.; 3],
    )
}

/// The wall standing on the floor at x = 0.6 m, from the floor to 0.3 m
/// from the camera, black and emitting red alone.
fn wall() -> Asset {
    model(
        vec![face(
            Vec3::new(WALL_X, 0., -1.65),
            Vec3::Z * 1.35,
            Vec3::Y * 3.,
        )],
        [0.; 3],
        [WALL_RADIANCE, 0., 0.],
    )
}

/// The rig over black sky with screen-space indirect light at `level`,
/// holding `models`, each placed at the origin: their instances in order.
fn stage(level: ScreenSpaceIndirect, models: Vec<Asset>) -> Option<(Rig, Vec<InstanceId>)> {
    let gpu = gpu()?;
    let mut rig = Rig::new(&gpu, VIEW, Antialiasing::Off, 0.);
    rig.views[0].settings.screen_space_indirect = level;
    let ids = models
        .into_iter()
        .map(|asset| rig.add_model(asset, Pose::at(Vec3::ZERO)))
        .collect();
    Some((rig, ids))
}

/// The pixels where `instance` is the surface, seen in the frame just
/// rendered.
fn surface(rig: &Rig, instance: InstanceId) -> Region {
    rig.read(DiagnosticTarget::SourceId)
        .region_of(diagnostics::source_id(instance))
}

/// The pixels of the frame just rendered, by the camera at `pose`, where
/// the floor (`floor`) is the surface at a point whose distance from the
/// wall lies within `from..to` metres.
fn floor_band(rig: &Rig, floor: InstanceId, pose: Pose, from: f32, to: f32) -> Region {
    let lens = rig.views[0].lens;
    let camera = lens.camera(pose);
    let surface = surface(rig, floor);
    Region::from_fn(lens.size, |pixel| {
        let ray = lens.ray(&camera, centre(pixel));
        let point = pose.position + ray * ((FLOOR_Z - pose.position.z) / ray.z);
        let distance = WALL_X - point.x;
        surface.contains(pixel) && (from..to).contains(&distance)
    })
}

/// The band of floor by the wall, and the band farther out.
const NEAR: (f32, f32) = (0.05, 0.4);
const FAR: (f32, f32) = (1.6, 2.4);

fn mean_red(image: &Image, region: &Region) -> f32 {
    assert!(!region.is_empty(), "a mean over no pixels");
    region.iter().map(|pixel| image.at(pixel)[0]).sum::<f32>() / region.len() as f32
}

/// Asserts `region` of `image` is exactly black.
fn assert_black(image: &Image, region: &Region, what: &str) {
    assert!(!region.is_empty(), "{what}: no pixels");
    for pixel in region.iter() {
        assert_eq!(image.at(pixel)[..3], [0.; 3], "{what}: lit at {pixel:?}");
    }
}

/// Asserts every pixel of `region` of `image` takes the wall's bounce: red
/// above 0, green and blue 0, and at most the wall's radiance times the
/// floor's albedo.
fn assert_bounce(image: &Image, region: &Region, what: &str) {
    assert!(!region.is_empty(), "{what}: no pixels");
    let bound = WALL_RADIANCE * FLOOR_ALBEDO * (1. + HALF_STEP);
    for pixel in region.iter() {
        let [r, g, b, _] = image.at(pixel);
        assert!(r > 0., "{what}: no bounce at {pixel:?}");
        assert_eq!([g, b], [0.; 2], "{what}: another colour at {pixel:?}");
        assert!(r <= bound, "{what}: {r} at {pixel:?} above {bound}");
    }
}

// Plausible defects: the pass lights the floor while off, or its lighting
// takes nothing or the wrong channels, or makes light (a gain, a measure
// past the hemisphere), or its reach fades the wrong way. Oracle: with the
// setting off the floor is exactly black, as nothing else lights it; at
// Low and High, from the third frame, the floor by the wall takes red
// alone, at most the wall's radiance times the floor's albedo, and no less
// on average than the floor farther out.
#[test]
fn a_lit_wall_bounces_onto_the_floor_beside_it_and_nothing_does_when_off() {
    let mut dump = Dump::new("a_lit_wall_bounces_onto_the_floor_beside_it");
    for level in [
        ScreenSpaceIndirect::Off,
        ScreenSpaceIndirect::Low,
        ScreenSpaceIndirect::High,
    ] {
        let Some((mut rig, ids)) = stage(level, vec![floor(), wall()]) else {
            return;
        };
        for frame in 0..3 {
            rig.render(frame);
        }
        let image = rig.read(DiagnosticTarget::Composite);
        dump.image(&format!("{level:?}"), &image);
        let pose = Pose::at(Vec3::ZERO);
        if level == ScreenSpaceIndirect::Off {
            assert_black(&image, &surface(&rig, ids[0]), "off");
            continue;
        }
        let near = floor_band(&rig, ids[0], pose, NEAR.0, NEAR.1);
        let far = floor_band(&rig, ids[0], pose, FAR.0, FAR.1);
        assert_bounce(&image, &near, &format!("{level:?}"));
        let (near, far) = (mean_red(&image, &near), mean_red(&image, &far));
        dump.metric(format!("{level:?} near"), near);
        dump.metric(format!("{level:?} far"), far);
        assert!(
            near >= far,
            "{level:?}: the floor by the wall {near} below {far}"
        );
    }
}

// Plausible defects: the lighting takes the frame's own result, or a kept
// result across a cut or a resize, which no longer stands for what is on
// screen (SI-1, SI-2). Oracle: the first frame, the frame of a camera cut
// and the frame after a resize show the floor exactly black, and the frame
// after each shows the bounce.
#[test]
fn the_bounce_is_one_frame_late_and_restarts_on_a_cut_and_a_resize() {
    let Some((mut rig, ids)) = stage(ScreenSpaceIndirect::Low, vec![floor(), wall()]) else {
        return;
    };
    let pose = Pose::at(Vec3::ZERO);
    let mut frame = 0;
    let mut shot = |rig: &mut Rig, cut: bool| {
        rig.views[0].input.camera_cut = cut;
        rig.render(frame);
        frame += 1;
        let image = rig.read(DiagnosticTarget::Composite);
        let floor = surface(rig, ids[0]);
        let near = floor_band(rig, ids[0], pose, NEAR.0, NEAR.1);
        (image, floor, near)
    };
    let (image, floor, _) = shot(&mut rig, false);
    assert_black(&image, &floor, "the first frame");
    let (image, _, near) = shot(&mut rig, false);
    assert_bounce(&image, &near, "the second frame");
    let (image, floor, _) = shot(&mut rig, true);
    assert_black(&image, &floor, "a camera cut");
    let (image, _, near) = shot(&mut rig, false);
    assert_bounce(&image, &near, "after a camera cut");
    rig.views[0].lens = View {
        size: [112, 84],
        ..VIEW
    };
    let (image, floor, _) = shot(&mut rig, false);
    assert_black(&image, &floor, "a resize");
    let (image, _, near) = shot(&mut rig, false);
    assert_bounce(&image, &near, "after a resize");
}

// Plausible defects: the kept result is read at the fragment's current
// position, or reprojected with the wrong camera, so under camera motion
// the floor takes another surface's light or none (SI-3). Oracle: with the
// camera moving sideways and back and turning each frame, the wall on
// screen throughout, the floor by the wall takes the bounce, within its
// bound, on every frame after the second.
#[test]
fn the_bounce_stays_under_camera_translation_and_rotation() {
    let Some((mut rig, ids)) = stage(ScreenSpaceIndirect::Low, vec![floor(), wall()]) else {
        return;
    };
    for frame in 0..24 {
        let t = frame as f32;
        let pose = Pose {
            position: Vec3::new(-0.01 * t, 0.005 * t, 0.01 * t),
            yaw: 0.002 * t,
        };
        rig.look(pose);
        rig.render(frame);
        if frame >= 2 {
            let image = rig.read(DiagnosticTarget::Composite);
            let near = floor_band(&rig, ids[0], pose, NEAR.0, NEAR.1);
            assert_bounce(&image, &near, &format!("frame {frame}"));
        }
    }
}

// Plausible defects: history or the lighting's read keeps a moving
// surface's light where it no longer is, so the surface it uncovers takes
// light it never could (SI-3, SI-4). Oracle: a white box by the wall,
// within a 1 m radius of it, takes the wall's bounce, and moves across in
// front of a white backdrop 4 m behind, beyond the radius from both, which
// therefore takes none: the backdrop stays exactly black on every frame,
// the pixels the box uncovers included.
#[test]
fn a_revealed_surface_takes_none_of_what_hid_it() {
    let backdrop = model(
        vec![face(Vec3::new(0., 0., -6.), Vec3::X * 6., Vec3::Y * 5.)],
        [1.; 3],
        [0.; 3],
    );
    let short_wall = model(
        vec![face(
            Vec3::new(WALL_X, 0., -1.5),
            Vec3::Z * 0.5,
            Vec3::Y * 1.,
        )],
        [0.; 3],
        [WALL_RADIANCE, 0., 0.],
    );
    let Some((mut rig, ids)) = stage(ScreenSpaceIndirect::Low, vec![backdrop, short_wall]) else {
        return;
    };
    rig.views[0].input.screen_space_indirect_radius = 1.;
    let box_id = rig.add_model(model(cube(0.15), [1.; 3], [0.; 3]), Pose::at(Vec3::ZERO));
    let mut lit = false;
    for frame in 0..20 {
        let at = Vec3::new(0.35 - 0.05 * frame as f32, 0., -1.5);
        let state = InstanceState {
            pose: Pose::at(at).matrix(),
            ..*rig.scene.instance(box_id).unwrap()
        };
        rig.scene
            .set_instance(&rig.gpu.queue, box_id, state)
            .unwrap();
        rig.render(frame);
        let image = rig.read(DiagnosticTarget::Composite);
        assert_black(&image, &surface(&rig, ids[0]), &format!("frame {frame}"));
        lit |= surface(&rig, box_id)
            .iter()
            .any(|pixel| image.at(pixel)[0] > 0.);
    }
    assert!(lit, "the box never took the wall's bounce");
}

// Plausible defects: the accumulation keeps more than its cap, or a
// variance or similarity test holds a removed source's light, so the
// bounce outlives its source (SI-5). Oracle: after more than 16 frames at
// rest, 65 frames after the wall is hidden the floor by the wall holds at
// most 2% of its resting bounce, from the cap of 16: (15/16)^64 is 0.016.
#[test]
fn the_bounce_fades_after_its_source_leaves() {
    let Some((mut rig, ids)) = stage(ScreenSpaceIndirect::Low, vec![floor(), wall()]) else {
        return;
    };
    let pose = Pose::at(Vec3::ZERO);
    for frame in 0..20 {
        rig.render(frame);
    }
    let near = floor_band(&rig, ids[0], pose, NEAR.0, NEAR.1);
    let resting = mean_red(&rig.read(DiagnosticTarget::Composite), &near);
    assert!(resting > 0., "no bounce at rest");
    let state = InstanceState {
        visible: false,
        ..*rig.scene.instance(ids[1]).unwrap()
    };
    rig.scene
        .set_instance(&rig.gpu.queue, ids[1], state)
        .unwrap();
    for frame in 20..85 {
        rig.render(frame);
    }
    let left = mean_red(&rig.read(DiagnosticTarget::Composite), &near);
    println!("resting {resting}, 65 frames after the wall left {left}");
    assert!(
        left <= 0.02 * resting,
        "{left} remains of a resting {resting}"
    );
}
