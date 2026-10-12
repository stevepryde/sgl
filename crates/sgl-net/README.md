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
a waiting lane-0 fragment. Weights count messages and fragments, not bytes:
give a lane of many small messages the weight its rate needs beside bulk.

A full lane refuses a send with `SendError::WouldBlock`: nothing was queued,
the connection is fine and other lanes still admit, so keep the message and
retry after a later `flush` and `poll` (the [crate docs](src/lib.rs) show the
loop); `capacity(lane)` reports what the lane admits now. Dropping a refused
message loses it. A peer that sends faster than this side polls is slowed,
not disconnected: once a lane's inbound bounds are full, UDP holds the
lane's next message unacknowledged until `poll` makes room, so the peer's
`send` returns `WouldBlock`; the threaded UDP server does the same while
its caller has not polled.

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
two polls, and a peer that sends more is disconnected with
`DisconnectReason::InboundOverflow`. On every transport, a peer
disconnected with `InboundOverflow` or `ProtocolViolation` still delivers
the messages queued for `poll` before the offending frame or datagram, then
its `Disconnected`; nothing from that frame or datagram is delivered (on
UDP, nor is a reliable message still held for lane room).
`Delivery::Unreliable(lane)` sends independent best-effort messages of at
most `MAX_UNRELIABLE_BYTES`: each is sent once, never retransmitted or
fragmented, delivered at most once, in no promised order. The sender never
drops an accepted message; on UDP the network may lose one, while on
WebSocket and in memory all arrive, in send order, while the receiver keeps
polling. A receiver that is not polled drops its oldest unpolled unreliable
messages, as a full UDP socket buffer does, while reliable ones wait. A
full unreliable queue refuses `send` with `WouldBlock` like a full
reliable lane, and a lane's unreliable messages take turns with its
reliable fragments. On UDP small messages, fragments, latest state and
acknowledgements share datagrams of up to `MAX_DATAGRAM_BYTES`, so a flush
of `max_packets_per_peer_flush` datagrams carries hundreds of small
messages. `Delivery::LatestState` coalesces snapshots so the newest state
wins; however busy the lanes, it goes out in every UDP flush, and on
WebSocket behind only the lane frames flushed before it.
On the native WebSocket server, each peer's first message turn in a poll
returns its newest pending latest state ahead of any lane backlog, at most
once per poll. Peers take turns within `max_events_per_poll`; size that
budget for the peers and lane messages the game needs each tick. A state
received after its peer's first message turn waits for the next poll.
The same encoded payload can cross UDP, WebSocket, or memory. Payload caps
are public constants and lane bounds `ReliableConfig` values in the
[transport API](src/lib.rs). Connection IDs identify a connection inside a
server process; they are not persistent player identities or wire-format IDs.

When a UDP socket temporarily refuses a datagram (`WouldBlock`, native
`ENOBUFS`, or interruption), its messages stay queued until a later flush.
That peer stops sending for this flush; other peers still progress. Refused
reliable fragments do not consume transmission, backoff, or close resend
budgets, and refused acknowledgements remain owed. Custom
`udp::DatagramTransport` implementations return `io::Result<()>` from `send`
and report local buffer exhaustion as `WouldBlock`; other per-datagram
errors retain the network-loss behavior.

A UDP server keeps a connection's admitted cookie for its whole lifetime.
Later valid confirmations receive an accept naming that cookie, so delayed
handshake responses cannot change an established session's wire identity.
A valid disconnect before admission retires its cookie too, so a reordered
confirmation cannot admit a connection the client already closed.

UDP gives each lane its own retransmission, so a lost bulk fragment never
delays another lane. A UDP lane keeps at most 32 fragments in flight, so it
carries at most 32 reliable messages per round trip: spread a higher rate
of small reliable messages over lanes, or batch them into one message. WebSocket carries every lane on one TCP stream: lanes
there interleave in 16 KiB fragments and keep their own admission, but a lost
TCP segment stalls every lane until it is retransmitted. Use UDP where
loss-isolated lanes matter.

A native WebSocket client tries resolved addresses in order within its
`handshake_timeout`. Each TCP attempt shares the remaining time with the
untried addresses, so a silent first address does not exhaust the whole budget.
The last address gets all remaining time; the HTTP upgrade uses that same deadline.

Native WebSocket server and client sockets disable Nagle's algorithm before
the handshake to avoid delayed-ACK stalls for small updates. This is best
effort: a platform that refuses `TCP_NODELAY` still keeps the connection.

Use `memory_duplex` for a local session and the simulated UDP network for
repeatable loss/reordering scenarios. Native UDP may run on the caller's
thread or a bounded worker. Native WebSocket uses an I/O worker; browser
WebSocket uses the browser API. Native socket turns bound completed messages
and raw bytes read, so unfinished fragment floods yield to outbound work and
other peers, including during graceful close. The caller still owns gameplay timing.

UDP disconnect stops admitting messages and drains those already accepted
until acknowledged or `EndpointConfig::close_grace_ms` after the first
disconnect. Each unacknowledged reliable fragment gets up to
`close_retransmits` more sends during that grace, regardless of earlier
sends; a fragment first sent during close gets that many resends too.
Retransmission timing still applies, and disconnecting again restarts
neither the resend budget nor the deadline. Acknowledgements and keepalives
continue during close within the usual datagram budget. Incoming reliable
fragments are buffered and acknowledged under the usual inbound limits,
including releasing held fragments on later polls; newly received messages
of every class are discarded during close. This keeps a receiver
that is holding data alive even when the grace exceeds `timeout_ms`.
Locally refused Disconnect copies retry on later flushes even after the
peer is removed. This best-effort notification queue holds at most
`max_peers` identities, expires them after `timeout_ms`, and replaces its
oldest entry if connection churn fills it. Dropping an endpoint makes a
final best-effort attempt; it cannot retry after its socket is destroyed.
Dropping a UDP endpoint, server
or client, or calling `ThreadedUdpServer::shutdown`, immediately abandons
queued sends and sends redundant best-effort disconnects before closing its
socket; reachable peers report `Peer` without waiting for the timeout. Polling
a threaded server after shutdown or a worker panic returns its queued
messages followed by one `Disconnected { reason: Local }` for each live
connection; it accepts no more sends.

The [browser probe](examples/browser_probe.rs) and its
[native fixture server](examples/ws_fixture_server.rs) demonstrate browser
transport integration. They are check fixtures, not a production game server.

Read the [netcode contract](../../specs/netcode.md) for admission, bounds,
reconnection, and delivery guarantees. Run `cargo test -p sgl-net` for focused
native checks; the [required check](../../CONTRIBUTING.md#validate) also covers
release mode, WASM, and real browser I/O.
For dependency setup, see [Building games with SGL](../../docs/README.md).
