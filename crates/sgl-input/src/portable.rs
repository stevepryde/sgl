use crate::{Axis, Button, Event, EventType, GamepadId, Update};

pub(crate) struct Backend {
    inner: gilrs::Gilrs,
    initial: Vec<Update>,
}
impl Backend {
    pub(crate) fn new() -> Result<Self, String> {
        let inner = gilrs::Gilrs::new().map_err(|e| e.to_string())?;
        let initial = inner
            .gamepads()
            .map(|(id, pad)| Update::Connected(GamepadId(id.into()), pad.name().into()))
            .collect();
        Ok(Self { inner, initial })
    }
    pub(crate) fn poll(&mut self) -> Vec<Update> {
        let mut updates = std::mem::take(&mut self.initial);
        while let Some(event) = self.inner.next_event() {
            let id = GamepadId(event.id.into());
            let event = match event.event {
                gilrs::EventType::Connected => {
                    updates.push(Update::Connected(
                        id,
                        self.inner.gamepad(event.id).name().into(),
                    ));
                    continue;
                }
                gilrs::EventType::Disconnected => EventType::Disconnected,
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
                    let axis = match axis {
                        gilrs::Axis::LeftStickX => Axis::LeftStickX,
                        gilrs::Axis::LeftStickY => Axis::LeftStickY,
                        gilrs::Axis::RightStickX => Axis::RightStickX,
                        gilrs::Axis::RightStickY => Axis::RightStickY,
                        _ => continue,
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
fn map_button(button: gilrs::Button) -> Option<Button> {
    Some(match button {
        gilrs::Button::South => Button::South,
        gilrs::Button::East => Button::East,
        gilrs::Button::West => Button::West,
        gilrs::Button::North => Button::North,
        gilrs::Button::DPadUp => Button::DPadUp,
        gilrs::Button::DPadDown => Button::DPadDown,
        gilrs::Button::DPadLeft => Button::DPadLeft,
        gilrs::Button::DPadRight => Button::DPadRight,
        gilrs::Button::LeftTrigger => Button::LeftTrigger,
        gilrs::Button::RightTrigger => Button::RightTrigger,
        gilrs::Button::LeftTrigger2 => Button::LeftTrigger2,
        gilrs::Button::RightTrigger2 => Button::RightTrigger2,
        gilrs::Button::LeftThumb => Button::LeftThumb,
        gilrs::Button::RightThumb => Button::RightThumb,
        gilrs::Button::Start => Button::Start,
        gilrs::Button::Select => Button::Select,
        gilrs::Button::Mode => Button::Mode,
        _ => return None,
    })
}
