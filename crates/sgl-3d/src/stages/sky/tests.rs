use super::{Computed, MULTISCATTERING_SIZE, SKY_VIEW_SIZE, Sky};
use crate::renderer::test_frame::TestRenderer;
use crate::settings::Settings;
use crate::shading::gbuffer;
use crate::{
    Backdrop, Camera, DirectionalLight, FrameEnvironment, FrameInput, Scene, Sky as SkyLook,
    test_support,
};
use glam::Vec3;

const SIZE: [u32; 2] = [8, 8];

/// A camera at `eye` looking along -Z.
fn camera(eye: Vec3) -> Camera {
    Camera {
        view: glam::camera::rh::view::look_to_mat4(eye, Vec3::NEG_Z, Vec3::Y),
        projection: crate::perspective(1., 1., 0.1),
        eye,
    }
}

/// A frame under `sky` seen from `eye`, lit by a sun `elevation` radians up.
fn sky_input(sky: SkyLook, eye: Vec3, elevation: f32) -> FrameInput {
    let mut input = FrameInput::new(camera(eye));
    input.environment = FrameEnvironment::Sky(sky);
    input.directional_lights[0] = Some(DirectionalLight {
        direction: -Vec3::new(0., elevation.sin(), -elevation.cos()),
        illuminance: 1.,
        ..DirectionalLight::default()
    });
    input
}

/// The texels of an RGBA16F `texture`, row-major.
fn texels(device: &wgpu::Device, queue: &wgpu::Queue, texture: &wgpu::Texture) -> Vec<[f32; 4]> {
    test_support::read(device, queue, texture, 8)
        .chunks(8)
        .map(|texel| std::array::from_fn(|c| test_support::half(&texel[c * 2..])))
        .collect()
}

/// The tables of `input`'s sky computed by a stage of their own.
fn tables(device: &wgpu::Device, queue: &wgpu::Queue, input: &FrameInput) -> Sky {
    let mut sky = Sky::default();
    sky.prepare(device, input, input.camera.eye, (true, false));
    let mut encoder = device.create_command_encoder(&Default::default());
    sky.encode_planned((queue, &mut encoder), None);
    queue.submit([encoder.finish()]);
    sky
}

// Plausible defects: a wrong coefficient, scale height or ozone profile, a
// medium scaled twice or not at all, a broken mapping or integral. The oracle
// is analytic: the closed-form optical depth of the vertical column, the sum
// over the media of each one's coefficient times its column (its scale height
// for an exponential profile, its tent's area for ozone), from the published
// coefficients (Hillaire 2020's Earth, as Wicked Engine's ShaderInterop_Weather.h
// lists them), never from the march. The table's texels are centred half a
// texel off the ground's zenith (u = v = 0), so the value there is
// extrapolated linearly from the first two texels along u; v's half texel is
// 1.5 m of height. 1 % is the spec's agreement.
#[test]
fn transmittance_table_meets_the_closed_form_column_at_the_ground_zenith() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let sky = tables(
        &device,
        &queue,
        &sky_input(SkyLook::default(), Vec3::new(0., 1., 0.), 1.),
    );
    let [transmittance, _, _] = sky.tables().unwrap();
    let texels = texels(&device, &queue, transmittance);
    let [first, second] = [texels[0], texels[1]];
    let rayleigh = [5.802e-6, 13.558e-6, 33.1e-6].map(|b: f64| b * 8000.);
    let mie = 4.440e-6 * 1200.;
    let ozone = [0.650e-6, 1.881e-6, 0.085e-6].map(|b: f64| b * 15_000.);
    let expected: [f64; 3] = std::array::from_fn(|c| (-(rayleigh[c] + mie + ozone[c])).exp());
    // Published: (0.9404, 0.8676, 0.7623).
    for c in 0..3 {
        let at_zenith = f64::from(first[c] - (second[c] - first[c]) * 0.5);
        assert!(
            (at_zenith / expected[c] - 1.).abs() < 0.01,
            "channel {c}: the table holds {at_zenith} at the ground's zenith, the closed form {}",
            expected[c]
        );
    }
}

