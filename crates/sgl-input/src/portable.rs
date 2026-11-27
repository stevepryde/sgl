use std::collections::HashSet;

use crate::{Axis, Button, Event, EventType, GamepadId, Update, sanitize_trigger};

/// Gilrs buttons SGL reports, and their SGL names.
const BUTTONS: [(gilrs::Button, Button); 17] = [
    (gilrs::Button::South, Button::South),
    (gilrs::Button::East, Button::East),
    (gilrs::Button::West, Button::West),
    (gilrs::Button::North, Button::North),
    (gilrs::Button::DPadUp, Button::DPadUp),
    (gilrs::Button::DPadDown, Button::DPadDown),
    (gilrs::Button::DPadLeft, Button::DPadLeft),
    (gilrs::Button::DPadRight, Button::DPadRight),
    (gilrs::Button::LeftTrigger, Button::LeftTrigger),
    (gilrs::Button::RightTrigger, Button::RightTrigger),
    (gilrs::Button::LeftTrigger2, Button::LeftTrigger2),
    (gilrs::Button::RightTrigger2, Button::RightTrigger2),
    (gilrs::Button::LeftThumb, Button::LeftThumb),
    (gilrs::Button::RightThumb, Button::RightThumb),
    (gilrs::Button::Start, Button::Start),
    (gilrs::Button::Select, Button::Select),
    (gilrs::Button::Mode, Button::Mode),
];

/// Gilrs axes SGL reports, and their SGL names.
const AXES: [(gilrs::Axis, Axis); 4] = [
    (gilrs::Axis::LeftStickX, Axis::LeftStickX),
    (gilrs::Axis::LeftStickY, Axis::LeftStickY),
    (gilrs::Axis::RightStickX, Axis::RightStickX),
    (gilrs::Axis::RightStickY, Axis::RightStickY),
];

/// Gilrs analog trigger buttons mapped to SGL trigger axes.
///
/// In `gilrs 0.11`, all platforms (Linux SDL/evdev mappings, Windows `XInput` and
/// WGI, and `wasm32` Web Gamepad API) normalize lower-trigger travel into
/// `EventType::ButtonChanged(Button::LeftTrigger2 | RightTrigger2, 0.0..=1.0, _)`,
/// while also emitting `ButtonPressed`/`ButtonReleased` across the threshold
/// (and `1.0`/`0.0` `ButtonChanged` for digital triggers). `Axis::LeftZ` and
/// `Axis::RightZ` are non-SDL axes unused on Windows and Web and can conflict
/// with trigger button events on Linux, so analog triggers are read solely from
/// `ButtonChanged`.
const TRIGGER_AXES: [(gilrs::Button, Axis); 2] = [
    (gilrs::Button::LeftTrigger2, Axis::LeftTrigger),
    (gilrs::Button::RightTrigger2, Axis::RightTrigger),
];

pub(crate) struct Backend {
    inner: gilrs::Gilrs,
    initial: Vec<Update>,
    /// Pads reported connected and not since disconnected.
    connected: HashSet<GamepadId>,
}
impl Backend {
    pub(crate) fn new() -> Result<Self, String> {
        let inner = gilrs::Gilrs::new().map_err(|e| e.to_string())?;
        let mut connected = HashSet::new();
        let initial = inner
            .gamepads()
            .filter_map(|(id, pad)| {
                on_connected(&mut connected, GamepadId(id.into()), pad.name().into())
            })
            .collect();
        Ok(Self {
            inner,
            initial,
            connected,
        })
    }
    pub(crate) fn poll(&mut self) -> Vec<Update> {
        let mut updates = std::mem::take(&mut self.initial);
        while let Some(event) = self.inner.next_event() {
            let id = GamepadId(event.id.into());
            let event = match event.event {
                gilrs::EventType::Connected => {
                    let name = self.inner.gamepad(event.id).name().into();
                    updates.extend(on_connected(&mut self.connected, id, name));
                    continue;
                }
                gilrs::EventType::Disconnected => {
                    updates.push(on_disconnected(&mut self.connected, id));
                    continue;
                }
                gilrs::EventType::ButtonPressed(button, _) => {
                    let Some(button) = map_button(button) else {
                        continue;
                    };
                    EventType::ButtonPressed(button)
                }
                gilrs::EventType::ButtonReleased(button, _) => {
                    let Some(button) = map_button(button) else {
                        continue;
                    };
                    EventType::ButtonReleased(button)
                }
                gilrs::EventType::ButtonChanged(button, value, _) => {
                    let Some(event) = map_trigger_change(button, value) else {
                        continue;
                    };
                    event
                }
                gilrs::EventType::AxisChanged(axis, value, _) => {
                    let Some(&(_, axis)) = AXES.iter().find(|(gilrs, _)| *gilrs == axis) else {
                        continue;
                    };
                    EventType::AxisChanged(axis, value)
                }
                _ => continue,
            };
            updates.push(Update::Input(Event { id, event }));
        }
        self.inner.inc();
        updates
    }
}

