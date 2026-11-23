//! `KHR_lights_punctual` lights loaded and placed, against values worked by
//! hand from the extension's text and each fixture's node transforms.
use super::rig_tests::Buffer;
use super::*;
use crate::content::light::{Light, LightShape};
use glam::Vec3;
use serde_json::{Value, json};
use std::f32::consts::{FRAC_PI_2, FRAC_PI_4};
use wasm_bindgen_test::wasm_bindgen_test;

/// A GLB of one triangle at node 0 and `nodes` after it, whose scene lists
/// node 0 and `roots`, defining `lights`, and listing the extension as
/// required when `required`.
fn fixture(lights: Value, nodes: Value, roots: &[usize], required: bool) -> Vec<u8> {
    let mut buffer = Buffer::default();
    let positions = buffer.floats(&[0., 0., 0., 1., 0., 0., 0., 1., 0.], "VEC3", 3);
    let normals = buffer.floats(&[0., 0., 1.].repeat(3), "VEC3", 3);
    let mut all_nodes = vec![json!({"mesh": 0})];
    all_nodes.extend(nodes.as_array().unwrap().iter().cloned());
    let mut scene_roots = vec![0];
    scene_roots.extend(roots);
    let extension = ["KHR_lights_punctual"];
    buffer.glb(json!({
        "asset": {"version": "2.0"},
        "extensionsUsed": extension,
        "extensionsRequired": if required { &extension[..] } else { &[] },
        "extensions": {"KHR_lights_punctual": {"lights": lights}},
        "meshes": [{"primitives": [{"attributes": {"POSITION": positions, "NORMAL": normals}}]}],
        "nodes": all_nodes,
        "scenes": [{"nodes": scene_roots}], "scene": 0
    }))
}

/// A node placing light `light`, with `fields` (a transform, children).
fn light_node(light: usize, mut fields: Value) -> Value {
    fields["extensions"] = json!({"KHR_lights_punctual": {"light": light}});
    fields
}

/// The parent of the placement fixtures: moved to (10, 0, 0), turned 90°
/// about +Y, which takes local (x, y, z) to (z, y, -x), and scaled (2, 1, 3)
/// before the turn. A point `p` below it is at (10, 0, 0) + turn(scale(p)).
fn parent(children: &[usize]) -> Value {
    let half = std::f32::consts::FRAC_1_SQRT_2;
    json!({
        "translation": [10., 0., 0.],
        "rotation": [0., half, 0., half],
        "scale": [2., 1., 3.],
        "children": children
    })
}

fn placed(asset: &Asset, placement: usize) -> PlacedLight {
    let placement = &asset.lights.placements[placement];
    asset.lights.definitions[placement.light].place(placement.transform)
}

fn local(light: PlacedLight) -> Light {
    match light {
        PlacedLight::Local(light) => light,
        PlacedLight::Directional(light) => panic!("a local light placed as {light:?}"),
    }
}

fn assert_near(got: Vec3, want: Vec3, what: &str) {
    assert!(
        got.abs_diff_eq(want, 1e-5),
        "{what}: {got} where the hand-worked value is {want}"
    );
}

// Defects: a placement ignores an ancestor's transform, applies scale to
// the light's range or intensity, keeps one placement per definition rather
// than per node, places a node the loaded scene does not hold, or loses the
// authored colour, intensity or range. Oracle: KHR_lights_punctual (the
// light inherits its node's world transform; range and intensity do not
// change with scale; a node names a light by index) and the parent's
// transform worked by hand: child A at (1, 2, 0) scales to (2, 2, 0), turns
// to (0, 2, -2) and moves to (10, 2, -2); child B at (0, 0, 1) scales to
// (0, 0, 3), turns to (3, 0, 0) and moves to (13, 0, 0).
#[wasm_bindgen_test(unsupported = test)]
fn two_nodes_place_one_point_light_below_a_transformed_parent() {
    let lights = json!([{"type": "point", "name": "lamp", "color": [1.0, 0.5, 0.25], "intensity": 40.0, "range": 6.0}]);
    let nodes = json!([
        parent(&[2, 3]),
        light_node(0, json!({"name": "a", "translation": [1., 2., 0.]})),
        light_node(0, json!({"name": "b", "translation": [0., 0., 1.]})),
        // In no scene: not placed.
        light_node(0, json!({"translation": [5., 5., 5.]})),
    ]);
    let asset = load_slice(&fixture(lights, nodes, &[1], false)).unwrap();
    assert!(asset.ignored.is_empty(), "{:?}", asset.ignored);
    assert_eq!(asset.lights.definitions.len(), 1);
    assert_eq!(asset.lights.definitions[0].name.as_deref(), Some("lamp"));
    let placements = &asset.lights.placements;
    assert_eq!(
        placements
            .iter()
            .map(|p| (p.light, p.node, p.name.as_deref()))
            .collect::<Vec<_>>(),
        [(0, 2, Some("a")), (0, 3, Some("b"))]
    );
    for (placement, position) in [(0, Vec3::new(10., 2., -2.)), (1, Vec3::new(13., 0., 0.))] {
        let light = local(placed(&asset, placement));
        assert_near(light.position, position, "position");
        assert_eq!(light.shape, LightShape::Point { radius: 0. });
        assert_eq!(
            (light.color, light.intensity, light.range),
            ([1.0, 0.5, 0.25], 40.0, 6.0)
        );
    }
}

