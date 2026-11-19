//! Where a spot's cookie falls (`light_cookie_uv`, light_cookie.wgsl), from
//! the scene's light record on the GPU, judged by the cookie's documented
//! geometry (`LightCookie`): a square image spanning twice the outer cone
//! angle about the spot's direction, its top toward the part of `up`
//! across the direction, its right the direction crossed with that.
use crate::asset::Image;
use crate::{Light, LightCookie, LightShape, Scene};
use glam::Vec3;

// Plausible defects: the image's top or right taken from the wrong axis
// (`up` unrejected, a right crossed the other way, v not flipped), the
// square scaled to the cone's half angle or its diameter, or fitted to the
// cone's circle rather than holding it inscribed. The oracle is the
// documented geometry, worked by hand for a spot pointing down (-Y, given
// at length 2) whose `up` (0, 0.5, -1) leans out of the plane across it:
// its part across the direction is -Z, the image's top, so its right is
// (-Y) x (-Z) = +X. A point on the axis is the image's centre; one at the
// outer angle toward -Z, +Z, +X and -X is the middle of its top, bottom,
// right and left edge; one at the outer angle's tangent along both +X and
// -Z is its top right corner.
#[test]
fn a_cookie_fills_the_square_about_its_cone() {
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    let mut scene = Scene::new(&device, &queue);
    let image = scene
        .add_decal_image(Image::Rgba8(image::RgbaImage::from_pixel(
            4,
            4,
            image::Rgba([255; 4]),
        )))
        .unwrap();
    let outer = 0.6f32;
    let position = Vec3::new(1., 2., 3.);
    scene
        .add_light(
            &device,
            &queue,
            Light {
                position,
                shape: LightShape::Spot {
                    direction: Vec3::new(0., -2., 0.),
                    inner_angle: 0.3,
                    outer_angle: outer,
                    radius: 0.,
                },
                range: 20.,
                cookie: Some(LightCookie {
                    image,
                    up: Vec3::new(0., 0.5, -1.),
                }),
                ..Default::default()
            },
        )
        .unwrap();
    let at_outer = |across: Vec3| (Vec3::NEG_Y * outer.cos() + across * outer.sin()) * 3.;
    let cases: [(Vec3, [f32; 2]); 6] = [
        (Vec3::NEG_Y * 2., [0.5, 0.5]),
        (at_outer(Vec3::NEG_Z), [0.5, 0.]),
        (at_outer(Vec3::Z), [0.5, 1.]),
        (at_outer(Vec3::X), [1., 0.5]),
        (at_outer(Vec3::NEG_X), [0., 0.5]),
        (Vec3::new(outer.tan(), -1., -outer.tan()) * 1.5, [1., 0.]),
    ];
    let offsets: Vec<[f32; 4]> = cases
        .iter()
        .map(|(offset, _)| offset.extend(0.).to_array())
        .collect();
    let observation = r#"
@group(0) @binding(3) var<storage,read> cases:array<vec4<f32>>;
@group(0) @binding(4) var<storage,read_write> result:array<vec4<f32>>;
@compute @workgroup_size(64) fn observe(@builtin(global_invocation_id) id:vec3<u32>) {
 if id.x>=arrayLength(&cases) { return; }
 result[id.x]=vec4(light_cookie_uv(lights[0],cases[id.x].xyz),0.,0.);
}
"#;
    let rows = crate::test_support::observe_surface(
        &device,
        &queue,
        &scene,
        bytemuck::cast_slice(&offsets),
        observation,
        1,
        cases.len(),
    );
    for ((offset, expected), row) in cases.iter().zip(rows) {
        let uv = [row[0], row[1]];
        assert!(
            uv.iter()
                .zip(expected)
                .all(|(got, want)| (got - want).abs() <= 1e-5),
            "offset {offset} from the light lands at {uv:?}, not {expected:?}"
        );
    }
}
