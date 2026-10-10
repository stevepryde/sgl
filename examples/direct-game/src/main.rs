//! Minimal proof that a game owns winit and calls SGL only to draw.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use sgl_2d::assets::{Assets, Handle, Texture};
use sgl_2d::canvas::{Camera, Context, DrawList, Renderer, SpriteInstance};
use sgl_core::math::{UVec2, Vec2};
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
    /// The game's one texture cache. The renderer keys uploads by handle, so
    /// every texture it draws comes from this cache.
    textures: Assets<Texture>,
    white: Handle<Texture>,
    checker: Handle<Texture>,
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
        let mut textures = Assets::new();
        let white = renderer.white_texture(&context, &mut textures);
        // A game loads its textures into the same cache, for example through
        // `AssetServer::load_texture`; this one is generated in place.
        let checker = textures.insert(PathBuf::from("direct-game://checker"), checker_texture());
        let pixels = textures.get(checker).expect("this cache issued the handle");
        renderer
            .upload_texture(&context, checker, pixels)
            .expect("the checker texture is valid");
        *ready.borrow_mut() = Some(Graphics {
            context,
            renderer,
            textures,
            white,
            checker,
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
            ..SpriteInstance::new(graphics.white, camera.center - Vec2::new(130.0, 0.0))
        });
        // Scale multiplies the texture's size; read it from the cache.
        let texels = graphics
            .textures
            .get(graphics.checker)
            .map_or(Vec2::ONE, |texture| {
                UVec2::new(texture.width, texture.height).as_vec2()
            });
        self.draw.push(SpriteInstance {
            scale: Vec2::splat(200.0) / texels,
            ..SpriteInstance::new(graphics.checker, camera.center + Vec2::new(130.0, 0.0))
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

/// An 8×8 two-tone checkerboard.
fn checker_texture() -> Texture {
    const SIDE: u32 = 8;
    let rgba = (0..SIDE * SIDE)
        .flat_map(|i| {
            if (i % SIDE + i / SIDE).is_multiple_of(2) {
                [40, 120, 200, 255]
            } else {
                [235, 240, 245, 255]
            }
        })
        .collect();
    Texture {
        width: SIDE,
        height: SIDE,
        rgba,
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