// Defects: a spot or directional light shines down another axis than its
// node's -Z, takes only its node's rotation where the extension has the
// node's world transform (scale included) orient it, or takes position or
// scale into a directional light's direction; units change on the way.
// Oracle: KHR_lights_punctual (a light shines down its local -Z; the
// node's world transform orients it, scale only through its effect on
// orientation; directional intensity is lux, spot candela) and the
// transforms worked by hand. The spot's node is turned 45° about +Y under
// the parent: its -Z, (-1, 0, -1)/√2, scales to (-2, 0, -3)/√2 and turns to
// (-3, 0, 2)/√2, so (-3, 0, 2)/√13 made unit; a rotation alone would give
// (-1, 0, 1)/√2. The sun's node is turned -90° about +X, which takes -Z to
// -Y, and is moved and scaled, which leaves its direction.
#[wasm_bindgen_test(unsupported = test)]
fn spots_and_suns_shine_down_their_nodes_world_minus_z() {
    let lights = json!([
        {"type": "spot", "intensity": 25.0, "spot": {"innerConeAngle": 0.3, "outerConeAngle": 0.6}},
        {"type": "directional", "color": [0.9, 0.8, 0.7], "intensity": 3.5}
    ]);
    let (sin, cos) = (
        std::f32::consts::FRAC_PI_8.sin(),
        std::f32::consts::FRAC_PI_8.cos(),
    );
    let half = std::f32::consts::FRAC_1_SQRT_2;
    let nodes = json!([
        parent(&[2]),
        light_node(0, json!({"rotation": [0., sin, 0., cos]})),
        light_node(
            1,
            json!({"translation": [5., 5., 5.], "rotation": [-half, 0., 0., half], "scale": [7., 7., 7.]})
        ),
    ]);
    let asset = load_slice(&fixture(lights, nodes, &[1, 3], false)).unwrap();
    let spot = local(placed(&asset, 0));
    let LightShape::Spot {
        direction,
        inner_angle,
        outer_angle,
        radius,
    } = spot.shape
    else {
        panic!("a spot placed as {:?}", spot.shape);
    };
    assert_near(
        direction,
        Vec3::new(-3., 0., 2.) / 13f32.sqrt(),
        "spot axis",
    );
    assert_eq!((inner_angle, outer_angle, radius), (0.3, 0.6, 0.));
    assert_near(spot.position, Vec3::new(10., 0., 0.), "spot position");
    assert_eq!(spot.intensity, 25.);
    let PlacedLight::Directional(sun) = placed(&asset, 1) else {
        panic!("a directional light placed as a local one");
    };
    assert_near(sun.direction, Vec3::NEG_Y, "sun direction");
    assert_eq!((sun.color, sun.illuminance), ([0.9, 0.8, 0.7], 3.5));
    assert_eq!((sun.angular_diameter, sun.shadow), (0., None));
}

// Defects: a spot without cone angles takes other defaults than the
// extension's, one of outer angle π/2 (allowed) is refused or kept at π/2
// (which `LightShape::Spot` refuses), or a light without a range is cut
// off short or left with an infinite range (which `Light` refuses). Oracle:
// KHR_lights_punctual's defaults (innerConeAngle 0, outerConeAngle π/4,
// range infinite) and `LightShape::Spot`'s `outer_angle < π/2` and
// `Light`'s finite range.
#[wasm_bindgen_test(unsupported = test)]
fn spot_cones_take_the_extension_s_defaults_and_hold_a_right_angle_below_it() {
    let lights = json!([
        {"type": "spot", "spot": {}},
        {"type": "spot", "range": 3.0, "spot": {"innerConeAngle": 1.0, "outerConeAngle": 1.5707964}}
    ]);
    let nodes = json!([light_node(0, json!({})), light_node(1, json!({}))]);
    let asset = load_slice(&fixture(lights, nodes, &[1, 2], false)).unwrap();
    let cone = |placement| match local(placed(&asset, placement)) {
        Light {
            shape:
                LightShape::Spot {
                    inner_angle,
                    outer_angle,
                    ..
                },
            range,
            intensity,
            ..
        } => (inner_angle, outer_angle, range, intensity),
        light => panic!("a spot placed as {light:?}"),
    };
    let (inner, outer, range, intensity) = cone(0);
    assert_eq!((inner, outer, intensity), (0., FRAC_PI_4, 1.));
    assert!(
        range.is_finite() && range >= 1000.,
        "an unbounded light reaches {range} m"
    );
    let (inner, outer, range, _) = cone(1);
    assert_eq!((inner, range), (1.0, 3.0));
    assert!(
        outer < FRAC_PI_2 && FRAC_PI_2 - outer < 1e-6,
        "outer angle {outer}"
    );
}

