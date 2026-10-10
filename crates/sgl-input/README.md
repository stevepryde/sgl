# SGL input

`sgl-input` provides caller-polled controller events and current button/stick
state without a renderer or Steam dependency. macOS 11.3+ uses Apple's Game Controller
framework; Windows/Linux use Gilrs. Construct one `Gamepads` instance on the
window's main thread and poll it while the native event loop runs.

```rust,no_run
use sgl_input::{Gamepads, Button, EventType};
let mut pads = Gamepads::new()?;
// Once per frame, including while the window is unfocused:
for event in pads.poll() {
    match event.event {
        EventType::ButtonPressed(Button::South) => { /* game action if focused */ }
        EventType::Disconnected => { /* clear that player's held actions */ }
        _ => {}
    }
}
# Ok::<(), String>(())
```

South/East/West/North are face-button positions. Stick values range from -1 to 1
with positive Y pointing up. `LeftTrigger`/`RightTrigger` are shoulder buttons;
`LeftTrigger2`/`RightTrigger2` are the lower triggers treated as buttons. Press and
release events preserve taps between polls; `gamepad(id)` exposes the latest
state and returns `None` after disconnection. On Windows, Linux and the web
(Gilrs), a button held when a pad connects is not reported, and a deflected
stick reads 0 until it changes. IDs are local to this owner and must be discarded on disconnection; backends
may reuse them for later connections. They are not saved player identities. macOS supports extended-gamepad profiles
and omits optional buttons a device does not provide.

On macOS, the owner enables process-local background event delivery so releases
are not lost during focus changes, and restores the previous value on drop.
It does not change system preferences.

The game owns player assignment, focus gating, action bindings, deadzones, menu
navigation and release-before-resume behavior. Poll while unfocused to drain
releases/disconnects, but do not dispatch gameplay or menu actions then. Do not
create another native controller owner in the same process: native element
callbacks belong to this owner and are removed when it drops.

Try `cargo run -p sgl-input --example controllers` with a focused window. The
example prints actual connection/button/stick events and updates its window title.
Device enumeration alone is not evidence that inputs work. Steam Input's native
action API is not implemented; emulated devices still depend on the OS backend.
See [the input contract](../../specs/input.md).

For dependency setup and other SGL building blocks, see
[Building games with SGL](../../docs/README.md). Repository checks and platform
requirements are in [Contributing](../../CONTRIBUTING.md).
