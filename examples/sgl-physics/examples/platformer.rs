//! A 2D character that walks, jumps, climbs a low step without jumping, and
//! stands on and drops through a one-way platform, moved by
//! `sgl_physics::move_and_slide` and drawn with `sgl-2d`. The game owns the
//! window, the input, gravity and the fixed step.
//!
//! Run with `cargo run -p sgl-physics-examples --example platformer`:
//! arrows or A/D to walk, Space, W or Up to jump, S or Down to drop through
//! the platform. `-- --headless` plays a scripted run without a window and
//! checks it; `cargo test -p sgl-physics-examples --example platformer`
//! does the same as a test.

#[cfg(not(target_arch = "wasm32"))]
mod game {
    use sgl_core::math::Vec2;
    use sgl_physics::{
        Collider, ColliderKind, ColliderSet, Contacts, MoverConfig, MoverOptions, MoverSettings,
        MoverState, Shape, move_and_slide,
    };

    /// The game's fixed step, in seconds.
    pub const DT: f32 = 1.0 / 60.0;
    const GRAVITY: f32 = 25.0;
    const RUN: f32 = 6.0;
    /// Lifts the feet 11² / (2 × 25) = 2.42, over the platform's top at 2.1.
    const JUMP: f32 = 11.0;
    /// The player: 0.7 wide, 1.4 tall.
    pub const PLAYER: Vec2 = Vec2::new(0.35, 0.7);

    pub const FLOOR: u64 = 1;
    /// A step 0.3 high, under the player's 0.35 step height.
    pub const STEP: u64 = 4;
    pub const PLATFORM: u64 = 5;

    /// The level's boxes by id, corners and kind, in world units, y up.
    pub const LEVEL: [(u64, Vec2, Vec2, ColliderKind); 5] = [
        (
            FLOOR,
            Vec2::new(-2.0, -1.0),
            Vec2::new(30.0, 0.0),
            ColliderKind::Solid,
        ),
        (
            2,
            Vec2::new(-3.0, -1.0),
            Vec2::new(-2.0, 8.0),
            ColliderKind::Solid,
        ),
        (
            3,
            Vec2::new(30.0, -1.0),
            Vec2::new(31.0, 8.0),
            ColliderKind::Solid,
        ),
        (
            STEP,
            Vec2::new(4.0, 0.0),
            Vec2::new(8.0, 0.3),
            ColliderKind::Solid,
        ),
        (
            PLATFORM,
            Vec2::new(12.0, 1.9),
            Vec2::new(16.0, 2.1),
            ColliderKind::OneWay,
        ),
    ];

    /// What the player holds this step.
    // Independent keys, held together at times: not one state.
    #[allow(clippy::struct_excessive_bools)]
    #[derive(Debug, Clone, Copy, Default)]
    pub struct Input {
        pub left: bool,
        pub right: bool,
        /// Pressed this step.
        pub jump: bool,
        pub down: bool,
    }

    pub struct Game {
        pub level: ColliderSet<Vec2>,
        pub player: MoverState<Vec2>,
        config: MoverConfig<Vec2>,
    }

    impl Game {
        pub fn new() -> Self {
            let mut level = ColliderSet::new(4.0);
            for (id, min, max, kind) in LEVEL {
                let half = (max - min) * 0.5;
                level.insert(Collider::new(id, Shape::Box { half }, min + half).with_kind(kind));
            }
            Self {
                level,
                player: MoverState {
                    position: Vec2::new(0.0, 3.0),
                    ..MoverState::default()
                },
                config: MoverConfig::new(MoverSettings {
                    up: Vec2::Y,
                    skin: 0.01,
                    snap_distance: 0.1,
                    normal_threshold: 0.7,
                    block_epsilon: 1e-4,
                }),
            }
        }

        /// One fixed step: the game sets the velocity, the mover moves.
        pub fn step(&mut self, input: Input) -> Contacts<Vec2> {
            let player = &mut self.player;
            player.velocity.x =
                (f32::from(u8::from(input.right)) - f32::from(u8::from(input.left))) * RUN;
            player.velocity.y = if input.jump && player.floor.is_some() {
                JUMP
            } else {
                player.velocity.y - GRAVITY * DT
            };
            let options = MoverOptions {
                step_height: 0.35,
                drop_through: input.down,
                ..MoverOptions::default()
            };
            move_and_slide(
                player,
                &Shape::Box { half: PLAYER },
                DT,
                &self.level,
                &self.config,
                &options,
            )
            .expect("a set always answers")
        }