// Plausible defects: the multiple-scattering series diverging (f_ms reaching
// 1 in a dense medium), a division by a zero extinction in an empty one, or a
// NaN from a light direction off the unit sphere. The oracle is a bound: every
// texel finite and non-negative, for every sky whose densities are 0, 1 and
// 10 times Earth's.
#[test]
fn multiple_scattering_is_finite_and_non_negative_from_empty_to_dense_air() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let scales = [0., 1., 10.];
    // One stage, whose pipelines every sky shares; no frame is finished, so
    // each encode computes every table.
    let mut sky = Sky::default();
    for rayleigh in scales {
        for mie in scales {
            for ozone in scales {
                let look = SkyLook {
                    rayleigh,
                    mie,
                    ozone,
                    ..SkyLook::default()
                };
                let input = sky_input(look, Vec3::Y, 0.3);
                sky.prepare(&device, &input, Vec3::Y, (true, false));
                let mut encoder = device.create_command_encoder(&Default::default());
                sky.encode_planned((&queue, &mut encoder), None);
                queue.submit([encoder.finish()]);
                assert!(sky.computed().multiscattering);
                let [_, multiscattering, _] = sky.tables().unwrap();
                let texels = texels(&device, &queue, multiscattering);
                assert_eq!(
                    texels.len(),
                    (MULTISCATTERING_SIZE * MULTISCATTERING_SIZE) as usize
                );
                for texel in texels {
                    assert!(
                        texel[..3].iter().all(|&c| c.is_finite() && c >= 0.),
                        "Rayleigh {rayleigh}, Mie {mie}, ozone {ozone}: {texel:?}"
                    );
                }
            }
        }
    }
}

// Plausible defects: light leaking into the sky from a light that is off (a
// ground term or multiple scattering not weighted by the light's colour), or a
// backdrop that adds something of its own. The oracle is the model: with no
// light the air in-scatters nothing, so the sky-view table and the backdrop
// are exactly 0.
#[test]
fn with_every_light_off_the_sky_view_table_and_the_backdrop_are_black() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let mut input = sky_input(SkyLook::default(), Vec3::Y, 0.5);
    input.directional_lights = [None; 2];
    let sky = tables(&device, &queue, &input);
    let [_, _, sky_view] = sky.tables().unwrap();
    let table = texels(&device, &queue, sky_view);
    assert_eq!(table.len(), (SKY_VIEW_SIZE[0] * SKY_VIEW_SIZE[1]) as usize);
    assert!(
        table.iter().all(|texel| texel[..3] == [0.; 3]),
        "the sky-view table is lit with no light"
    );
    let settings = Settings::default();
    let mut scene = Scene::new(&device, &queue);
    let mut renderer = TestRenderer::for_test(&device, &queue, SIZE, &settings);
    let mut frame = renderer.prepare_test_frame(&device, &queue, &mut scene, &input, &settings);
    let mut encoder = device.create_command_encoder(&Default::default());
    renderer.encode_test_opaque(&device, &queue, &mut encoder, &scene, &mut frame, false);
    queue.submit([encoder.finish()]);
    let color = texels(&device, &queue, renderer.targets().color.texture());
    assert!(
        color.iter().all(|texel| texel[..3] == [0.; 3]),
        "the backdrop shows a sky with no light: {:?}",
        color[0]
    );
}

