//! Run `cargo run -p sgl-input --example controllers` and focus the window.
//! Press buttons / move sticks, disconnect and reconnect. Events are printed
//! without requiring a renderer, Steam, or game-specific mappings.
use sgl_input::Gamepads;
use std::time::Duration;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

#[derive(Default)]
struct Demo {
    window: Option<Window>,
    input: Option<Gamepads>,
}
impl ApplicationHandler for Demo {
    fn resumed(&mut self, events: &ActiveEventLoop) {
        if self.window.is_none() {
            self.window = Some(
                events
                    .create_window(Window::default_attributes().with_title("SGL controller input"))
                    .unwrap(),
            );
            self.input = Some(Gamepads::new().expect("initialize controller input"));
        }
    }
    fn window_event(&mut self, events: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        if event == WindowEvent::CloseRequested {
            events.exit();
        }
    }
    fn about_to_wait(&mut self, events: &ActiveEventLoop) {
        if let (Some(input), Some(window)) = (&mut self.input, &self.window) {
            for event in input.poll() {
                println!("{event:?}");
                window.set_title(&format!("SGL controller input: {:?}", event.event));
            }
        }
        events.set_control_flow(ControlFlow::wait_duration(Duration::from_millis(16)));
    }
}
fn main() {
    EventLoop::new()
        .unwrap()
        .run_app(&mut Demo::default())
        .unwrap();
}
