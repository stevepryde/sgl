//! Crystal's temporal resolve on a plane mirror. The reflected image moves
//! with the virtual light behind the mirror as the camera translates.
use super::followed_surface_tests::{Followed, followed};
use super::temporal_neighborhood_tests::{SIZE, camera, device};

// Jitter moves the sample grid while the mirror and its virtual light
// remain stationary. The current gradient admits either previous light
// under neighbourhood clipping; changing only that light must change RGB.
#[test]
fn a_still_mirror_under_jitter_discards_no_history() {
    let Some((device, queue)) = device() else {
        return;
    };
    for reversed in [false, true] {
        let run = |history| {
            followed(
                &device,
                &queue,
                &Followed {
                    reversed,
                    z: 2.,
                    previous_eye: [0.; 3],
                    virtual_z: 4.,
                    jitter: [
                        [0.3 / SIZE[0] as f32, -0.2 / SIZE[1] as f32],
                        [-0.4 / SIZE[0] as f32, 0.35 / SIZE[1] as f32],
                    ],
                    motion: [0., 0., 0., 1.],
                    history,
                    current: |x, _| light(x as f32),
                },
            )
        };
        let (dim, variance) = run(|x, _| light(x as f32) * 0.99);
        let (bright, _) = run(|x, _| light(x as f32) * 1.01);
        for (p, ((a, b), v)) in dim.iter().zip(&bright).zip(variance).enumerate() {
            assert!(
                b[0] > a[0] + 0.001 && v < 1.,
                "reversed={reversed} pixel={p} lost history: {a:?} vs {b:?}, variance {v}"
            );
        }
    }
}

/// Camera translation for a stationary virtual image at Z = 4 m to move
/// right by `pixels`: screen displacement is focal length × travel / Z.
/// The mirror at Z = 2 m therefore moves twice as far on screen.
fn previous_eye(pixels: f32) -> [f32; 3] {
    let focal_length_pixels = camera(true, 0).m_proj[0] * SIZE[0] as f32 / 2.;
    [4. * pixels / focal_length_pixels, 0., 0.]
}

fn stripes(x: u32, _: u32) -> f32 {
    if matches!(x, 30 | 32 | 34) { 4. } else { 0. }
}

// Three narrow parallel lights reflected in a mirror. After a lateral camera
// step the same light fills the next pixel, while the mirror itself slides
// two pixels. At the middle light's old location both neighbours are now
// bright, so the 3x3 mean favours a bright history. At its new location both
// neighbours are dark and the mean favours darkness. The known light positions
// give the correct answer in both cases independently of the denoiser.
#[test]
fn a_reflected_highlight_moves_by_its_own_parallax() {
    let Some((device, queue)) = device() else {
        return;
    };
    let (radiance, _) = followed(
        &device,
        &queue,
        &Followed {
            reversed: true,
            z: 2.,
            previous_eye: previous_eye(1.),
            virtual_z: 4.,
            jitter: [[0.; 2]; 2],
            motion: [4. / SIZE[0] as f32, 0., 0., 1.],
            history: stripes,
            current: |x, y| stripes(x.saturating_sub(1), y),
        },
    );
    let row = (SIZE[1] / 2 * SIZE[0]) as usize;
    // The translation is a whole pixel, so the same texel footprint is
    // fully lit at the new location and fully dark at the old one.
    assert!(
        radiance[row + 32][0] < 0.005,
        "trail {:?}",
        radiance[row + 32]
    );
    assert!(
        radiance[row + 33][0] > 3.995,
        "missing light {:?}",
        radiance[row + 33]
    );
}

fn light(x: f32) -> f32 {
    0.75 - x / 256.
}

// The camera now moves an eighth of a reflected texel. A smooth light
// gradient brightens by one percent. The extra eighth of a texel between
// surface and virtual motion makes the surface history slightly brighter,
// but the lighting change still exceeds the local spatial variation. The
// old discard treated this as disocclusion and returned the current frame.
#[test]
fn a_slowly_moving_mirror_keeps_history_as_the_light_changes() {
    let Some((device, queue)) = device() else {
        return;
    };
    let (radiance, variance) = followed(
        &device,
        &queue,
        &Followed {
            reversed: true,
            z: 2.,
            previous_eye: previous_eye(0.125),
            virtual_z: 4.,
            jitter: [
                [0.3 / SIZE[0] as f32, -0.2 / SIZE[1] as f32],
                [-0.4 / SIZE[0] as f32, 0.35 / SIZE[1] as f32],
            ],
            motion: [0.5 / SIZE[0] as f32, 0., 0., 1.],
            history: |x, _| light(x as f32),
            current: |x, _| light(x as f32 - 0.125) * 1.01,
        },
    );
    let row = (SIZE[1] / 2 * SIZE[0]) as usize;
    for x in 8..SIZE[0] - 8 {
        let pixel = row + x as usize;
        let this_frame = light(x as f32 - 0.125) * 1.01;
        // Beyond the binary16 storage step, the previous (dimmer) light
        // still contributes. A reset stores this frame and variance 1.
        assert!(
            variance[pixel] < 1.
                && (light(x as f32 - 0.125) - 0.001..this_frame - 0.001)
                    .contains(&radiance[pixel][0]),
            "column {x}, current {this_frame}, resolved {:?}, variance {}",
            radiance[pixel],
            variance[pixel]
        );
    }
}

// Sliding an unbounded flat mirror sideways in its own plane changes which
// material point we see, while its plane and the virtual light remain fixed.
// A slow change in the light must therefore accumulate at the stationary
// reflection position even as measured receiver motion crosses whole pixels.
#[test]
fn a_mirror_sliding_in_its_plane_keeps_the_stationary_lights_history() {
    let Some((device, queue)) = device() else {
        return;
    };
    let run = |old_light| {
        followed(
            &device,
            &queue,
            &Followed {
                reversed: true,
                z: 2.,
                previous_eye: [0.; 3],
                virtual_z: 4.,
                jitter: [[0.; 2]; 2],
                motion: [8. / SIZE[0] as f32, 0., 0., 1.],
                history: old_light,
                current: |x, _| light(x as f32),
            },
        )
        .0
    };
    let steady = run(|x, _| light(x as f32));
    let brightened = run(|x, _| light(x as f32) * 0.99);
    for x in 8..SIZE[0] - 8 {
        let i = (SIZE[1] / 2 * SIZE[0] + x) as usize;
        assert!(
            steady[i][0] > brightened[i][0] + 0.001,
            "column {x} retained no previous light: steady {:?}, brightened {:?}",
            steady[i],
            brightened[i]
        );
    }
}
