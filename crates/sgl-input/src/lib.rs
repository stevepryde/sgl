//! Caller-polled controller input. Games own the window/event loop, focus,
//! bindings, active-player selection and menu behavior.
//!
//! Construct and poll on the window's main thread. macOS uses Apple's Game
//! Controller framework; other targets use Gilrs. See the `controllers` example.
use std::collections::{BTreeMap, HashMap, HashSet};

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod macos;
#[cfg(target_os = "macos")]
use macos::Backend;
#[cfg(not(target_os = "macos"))]
mod portable;
#[cfg(not(target_os = "macos"))]
use portable::Backend;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GamepadId(pub(crate) usize);

/// Face buttons are named by position: South is Xbox A / `PlayStation` Cross.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Button {
    South,
    East,
    West,
    North,
    DPadUp,
    DPadDown,
    DPadLeft,
    DPadRight,
    LeftTrigger,
    RightTrigger,
    LeftTrigger2,
    RightTrigger2,
    LeftThumb,
    RightThumb,
    Select,
    Start,
    Mode,
}

/// Stick axes range from -1 to 1. Positive Y always points up.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Axis {
    LeftStickX,
    LeftStickY,
    RightStickX,
    RightStickY,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum EventType {
    Connected,
    Disconnected,
    ButtonPressed(Button),
    ButtonReleased(Button),
    AxisChanged(Axis, f32),
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Event {
    pub id: GamepadId,
    pub event: EventType,
}

#[derive(Debug)]
pub struct Gamepad {
    name: String,
    pressed: HashSet<Button>,
    axes: HashMap<Axis, f32>,
}
impl Gamepad {
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
    #[must_use]
    pub fn is_pressed(&self, button: Button) -> bool {
        self.pressed.contains(&button)
    }
    #[must_use]
    pub fn value(&self, axis: Axis) -> f32 {
        self.axes.get(&axis).copied().unwrap_or(0.0)
    }
}

pub(crate) enum Update {
    Connected(GamepadId, String),
    Input(Event),
}

#[derive(Default)]
struct State {
    pads: BTreeMap<GamepadId, Gamepad>,
}
impl State {
    fn apply(&mut self, update: Update) -> Option<Event> {
        let event = match update {
            Update::Connected(id, name) => {
                self.pads.insert(
                    id,
                    Gamepad {
                        name,
                        pressed: HashSet::new(),
                        axes: HashMap::new(),
                    },
                );
                return Some(Event {
                    id,
                    event: EventType::Connected,
                });
            }
            Update::Input(event) => event,
        };
        if event.event == EventType::Disconnected {
            self.pads.remove(&event.id)?;
        } else {
            // Callbacks already in flight for a removed controller are ignored.
            let pad = self.pads.get_mut(&event.id)?;
            match event.event {
                EventType::ButtonPressed(button) => {
                    pad.pressed.insert(button);
                }
                EventType::ButtonReleased(button) => {
                    pad.pressed.remove(&button);
                }
                EventType::AxisChanged(axis, value) => {
                    pad.axes.insert(axis, value);
                }
                _ => {}
            }
        }
        Some(event)
    }
}

pub struct Gamepads {
    backend: Backend,
    state: State,
}
impl Gamepads {
    pub fn new() -> Result<Self, String> {
        Ok(Self {
            backend: Backend::new()?,
            state: State::default(),
        })
    }
    /// Drain ordered events and update current state. Poll even while unfocused
    /// so disconnects/releases are consumed; gate game actions on window focus.
    /// Press/release pairs remain in the event list even within a single poll.
    pub fn poll(&mut self) -> Vec<Event> {
        self.backend
            .poll()
            .into_iter()
            .filter_map(|event| self.state.apply(event))
            .collect()
    }
    #[must_use]
    pub fn gamepad(&self, id: GamepadId) -> Option<&Gamepad> {
        self.state.pads.get(&id)
    }
    pub fn gamepads(&self) -> impl Iterator<Item = (GamepadId, &Gamepad)> {
        self.state.pads.iter().map(|(&id, pad)| (id, pad))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quick_taps_survive_polling_and_disconnect_clears_held_state() {
        let mut state = State::default();
        let id = GamepadId(0);
        state.apply(Update::Connected(id, "pad".into()));
        let events: Vec<_> = [
            EventType::ButtonPressed(Button::South),
            EventType::ButtonReleased(Button::South),
        ]
        .into_iter()
        .filter_map(|event| state.apply(Update::Input(Event { id, event })))
        .collect();
        assert_eq!(
            events.len(),
            2,
            "a complete tap between frames must remain actionable"
        );
        assert!(!state.pads[&id].is_pressed(Button::South));
        state.apply(Update::Input(Event {
            id,
            event: EventType::ButtonPressed(Button::DPadLeft),
        }));
        assert!(state.pads[&id].is_pressed(Button::DPadLeft));
        state.apply(Update::Input(Event {
            id,
            event: EventType::Disconnected,
        }));
        assert!(state.pads.is_empty());
        assert!(
            state
                .apply(Update::Input(Event {
                    id,
                    event: EventType::ButtonPressed(Button::South)
                }))
                .is_none()
        );
        let new_id = GamepadId(1);
        state.apply(Update::Connected(new_id, "pad".into()));
        assert!(!state.pads[&new_id].is_pressed(Button::DPadLeft));
    }
}
