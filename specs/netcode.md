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
3. Delivery is either reliable ordered on a lane (`Delivery::Reliable(Lane)`;
   `RELIABLE_LANES` lanes, currently one, `Delivery::RELIABLE_ORDERED`) or
   newest-wins latest-state.
4. Transports own framing, sequencing, acknowledgement, retransmission, queue
   bounds, connection identity, and socket or browser I/O. The game supplies
   UDP magic, WebSocket magic, path, and subprotocol.
5. The caller supplies time to transport operations. Polling does not sleep,
   render, or step gameplay.
6. The same encoded payload crosses UDP, WebSocket, and the memory adapter.
   A headless server can use `sgl-net` without `sgl-2d`.
7. `Connected` carries a connection id. `send` either queues the whole
   payload or returns a `SendError` and changes nothing; it never ends a
   connection. `capacity` reports what a reliable lane admits now; it is
   advisory, and `send` is the only atomic admission.
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
    `SendError::PayloadTooLarge`. A reliable lane holding
    `RELIABLE_OUTBOUND_MESSAGES` or `RELIABLE_OUTBOUND_BYTES` (unflushed,
    in flight and unacknowledged alike), or a shared ceiling such as the UDP
    endpoint's `global_reliable_outbound_*`, refuses the send with
    `SendError::WouldBlock`: nothing is queued, the caller keeps the payload
    and may retry after a later `flush` and `poll`, and the connection and
    every message accepted before it are unaffected. A retried message is
    ordered after whatever the lane accepted meanwhile. The threaded UDP
    worker moves a message to its endpoint only when the endpoint has room,
    and the browser holds released frames while `bufferedAmount` is above
    its watermark; neither refuses an accepted message or disconnects. A peer
    that exceeds this side's inbound bounds is closed alone with
    `DisconnectReason::InboundOverflow`. Other connections are never
    affected.
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
- A producer faster than the link is refused, not disconnected, and the
  retained message lands exactly once.
- A seeded `SimulatedNetwork` run replays the same event trace.
- `sgl-net` has no dependency on `sgl-2d` or a game crate.
