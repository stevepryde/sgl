//! Exercise oversized swapchain resizes against wgpu's real validation, and
//! a game-supplied device request on the window path.

#[cfg(target_arch = "wasm32")]
fn main() {}

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    use std::sync::Arc;
    use winit::application::ApplicationHandler;
    use winit::event::WindowEvent;
    use winit::event_loop::{ActiveEventLoop, EventLoop};
    use winit::window::{Window, WindowId};

    fn unavailable(reason: &str) {
        let required = std::env::var("SGL_REQUIRE_GPU").is_ok_and(|v| !v.is_empty() && v != "0");
        assert!(!required, "surface test unavailable: {reason}");
        eprintln!("skipping surface test: {reason}");
    }

    /// Below wgpu's default of 8192, so every adapter can grant it.
    const LOWERED_MAX_TEXTURE_2D: u32 = 4096;

    fn request_max_texture_2d(
        max: u32,
    ) -> impl FnOnce(&wgpu::Adapter) -> wgpu::DeviceDescriptor<'static> {
        move |_| wgpu::DeviceDescriptor {
            required_limits: wgpu::Limits {
                max_texture_dimension_2d: max,
                ..wgpu::Limits::default()
            },
            ..wgpu::DeviceDescriptor::default()
        }
    }

    /// A hidden window; a window's surface binds to one context at a time.
    fn hidden_window(event_loop: &ActiveEventLoop) -> Option<Arc<Window>> {
        match event_loop.create_window(Window::default_attributes().with_visible(false)) {
            Ok(window) => Some(Arc::new(window)),
            Err(err) => {
                unavailable(&err.to_string());
                None
            }
        }
    }

    struct Test;
    impl ApplicationHandler for Test {
        fn resumed(&mut self, event_loop: &ActiveEventLoop) {
            let Some(window) = hidden_window(event_loop) else {
                event_loop.exit();
                return;
            };
            let mut ctx = match sgl_2d::canvas::Context::try_new(window, false) {
                Ok(ctx) => ctx,
                Err(err) => {
                    unavailable(&err.to_string());
                    event_loop.exit();
                    return;
                }
            };
            let limit = ctx.device.limits().max_texture_dimension_2d;
            let scope = ctx.device.push_error_scope(wgpu::ErrorFilter::Validation);
            for (width, height) in [(16384, 1080), (1080, 16384)] {
                ctx.resize(width, height);
                let configured = ctx.size();
                assert!(configured.0 <= limit && configured.1 <= limit);
                assert!(configured.0 > 0 && configured.1 > 0);
            }
            ctx.resize(640, 480);
            assert_eq!(ctx.size(), (640, 480), "ordinary resize must still apply");
            ctx.resize(0, 480);
            assert_eq!(ctx.size(), (640, 480), "minimization retains configuration");
            assert!(pollster::block_on(scope.pop()).is_none());
            drop(ctx);

            // The game's request reaches the window's device.
            let Some(window) = hidden_window(event_loop) else {
                event_loop.exit();
                return;
            };
            let ctx = sgl_2d::canvas::Context::try_new_with(
                window,
                false,
                request_max_texture_2d(LOWERED_MAX_TEXTURE_2D),
            )
            .expect("an adapter grants a lowered limit");
            assert_eq!(
                ctx.device.limits().max_texture_dimension_2d,
                LOWERED_MAX_TEXTURE_2D
            );
            drop(ctx);

            // A request the adapter cannot grant is an error, not a panic.
            let Some(window) = hidden_window(event_loop) else {
                event_loop.exit();
                return;
            };
            let refused = sgl_2d::canvas::Context::try_new_with(
                window,
                false,
                request_max_texture_2d(u32::MAX),
            );
            assert!(matches!(
                refused,
                Err(sgl_2d::canvas::RendererInitError::Device(_))
            ));
            event_loop.exit();
        }

        fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, _: WindowEvent) {}
    }
    let event_loop = match EventLoop::new() {
        Ok(event_loop) => event_loop,
        Err(err) => {
            unavailable(&err.to_string());
            return;
        }
    };
    event_loop.run_app(&mut Test).expect("run surface test");
}
