//! Authored glTF lights (`KHR_lights_punctual`, `Asset::lights`) loaded,
//! placed with `PunctualLight::place` and installed as a game installs
//! them, in the scenario harness (`specs/testing.md`, "SGL3D scenario
//! harness"). The oracles are the extension's photometry, not SGL3D's
//! shading model: a white wall's pixel lit by a point light of I candela
//! d metres away along its normal takes exactly the light of a directional
//! light of I/d² lux shining along that normal (one incoming direction and
//! one view, so any BRDF reflects them alike), a spot whole inside its
//! inner cone, the extension's reference falloff between its cones, and
//! nothing beyond its outer cone or beyond a light's range. No temporal
//! feature runs: antialiasing is off and every frame is a camera cut.
//!
//! Coverage (requirement 3): at rest, with a transformed parent and two
//! nodes placing one light. Import makes plain data, which neither motion,
//! a cut, a resize nor a setting reaches; the lights it makes are the
//! scene's and the frame's own, whose suites cover them in motion.
#![cfg(not(target_arch = "wasm32"))]
mod scenario;

use scenario::geometry::{Pose, View};
use scenario::image::{Image, centre};
use scenario::rig::{HALF_STEP, Rig, gpu};
use serde_json::{Value, json};
use sgl_3d::asset::{Asset, PlacedLight, load_slice};
use sgl_3d::glam::{Mat4, Quat, Vec3};
use sgl_3d::settings::Antialiasing;

/// The camera's lens: a vertical field of view whose half-angle's tangent
/// is 1/2, so the wall 6 m ahead spans 8 by 6 m.
const VIEW: View = View {
    size: [128, 96],
    fov_y: 0.927_295_2,
    near: 0.05,
};
/// The wall's plane, facing +Z, 6 m ahead of the camera at the origin.
const WALL_Z: f32 = -6.;
/// Each light's distance from the wall point it lights, along the wall's
/// normal.
const DISTANCE: f32 = 2.;
/// The point and spot lights' candela, and the directional light's lux:
/// the same illuminance at `DISTANCE` (40 / 2²).
const CANDELA: f32 = 40.;
const LUX: f32 = CANDELA / (DISTANCE * DISTANCE);
const INNER: f32 = 0.2;
const OUTER: f32 = 0.6;
/// The bounded point light's range, short of `DISTANCE`.
const SHORT_RANGE: f32 = 1.5;

/// The wall pixels each light is judged at, well apart.
const POINT_A: [u32; 2] = [24, 24];
const POINT_B: [u32; 2] = [104, 24];
const SPOT_WHOLE: [u32; 2] = [24, 72];
const SPOT_FALLING: [u32; 2] = [64, 72];
const SPOT_BEYOND: [u32; 2] = [104, 72];
const SHORT: [u32; 2] = [64, 24];

/// Where the camera at the origin sees the wall through `pixel`.
fn on_wall(pixel: [u32; 2]) -> Vec3 {
    let camera = VIEW.camera(Pose::at(Vec3::ZERO));
    let ray = VIEW.ray(&camera, centre(pixel));
    ray * (WALL_Z / ray.z)
}

/// Where a light lighting `pixel`'s wall point sits: `DISTANCE` in front.
fn before(pixel: [u32; 2]) -> Vec3 {
    on_wall(pixel) + Vec3::Z * DISTANCE
}

/// The light-placing nodes' parent: moved, turned 90° about +Y and scaled
/// unevenly, so a child's world position and axis depend on all three.
fn parent() -> Mat4 {
    Mat4::from_scale_rotation_translation(
        Vec3::new(2., 1., 3.),
        Quat::from_rotation_y(std::f32::consts::FRAC_PI_2),
        Vec3::new(1., -0.5, -3.),
    )
}

fn node(light: usize, mut fields: Value) -> Value {
    fields["extensions"] = json!({"KHR_lights_punctual": {"light": light}});
    fields
}

/// A child of `parent()` placing `light` at world `position`.
fn child(light: usize, position: Vec3) -> Value {
    let local = parent().inverse().transform_point3(position);
    node(light, json!({"translation": local.to_array()}))
}

/// A root node placing the spot at `pixel`'s light position, turned `tilt`
/// radians about +X off the wall's normal, the angle at which the wall
/// point lies off the spot's axis.
fn tilted_spot(pixel: [u32; 2], tilt: f32) -> Value {
    let rotation = Quat::from_rotation_x(tilt);
    node(
        2,
        json!({"translation": before(pixel).to_array(), "rotation": rotation.to_array()}),
    )
}

