//! Minimal proof that a game owns winit and calls SGL only to draw.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use sgl_2d::render::{PixelRect, Renderer, Sprite, SpriteBatch, TextureId};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

type ReadyRenderer = Rc<RefCell<Option<(Renderer, TextureId)>>>;

#[derive(Default)]
struct Game {
    window: Option<Arc<Window>>,
    renderer: ReadyRenderer,
    batch: SpriteBatch,
}

impl Game {
    fn install_renderer(ready: &ReadyRenderer, mut renderer: Renderer) {
        let texture = renderer
            .upload_rgba8("white pixel", 1, 1, &[255, 255, 255, 255])
            .expect("the fixed one-pixel texture is valid");
        *ready.borrow_mut() = Some((renderer, texture));
    }

    fn redraw(&mut self) {
        let mut ready = self.renderer.borrow_mut();
        let Some((renderer, texture)) = ready.as_mut() else {
            return;
        };
        self.batch.clear();
        self.batch
            .push(Sprite {
                texture: *texture,
                position: [0.0, 0.0],
                size: [0.75, 0.75],
                pivot: [0.5, 0.5],
                rotation_radians: 0.0,
                source: PixelRect::new(0, 0, 1, 1),
                tint: [0.95, 0.55, 0.15, 1.0],
                flip_x: false,
                flip_y: false,
            })
            .expect("the fixed sample sprite is valid");
        renderer
            .render(
                [0.03, 0.04, 0.07, 1.0],
                [
                    [1.0, 0.0, 0.0, 0.0],
                    [0.0, 1.0, 0.0, 0.0],
                    [0.0, 0.0, 1.0, 0.0],
                    [0.0, 0.0, 0.0, 1.0],
                ],
                &self.batch,
            )
            .expect("sample render failed");
    }
}

impl ApplicationHandler for Game {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attributes = Window::default_attributes()
            .with_title("SGL direct game")
            .with_inner_size(LogicalSize::new(960, 540));
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
            let renderer = pollster::block_on(Renderer::new(window.clone()))
                .expect("renderer creation failed");
            Self::install_renderer(&self.renderer, renderer);
            window.request_redraw();
        }

        #[cfg(target_arch = "wasm32")]
        {
            let ready = Rc::clone(&self.renderer);
            wasm_bindgen_futures::spawn_local(async move {
                let renderer = Renderer::new(window.clone())
                    .await
                    .expect("renderer creation failed");
                Self::install_renderer(&ready, renderer);
                window.request_redraw();
            });
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some((renderer, _)) = self.renderer.borrow_mut().as_mut() {
                    renderer.resize(size.width, size.height);
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