        /// The id of what the player stands on.
        pub fn floor(&self) -> Option<u64> {
            self.player.floor.map(|floor| floor.id)
        }
    }

    /// Plays a scripted run and checks each outcome the requirement names:
    /// the player lands; walks onto the step without jumping; walks on
    /// under the platform; jumps up through it and lands on it; and drops
    /// through it to the floor when holding down.
    pub fn play_script() -> Result<(), String> {
        let mut game = Game::new();
        let idle = Input::default();
        let right = Input {
            right: true,
            ..Input::default()
        };
        run(&mut game, idle, 60);
        expect(&game, FLOOR, "landed on the floor")?;

        let climbed = (0..120).any(|_| game.player.position.x < 6.0 && game.step(right).stepped);
        if !climbed {
            return Err("walked into the step without climbing it".into());
        }
        while game.player.position.x < 6.0 {
            game.step(right);
        }
        expect(&game, STEP, "stood on the step")?;

        while game.player.position.x < 14.0 {
            game.step(right);
        }
        run(&mut game, idle, 30);
        expect(&game, FLOOR, "walked off the step under the platform")?;

        game.step(Input {
            jump: true,
            ..Input::default()
        });
        run(&mut game, idle, 90);
        expect(&game, PLATFORM, "jumped up through the platform onto it")?;

        let down = Input {
            down: true,
            ..Input::default()
        };
        run(&mut game, down, 15);
        run(&mut game, idle, 60);
        expect(&game, FLOOR, "dropped through the platform")
    }

    fn run(game: &mut Game, input: Input, steps: u32) {
        for _ in 0..steps {
            game.step(input);
        }
    }

    fn expect(game: &Game, floor: u64, what: &str) -> Result<(), String> {
        if game.floor() == Some(floor) {
            println!("{what}: on {floor} at {:?}", game.player.position);
            Ok(())
        } else {
            Err(format!(
                "expected to have {what} (on {floor}), but stand on {:?} at {:?}",
                game.floor(),
                game.player.position
            ))
        }
    }

