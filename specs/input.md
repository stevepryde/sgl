# Controller input

SGL owns reusable controller discovery and normalized events/state. The consuming
game owns composition, its window/event loop, focus policy and action bindings.
This boundary was requested while fixing Detonator Squad's macOS controller path.

1. **Backend.** `sgl-input` must use Apple's Game Controller framework on macOS 11.3+
   and Gilrs on Windows/Linux. It must not require Steam or rendering. The macOS
   owner must be created/polled/dropped on the main thread, with native callbacks
   removed before their owned elements are released. It must receive releases
   while unfocused and restore process-local monitoring preferences on drop.
2. **Input.** It must expose face buttons by position, D-pad, shoulders, lower
   triggers as digital buttons, stick clicks, menu/options/home when available,
   and both sticks. Stick axes must use -1..1 with positive Y up. A press and
   release between polls must remain two ordered events even when held state
   ends released. Absent optional controls must not generate invented input.
3. **Lifecycle.** It must discover controllers already connected and hot-plugged
   after initialization, each with one `Connected` (a repeated report is
   ignored). On macOS the buttons already held and the sticks' positions
   follow it; on Gilrs targets input already held at connection is not
   reported until it changes (gilrs starts a pad's state empty), and a
   release with no reported press is dropped. Disconnect must remove their
   held state; late callbacks from that connection must not affect a
   replacement controller. Device names must be display metadata, never
   identity keys.
4. **Composition.** The game must supply a running window event loop and continue
   polling while unfocused. The library must not select a player, trigger game
   actions, change system settings, initialize Steam, or invent game bindings.

Acceptance: use the renderer-free `controllers` example on a focused native
window to exercise actual buttons, both sticks, D-pad and hot-plug. Exercise
press/release-before-poll and disconnect/reconnect state at the shared event
boundary. Validate the consuming game's menu activation, binding capture,
movement, bombs, pause and focus loss. Record hardware/OS limits honestly;
controller discovery and synthetic UI keys alone cannot establish compatibility.
