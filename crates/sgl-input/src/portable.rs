use std::collections::HashSet;

use crate::{Axis, Button, Event, EventType, GamepadId, Update};

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
            .flat_map(|(id, pad)| {
                on_connected(
                    &mut connected,
                    GamepadId(id.into()),
                    pad.name().into(),
                    |button| pad.is_pressed(button),
                    |axis| pad.value(axis),
                )
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
                    let pad = self.inner.gamepad(event.id);
                    updates.extend(on_connected(
                        &mut self.connected,
                        id,
                        pad.name().into(),
                        |button| pad.is_pressed(button),
                        |axis| pad.value(axis),
                    ));
                    continue;
                }
                gilrs::EventType::Disconnected => {
                    self.connected.remove(&id);
                    EventType::Disconnected
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

/// The updates for a pad that connects: `Connected`, then each button already
/// held and every stick's position, so held state is right before the next
/// change (as the macOS backend seeds it). Nothing for a pad already
/// connected: Windows can report again a pad that was listed at startup.
fn on_connected(
    connected: &mut HashSet<GamepadId>,
    id: GamepadId,
    name: String,
    pressed: impl Fn(gilrs::Button) -> bool,
    value: impl Fn(gilrs::Axis) -> f32,
) -> Vec<Update> {
    if !connected.insert(id) {
        return Vec::new();
    }
    let input = |event| Update::Input(Event { id, event });
    std::iter::once(Update::Connected(id, name))
        .chain(
            BUTTONS
                .iter()
                .filter(|(gilrs, _)| pressed(*gilrs))
                .map(|&(_, button)| input(EventType::ButtonPressed(button))),
        )
        .chain(
            AXES.iter()
                .map(|&(gilrs, axis)| input(EventType::AxisChanged(axis, value(gilrs)))),
        )
        .collect()
}

fn map_button(button: gilrs::Button) -> Option<Button> {
    BUTTONS
        .iter()
        .find(|(gilrs, _)| *gilrs == button)
        .map(|&(_, button)| button)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::State;
    use wasm_bindgen_test::wasm_bindgen_test;

    /// #317: a pad that connects with South held and the left stick pushed
    /// right reads that state at once, and a repeated `Connected` for it
    /// reports nothing and keeps the state.
    #[wasm_bindgen_test(unsupported = test)]
    fn a_pad_connecting_with_input_held_reads_it_at_once() {
        let mut connected = HashSet::new();
        let mut state = State::default();
        let id = GamepadId(3);
        let mut connect = |state: &mut State| {
            on_connected(
                &mut connected,
                id,
                "pad".into(),
                |button| button == gilrs::Button::South,
                |axis| {
                    if axis == gilrs::Axis::LeftStickX {
                        0.75
                    } else {
                        0.0
                    }
                },
            )
            .into_iter()
            .filter_map(|update| state.apply(update))
            .collect::<Vec<_>>()
        };

        let first = connect(&mut state);
        assert_eq!(
            first[0],
            Event {
                id,
                event: EventType::Connected
            }
        );
        let pad = state.pads.get(&id).expect("connected");
        assert!(pad.is_pressed(Button::South));
        assert!(!pad.is_pressed(Button::East));
        assert_eq!(pad.value(Axis::LeftStickX).to_bits(), 0.75f32.to_bits());

        assert!(
            connect(&mut state).is_empty(),
            "a repeated Connected is ignored"
        );
        assert!(state.pads[&id].is_pressed(Button::South));
    }
}
