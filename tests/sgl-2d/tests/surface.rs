//! Exercise oversized swapchain resizes against wgpu's real validation, a
//! game-supplied device request on the window path, and a game-supplied
//! scene with a HUD over it, read back and blitted to the swapchain.

#[cfg(target_arch = "wasm32")]
fn main() {}

#[cfg(not(target_arch = "wasm32"))]
fn unavailable(reason: &str) {
    let required = std::env::var("SGL_REQUIRE_GPU").is_ok_and(|v| !v.is_empty() && v != "0");
    assert!(!required, "surface test unavailable: {reason}");
    eprintln!("skipping surface test: {reason}");
}

/// #926 on a real window: the game renders into `scene_view`,
/// `render_screen` draws the HUD over it, and `read_scene` returns that
/// image before any surface frame exists; a resize to another aspect,
/// `blit_scene` and `present` raise no validation error and leave the
/// canvas as composed.
#[cfg(not(target_arch = "wasm32"))]
fn supplied_scene_with_hud(window: std::sync::Arc<winit::window::Window>) {
    use sgl_2d::assets::Assets;
    use sgl_2d::canvas::{Context, DrawList, Overlay, Rect, Renderer};

    /// The canvas size; its 2:1 aspect differs from the square surface the
    /// blit fills.
    const CANVAS: (u32, u32) = (64, 32);
    /// The scene the test clears into the canvas itself, `(0.2, 0.4, 0.6)`:
    /// exact in 8 bits, and not the renderer's red clear.
    const SUPPLIED: [u8; 4] = [51, 102, 153, 255];
    /// The HUD fill's colour, opaque green, over columns 8..28, rows 4..16.
    const HUD: [u8; 4] = [0, 255, 0, 255];

    let mut ctx = Context::try_new(window, false).expect("a context on this adapter came up");
    let mut renderer = Renderer::new(&ctx, CANVAS.0, CANVAS.1, [1.0, 0.0, 0.0]);
    let mut assets = Assets::new();
    let white = renderer.white_texture(&ctx, &mut assets);
    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("supplied scene"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: renderer.scene_view(),
            resolve_target: None,
            depth_slice: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color {
                    r: 0.2,
                    g: 0.4,
                    b: 0.6,
                    a: 1.0,
                }),
                store: wgpu::StoreOp::Store,
            },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
    ctx.queue.submit(Some(encoder.finish()));
    let mut list = DrawList::new();
    Overlay {
        color: [0.0, 1.0, 0.0, 1.0],
        ..Overlay::new(white)
    }
    .fill_rect(&mut list.screen, Rect::new(8.0, 4.0, 20.0, 12.0));
    renderer.render_screen(&ctx, &mut list);

    let assert_composed = |pixels: Vec<u8>, when: &str| {
        for (i, px) in pixels.chunks_exact(4).enumerate() {
            let (x, y) = (i % CANVAS.0 as usize, i / CANVAS.0 as usize);
            let inside = (8..28).contains(&x) && (4..16).contains(&y);
            let expected = if inside { HUD } else { SUPPLIED };
            assert_eq!(px, expected, "{when}: ({x}, {y})");
        }
    };
    assert_composed(
        renderer
            .read_scene(&ctx)
            .expect("readback before any frame"),
        "before any surface frame",
    );

    let scope = ctx.device.push_error_scope(wgpu::ErrorFilter::Validation);
    ctx.resize(96, 96);
    let frame = ctx.acquire();
    let presented = frame.is_some();
    if let Some(frame) = frame {
        renderer.blit_scene(&ctx, &frame);
        frame.present();
    }
    assert!(pollster::block_on(scope.pop()).is_none());
    if !presented {
        let reason = "the hidden window gave no swapchain frame";
        if cfg!(target_os = "macos") {
            // wgpu's Metal backend reports any window that is not
            // visible on screen as occluded and hands out no frame.
            eprintln!("skipping the blit and present: {reason} (macOS occlusion)");
        } else {
            unavailable(reason);
        }
        return;
    }
    assert_composed(
        renderer.read_scene(&ctx).expect("readback after the blit"),
        "after the blit",
    );
}

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    use std::sync::Arc;
    use winit::application::ApplicationHandler;
    use winit::event::WindowEvent;
    use winit::event_loop::{ActiveEventLoop, EventLoop};
    use winit::window::{Window, WindowId};

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

            if let Some(window) = hidden_window(event_loop) {
                supplied_scene_with_hud(window);
            }
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
