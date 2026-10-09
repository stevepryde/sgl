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
`send`, `capacity`, `flush`, and disconnect operations. Supply monotonic
`now_ms` values from the game's clock, poll events, enqueue encoded
messages, and flush outbound work. Handle `SendError` and lifecycle events
explicitly; bounded queues are part of the contract.

`Delivery::Reliable(lane)` preserves every accepted message in order within
its lane; `Delivery::RELIABLE_ORDERED` is lane 0. There are `RELIABLE_LANES`
(4) independent lanes and no order across them: put traffic that must not
wait behind a bulk transfer (inputs, events) on one lane and the bulk on
another. Each lane has its own bounds and a scheduling weight in
`ReliableConfig`, passed to every transport's configuration (`reliable`) and
to `memory_duplex_with`; both ends use the same one. Lanes share the
connection by weighted round robin, so every busy lane progresses: with
`config.reliable.lanes[0].weight = 8` and lane 1 at 1, lane 0 sends eight
fragments for each bulk fragment, and at most one bulk fragment goes before
a waiting lane-0 fragment.

A full lane refuses a send with `SendError::WouldBlock`: nothing was queued,
the connection is fine and other lanes still admit, so keep the message and
retry after a later `flush` and `poll` (the [crate docs](src/lib.rs) show the
loop); `capacity(lane)` reports what the lane admits now. Dropping a refused
message loses it. A peer that floods this side's inbound bounds is
disconnected with `DisconnectReason::InboundOverflow`.

Reliable messages may be as large as `ReliableConfig::max_message_bytes`
(default 64 KiB, up to `RELIABLE_MESSAGE_BYTES_LIMIT`, 16 MiB); set the same
value on both ends. Transports fragment and reassemble them, holding each
message once on the sender and at most one partial message per lane on the
receiver, so a game sends a 4 MiB snapshot as one message with no
segmentation of its own. A lane holding nothing admits one message of any
size up to the cap even when its `outbound_bytes` is smaller, and a lane's
inbound queue holds one message larger than `inbound_bytes` beside smaller
ones, so a bulk lane keeps small allowances; on UDP the endpoint's
`global_reliable_*_bytes` must hold at least one message. A native
WebSocket receiver that polls slowly stops reading instead of overflowing,
which makes the sender's `send` return `WouldBlock`; the browser cannot, so
a browser game sizes each lane's inbound bounds for what arrives between
two polls.
`Delivery::Unreliable(lane)` sends independent best-effort messages of at
most `MAX_UNRELIABLE_BYTES`: each is sent once, never retransmitted or
fragmented, delivered at most once, in no promised order. The sender never
drops an accepted message; on UDP the network may lose one, while on
WebSocket and in memory all arrive, in send order, while the receiver keeps
polling. A receiver that is not polled drops its oldest unpolled unreliable
messages, as a full UDP socket buffer does; reliable overflow still closes
the peer. A full unreliable queue refuses `send` with `WouldBlock` like a
full reliable lane, and a lane's unreliable messages take turns with its
reliable fragments. On UDP each reliable fragment and each unreliable
message takes its own datagram, so a peer sends at most
`max_packets_per_peer_flush` of them per flush (one fewer while latest state
is pending). `Delivery::LatestState` coalesces
snapshots so the newest state wins.
The same encoded payload can cross UDP, WebSocket, or memory. Payload caps
are public constants and lane bounds `ReliableConfig` values in the
[transport API](src/lib.rs). Connection IDs identify a connection inside a
server process; they are not persistent player identities or wire-format IDs.

UDP gives each lane its own retransmission, so a lost bulk fragment never
delays another lane. WebSocket carries every lane on one TCP stream: lanes
there interleave in 16 KiB fragments and keep their own admission, but a lost
TCP segment stalls every lane until it is retransmitted. Use UDP where
loss-isolated lanes matter.

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