// Defects: a file that requires the extension is refused, or loads without
// its lights; one that defines no lights gains some. Oracle: glTF 2.0
// 5.17.2 (a loader that supports a required extension loads the file) and
// S3D-2 (file and embedded-byte imports share one decoder).
#[wasm_bindgen_test(unsupported = test)]
fn a_file_requiring_the_extension_loads_its_lights() {
    let lights = json!([{"type": "point"}]);
    let nodes = json!([light_node(0, json!({}))]);
    let asset = load_slice(&fixture(lights, nodes, &[1], true)).unwrap();
    assert_eq!(asset.lights.placements.len(), 1);
    let plain = load_slice(&fixture(json!([]), json!([]), &[], false)).unwrap();
    assert_eq!(plain.lights, AssetLights::default());
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn a_file_requiring_the_extension_loads_its_lights_from_a_path() {
    let directory = std::env::temp_dir().join(format!("sgl-gltf-lights-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("lights.glb");
    let lights = json!([{"type": "directional", "intensity": 2.0}]);
    let nodes = json!([light_node(0, json!({}))]);
    std::fs::write(&path, fixture(lights, nodes, &[1], true)).unwrap();
    let asset = load(&path);
    let _ = std::fs::remove_dir_all(&directory);
    let asset = asset.unwrap();
    assert_eq!(asset.lights.placements.len(), 1);
    assert_eq!(asset.lights.definitions[0].kind, PunctualKind::Directional);
}

// Defects: a light the extension forbids loads into a `Light` SGL3D
// refuses or misrenders, or the loader panics on it. Oracle:
// KHR_lights_punctual's ranges (color and intensity nonnegative, range
// > 0, 0 <= inner <= outer <= π/2, a light index within `lights`, a known
// type) and the light's direction, which a zero scale leaves undefined.
#[wasm_bindgen_test(unsupported = test)]
fn lights_the_extension_forbids_fail_the_load() {
    let node = |fields| json!([light_node(0, fields)]);
    let cases = [
        (
            json!([{"type": "point", "intensity": -1.0}]),
            node(json!({})),
            "light 0",
        ),
        (
            json!([{"type": "point", "color": [1.0, -0.5, 1.0]}]),
            node(json!({})),
            "light 0",
        ),
        (
            json!([{"type": "point", "range": 0.0}]),
            node(json!({})),
            "light 0",
        ),
        (
            json!([{"type": "spot", "spot": {"innerConeAngle": 0.5, "outerConeAngle": 0.4}}]),
            node(json!({})),
            "light 0",
        ),
        (
            json!([{"type": "spot", "spot": {"outerConeAngle": 1.6}}]),
            node(json!({})),
            "light 0",
        ),
        (
            json!([{"type": "spot", "spot": {"innerConeAngle": -0.1}}]),
            node(json!({})),
            "light 0",
        ),
        (
            json!([{"type": "spot", "spot": {"outerConeAngle": 0.0}}]),
            node(json!({})),
            "light 0",
        ),
        (json!([{"type": "spot"}]), node(json!({})), "spot"),
        (json!([{"type": "area"}]), node(json!({})), "type"),
        (
            json!([{"type": "point"}]),
            json!([light_node(1, json!({}))]),
            "light",
        ),
        (
            json!([{"type": "spot", "spot": {}}]),
            node(json!({"scale": [1., 1., 0.]})),
            "node 1",
        ),
        (
            json!([{"type": "directional"}]),
            node(json!({"scale": [1., 1., 0.]})),
            "node 1",
        ),
    ];
    for (lights, nodes, named) in cases {
        let error = load_slice(&fixture(lights.clone(), nodes, &[1], false))
            .err()
            .unwrap_or_else(|| panic!("{lights} loaded"))
            .to_string();
        assert!(error.contains(named), "{lights}: {error}");
    }
    // A zero scale leaves a point light its position.
    let lights = json!([{"type": "point"}]);
    assert!(
        load_slice(&fixture(
            lights,
            node(json!({"scale": [0., 0., 0.]})),
            &[1],
            false
        ))
        .is_ok()
    );
}
