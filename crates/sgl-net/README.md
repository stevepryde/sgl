# sgl-net

Payload-opaque networking for native clients, browser clients, and headless
servers. Games own message schemas, serialization, authority, prediction,
rooms, and reconnect policy. SGL owns transport framing, queues, delivery,
and connection lifecycle, with no rendering or windowing dependency.

## Choose a transport

| Need | Entry point |
| --- | --- |
| In-process client/server without sockets | [`memory_duplex`](src/memory.rs) |
| Native UDP or deterministic simulated datagrams | [`udp`](src/udp/mod.rs) |
| Native or browser WebSocket | [`websocket`](src/websocket/mod.rs) |
| Several server transports behind one interface | [`ServerIoMux`](src/mux.rs) |

The shared [`ClientIo` and `ServerIo`](src/lib.rs) traits provide `poll`,
`send`, `flush`, and disconnect operations. Supply monotonic `now_ms` values
from the game's clock, poll events, enqueue encoded messages, and flush
outbound work. Handle `SendError` and lifecycle events explicitly; bounded
queues are part of the contract.

`Delivery::ReliableOrdered` preserves every accepted message in order.
`Delivery::LatestState` coalesces snapshots so the newest state wins.
The same encoded payload can cross UDP, WebSocket, or memory. Payload caps
and queue bounds are public constants in the [transport API](src/lib.rs).
Connection IDs identify a connection inside a server process; they are not
persistent player identities or wire-format IDs.

Use `memory_duplex` for a local session and the simulated UDP network for
repeatable loss/reordering scenarios. Native UDP may run on the caller's
thread or a bounded worker. Native WebSocket uses an I/O worker; browser
WebSocket uses the browser API. The caller still owns gameplay timing.

The [browser probe](examples/browser_probe.rs) and its
[native fixture server](examples/ws_fixture_server.rs) demonstrate browser
transport integration. They are check fixtures, not a production game server.

Read the [netcode contract](../../specs/netcode.md) for admission, bounds,
reconnection, and delivery guarantees. Run `cargo test -p sgl-net` for focused
native checks; the [required check](../../CONTRIBUTING.md#validate) also covers
release mode, WASM, and real browser I/O.
For dependency setup, see [Building games with SGL](../../docs/README.md).
