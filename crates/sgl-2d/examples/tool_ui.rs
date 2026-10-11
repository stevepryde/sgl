//! Run with `cargo run -p sgl-2d --example tool_ui`.
//! The game owns window events, pane layout, and all editable values.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use std::sync::Arc;
    use std::time::Instant;

    use sgl_2d::assets::{Assets, Texture};
    use sgl_2d::canvas::text::{HAlign, TextRenderer, TextStyle, VAlign};
    use sgl_2d::canvas::{Camera, Context, DrawList, Rect, Renderer};
    use sgl_2d::ui::{IconButton, Splitter, SplitterAxis, Ui, UiCursor, UiInput, UiKey, UiTheme};
    use sgl_core::math::Vec2;
    use winit::application::ApplicationHandler;
    use winit::dpi::LogicalSize;
    use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
    use winit::event_loop::{ActiveEventLoop, EventLoop};
    use winit::keyboard::{Key, ModifiersState, NamedKey};
    use winit::window::{CursorIcon, Window, WindowId};

    struct Demo {
        window: Arc<Window>,
        context: Context,
        renderer: Renderer,
        assets: Assets<Texture>,
        text: TextRenderer,
        ui: Ui,
        input: UiInput,
        previous: Instant,
        draft: String,
        offset: f32,
        prefab_offset: f32,
        pane: f32,
        visible: [bool; 20],
        selected: usize,
        light_scene: bool,
        light_tools: bool,
        modifiers: ModifiersState,
        grid: bool,
    }

    impl Demo {
        fn new(event_loop: &ActiveEventLoop) -> Self {
            let window = Arc::new(
                event_loop
                    .create_window(
                        Window::default_attributes()
                            .with_title("SGL tool UI")
                            .with_inner_size(LogicalSize::new(960, 640))
                            .with_min_inner_size(LogicalSize::new(540, 360)),
                    )
                    .expect("create window"),
            );
            let context = Context::new(window.clone(), true);
            let size = window.inner_size();
            let mut renderer = Renderer::new(&context, size.width, size.height, [0.04, 0.05, 0.07]);
            let mut assets = Assets::default();
            let ui = Ui::new(&mut assets);
            renderer
                .upload_texture(
                    &context,
                    ui.white_texture(),
                    assets.get(ui.white_texture()).unwrap(),
                )
                .expect("white texture uploads");
            Self {
                window,
                context,
                renderer,
                assets,
                ui,
                text: TextRenderer::new(include_bytes!(
                    "../tests/fixtures/IBMPlexSans-Regular.ttf"
                ))
                .expect("bundled font"),
                input: UiInput::default(),
                previous: Instant::now(),
                draft: "Forest room".into(),
                offset: 0.0,
                prefab_offset: 0.0,
                pane: 280.0,
                visible: [true; 20],
                selected: 0,
                light_scene: false,
                light_tools: false,
                modifiers: ModifiersState::default(),
                grid: true,
            }
        }

        #[allow(clippy::too_many_lines)]
        fn redraw(&mut self) {
            let size = self.window.inner_size();
            if size.width == 0 || size.height == 0 {
                return;
            }
            if self.context.size() != (size.width, size.height) {
                self.context.resize(size.width, size.height);
            }
            self.renderer
                .set_target_size(&self.context, size.width, size.height);
            // Derive layout and input from the same physical-to-point transform.
            let scale = self.window.scale_factor() as f32;
            self.renderer.set_ui_scale(scale);
            self.text.set_pixel_scale(self.renderer.ui_pixel_scale());
            let (width, height) = self.renderer.target_size();
            let view = self.renderer.ui_size();
            self.input.dt = self.previous.elapsed().as_secs_f32();
            self.previous = Instant::now();
            let theme = if self.light_tools {
                UiTheme::tools_light()
            } else {
                UiTheme::tools_dark()
            };
            self.ui.set_theme(theme);
            let mut list = DrawList::default();
            let mut frame = self.ui.begin(&mut self.text, &mut list, self.input.clone());
            let bounds = Rect::new(0.0, 0.0, view.x, view.y);
            // Deliberately contrasting scene bands expose translucent chrome.
            frame.rect(
                bounds,
                if self.light_scene {
                    [0.92, 0.92, 0.84, 1.0]
                } else {
                    [0.025, 0.035, 0.06, 1.0]
                },
            );
            for x in (0..view.x as u32).step_by(48).filter(|_| self.grid) {
                frame.rect(
                    Rect::new(x as f32, 0.0, 1.0, view.y),
                    [0.45, 0.5, 0.55, 0.35],
                );
            }
            self.pane = self.pane.max(220.0).min((view.x - 240.0).min(440.0));
            let style = TextStyle::new(16.0, theme.text);
            // Measure actions and wrap the toolbar when its row fills.
            frame.panel(Rect::new(0.0, 0.0, view.x, 46.0));
            let mut x = 12.0;
            let mut y = 8.0;
            for (id, label) in [
                ("scene", "Bright / dark scene"),
                ("theme", "Light / dark tools"),
            ] {
                let action_w = frame.measure_text(label, 15.0).x + 24.0;
                if x + action_w > view.x - 12.0 {
                    x = 12.0;
                    y += 38.0;
                    frame.panel(Rect::new(0.0, y - 8.0, view.x, 46.0));
                }
                if frame.button(id, Rect::new(x, y, action_w, 30.0), label, 15.0) {
                    if id == "scene" {
                        self.light_scene = !self.light_scene;
                    } else {
                        self.light_tools = !self.light_tools;
                    }
                }
                x += action_w + 8.0;
            }
            for (id, glyph, help, disabled) in [
                (
                    "grid",
                    "#",
                    "Toggle grid (G when tools are not focused)",
                    false,
                ),
                (
                    "undo",
                    "<",
                    "Undo unavailable: this demo has no edit history",
                    true,
                ),
            ] {
                if x + 30.0 > view.x - 12.0 {
                    x = 12.0;
                    y += 38.0;
                    frame.panel(Rect::new(0.0, y - 8.0, view.x, 46.0));
                }
                let hit = Rect::new(x, y, 30.0, 30.0);
                if frame.icon_button(
                    id,
                    hit,
                    glyph,
                    16.0,
                    IconButton {
                        selected: id == "grid" && self.grid,
                        disabled,
                    },
                ) {
                    self.grid = !self.grid;
                }
                frame.tooltip_for(id, hit, bounds, help, 14.0);
                x += 38.0;
            }
            let top = y + 42.0;
            let resize = frame.splitter(
                "sidebar",
                Rect::new(self.pane, top, 8.0, view.y - top),
                &mut self.pane,
                Splitter {
                    axis: SplitterAxis::Horizontal,
                    min: 220.0,
                    max: (view.x - 240.0).min(440.0),
                },
            );
            frame.rect(
                Rect::new(self.pane, top, 8.0, view.y - top),
                theme.scroll_thumb,
            );
            frame.panel(Rect::new(0.0, top, self.pane, view.y - top));
            frame.label(
                Rect::new(12.0, top + 8.0, self.pane - 24.0, 28.0),
                "Room name",
                &style,
                HAlign::Left,
                VAlign::Center,
            );
            frame.line_edit(
                "room",
                Rect::new(12.0, top + 40.0, self.pane - 24.0, 30.0),
                &mut self.draft,
                50,
                16.0,
            );
            frame.label(
                Rect::new(12.0, top + 80.0, self.pane - 24.0, 28.0),
                "Layers",
                &style,
                HAlign::Left,
                VAlign::Center,
            );
            frame.label(
                Rect::new(self.pane - 80.0, top + 80.0, 56.0, 28.0),
                "Visible",
                &style,
                HAlign::Right,
                VAlign::Center,
            );
            let scroll_y = top + 116.0;
            frame.scroll_area_begin(
                "layers",
                Rect::new(12.0, scroll_y, self.pane - 24.0, view.y - scroll_y - 12.0),
                726.0,
                &mut self.offset,
            );
            for index in 0..20 {
                // Selection/focus borders extend outside the hit rect. Keep
                // three points inside the clip, plus a gap before the scrollbar.
                let row_y = scroll_y + 3.0 + index as f32 * 36.0 - self.offset;
                // Reserve trailing hit areas before allocating the label width.
                frame.radio(
                    &format!("layer-{index}"),
                    Rect::new(15.0, row_y, self.pane - 75.0, 32.0),
                    &format!("Layer {}", index + 1),
                    16.0,
                    index,
                    &mut self.selected,
                );
                frame.checkbox(
                    &format!("visible-{index}"),
                    Rect::new(self.pane - 52.0, row_y, 28.0, 32.0),
                    "",
                    16.0,
                    &mut self.visible[index],
                );
            }
            frame.scroll_area_end(&mut self.offset);
            let content_x = self.pane + 20.0;
            let columns = ((view.x - content_x - 12.0) / 128.0).floor().max(1.0) as usize;
            frame.scroll_area_begin(
                "prefabs",
                Rect::new(
                    content_x,
                    top,
                    view.x - content_x - 12.0,
                    view.y - top - 12.0,
                ),
                6_usize.div_ceil(columns) as f32 * 84.0,
                &mut self.prefab_offset,
            );
            for index in 0..6 {
                let cell = Rect::new(
                    content_x + (index % columns) as f32 * 128.0,
                    top + (index / columns) as f32 * 84.0 - self.prefab_offset,
                    116.0,
                    72.0,
                );
                frame.panel(cell);
                frame.label(
                    cell,
                    &format!("Prefab {}", index + 1),
                    &style,
                    HAlign::Center,
                    VAlign::Center,
                );
            }
            frame.scroll_area_end(&mut self.prefab_offset);
            frame.end();
            self.window.set_cursor(match resize.cursor {
                Some(UiCursor::ResizeHorizontal) => CursorIcon::EwResize,
                Some(UiCursor::ResizeVertical) => CursorIcon::NsResize,
                None => CursorIcon::Default,
            });
            // Dispatch game shortcuts after the UI, including dismissal frames.
            if !self.ui.keyboard_captured()
                && self.input.chars.iter().any(|c| matches!(c, 'g' | 'G'))
            {
                self.grid = !self.grid;
            }
            for handle in self.text.end_frame(&mut self.assets, &mut list) {
                // A page too large for this device leaves its glyph undrawn.
                if let Err(error) = self.renderer.upload_texture(
                    &self.context,
                    handle,
                    self.assets.get(handle).unwrap(),
                ) {
                    eprintln!("glyph page not uploaded: {error}");
                }
            }
            if let Some(surface) = self.context.acquire() {
                self.renderer.render(
                    &self.context,
                    &surface,
                    &mut list,
                    &Camera::new(width, height),
                );
                surface.present();
            }
            self.input = UiInput {
                mouse_pos: self.input.mouse_pos,
                mouse_down: self.input.mouse_down,
                shift: self.modifiers.shift_key(),
                ..UiInput::default()
            };
        }
    }

    #[derive(Default)]
    struct App {
        demo: Option<Demo>,
    }

    impl ApplicationHandler for App {
        fn resumed(&mut self, event_loop: &ActiveEventLoop) {
            if self.demo.is_none() {
                self.demo = Some(Demo::new(event_loop));
            }
        }

        fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
            let Some(demo) = &mut self.demo else {
                return;
            };
            match event {
                WindowEvent::CloseRequested => event_loop.exit(),
                WindowEvent::RedrawRequested => demo.redraw(),
                WindowEvent::CursorMoved { position, .. } => {
                    let point = position.to_logical::<f32>(demo.window.scale_factor());
                    demo.input.mouse_pos = Vec2::new(point.x, point.y);
                }
                WindowEvent::MouseInput {
                    state,
                    button: MouseButton::Left,
                    ..
                } => {
                    demo.input.mouse_down = state == ElementState::Pressed;
                    demo.input.mouse_pressed |= state == ElementState::Pressed;
                    demo.input.mouse_released |= state == ElementState::Released;
                }
                WindowEvent::MouseWheel { delta, .. } => {
                    demo.input.scroll += match delta {
                        MouseScrollDelta::LineDelta(x, y) => Vec2::new(x, y),
                        MouseScrollDelta::PixelDelta(p) => {
                            Vec2::new(p.x as f32, p.y as f32)
                                / demo.window.scale_factor() as f32
                                / 40.0
                        }
                    };
                }
                WindowEvent::ModifiersChanged(modifiers) => {
                    demo.modifiers = modifiers.state();
                    demo.input.shift = demo.modifiers.shift_key();
                }
                WindowEvent::KeyboardInput { event, .. } if event.state.is_pressed() => {
                    let command = if cfg!(target_os = "macos") {
                        demo.modifiers.super_key()
                    } else {
                        demo.modifiers.control_key()
                    };
                    let key = match &event.logical_key {
                        Key::Named(NamedKey::Tab) => Some(UiKey::Tab),
                        Key::Named(NamedKey::Enter) => Some(UiKey::Enter),
                        Key::Named(NamedKey::Space) => Some(UiKey::Space),
                        Key::Named(NamedKey::Escape) => Some(UiKey::Escape),
                        Key::Named(NamedKey::ArrowLeft) => Some(UiKey::Left),
                        Key::Named(NamedKey::ArrowRight) => Some(UiKey::Right),
                        Key::Named(NamedKey::Home) => Some(UiKey::Home),
                        Key::Named(NamedKey::End) => Some(UiKey::End),
                        Key::Named(NamedKey::Delete) => Some(UiKey::Delete),
                        Key::Named(NamedKey::Backspace) => {
                            demo.input.backspace = true;
                            None
                        }
                        Key::Character(c) if command && c.eq_ignore_ascii_case("a") => {
                            Some(UiKey::SelectAll)
                        }
                        _ => None,
                    };
                    if let Some(key) = key {
                        demo.input.keys.push(key);
                    }
                    if !command
                        && !demo.modifiers.alt_key()
                        && let Some(text) = event.text
                    {
                        demo.input
                            .chars
                            .extend(text.chars().filter(|c| !c.is_control()));
                    }
                }
                WindowEvent::Focused(false) => {
                    demo.ui.cancel_interactions();
                    demo.modifiers = ModifiersState::default();
                    demo.input = UiInput::default();
                }
                _ => {}
            }
        }

        fn about_to_wait(&mut self, _: &ActiveEventLoop) {
            if let Some(demo) = &self.demo {
                demo.window.request_redraw();
            }
        }
    }

    pub fn run() -> Result<(), Box<dyn std::error::Error>> {
        EventLoop::new()?.run_app(&mut App::default())?;
        Ok(())
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    native::run()
}

#[cfg(target_arch = "wasm32")]
fn main() {}