// Plausible defects: tables computed every frame (the cost the stage exists
// to avoid), a changed input that leaves a stale table, the sky view not
// following the lights or the camera's height, the medium's tables not
// following its fields, what a table holds recorded at encode, so a frame
// that was never finished leaves the next believing it computed, or a table
// an unfinished frame overwrote taken as still holding the last finished
// frame's, so a return to that frame's inputs shows the unfinished one's.
// The oracle is the contract (X-4): each table is computed exactly when its
// inputs differ from those of the last finished frame that computed it, or
// a frame that was not finished computed it since. And the aerial volume
// lives exactly while aerial perspective is in effect: neither the sky
// stage nor the fog stage, which integrates it, holds it after.
#[test]
fn each_table_is_computed_when_its_inputs_change_and_a_frame_is_unfinished() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let settings = Settings::default();
    let mut scene = Scene::new(&device, &queue);
    let mut renderer = TestRenderer::for_test(&device, &queue, SIZE, &settings);
    let output = crate::view::targets::target(&device, "sky frames", SIZE, gbuffer::COLOR);
    let mut frame = |renderer: &mut TestRenderer, input: &FrameInput, finish: bool| {
        let mut encoder = device.create_command_encoder(&Default::default());
        renderer.render(
            &device,
            &queue,
            &mut encoder,
            &mut scene,
            input,
            &settings,
            &output,
            None,
        );
        queue.submit([encoder.finish()]);
        if finish {
            renderer.finish_frame(&mut scene);
        }
        renderer.view.sky.computed()
    };
    let all = Computed {
        transmittance: true,
        multiscattering: true,
        sky_view: true,
    };
    let view = Computed {
        sky_view: true,
        ..Computed::default()
    };
    let none = Computed::default();
    let eye = Vec3::new(0., 2., 0.);
    let input = sky_input(SkyLook::default(), eye, 0.5);
    assert_eq!(
        frame(&mut renderer, &input, true),
        all,
        "the first sky frame"
    );
    assert_eq!(frame(&mut renderer, &input, true), none, "an equal frame");
    let moved = FrameInput {
        camera: camera(Vec3::new(30., 2., -40.)),
        ..input
    };
    assert_eq!(
        frame(&mut renderer, &moved, true),
        none,
        "the camera moved across"
    );
    let climbed = FrameInput {
        camera: camera(Vec3::new(0., 50., 0.)),
        ..input
    };
    assert_eq!(
        frame(&mut renderer, &climbed, true),
        view,
        "the camera climbed"
    );
    let lower = sky_input(SkyLook::default(), Vec3::new(0., 50., 0.), 0.2);
    assert_eq!(
        frame(&mut renderer, &lower, true),
        view,
        "the sun went lower"
    );
    let hazy = sky_input(
        SkyLook {
            mie: 3.,
            ..SkyLook::default()
        },
        Vec3::new(0., 50., 0.),
        0.2,
    );
    // An unfinished frame overwrites the tables it computes: a return to
    // the last finished frame's inputs computes them again.
    assert_eq!(
        frame(&mut renderer, &hazy, false),
        all,
        "the haze thickened in a frame that was not finished"
    );
    assert_eq!(
        frame(&mut renderer, &lower, true),
        all,
        "back to the clear air the last finished frame computed"
    );
    let lowest = sky_input(SkyLook::default(), Vec3::new(0., 50., 0.), 0.1);
    assert_eq!(
        frame(&mut renderer, &lowest, false),
        view,
        "the sun went lower in a frame that was not finished"
    );
    assert_eq!(
        frame(&mut renderer, &lower, true),
        view,
        "back to the sun the last finished frame computed"
    );
    assert_eq!(
        frame(&mut renderer, &hazy, false),
        all,
        "the haze thickened"
    );
    assert_eq!(
        frame(&mut renderer, &hazy, true),
        all,
        "the same frame again after it was not finished"
    );
    assert_eq!(
        frame(&mut renderer, &hazy, true),
        none,
        "once it was finished"
    );
    // Aerial perspective runs here without the fog (the frame's atmosphere
    // is off): the fog stage integrates the sky's aerial volume alone, and
    // lets go of it and of that integrated volume once aerial perspective
    // is out of effect, at strength 0 (which no table depends on) as
    // without a sky.
    let holds = |renderer: &TestRenderer| {
        (
            renderer.view.sky.aerial_volume().is_some(),
            renderer.view.fog.holds_aerial(),
        )
    };
    assert_eq!(holds(&renderer), (true, true), "aerial perspective");
    let mut clear = hazy;
    if let FrameEnvironment::Sky(sky) = &mut clear.environment {
        sky.aerial_perspective = 0.;
    }
    assert_eq!(frame(&mut renderer, &clear, true), none, "strength 0");
    assert_eq!(holds(&renderer), (false, false), "strength 0");
    assert_eq!(frame(&mut renderer, &hazy, true), none, "strength 1");
    assert_eq!(holds(&renderer), (true, true), "strength 1");
    // A frame without a sky drops the tables and the aerial volume; the
    // next sky frame computes them all again.
    let mut mapless = hazy;
    mapless.environment = FrameEnvironment::None;
    mapless.backdrop = Backdrop::Color([0.; 3]);
    assert_eq!(frame(&mut renderer, &mapless, true), none, "no sky");
    assert!(renderer.view.sky.tables().is_none(), "no sky, no tables");
    assert_eq!(holds(&renderer), (false, false), "no sky");
    assert_eq!(frame(&mut renderer, &hazy, true), all, "the sky again");
}

// Plausible defects: a filtered level reading the wrong level or an unwritten
// part of the atlas (a wrong mip or origin), weights left unnormalised, or a
// level never written. The oracle is a bound: each of levels 1 to 9 is a
// normalised, N.L-weighted mean of the level before (Three's GGX filter), so
// by induction each of its texels lies within the range of the sharp level's
// texels in every channel (to half-float rounding), and is lit where the
// sharp level is lit everywhere (a sun at 30° lights the whole sky and the
// ground).
#[test]
fn every_filtered_level_is_a_mean_of_the_sharp_level() {
    use super::environment::ATLAS;
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let sky = tables(
        &device,
        &queue,
        &sky_input(
            SkyLook::default(),
            Vec3::new(0., 2., 0.),
            30f32.to_radians(),
        ),
    );
    let atlas = texels(&device, &queue, sky.atlas().unwrap());
    let [width, _] = ATLAS.size();
    let level_range = |level: u32| {
        let placed = ATLAS.level(level);
        let mut range = ([f32::INFINITY; 3], [f32::NEG_INFINITY; 3]);
        for face in 0..6 {
            let [x0, y0] = placed.face_origin(face);
            for y in y0..y0 + placed.size {
                for x in x0..x0 + placed.size {
                    let texel = atlas[(y * width + x) as usize];
                    for (c, &value) in texel[..3].iter().enumerate() {
                        range.0[c] = range.0[c].min(value);
                        range.1[c] = range.1[c].max(value);
                    }
                }
            }
        }
        range
    };
    let (low, high) = level_range(0);
    assert!(
        low.iter().all(|&c| c > 0.),
        "the sharp level is unlit: {low:?}"
    );
    for level in 1..ATLAS.levels() {
        let (level_low, level_high) = level_range(level);
        for c in 0..3 {
            assert!(
                level_low[c] >= low[c] * (1. - 1. / 512.)
                    && level_high[c] <= high[c] * (1. + 1. / 512.),
                "level {level}, channel {c}: {}..{} outside the sharp level's {}..{}",
                level_low[c],
                level_high[c],
                low[c],
                high[c]
            );
        }
    }
}

