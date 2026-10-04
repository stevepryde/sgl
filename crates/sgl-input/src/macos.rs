//! The only FFI boundary. All framework access and callback installation/removal
//! happens on the main thread. Blocks own only a channel sender and input IDs;
//! they never borrow Rust state or dereference the framework's callback pointer.
use std::collections::BTreeMap;
use std::ptr::NonNull;
use std::sync::mpsc::{self, Receiver, Sender};

use crate::{Axis, Button, Event, EventType, GamepadId, Update};
use block2::RcBlock;
use objc2::{MainThreadMarker, rc::Retained, runtime::Bool};
use objc2_game_controller::{
    GCController, GCControllerAxisInput, GCControllerButtonInput, GCDevice, GCExtendedGamepad,
};

struct Pad {
    controller: Retained<GCController>,
    buttons: Vec<(Button, Retained<GCControllerButtonInput>)>,
    axes: Vec<(Axis, Retained<GCControllerAxisInput>)>,
}
impl Pad {
    fn new(
        controller: Retained<GCController>,
        profile: &GCExtendedGamepad,
        id: GamepadId,
        sender: &Sender<Update>,
    ) -> Self {
        // SAFETY: Called only by the main-thread Backend with a retained profile.
        // These accessors return retained, typed elements. Optional controls are
        // omitted rather than inventing buttons for devices that lack them.
        let (buttons, axes) = unsafe {
            let dpad = profile.dpad();
            let mut buttons = vec![
                (Button::South, profile.buttonA()),
                (Button::East, profile.buttonB()),
                (Button::West, profile.buttonX()),
                (Button::North, profile.buttonY()),
                (Button::DPadUp, dpad.up()),
                (Button::DPadDown, dpad.down()),
                (Button::DPadLeft, dpad.left()),
                (Button::DPadRight, dpad.right()),
                (Button::LeftTrigger, profile.leftShoulder()),
                (Button::RightTrigger, profile.rightShoulder()),
                (Button::LeftTrigger2, profile.leftTrigger()),
                (Button::RightTrigger2, profile.rightTrigger()),
                (Button::Start, profile.buttonMenu()),
            ];
            for (button, input) in [
                (Button::Select, profile.buttonOptions()),
                (Button::Mode, profile.buttonHome()),
                (Button::LeftThumb, profile.leftThumbstickButton()),
                (Button::RightThumb, profile.rightThumbstickButton()),
            ] {
                if let Some(input) = input {
                    buttons.push((button, input));
                }
            }
            let left = profile.leftThumbstick();
            let right = profile.rightThumbstick();
            (
                buttons,
                vec![
                    (Axis::LeftStickX, left.xAxis()),
                    (Axis::LeftStickY, left.yAxis()),
                    (Axis::RightStickX, right.xAxis()),
                    (Axis::RightStickY, right.yAxis()),
                ],
            )
        };
        for (button, input) in &buttons {
            let button = *button;
            let events = sender.clone();
            let handler = RcBlock::new(
                move |_: NonNull<GCControllerButtonInput>, _: f32, pressed: Bool| {
                    let event = if pressed.as_bool() {
                        EventType::ButtonPressed(button)
                    } else {
                        EventType::ButtonReleased(button)
                    };
                    let _ = events.send(Update::Input(Event { id, event }));
                },
            );
            // SAFETY: The setter copies the block. Captures are owned and valid
            // until the framework drops the copy; its pointer is never dereferenced.
            unsafe {
                input.setPressedChangedHandler(RcBlock::as_ptr(&handler));
            }
            // Seed held input at connection; subsequent changes arrive in order.
            if unsafe { input.isPressed() } {
                let _ = sender.send(Update::Input(Event {
                    id,
                    event: EventType::ButtonPressed(button),
                }));
            }
        }
        for (axis, input) in &axes {
            let axis = *axis;
            let events = sender.clone();
            let handler = RcBlock::new(move |_: NonNull<GCControllerAxisInput>, value: f32| {
                let _ = events.send(Update::Input(Event {
                    id,
                    event: EventType::AxisChanged(axis, value),
                }));
            });
            // SAFETY: Same copied-block ownership as buttons. Apple's stick Y
            // already points up, matching the public API; do not invert it.
            unsafe {
                input.setValueChangedHandler(RcBlock::as_ptr(&handler));
                let _ = sender.send(Update::Input(Event {
                    id,
                    event: EventType::AxisChanged(axis, input.value()),
                }));
            }
        }
        Self {
            controller,
            buttons,
            axes,
        }
    }
}
impl Drop for Pad {
    fn drop(&mut self) {
        // SAFETY: Backend is main-thread-bound and retains each element. Clear
        // our copied blocks before releasing objects on disconnect or shutdown.
        unsafe {
            for (_, input) in &self.buttons {
                input.setPressedChangedHandler(std::ptr::null_mut());
            }
            for (_, input) in &self.axes {
                input.setValueChangedHandler(std::ptr::null_mut());
            }
        }
    }
}

pub(crate) struct Backend {
    _main_thread: MainThreadMarker,
    pads: BTreeMap<GamepadId, Pad>,
    next_id: usize,
    previous_background_events: bool,
    sender: Sender<Update>,
    receiver: Receiver<Update>,
}
impl Backend {
    pub(crate) fn new() -> Result<Self, String> {
        let main_thread =
            MainThreadMarker::new().ok_or("Create gamepad input on the window's main thread")?;
        // SAFETY: Main-thread, process-local framework setting. Keep releases
        // flowing when the window loses focus; the game gates actions, not state.
        let previous_background_events = unsafe {
            let previous = GCController::shouldMonitorBackgroundEvents();
            GCController::setShouldMonitorBackgroundEvents(true);
            previous
        };
        let (sender, receiver) = mpsc::channel();
        Ok(Self {
            _main_thread: main_thread,
            pads: BTreeMap::new(),
            next_id: 0,
            previous_background_events,
            sender,
            receiver,
        })
    }
    pub(crate) fn poll(&mut self) -> Vec<Update> {
        // SAFETY: MainThreadMarker prevents moving this owner to another thread.
        // The returned array and each controller are retained while inspected.
        let array = unsafe { GCController::controllers() };
        let controllers: Vec<_> = (0..array.count()).map(|i| array.objectAtIndex(i)).collect();
        let mut updates = Vec::new();
        self.pads.retain(|id, pad| {
            if controllers
                .iter()
                .any(|c| std::ptr::eq(&**c, &*pad.controller))
            {
                true
            } else {
                updates.push(Update::Input(Event {
                    id: *id,
                    event: EventType::Disconnected,
                }));
                false
            }
        });
        for controller in controllers {
            if self
                .pads
                .values()
                .any(|pad| std::ptr::eq(&*controller, &*pad.controller))
            {
                continue;
            }
            // SAFETY: Accessing retained controller properties on the main thread.
            let Some(profile) = (unsafe { controller.extendedGamepad() }) else {
                continue;
            };
            let name = unsafe { controller.vendorName() }
                .map_or_else(|| "Controller".into(), |s| s.to_string());
            let id = GamepadId(self.next_id);
            self.next_id += 1;
            updates.push(Update::Connected(id, name));
            self.pads
                .insert(id, Pad::new(controller, &profile, id, &self.sender));
        }
        updates.extend(self.receiver.try_iter());
        updates
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        self.pads.clear();
        // SAFETY: The main-thread-bound owner restores the process-local value
        // only after removing its callbacks. No system preference is changed.
        unsafe {
            GCController::setShouldMonitorBackgroundEvents(self.previous_background_events);
        }
    }
}
