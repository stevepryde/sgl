use super::{SKY_GROUND_RADIUS, SKY_TOP_RADIUS, SkyAtmosphereUniform, transmittance};
use crate::Sky;
use wasm_bindgen_test::wasm_bindgen_test;

/// Bruneton's inverse transmittance mapping (sky_bruneton.wgsl's
/// `transmittance_lut_uv_to_r_mu`): where a texel centre lies, which the test
/// asks both integrals about.
#[cfg(not(target_arch = "wasm32"))]
fn texel_r_mu(uv: [f64; 2]) -> (f64, f64) {
    let (ground, top) = (f64::from(SKY_GROUND_RADIUS), f64::from(SKY_TOP_RADIUS));
    let h = (top * top - ground * ground).sqrt();
    let rho = h * uv[1];
    let r = (rho * rho + ground * ground).sqrt();
    let d_min = top - r;
    let d_max = rho + h;
    let d = d_min + uv[0] * (d_max - d_min);
    let mu = if d == 0. {
        1.
    } else {
        (h * h - rho * rho - d * d) / (2. * r * d)
    };
    (r, mu.clamp(-1., 1.))
}

// Plausible defects: the GPU integral and its Rust twin drifting apart (a
// sample position, a step, a profile or a coefficient changed in one only), so
// the sun colour every consumer takes (view::directional_light) no longer
// matches the sky the backdrop shows. Two implementations of one integral:
// each texel at heights from the ground to near the top and elevations from
// the zenith to the horizon is compared with the twin at the texel's own
// radius and zenith cosine, within RGBA16F's rounding (one half step) and
// float rounding of the 40-step sum.
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn the_rust_twin_agrees_with_the_transmittance_table() {
    use crate::stages::sky::TRANSMITTANCE_SIZE;
    let Some((device, queue)) = crate::test_support::device() else {
        return;
    };
    let look = Sky {
        mie: 2.,
        ..Sky::default()
    };
    let mut input = crate::FrameInput::new(crate::Camera {
        view: glam::Mat4::IDENTITY,
        projection: crate::perspective(1., 1., 0.1),
        eye: glam::Vec3::Y,
    });
    input.environment = crate::FrameEnvironment::Sky(look);
    let mut stage = crate::stages::sky::Sky::default();
    stage.prepare(&device, &input, input.camera.eye, true);
    let mut encoder = device.create_command_encoder(&Default::default());
    stage.encode_planned((&queue, &mut encoder), None);
    queue.submit([encoder.finish()]);
    let [table, _, _] = stage.tables().unwrap();
    let bytes = crate::test_support::read(&device, &queue, table, 8);
    let atmosphere = SkyAtmosphereUniform::of(&look);
    let [width, height] = TRANSMITTANCE_SIZE;
    let mut checked = 0;
    for y in [0, 1, 16, 64, height - 1] {
        for x in [0, 3, 40, 128, 200, width - 2, width - 1] {
            let uv = [
                (f64::from(x) + 0.5) / f64::from(width),
                (f64::from(y) + 0.5) / f64::from(height),
            ];
            let (r, mu) = texel_r_mu(uv);
            let twin = transmittance(&atmosphere, r as f32, mu as f32);
            let at = ((y * width + x) * 8) as usize;
            for c in 0..3 {
                let gpu = crate::test_support::half(&bytes[at + c * 2..]);
                let tolerance = twin[c].abs() / 1024. + 1e-4;
                assert!(
                    (gpu - twin[c]).abs() <= tolerance,
                    "texel {x},{y} (r {r}, mu {mu}), channel {c}: the table holds {gpu}, the twin {}",
                    twin[c]
                );
            }
            checked += 1;
        }
    }
    assert_eq!(checked, 35);
}

// Plausible defects: a medium whose spectral ordering is inverted (a channel
// swapped, a sign lost) or an integral that does not lengthen with the path.
// The oracle is Rayleigh's published ordering: the air scatters blue most
// and red least, so near the horizon, through far more air than toward the
// zenith, every channel is dimmer and red outlasts green outlasts blue.
#[wasm_bindgen_test(unsupported = test)]
fn low_sun_is_dimmer_and_redder_than_the_zenith() {
    let atmosphere = SkyAtmosphereUniform::of(&Sky::default());
    let r = SKY_GROUND_RADIUS + 1.;
    let zenith = transmittance(&atmosphere, r, 1.);
    let low = transmittance(&atmosphere, r, 2f32.to_radians().sin());
    for c in 0..3 {
        assert!(
            low[c] < zenith[c],
            "channel {c}: {low} at 2°, {zenith} at the zenith"
        );
    }
    assert!(low.x > low.y && low.y > low.z, "{low} at 2°");
}