/// The aerial volume of `look` over a camera 1 m up whose middle row of
/// columns (row 15 of 32) lies level, so their rays run along the ground.
fn level_aerial(device: &wgpu::Device, queue: &wgpu::Queue, look: SkyLook) -> Vec<[f32; 4]> {
    let mut input = sky_input(look, Vec3::Y, 1.);
    let projection = input.camera.projection;
    let row = crate::shading::sky::AERIAL_SLICES / 2 - 1;
    let ndc_y = 1. - 2. * (row as f32 + 0.5) / crate::shading::sky::AERIAL_SLICES as f32;
    let tilt = (ndc_y / projection.y_axis.y).atan();
    input.camera.view = glam::camera::rh::view::look_to_mat4(
        Vec3::Y,
        Vec3::new(0., -tilt.sin(), -tilt.cos()),
        Vec3::Y,
    );
    let mut sky = Sky::default();
    sky.prepare(device, &input, input.camera.eye, (true, true));
    let mut encoder = device.create_command_encoder(&Default::default());
    sky.encode_planned((queue, &mut encoder), None);
    queue.submit([encoder.finish()]);
    texels(device, queue, sky.aerial_texture().expect("aerial volume"))
}

/// The level column's texel at the end of `slice`.
fn level_texel(volume: &[[f32; 4]], slice: u32) -> [f32; 4] {
    let side = crate::shading::sky::AERIAL_SLICES;
    volume[((slice * side + side / 2 - 1) * side + side / 2) as usize]
}

/// The closed-form mean transmittance of `metres` of Earth's ground-level
/// air: Rayleigh's scattering and Mie's extinction coefficients at sea level
/// (Hillaire 2020's Earth, as Wicked Engine's ShaderInterop_Weather.h lists
/// them); ozone's tent is empty below 10 km.
fn ground_transmittance(metres: f64) -> f64 {
    let rayleigh = [5.802e-6, 13.558e-6, 33.1e-6];
    let mie = 4.440e-6;
    rayleigh
        .iter()
        .map(|r| (-(r + mie) * metres).exp())
        .sum::<f64>()
        / 3.
}

// Plausible defects: the slices' distances in kilometres where the march
// takes metres (or the reverse), slices spaced linearly where the reader
// takes them squared, a mis-scaled step, or Mie's extinction missing. The
// oracle is analytic: the closed-form transmittance of a level path through
// ground-level air (ground_transmittance), never the march, at the ends of
// slices 0, 11 and 31, which the spec's squared spacing puts at 31.25 m,
// 4.5 km and 32 km. The path rises with the planet's curvature (80 m at
// 32 km), which thins Mie's air by under 0.5 % of the optical depth; 1 % is
// the spec's agreement.
#[test]
fn aerial_volume_meets_the_closed_form_along_the_ground() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let volume = level_aerial(&device, &queue, SkyLook::default());
    for (slice, metres) in [(0, 31.25), (11, 4500.), (31, 32_000.)] {
        let held = f64::from(level_texel(&volume, slice)[3]);
        let expected = ground_transmittance(metres);
        assert!(
            (held / expected - 1.).abs() < 0.01,
            "slice {slice} ({metres} m): the volume holds {held}, the closed form {expected}"
        );
    }
}

// Plausible defects: the game's strength ignored or applied twice. The
// oracle is the contract: strength s counts each metre of view distance as s
// metres of air, so at strength 4 slice 5's end (32 km × (6/32)² = 1125 m)
// holds the closed form of 4.5 km. On this level ray the air is uniform, so
// scaling only the steps or only the sample positions gives the same
// transmittance: the test cannot tell those apart.
#[test]
fn aerial_strength_counts_each_metre_as_that_many() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let look = SkyLook {
        aerial_perspective: 4.,
        ..SkyLook::default()
    };
    let volume = level_aerial(&device, &queue, look);
    let held = f64::from(level_texel(&volume, 5)[3]);
    let expected = ground_transmittance(4500.);
    assert!(
        (held / expected - 1.).abs() < 0.01,
        "the volume holds {held}, the closed form {expected}"
    );
}
