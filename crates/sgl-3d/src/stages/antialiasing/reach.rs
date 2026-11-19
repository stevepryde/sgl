//! TAA's motion reach (Frame item 11): for each 32-pixel tile of the frame,
//! the largest motion, in render pixels a frame, in its 3x3 tile
//! neighbourhood, which TAA takes as the least speed of the tile's pixels
//! for its rejection and recovery hold, so a pixel that a surface moving up
//! to a tile a frame has just left is rejected whatever moved it. Its
//! passes are motion blur's tile and neighbourhood maximum (Wicked Engine's,
//! after McGuire et al. 2012), the shared `motion_tiles` module run by
//! `view::motion_tiles` on its own targets: motion blur runs them after
//! antialiasing at the scene size on blur vectors its shutter scales, this
//! before TAA at the render size on the surface's motion in pixels.
//!
//! Reads: the surface's motion (the Surface contract). Writes: its tile
//! targets, 1/32 of the render size, held while TAA runs. Timing group:
//! `TAA motion reach`.
use crate::shading::motion_tiles::{MOTION_TILES, MotionTilesUniform};
use crate::view::frame::FrameContext;
use crate::view::motion_tiles::MotionTiles;

pub(crate) struct Reach {
    tiles: MotionTiles,
}

impl Reach {
    pub fn new(device: &wgpu::Device) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("TAA motion reach"),
            source: wgpu::ShaderSource::Wgsl(crate::shading::compose(&[&MOTION_TILES]).into()),
        });
        Self {
            tiles: MotionTiles::new(device, &module),
        }
    }

    /// Drops the bind groups, which hold the shared motion target.
    pub fn forget_inputs(&mut self) {
        self.tiles.forget_inputs();
    }

    #[cfg(all(test, not(target_arch = "wasm32")))]
    pub fn cached_groups(&self) -> usize {
        self.tiles.cached_groups()
    }

    /// The frame's motion reach, in `motion_tiles::TILE_SIZE` tiles of the
    /// render size: each tile neighbourhood's largest motion in render
    /// pixels a frame (xy; its smallest in zw).
    pub fn encode(&mut self, ctx: &mut FrameContext<'_>) -> wgpu::TextureView {
        let motion = &ctx.targets.motion;
        // The surface's motion is current minus previous UV: times the
        // render size, pixels a frame, which the tiles cover one for one.
        let values = MotionTilesUniform {
            velocity_scale: ctx.sizes.render.map(|v| v as f32),
            render_scale: [1.; 2],
        };
        let mut pass = ctx
            .encoder
            .begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("TAA motion reach"),
                timestamp_writes: ctx.timing.and_then(|t| t.compute_pass("TAA motion reach")),
            });
        self.tiles
            .encode(ctx.device, ctx.queue, &mut pass, [motion, motion], values)
            .clone()
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use crate::renderer::test_frame::TestRenderer;
    use crate::settings::{self, Antialiasing, RenderPreset, Settings};
    use crate::{Camera, FrameInput, Scene, test_support};

    // Defect: the reach keeps a bind group set for every motion target it
    // has bound, so a resize that rebuilds the shared targets at the same
    // size (here only bloom's targets change, with the preset) keeps the
    // old render-size motion target and its groups alive. The oracle is the
    // groups the reach retains: one set, for the motion target it binds
    // now, after each such resize.
    #[test]
    fn a_resize_leaves_the_reach_one_group_set() {
        let Some((device, queue)) = test_support::device() else {
            return;
        };
        let size = [64, 48];
        let high = Settings {
            antialiasing: Antialiasing::Taa,
            scene_resolution: settings::SceneResolution::Full,
            preset: RenderPreset::High,
            ..Settings::default()
        };
        let low = Settings {
            preset: RenderPreset::Low,
            ..high
        };
        let mut renderer = TestRenderer::for_test(&device, &queue, size, &high);
        let mut scene = Scene::new(&device, &queue);
        test_support::add_static(&device, &queue, &mut scene, test_support::cube());
        let output = crate::view::targets::target(
            &device,
            "reach groups frame",
            size,
            crate::shading::gbuffer::COLOR,
        );
        let input = FrameInput::new(Camera {
            eye: glam::Vec3::new(0., 0., 4.),
            view: glam::Mat4::from_translation(glam::Vec3::new(0., 0., -4.)),
            projection: crate::perspective(1., size[0] as f32 / size[1] as f32, 0.1),
        });
        for settings in [high, low, high, low] {
            renderer.view.resize(&device, size, 1., &settings);
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
            assert_eq!(renderer.reach_groups(), Some(1), "{:?}", settings.preset);
        }
    }
}
