//! The HDR effect's pass on real frames: what it keeps of what a game's
//! effect returns. Its resources and compilation are tested with the
//! renderer's (`renderer::compile_tests`), and what it computes, frame by
//! frame, in the scenario suite (`tests/sgl-3d/tests/scenario_hdr_effect.rs`).
use crate::renderer::test_frame::TestRenderer;
use crate::settings::{self, Settings};
use crate::shading::gbuffer;
use crate::{Backdrop, Camera, FrameInput, Scene, ShaderSource, test_support};
use glam::{Mat4, Vec3};

// Plausible defect: an effect's negative, NaN or overflowing result
// reaching the stages after it unbounded, where bloom's downsample and
// depth of field's gather spread one such pixel over its neighbours and
// the tone map turns NaN into black or garbage. The oracle is the HDR
// effect contract: the frame keeps each channel within 0..SCENE_COLOR_MAX,
// binary16's largest finite value, and a channel that is negative or not
// finite reads 0. The NaN is made at run time from the pixel, so no
// compiler folds it.
#[test]
fn an_effects_result_is_bounded() {
    let Some((device, queue)) = test_support::device() else {
        return;
    };
    let size = [32u32, 16];
    let settings = Settings {
        antialiasing: settings::Antialiasing::Off,
        bloom: settings::Bloom::Off,
        ..Settings::default()
    };
    let mut renderer = TestRenderer::for_test(&device, &queue, size, &settings);
    let mut scene = Scene::new(&device, &queue);
    let effect = scene
        .add_hdr_effect(ShaderSource {
            wgsl: "struct EffectParams { unused:vec4<f32>, }
fn hdr_effect(color:vec3<f32>,ctx:EffectContext,params:EffectParams)->vec3<f32> {
 let nan=bitcast<f32>(0x7fc00000u+u32(max(ctx.uv.x,0.)*0.));
 return vec3(-1.-ctx.uv.x,nan,1e30*(1.+ctx.uv.y));
}"
            .into(),
            label: "unbounded".into(),
        })
        .unwrap();
    let mut input = FrameInput::new(Camera {
        view: Mat4::IDENTITY,
        projection: crate::perspective(1., 2., 0.1),
        eye: Vec3::ZERO,
    });
    input.backdrop = Backdrop::Color([0.5; 3]);
    input.hdr_effect = Some(scene.hdr_effect(effect, &[0; 16]).unwrap());
    let output = crate::view::targets::target(&device, "effect output", size, gbuffer::COLOR);
    let mut encoder = device.create_command_encoder(&Default::default());
    renderer.render(
        &device,
        &queue,
        &mut encoder,
        &mut scene,
        &input,
        &settings,
        &output,
        None,
    );
    queue.submit([encoder.finish()]);
    renderer.finish_frame(&mut scene);
    let bytes = test_support::read(
        &device,
        &queue,
        renderer.view.hdr_effect.output().expect("it ran").texture(),
        8,
    );
    for texel in bytes.chunks_exact(8) {
        let rgb: [f32; 3] = std::array::from_fn(|c| test_support::half(&texel[c * 2..]));
        assert_eq!(rgb, [0., 0., 65504.]);
    }
}
