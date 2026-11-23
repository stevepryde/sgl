//! Planar reflectors as scene content: what the operations accept, refuse
//! and keep (the architecture's Scene content, Planar reflector).
use crate::{AlphaMode, PlanarReflector, Scene, SceneError, SurfaceMaterial, test_support};
use glam::Vec3;

fn scene() -> Option<(wgpu::Device, wgpu::Queue, Scene)> {
    let (device, queue) = test_support::device()?;
    let scene = Scene::new(&device, &queue);
    Some((device, queue, scene))
}

// Plausible defects: a degenerate plane is stored and later divides by
// zero; a refused edit changes the reflector. The oracle is the contract:
// a non-finite origin and a normal without a direction are refused with
// the typed error, on add and on set, and a refused set leaves the plane.
#[test]
fn a_plane_needs_a_finite_origin_and_a_direction() {
    let Some((_, _, mut scene)) = scene() else {
        return;
    };
    let invalid = [
        PlanarReflector::new(Vec3::ZERO, Vec3::ZERO),
        PlanarReflector::new(Vec3::new(f32::NAN, 0., 0.), Vec3::Y),
        PlanarReflector::new(Vec3::ZERO, Vec3::new(0., f32::INFINITY, 0.)),
    ];
    for reflector in invalid {
        assert!(
            matches!(
                scene.add_planar_reflector(reflector),
                Err(SceneError::InvalidPlanarReflector)
            ),
            "{reflector:?}"
        );
    }
    let floor = PlanarReflector::new(Vec3::new(0., 2., 0.), Vec3::new(0., 3., 0.));
    let id = scene.add_planar_reflector(floor).unwrap();
    assert!(matches!(
        scene.set_planar_reflector(id, invalid[0]),
        Err(SceneError::InvalidPlanarReflector)
    ));
    assert_eq!(scene.planar_reflector(id).unwrap(), floor);
    // The plane is the description's, normalized: y = 2.
    let plane = scene.planar_reflectors.get(id).unwrap().plane;
    assert_eq!(plane.to_array(), [0., 1., 0., -2.]);
}

// Plausible defects: a reflector is removed under a material that names
// it, which then names nothing; a material may name an ended identity; a
// blended material is accepted and never shows a picture. The oracle is
// the contract's refusals, each leaving the scene as it was.
#[test]
fn a_material_names_a_live_reflector_and_holds_it() {
    let Some((device, queue, mut scene)) = scene() else {
        return;
    };
    let ids = scene
        .add_asset(&device, &queue, test_support::cube())
        .unwrap();
    let material = ids.materials[0];
    let values = scene.material(material).unwrap();
    let id = scene
        .add_planar_reflector(PlanarReflector::new(Vec3::ZERO, Vec3::Y))
        .unwrap();
    let named = SurfaceMaterial {
        planar_reflector: Some(id),
        ..values
    };
    // A blended material cannot name one.
    let blended = SurfaceMaterial {
        alpha: AlphaMode::Blend {
            receives_screen_space_reflections: false,
            keeps_specular: false,
        },
        ..named
    };
    assert!(matches!(
        scene.set_material(&queue, material, blended),
        Err(SceneError::BlendedPlanarReflector)
    ));
    assert_eq!(scene.material(material).unwrap(), values);
    assert!(scene.mirrors().is_empty());
    scene.set_material(&queue, material, named).unwrap();
    assert!(matches!(
        scene.remove_planar_reflector(id),
        Err(SceneError::PlanarReflectorInUse)
    ));
    // The material lets go of it, and it can be removed; its identity then
    // names nothing.
    scene.set_material(&queue, material, values).unwrap();
    scene.remove_planar_reflector(id).unwrap();
    assert!(matches!(
        scene.planar_reflector(id),
        Err(SceneError::UnknownPlanarReflector)
    ));
    assert!(matches!(
        scene.set_material(&queue, material, named),
        Err(SceneError::UnknownPlanarReflector)
    ));
}

// Plausible defect: a moved render origin leaves a reflector's plane where
// it was, so every mirror reflects about a plane displaced by the move. The
// oracle is the origin contract: every position the scene holds becomes
// what it was less the move.
#[test]
fn a_moved_origin_moves_the_plane() {
    let Some((device, queue, mut scene)) = scene() else {
        return;
    };
    let id = scene
        .add_planar_reflector(PlanarReflector::new(Vec3::new(1., 5., -2.), Vec3::Y))
        .unwrap();
    scene
        .move_origin(&device, &queue, Vec3::new(10., 3., 0.))
        .unwrap();
    assert_eq!(
        scene.planar_reflector(id).unwrap().origin,
        Vec3::new(-9., 2., -2.)
    );
    assert_eq!(
        scene.planar_reflectors.get(id).unwrap().plane.to_array(),
        [0., 1., 0., -2.]
    );
}
