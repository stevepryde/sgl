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

/// Controller axes.
///
/// Stick axes (`LeftStickX`, `LeftStickY`, `RightStickX`, `RightStickY`) range
/// from `-1.0..=1.0` with positive Y always pointing up. Trigger axes
/// (`LeftTrigger`, `RightTrigger`) range from `0.0..=1.0`, where `0.0` is
/// released and `1.0` is fully pressed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Axis {
    LeftStickX,
    LeftStickY,
    RightStickX,
    RightStickY,
    LeftTrigger,
    RightTrigger,
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

/// Clamp analog trigger readings to `0.0..=1.0`, replacing non-finite values
/// with `0.0` (released).
pub(crate) fn sanitize_trigger(value: f32) -> f32 {
    if value.is_finite() {
        let clamped = value.clamp(0.0, 1.0);
        if clamped == 0.0 { 0.0 } else { clamped }
    } else {
        0.0
    }
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
                    // A press of a held button (the seeded state and the
                    // handler both reporting one press on macOS) is dropped.
                    if !pad.pressed.insert(button) {
                        return None;
                    }
                }
                EventType::ButtonReleased(button) => {
                    // A release with no reported press (held when the pad
                    // connected on Gilrs) never reaches the game unmatched.
                    if !pad.pressed.remove(&button) {
                        return None;
                    }
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

    /// #317: a release with no reported press (a button held when the pad
    /// connected) yields no event and changes no state; a matched release
    /// still reports.
    #[test]
    fn an_unmatched_release_is_dropped() {
        let mut state = State::default();
        let id = GamepadId(0);
        state.apply(Update::Connected(id, "pad".into()));
        state.apply(Update::Input(Event {
            id,
            event: EventType::ButtonPressed(Button::East),
        }));
        let release = |button| {
            Update::Input(Event {
                id,
                event: EventType::ButtonReleased(button),
            })
        };
        assert!(state.apply(release(Button::South)).is_none());
        assert!(state.pads[&id].is_pressed(Button::East));
        assert!(!state.pads[&id].is_pressed(Button::South));
        assert!(state.apply(release(Button::East)).is_some());
        assert!(!state.pads[&id].is_pressed(Button::East));
    }

    /// #531: a press reported twice without a release in between (macOS
    /// seeding held state as its handler fires) reaches the game once, so
    /// presses and releases stay paired (specs/input.md).
    #[test]
    fn a_repeated_press_is_reported_once() {
        let mut state = State::default();
        let id = GamepadId(0);
        state.apply(Update::Connected(id, "pad".into()));
        let events: Vec<_> = [
            EventType::ButtonPressed(Button::South),
            EventType::ButtonPressed(Button::South),
            EventType::ButtonReleased(Button::South),
        ]
        .into_iter()
        .filter_map(|event| state.apply(Update::Input(Event { id, event })))
        .map(|event| event.event)
        .collect();
        assert_eq!(
            events,
            [
                EventType::ButtonPressed(Button::South),
                EventType::ButtonReleased(Button::South),
            ]
        );
        assert!(!state.pads[&id].is_pressed(Button::South));
    }

    #[test]
    fn trigger_axes_default_to_zero_update_and_clear_on_disconnect() {
        let mut state = State::default();
        let id = GamepadId(0);
        state.apply(Update::Connected(id, "pad".into()));
        assert_eq!(
            state.pads[&id].value(Axis::LeftTrigger).to_bits(),
            0.0_f32.to_bits()
        );
        assert_eq!(
            state.pads[&id].value(Axis::RightTrigger).to_bits(),
            0.0_f32.to_bits()
        );

        let events: Vec<_> = [
            EventType::AxisChanged(Axis::LeftTrigger, 0.25),
            EventType::AxisChanged(Axis::RightTrigger, 0.75),
            EventType::ButtonPressed(Button::LeftTrigger2),
            EventType::AxisChanged(Axis::LeftTrigger, 1.0),
        ]
        .into_iter()
        .filter_map(|event| state.apply(Update::Input(Event { id, event })))
        .map(|event| event.event)
        .collect();
        assert_eq!(
            events,
            [
                EventType::AxisChanged(Axis::LeftTrigger, 0.25),
                EventType::AxisChanged(Axis::RightTrigger, 0.75),
                EventType::ButtonPressed(Button::LeftTrigger2),
                EventType::AxisChanged(Axis::LeftTrigger, 1.0),
            ]
        );
        assert_eq!(
            state.pads[&id].value(Axis::LeftTrigger).to_bits(),
            1.0_f32.to_bits()
        );
        assert_eq!(
            state.pads[&id].value(Axis::RightTrigger).to_bits(),
            0.75_f32.to_bits()
        );
        assert!(state.pads[&id].is_pressed(Button::LeftTrigger2));
        assert!(!state.pads[&id].is_pressed(Button::RightTrigger2));

        state.apply(Update::Input(Event {
            id,
            event: EventType::Disconnected,
        }));
        assert!(state.pads.is_empty());
        assert!(
            state
                .apply(Update::Input(Event {
                    id,
                    event: EventType::AxisChanged(Axis::LeftTrigger, 0.5),
                }))
                .is_none()
        );

        let new_id = GamepadId(1);
        state.apply(Update::Connected(new_id, "pad".into()));
        assert_eq!(
            state.pads[&new_id].value(Axis::LeftTrigger).to_bits(),
            0.0_f32.to_bits()
        );
        assert_eq!(
            state.pads[&new_id].value(Axis::RightTrigger).to_bits(),
            0.0_f32.to_bits()
        );
    }

    #[test]
    fn sanitize_trigger_clamps_and_replaces_non_finite() {
        assert_eq!(sanitize_trigger(0.0).to_bits(), 0.0_f32.to_bits());
        assert_eq!(sanitize_trigger(0.42).to_bits(), 0.42_f32.to_bits());
        assert_eq!(sanitize_trigger(1.0).to_bits(), 1.0_f32.to_bits());
        assert_eq!(sanitize_trigger(-0.25).to_bits(), 0.0_f32.to_bits());
        assert_eq!(sanitize_trigger(1.5).to_bits(), 1.0_f32.to_bits());

        for value in [-0.0_f32, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert_eq!(sanitize_trigger(value).to_bits(), 0.0_f32.to_bits());
        }
    }
}
