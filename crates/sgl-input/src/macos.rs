//! The only FFI boundary. All framework access and callback installation/removal
//! happens on the main thread. Blocks own only a channel sender and input IDs;
//! they never borrow Rust state or dereference the framework's callback pointer.
use std::cell::Cell;
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
        for &(axis, ref input) in &axes {
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
    connections: Connections<Pad>,
    sender: Sender<Update>,
    receiver: Receiver<Update>,
}
impl Backend {
    pub(crate) fn new() -> Result<Self, String> {
        let main_thread =
            MainThreadMarker::new().ok_or("Create gamepad input on the window's main thread")?;
        // SAFETY: Main-thread, process-local framework setting. Keep releases
        // flowing when the window loses focus; the game gates actions, not state.
        BACKGROUND_EVENTS.with(|owners| unsafe {
            owners.acquire(
                || GCController::shouldMonitorBackgroundEvents(),
                |on| GCController::setShouldMonitorBackgroundEvents(on),
            );
        });
        let (sender, receiver) = mpsc::channel();
        Ok(Self {
            _main_thread: main_thread,
            connections: Connections {
                pads: BTreeMap::new(),
                next_id: 0,
            },
            sender,
            receiver,
        })
    }
    pub(crate) fn poll(&mut self) -> Vec<Update> {
        // SAFETY: MainThreadMarker prevents moving this owner to another thread.
        // The returned array and each controller are retained while inspected.
        let array = unsafe { GCController::controllers() };
        let controllers: Vec<_> = (0..array.count()).map(|i| array.objectAtIndex(i)).collect();
        let sender = &self.sender;
        self.connections.poll(
            controllers,
            &self.receiver,
            |pad, controller| {
                std::ptr::eq(
                    Retained::as_ptr(controller),
                    Retained::as_ptr(&pad.controller),
                )
            },
            |controller, id| {
                // SAFETY: Accessing retained controller properties on the main
                // thread.
                let profile = unsafe { controller.extendedGamepad() }?;
                let name = unsafe { controller.vendorName() }
                    .map_or_else(|| "Controller".into(), |s| s.to_string());
                Some((Pad::new(controller, &profile, id, sender), name))
            },
        )
    }
}

/// Connected pads by connection id, and the order one poll reports them in.
/// Generic over the pad and the controller handle so the event order is
/// tested without controller hardware.
struct Connections<P> {
    pads: BTreeMap<GamepadId, P>,
    next_id: usize,
}
impl<P> Connections<P> {
    /// One poll's updates in event order. Callbacks already queued happened
    /// before this poll saw any controller missing, so they come first, then
    /// the disconnects, then new connections followed by the state they seed.
    /// Ids are never reused, so a callback that arrives after its disconnect
    /// cannot reach a replacement controller.
    fn poll<C>(
        &mut self,
        present: Vec<C>,
        receiver: &Receiver<Update>,
        same: impl Fn(&P, &C) -> bool,
        mut connect: impl FnMut(C, GamepadId) -> Option<(P, String)>,
    ) -> Vec<Update> {
        let mut updates: Vec<_> = receiver.try_iter().collect();
        self.pads.retain(|id, pad| {
            let kept = present.iter().any(|controller| same(pad, controller));
            if !kept {
                updates.push(Update::Input(Event {
                    id: *id,
                    event: EventType::Disconnected,
                }));
            }
            kept
        });
        for controller in present {
            if self.pads.values().any(|pad| same(pad, &controller)) {
                continue;
            }
            let id = GamepadId(self.next_id);
            let Some((pad, name)) = connect(controller, id) else {
                continue;
            };
            self.next_id += 1;
            updates.push(Update::Connected(id, name));
            self.pads.insert(id, pad);
        }
        updates.extend(receiver.try_iter());
        updates
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        self.connections.pads.clear();
        // SAFETY: The main-thread-bound owner restores the process-local value
        // only after removing its callbacks, and only as the last owner. No
        // system preference is changed.
        BACKGROUND_EVENTS.with(|owners| unsafe {
            owners.release(|on| GCController::setShouldMonitorBackgroundEvents(on));
        });
    }
}

