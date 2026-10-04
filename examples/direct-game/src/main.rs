//! Minimal proof that a game owns winit and calls SGL only to draw.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use sgl_2d::assets::{Assets, Handle, Texture};
use sgl_2d::canvas::{Camera, Context, DrawList, Renderer, SpriteInstance};
use sgl_core::math::Vec2;
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

/// The logical resolution the scene renders at before the letterbox blit.
const LOGICAL_WIDTH: u32 = 960;
const LOGICAL_HEIGHT: u32 = 540;

struct Graphics {
    context: Context,
    renderer: Renderer,
    white: Handle<Texture>,
}

type ReadyGraphics = Rc<RefCell<Option<Graphics>>>;

#[derive(Default)]
struct Game {
    window: Option<Arc<Window>>,
    graphics: ReadyGraphics,
    draw: DrawList,
}

impl Game {
    fn install_graphics(ready: &ReadyGraphics, context: Context) {
        let mut renderer = Renderer::new(&context, LOGICAL_WIDTH, LOGICAL_HEIGHT, [0.2, 0.22, 0.3]);
        let white = renderer.white_texture(&context, &mut Assets::default());
        *ready.borrow_mut() = Some(Graphics {
            context,
            renderer,
            white,
        });
    }

    fn redraw(&mut self) {
        let mut ready = self.graphics.borrow_mut();
        let (Some(window), Some(graphics)) = (&self.window, ready.as_mut()) else {
            return;
        };
        let camera = Camera::new(LOGICAL_WIDTH, LOGICAL_HEIGHT);
        self.draw.clear();
        self.draw.push(SpriteInstance {
            // The white texture is 1×1, so scale is the size in logical pixels.
            scale: Vec2::splat(200.0),
            color: [0.98, 0.77, 0.42, 1.0],
            ..SpriteInstance::new(graphics.white, camera.center)
        });
        let Some(frame) = graphics.context.acquire() else {
            return;
        };
        graphics
            .renderer
            .render(&graphics.context, &frame, &mut self.draw, &camera);
        window.pre_present_notify();
        frame.present();
    }
}

impl ApplicationHandler for Game {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attributes = Window::default_attributes()
            .with_title("SGL direct game")
            .with_inner_size(LogicalSize::new(LOGICAL_WIDTH, LOGICAL_HEIGHT));
        #[cfg(target_arch = "wasm32")]
        let attributes = {
            use winit::platform::web::WindowAttributesExtWebSys;
            attributes.with_append(true)
        };
        let window = Arc::new(
            event_loop
                .create_window(attributes)
                .expect("window creation failed"),
        );
        self.window = Some(window.clone());

        #[cfg(not(target_arch = "wasm32"))]
        {
            Self::install_graphics(&self.graphics, Context::new(window.clone(), true));
            window.request_redraw();
        }

        #[cfg(target_arch = "wasm32")]
        {
            let ready = Rc::clone(&self.graphics);
            wasm_bindgen_futures::spawn_local(async move {
                let context = Context::new_async(window.clone(), true).await;
                Self::install_graphics(&ready, context);
                window.request_redraw();
            });
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(graphics) = self.graphics.borrow_mut().as_mut() {
                    graphics.context.resize(size.width, size.height);
                }
            }
            WindowEvent::RedrawRequested => self.redraw(),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Poll);
    event_loop.run_app(&mut Game::default())?;
    Ok(())
}

#[cfg(target_arch = "wasm32")]
fn main() {
    console_error_panic_hook::set_once();
    let event_loop = EventLoop::new().expect("event loop creation failed");
    event_loop.set_control_flow(ControlFlow::Poll);
    use winit::platform::web::EventLoopExtWebSys;
    event_loop.spawn_app(Game::default());
}