/// The fixture as a GLB: a white, fully rough wall of 12 by 9 m and the
/// lights, with the extension required.
fn fixture() -> Vec<u8> {
    let (x, y) = (6f32, 4.5f32);
    let positions = [[-x, -y], [x, -y], [x, y], [-x, y]].map(|[a, b]| [a, b, WALL_Z]);
    let normals = [[0f32, 0., 1.]; 4];
    let indices: [u16; 6] = [0, 1, 2, 0, 2, 3];
    let mut bin: Vec<u8> = bytemuck::cast_slice(positions.as_flattened()).to_vec();
    bin.extend_from_slice(bytemuck::cast_slice(normals.as_flattened()));
    bin.extend_from_slice(bytemuck::cast_slice(&indices));
    bin.resize(bin.len().next_multiple_of(4), 0);
    // Turning -90° about +Y under the parent's 90° turn leaves the sun's
    // -Z along the world's -Z, whatever the parent's scale.
    let sun = Quat::from_rotation_y(-std::f32::consts::FRAC_PI_2);
    let document = json!({
        "asset": {"version": "2.0"},
        "extensionsUsed": ["KHR_lights_punctual"],
        "extensionsRequired": ["KHR_lights_punctual"],
        "extensions": {"KHR_lights_punctual": {"lights": [
            {"type": "point", "intensity": CANDELA},
            {"type": "directional", "intensity": LUX},
            {"type": "spot", "intensity": CANDELA, "spot": {"innerConeAngle": INNER, "outerConeAngle": OUTER}},
            {"type": "point", "intensity": CANDELA, "range": SHORT_RANGE}
        ]}},
        "buffers": [{"byteLength": bin.len()}],
        "bufferViews": [
            {"buffer": 0, "byteOffset": 0, "byteLength": 48},
            {"buffer": 0, "byteOffset": 48, "byteLength": 48},
            {"buffer": 0, "byteOffset": 96, "byteLength": 12}
        ],
        "accessors": [
            {"bufferView": 0, "componentType": 5126, "count": 4, "type": "VEC3", "min": [-x, -y, WALL_Z], "max": [x, y, WALL_Z]},
            {"bufferView": 1, "componentType": 5126, "count": 4, "type": "VEC3"},
            {"bufferView": 2, "componentType": 5123, "count": 6, "type": "SCALAR"}
        ],
        "materials": [{"pbrMetallicRoughness": {"baseColorFactor": [1, 1, 1, 1], "metallicFactor": 0, "roughnessFactor": 1}}],
        "meshes": [{"primitives": [{"attributes": {"POSITION": 0, "NORMAL": 1}, "indices": 2, "material": 0}]}],
        "nodes": [
            {"mesh": 0},
            {
                "translation": parent().w_axis.truncate().to_array(),
                "rotation": Quat::from_rotation_y(std::f32::consts::FRAC_PI_2).to_array(),
                "scale": [2, 1, 3],
                "children": [2, 3, 4]
            },
            child(0, before(POINT_A)),
            child(0, before(POINT_B)),
            node(1, json!({"rotation": sun.to_array()})),
            tilted_spot(SPOT_WHOLE, 0.1),
            tilted_spot(SPOT_FALLING, 0.4),
            tilted_spot(SPOT_BEYOND, 0.7),
            node(3, json!({"translation": before(SHORT).to_array()}))
        ],
        "scenes": [{"nodes": [0, 1, 5, 6, 7, 8]}], "scene": 0
    });
    let mut json = serde_json::to_vec(&document).unwrap();
    json.resize(json.len().next_multiple_of(4), b' ');
    let mut glb = Vec::new();
    for word in [
        0x4654_6c67u32,
        2,
        (28 + json.len() + bin.len()) as u32,
        json.len() as u32,
        0x4e4f_534a,
    ] {
        glb.extend(word.to_le_bytes());
    }
    glb.extend(json);
    glb.extend((bin.len() as u32).to_le_bytes());
    glb.extend(0x004e_4942u32.to_le_bytes());
    glb.extend(bin);
    glb
}

/// The rig holding the fixture's wall, and the fixture's lights, each as
/// placed, in placement order.
fn stage() -> Option<(Rig, Asset, Vec<PlacedLight>)> {
    let gpu = gpu()?;
    let asset = load_slice(&fixture()).unwrap();
    let lights = asset
        .lights
        .placements
        .iter()
        .map(|placement| asset.lights.definitions[placement.light].place(placement.transform))
        .collect();
    let mut rig = Rig::new(&gpu, VIEW, Antialiasing::Off, 0.);
    rig.add_model(asset.clone(), Pose::at(Vec3::ZERO));
    Some((rig, asset, lights))
}

