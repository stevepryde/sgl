# Netcode

SGL networking is payload transport. Games own messages, authority, rooms,
prediction, and failure policy.

`sgl-net` is the shared replacement for the transport copies originally in
tiny, Torchmates, and Elemental Chaos. Those games agree on the contract below.

## Requirements

1. Native peers can use UDP, browser peers can use binary WebSocket, and solo
   or tests can use an in-memory duplex.
2. Every transport exposes connect/disconnect events, bounded receive polling,
   send, flush, and close. Native UDP can be caller-polled or owned by a bounded
   worker; a native WebSocket server or client owns one I/O worker thread that
   serves all its connections and wakes only for socket readiness, caller work
   (released frames, a due ping, a close), a handshake deadline or an accept
   retry. A server stops accepting only when admission stops. After an
   interrupted accept, or one that took a connection the client had already
   reset, it accepts again at once; after any other accept error it retries
   after a short back-off.
3. Delivery is either reliable ordered or newest-wins latest-state.
4. Transports own framing, sequencing, acknowledgement, retransmission, queue
   bounds, connection identity, and socket or browser I/O. The game supplies
   UDP magic, WebSocket magic, path, and subprotocol.
5. The caller supplies time to transport operations. Polling does not sleep,
   render, or step gameplay.
6. The same encoded payload crosses UDP, WebSocket, and the memory adapter.
   A headless server can use `sgl-net` without `sgl-2d`.
7. `Connected` carries a connection id. `send` either queues or returns a send
   error.
8. Generic endpoint adapters work over any `DatagramTransport`; the simulated
   network can create multiple independently addressed endpoints for acceptance
   tests without real sockets or sleeps.

9. Connection ids are unique for the life of a transport and never reused
   after `Disconnected`. Reconnecting yields a new id and a new epoch; late
   datagrams from an old epoch never surface on the new connection.
10. Latest-state delivery coalesces: a poll observes the newest payload and
    never a stale one. Reliable delivery is exact, in order, and unduplicated.
11. Every bound is a public constant and is enforced before I/O. A payload
    over `MAX_RELIABLE_MESSAGE_BYTES` or `MAX_LATEST_STATE_BYTES` returns
    `SendError` and changes nothing. A reliable queue past
    `RELIABLE_OUTBOUND_MESSAGES` / `RELIABLE_OUTBOUND_BYTES` returns
    `SendError::ReliableOverflow`; because a reliable stream cannot drop a
    message, UDP additionally closes the connection with the largest backlog
    with `DisconnectReason::ReliableOverflow`. Inbound overflow closes only
    the offending peer. Other connections are never affected.
12. UDP datagrams are at most `MAX_DATAGRAM_BYTES` (1200); the reliable window
    is `WINDOW` (32) fragments; handshakes use a keyed cookie challenge with a
    per-prefix challenge budget and a confirm replay cache.
13. WebSocket frames use the 17-byte envelope: magic, version, delivery class,
    big-endian sequence (0 for reliable, strictly increasing for latest),
    big-endian length. Text frames, wrong magic, wrong version, and frames
    over `MAX_WEBSOCKET_FRAME_BYTES` are rejected. Browser reconnect follows
    `ReconnectPolicy` with bounded attempts and delay, driven by `poll(now_ms)`.

## Acceptance

- The same game-owned encoded payload crosses UDP, WebSocket, and memory adapters.
- The memory adapter runs without threads or sleeps.
- Bounds fail locally and observably without corrupting another connection.
- A seeded `SimulatedNetwork` run replays the same event trace.
- `sgl-net` has no dependency on `sgl-2d` or a game crate.