/// `Connected` for a pad, or nothing when it is already connected: Windows
/// can report again a pad that was listed at startup. Input already held when
/// a pad connects is not reported until it changes: gilrs starts each pad's
/// cached state empty and reports only later changes.
fn on_connected(connected: &mut HashSet<GamepadId>, id: GamepadId, name: String) -> Option<Update> {
    connected.insert(id).then_some(Update::Connected(id, name))
}

/// `Disconnected` for a pad, which a later `Connected` reports again.
fn on_disconnected(connected: &mut HashSet<GamepadId>, id: GamepadId) -> Update {
    connected.remove(&id);
    Update::Input(Event {
        id,
        event: EventType::Disconnected,
    })
}

fn map_button(button: gilrs::Button) -> Option<Button> {
    BUTTONS
        .iter()
        .find(|(gilrs, _)| *gilrs == button)
        .map(|&(_, button)| button)
}

fn map_trigger_change(button: gilrs::Button, value: f32) -> Option<EventType> {
    TRIGGER_AXES
        .iter()
        .find(|(gilrs, _)| *gilrs == button)
        .map(|&(_, axis)| EventType::AxisChanged(axis, sanitize_trigger(value)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::State;
    use wasm_bindgen_test::wasm_bindgen_test;

    /// #317: a repeated `Connected` for a pad already connected reports
    /// nothing and keeps its held state; after a disconnect it connects again.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_repeated_connected_is_ignored() {
        let mut connected = HashSet::new();
        let mut state = State::default();
        let id = GamepadId(3);
        let press = Update::Input(Event {
            id,
            event: EventType::ButtonPressed(Button::South),
        });
        let apply = |state: &mut State, update: Option<Update>| {
            update.and_then(|update| state.apply(update))
        };

        assert!(apply(&mut state, on_connected(&mut connected, id, "pad".into())).is_some());
        state.apply(press);
        assert!(apply(&mut state, on_connected(&mut connected, id, "pad".into())).is_none());
        assert!(state.pads[&id].is_pressed(Button::South));

        state.apply(on_disconnected(&mut connected, id));
        assert!(apply(&mut state, on_connected(&mut connected, id, "pad".into())).is_some());
        assert!(!state.pads[&id].is_pressed(Button::South));
    }

    #[wasm_bindgen_test(unsupported = test)]
    fn trigger_button_changes_map_to_sanitized_trigger_axes() {
        assert_eq!(
            map_trigger_change(gilrs::Button::LeftTrigger2, 0.35),
            Some(EventType::AxisChanged(Axis::LeftTrigger, 0.35))
        );
        assert_eq!(
            map_trigger_change(gilrs::Button::RightTrigger2, 1.4),
            Some(EventType::AxisChanged(Axis::RightTrigger, 1.0))
        );
        assert_eq!(
            map_trigger_change(gilrs::Button::LeftTrigger2, -0.5),
            Some(EventType::AxisChanged(Axis::LeftTrigger, 0.0))
        );
        assert_eq!(
            map_trigger_change(gilrs::Button::RightTrigger2, f32::NAN),
            Some(EventType::AxisChanged(Axis::RightTrigger, 0.0))
        );
        assert_eq!(map_trigger_change(gilrs::Button::LeftTrigger, 1.0), None);
        assert_eq!(map_trigger_change(gilrs::Button::RightTrigger, 1.0), None);
        assert_eq!(map_trigger_change(gilrs::Button::South, 1.0), None);
        assert_eq!(
            map_button(gilrs::Button::LeftTrigger2),
            Some(Button::LeftTrigger2)
        );
        assert_eq!(
            map_button(gilrs::Button::RightTrigger2),
            Some(Button::RightTrigger2)
        );
    }
}