/// The linear scene of a camera cut lit by `light` alone, installed as a
/// game installs it: a local light in the scene, a directional one in the
/// frame's first slot.
fn shot(rig: &mut Rig, light: PlacedLight) -> Image {
    let id = match light {
        PlacedLight::Local(light) => Some(
            rig.scene
                .add_light(&rig.gpu.device, &rig.gpu.queue, light)
                .expect("the scene takes a placed light"),
        ),
        PlacedLight::Directional(light) => {
            rig.views[0].input.directional_lights = [Some(light), None];
            None
        }
    };
    rig.views[0].input.camera_cut = true;
    rig.render(0);
    let image = rig.read(sgl_3d::diagnostics::DiagnosticTarget::Composite);
    if let Some(id) = id {
        rig.scene.remove_light(id).unwrap();
    }
    rig.views[0].input.directional_lights = [None; 2];
    image
}

/// Asserts `got`'s pixel holds `scale` times `want`'s, within a step of
/// each of the two stores into the Rgba16Float target that hold them, and
/// is lit.
fn assert_scaled(got: &Image, want: &Image, scale: f32, pixel: [u32; 2], what: &str) {
    let (got, want) = (got.at(pixel), want.at(pixel));
    for c in 0..3 {
        let expected = want[c] * scale;
        assert!(
            expected > 0.01,
            "{what}: the oracle frame is unlit at {pixel:?}"
        );
        assert!(
            (got[c] - expected).abs() <= 2. * HALF_STEP * expected,
            "{what}: {} at {pixel:?}, where {scale} of the directional light's {} is {expected}",
            got[c],
            want[c]
        );
    }
}

fn assert_black(image: &Image, pixel: [u32; 2], what: &str) {
    assert_eq!(image.at(pixel)[..3], [0.; 3], "{what} at {pixel:?}");
}

// Defects: imported candela or lux misread (Bevy's ×4π, a lumen reading),
// a light placed off its node's world position (a parent's transform
// dropped or misapplied), the two placements of one light collapsed into
// one, or a directional light that shines off its node's -Z. Oracle:
// KHR_lights_punctual: a point light's illuminance at d along the normal is
// I/d², a directional light's is its lux, both from one direction, so
// equal at 40 cd, 2 m and 10 lux.
#[test]
fn imported_point_lights_light_the_wall_as_the_imported_sun_of_equal_illuminance() {
    let Some((mut rig, asset, lights)) = stage() else {
        return;
    };
    let placements = &asset.lights.placements;
    assert_eq!(
        placements.iter().map(|p| p.light).collect::<Vec<_>>(),
        [0, 0, 1, 2, 2, 2, 3],
        "the placements in scene order"
    );
    let sun = shot(&mut rig, lights[2]);
    for (light, pixel, what) in [(0, POINT_A, "first lamp"), (1, POINT_B, "second lamp")] {
        let lamp = shot(&mut rig, lights[light]);
        assert_scaled(&lamp, &sun, 1., pixel, what);
    }
}

// Defects: an imported spot's cone misplaced, its angles misread (degrees,
// halved, swapped) or its falloff not the extension's, or a bounded light
// reaching past its range. Oracle: KHR_lights_punctual: a spot's candela is
// its brightness inside its inner cone; between the cones its reference
// code's falloff ((cos θ − cos outer)/(cos inner − cos outer))², at θ =
// 0.4 here; none beyond its outer cone; and a light lights nothing beyond
// its range.
#[test]
fn an_imported_spot_s_cone_and_a_light_s_range_bound_its_light() {
    let Some((mut rig, _, lights)) = stage() else {
        return;
    };
    let sun = shot(&mut rig, lights[2]);
    assert_scaled(
        &shot(&mut rig, lights[3]),
        &sun,
        1.,
        SPOT_WHOLE,
        "inside the inner cone",
    );
    let falloff = ((0.4f32.cos() - OUTER.cos()) / (INNER.cos() - OUTER.cos())).powi(2);
    assert_scaled(
        &shot(&mut rig, lights[4]),
        &sun,
        falloff,
        SPOT_FALLING,
        "between the cones",
    );
    assert_black(
        &shot(&mut rig, lights[5]),
        SPOT_BEYOND,
        "beyond the outer cone",
    );
    assert_black(&shot(&mut rig, lights[6]), SHORT, "beyond the range");
}