thread_local! {
    /// Owners are main-thread-bound, so the main thread's count is the
    /// process's.
    static BACKGROUND_EVENTS: BackgroundEvents = const { BackgroundEvents::new() };
}

/// Background event delivery shared by every live owner: the first turns it
/// on and remembers the value it found; the last restores that value. An
/// owner replaced in place (`pads = Gamepads::new()?`) is built before the old
/// one drops, so a per-owner saved value would restore "off" under the live
/// one. Generic over the framework calls so the sequence is tested without it.
struct BackgroundEvents {
    owners: Cell<usize>,
    previous: Cell<bool>,
}
impl BackgroundEvents {
    const fn new() -> Self {
        Self {
            owners: Cell::new(0),
            previous: Cell::new(false),
        }
    }
    fn acquire(&self, read: impl FnOnce() -> bool, set: impl FnOnce(bool)) {
        if self.owners.get() == 0 {
            self.previous.set(read());
            set(true);
        }
        self.owners.set(self.owners.get() + 1);
    }
    fn release(&self, set: impl FnOnce(bool)) {
        let owners = self.owners.get() - 1;
        self.owners.set(owners);
        if owners == 0 {
            set(self.previous.get());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Button, State};

    /// #530: replacing a live owner keeps background delivery on, and the
    /// value found before the first owner returns once none is left (the
    /// README's restore-on-drop promise). `flag` stands in for the
    /// framework's process-wide setting.
    #[test]
    fn replacing_a_live_owner_keeps_background_events_on() {
        let flag = Cell::new(false);
        let owners = BackgroundEvents::new();
        let acquire = || owners.acquire(|| flag.get(), |on| flag.set(on));
        let release = || owners.release(|on| flag.set(on));

        acquire();
        assert!(flag.get());
        // `pads = Gamepads::new()?`: the new owner exists before the old drops.
        acquire();
        release();
        assert!(flag.get(), "the live owner keeps background events");
        release();
        assert!(!flag.get(), "the last owner restores the original value");

        // A value the game had already turned on stays on afterwards.
        flag.set(true);
        acquire();
        release();
        assert!(flag.get());
    }

    /// #302: a press and release queued before an unplug are reported before
    /// its disconnect, and a callback from the old connection that arrives
    /// after it does not touch the replacement controller. Controllers and
    /// pads are plain tokens; `State` is the real event boundary.
    #[test]
    fn input_queued_before_an_unplug_precedes_its_disconnect() {
        let (sender, receiver) = mpsc::channel();
        let mut connections = Connections {
            pads: BTreeMap::new(),
            next_id: 0,
        };
        let mut state = State::default();
        let mut reported = Vec::new();
        let mut poll = |present: Vec<u32>| {
            let updates = connections.poll(
                present,
                &receiver,
                |pad: &u32, controller: &u32| pad == controller,
                |controller, _| Some((controller, "pad".into())),
            );
            reported.extend(updates.into_iter().filter_map(|u| state.apply(u)));
        };
        let input = |id, event| Update::Input(Event { id, event });
        let (old, new) = (GamepadId(0), GamepadId(1));

        poll(vec![1]);
        sender
            .send(input(old, EventType::ButtonPressed(Button::South)))
            .unwrap();
        sender
            .send(input(old, EventType::ButtonReleased(Button::South)))
            .unwrap();
        // Unplugged and replaced before the next poll.
        poll(vec![2]);
        // A late callback from the unplugged connection.
        sender
            .send(input(old, EventType::ButtonPressed(Button::South)))
            .unwrap();
        poll(vec![2]);

        let event = |id, event| Event { id, event };
        assert_eq!(
            reported,
            [
                event(old, EventType::Connected),
                event(old, EventType::ButtonPressed(Button::South)),
                event(old, EventType::ButtonReleased(Button::South)),
                event(old, EventType::Disconnected),
                event(new, EventType::Connected),
            ]
        );
        assert!(!state.pads[&new].is_pressed(Button::South));
    }
}
