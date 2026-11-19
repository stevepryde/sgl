//! The directional-light resolver under a procedural sky.
use super::{FrameShadow, directional_light, directional_shadow};
use crate::content::lighting::{DirectionalLight, DirectionalShadow, FrameEnvironment, Sky};
use crate::{Camera, FrameInput};
use glam::{Mat4, Vec3};
use wasm_bindgen_test::wasm_bindgen_test;

/// A frame seen from `eye` with one shadowed light whose centre stands
/// `elevation` radians above the horizontal, under `environment`.
fn input(environment: FrameEnvironment, eye: Vec3, elevation: f32) -> FrameInput {
    let mut input = FrameInput::new(Camera {
        view: Mat4::IDENTITY,
        projection: crate::perspective(1., 1., 0.1),
        eye,
    });
    input.environment = environment;
    input.directional_lights[0] = Some(DirectionalLight {
        direction: -Vec3::new(0., elevation.sin(), -elevation.cos()),
        illuminance: 1.,
        shadow: Some(DirectionalShadow::default()),
        ..DirectionalLight::default()
    });
    input
}

// Plausible defects: a sun below the planet's horizon still lighting surfaces
// through the planet, or still casting the frame's shadow (and so its
// cascades and its ray-traced shadow slot). The oracle is the spec: a light
// whose disc is wholly below the horizon is off and casts no shadow. From
// 1 m above the ground the horizon dips by acos(R / (R + 1)), about 0.03°;
// a sun whose centre is 1° below the horizontal has its 0.53° disc wholly
// below it. Under a map the same light is on: the sky is what turns it off.
#[wasm_bindgen_test(unsupported = test)]
fn a_light_wholly_below_the_horizon_is_off_and_casts_no_shadow() {
    let eye = Vec3::new(0., 1., 0.);
    let sky = FrameEnvironment::Sky(Sky::default());
    let below = input(sky, eye, -1f32.to_radians());
    assert_eq!(directional_light(&below, 0, eye), None);
    assert_eq!(directional_shadow(&below, eye), None);
    let shadow = FrameShadow::camera(
        &below,
        1024,
        crate::view::effective::ShadowFilter::default(),
        0,
        glam::DVec3::ZERO,
    );
    assert_eq!(shadow.light, None);
    let mapped = input(FrameEnvironment::None, eye, -1f32.to_radians());
    assert!(directional_light(&mapped, 0, eye).is_some());
    assert!(directional_shadow(&mapped, eye).is_some());
    // Well above the horizon the sky keeps it on, with its shadow.
    let above = input(sky, eye, 10f32.to_radians());
    assert!(directional_light(&above, 0, eye).is_some());
    assert_eq!(
        directional_shadow(&above, eye).map(|(index, _)| index),
        Some(0)
    );
}

// Plausible defect: a light whose centre has set below the horizon while
// part of its disc still shows takes the transmittance of a path that ends
// on the ground a few hundred metres off, so the setting sun turns white and
// several times brighter. The oracle is physical: the air toward a lower
// light is never thinner, and less of its disc shows, so as a light falls
// from above the horizon to wholly below it its colour, and its disc's
// radiance in the backdrop until the disc is gone, never rise in any channel.
// From 2 m up the horizon dips 0.045°; elevations from 1° to -1° in steps of
// 0.002° take the 0.53° disc across it.
#[wasm_bindgen_test(unsupported = test)]
fn a_setting_light_never_brightens() {
    let eye = Vec3::new(0., 2., 0.);
    let sky = FrameEnvironment::Sky(Sky::default());
    let mut previous: Option<([f32; 3], [f32; 3])> = None;
    for step in 0..=1000 {
        let elevation = (1. - 0.002 * step as f32).to_radians();
        let setting = input(sky, eye, elevation);
        let color = directional_light(&setting, 0, eye)
            .map_or([0.; 3], |light| light.color.map(|c| c * light.illuminance));
        let (_, discs) = super::sky_backdrop(&setting, eye);
        let disc = [discs[0][0], discs[0][1], discs[0][2]];
        if let Some((last_color, last_disc)) = previous {
            for c in 0..3 {
                assert!(
                    color[c] <= last_color[c] * (1. + 1e-5)
                        && disc[c] <= last_disc[c] * (1. + 1e-5),
                    "at {}°, channel {c}: light {} after {}, disc {} after {}",
                    elevation.to_degrees(),
                    color[c],
                    last_color[c],
                    disc[c],
                    last_disc[c]
                );
            }
        }
        previous = Some((color, disc));
    }
    assert_eq!(previous.map(|(color, _)| color), Some([0.; 3]));
}