    #[cfg(test)]
    mod tests {
        /// The scripted run, without a window. Oracle: the requirement for
        /// each stage (what the player stands on after it).
        #[test]
        fn the_scripted_run_climbs_lands_and_drops_through() {
            super::play_script().unwrap();
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod window {
    use std::sync::Arc;
    use std::time::Instant;

    use sgl_2d::assets::{Assets, Handle, Texture};
    use sgl_2d::canvas::{Camera, Context, DrawList, Renderer, SpriteInstance};
    use sgl_core::math::Vec2;
    use sgl_physics::ColliderKind;
    use winit::application::ApplicationHandler;
    use winit::dpi::LogicalSize;
    use winit::event::{ElementState, WindowEvent};
    use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
    use winit::keyboard::{Key, NamedKey};
    use winit::window::{Window, WindowId};

    use crate::game::{DT, Game, Input, LEVEL, PLAYER};

    const WIDTH: u32 = 960;
    const HEIGHT: u32 = 540;
    /// The view in logical pixels, as floats.
    const VIEW: Vec2 = Vec2::new(960.0, 540.0);
    /// Logical pixels per world unit.
    const SCALE: f32 = 30.0;

    struct Graphics {
        window: Arc<Window>,
        context: Context,
        renderer: Renderer,
        /// The game's texture cache: the renderer keys uploads by handle.
        _textures: Assets<Texture>,
        white: Handle<Texture>,
    }

    struct App {
        graphics: Option<Graphics>,
        game: Game,
        held: Input,
        jump: bool,
        clock: Instant,
        lag: f32,
        draw: DrawList,
    }

    impl App {
        fn new() -> Self {
            Self {
                graphics: None,
                game: Game::new(),
                held: Input::default(),
                jump: false,
                clock: Instant::now(),
                lag: 0.0,
                draw: DrawList::default(),
            }
        }

        fn key(&mut self, key: &Key, pressed: bool) {
            match key {
                Key::Named(NamedKey::ArrowLeft) => self.held.left = pressed,
                Key::Named(NamedKey::ArrowRight) => self.held.right = pressed,
                Key::Named(NamedKey::ArrowDown) => self.held.down = pressed,
                Key::Named(NamedKey::ArrowUp | NamedKey::Space) => self.jump |= pressed,
                Key::Character(c) => match c.to_ascii_lowercase().as_str() {
                    "a" => self.held.left = pressed,
                    "d" => self.held.right = pressed,
                    "s" => self.held.down = pressed,
                    "w" => self.jump |= pressed,
                    _ => {}
                },
                _ => {}
            }
        }

        fn redraw(&mut self) {
            // A fixed step, as many times as real time has passed (at most
            // five a frame), a jump press consumed by the first.
            self.lag = (self.lag + self.clock.elapsed().as_secs_f32()).min(5.0 * DT);
            self.clock = Instant::now();
            while self.lag >= DT {
                self.game.step(Input {
                    jump: self.jump,
                    ..self.held
                });
                self.jump = false;
                self.lag -= DT;
            }

            let Some(graphics) = &mut self.graphics else {
                return;
            };
            self.draw.clear();
            // World units, y up, to logical pixels, y down, with the
            // camera following the player across.
            let follow = self.game.player.position.x;
            let to_screen = |center: Vec2| {
                Vec2::new(
                    (center.x - follow) * SCALE + VIEW.x * 0.5,
                    VIEW.y - 60.0 - center.y * SCALE,
                )
            };
            let mut quad = |center: Vec2, half: Vec2, color: [f32; 4]| {
                self.draw.push(SpriteInstance {
                    scale: half * (2.0 * SCALE),
                    color,
                    ..SpriteInstance::new(graphics.white, to_screen(center))
                });
            };
            for (_, min, max, kind) in LEVEL {
                let color = match kind {
                    ColliderKind::OneWay => [0.55, 0.75, 0.95, 1.0],
                    _ => [0.35, 0.38, 0.45, 1.0],
                };
                quad((min + max) * 0.5, (max - min) * 0.5, color);
            }
            quad(self.game.player.position, PLAYER, [0.98, 0.77, 0.42, 1.0]);
            let Some(frame) = graphics.context.acquire() else {
                return;
            };
            graphics.renderer.render(
                &graphics.context,
                &frame,
                &mut self.draw,
                &Camera::new(WIDTH, HEIGHT),
            );
            graphics.window.pre_present_notify();
            frame.present();
        }
    }

    impl ApplicationHandler for App {
        fn resumed(&mut self, event_loop: &ActiveEventLoop) {
            if self.graphics.is_some() {
                return;
            }
            let window = Arc::new(
                event_loop
                    .create_window(
                        Window::default_attributes()
                            .with_title("SGL platformer")
                            .with_inner_size(LogicalSize::new(WIDTH, HEIGHT)),
                    )
                    .expect("create window"),
            );
            let context = Context::new(window.clone(), true);
            let mut renderer = Renderer::new(&context, WIDTH, HEIGHT, [0.08, 0.09, 0.12]);
            let mut textures = Assets::new();
            let white = renderer.white_texture(&context, &mut textures);
            self.clock = Instant::now();
            self.graphics = Some(Graphics {
                window,
                context,
                renderer,
                _textures: textures,
                white,
            });
        }

        fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
            match event {
                WindowEvent::CloseRequested => event_loop.exit(),
                WindowEvent::Resized(size) => {
                    if let Some(graphics) = &mut self.graphics {
                        graphics.context.resize(size.width, size.height);
                    }
                }
                WindowEvent::KeyboardInput { event, .. } if !event.repeat => {
                    self.key(&event.logical_key, event.state == ElementState::Pressed);
                }
                WindowEvent::RedrawRequested => self.redraw(),
                _ => {}
            }
        }

        fn about_to_wait(&mut self, _: &ActiveEventLoop) {
            if let Some(graphics) = &self.graphics {
                graphics.window.request_redraw();
            }
        }
    }

    pub fn run() -> Result<(), Box<dyn std::error::Error>> {
        let event_loop = EventLoop::new()?;
        event_loop.set_control_flow(ControlFlow::Poll);
        event_loop.run_app(&mut App::new())?;
        Ok(())
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::args().any(|arg| arg == "--headless") {
        return game::play_script().map_err(Into::into);
    }
    window::run()
}

#[cfg(target_arch = "wasm32")]
fn main() {}
