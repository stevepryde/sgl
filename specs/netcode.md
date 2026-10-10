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
3. Delivery has three classes: reliable ordered on a lane
   (`Delivery::Reliable(Lane)`; `RELIABLE_LANES` (4) independent lanes,
   `Delivery::RELIABLE_ORDERED` is lane 0), unreliable on a lane
   (`Delivery::Unreliable(Lane)`: independent best-effort messages), and
   newest-wins latest-state. What each lane carries is the game's choice.
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
    never a stale one. Reliable delivery is exact, in order, and unduplicated
    within each lane; there is no order across lanes. On UDP each lane has its
    own sequence space, window and retransmission, so a lost fragment of one
    lane never delays another lane's delivery. "Unreliable" means never
    retransmitted and unordered: each unreliable message is sent whole (at
    most `MAX_UNRELIABLE_BYTES`, 1168), never fragmented or retransmitted, and
    delivered at most once or not at all, in no promised order relative to any
    other message. The sending side never drops an accepted message of any
    class while the connection lives; the network may lose an unreliable one
    (UDP), and a receiver drops network duplicates. A receiver that is not
    polled drops its oldest unpolled unreliable messages, as a full UDP socket
    buffer does, while its reliable messages wait (11). On WebSocket and in
    memory every unreliable message arrives in send order while the receiver
    keeps polling, though that order is not promised.
11. Every bound is a public constant or a validated configuration value and is
    enforced before I/O. A payload over the transport's
    `ReliableConfig::max_message_bytes` (default
    `DEFAULT_RELIABLE_MESSAGE_BYTES`, 64 KiB; at most
    `RELIABLE_MESSAGE_BYTES_LIMIT`, 16 MiB), `MAX_UNRELIABLE_BYTES` or
    `MAX_LATEST_STATE_BYTES` returns `SendError::PayloadTooLarge`. Each lane
    has its own weight and bounds in `ReliableConfig` (`LaneConfig`;
    defaults `DEFAULT_LANE_*`), which every transport takes (`reliable` on
    `EndpointConfig`, `NativeWebSocketServerConfig`,
    `NativeWebSocketClientConfig` and `BrowserWebSocketConfig`;
    `memory_duplex_with`) and validates before use; both ends use the same
    configuration. A lane holding its `outbound_messages` or `outbound_bytes`
    (unflushed, in flight and unacknowledged alike), a lane whose unreliable
    queue holds `unreliable_messages` or `unreliable_bytes` not yet sent, or
    a shared ceiling such as the UDP endpoint's `global_reliable_outbound_*`,
    refuses the send with `SendError::WouldBlock`: nothing is queued, the
    caller keeps the payload and may retry after a later `flush` and `poll`,
    and the connection and every message accepted before it are unaffected.
    A lane holding no reliable bytes admits one message of any size up to
    the cap (the one-message rule), so a byte bound smaller than a message
    never refuses it for good and a lane holds at most the larger of the
    two; `capacity` then reports the cap. The UDP endpoint's shared byte
    ceilings are validated to hold at least one message. A full lane never
    refuses another lane, and a full unreliable queue never refuses reliable
    messages. `capacity` reports the reliable allowance. A retried message
    is ordered after whatever the lane accepted meanwhile. The threaded UDP
    worker moves a message to its endpoint only when the endpoint has room
    on that lane, rotating between backlogged peers and lanes, and the
    browser holds released frames while `bufferedAmount` is above its
    watermark; neither refuses an accepted message or disconnects. A UDP
    `disconnect` (caller-polled or threaded) admits nothing more but still
    sends the reliable and unreliable messages accepted before it, closing
    once they are acknowledged or `close_grace_ms` after the disconnect,
    whichever is first. A lane's
    completed reliable messages not yet returned by `poll` are bounded by
    its `inbound_messages`, and by its `inbound_bytes` beside at most one
    larger message, so a message of any admitted size followed by smaller
    ones before the next poll fits; a lane holds at most `inbound_bytes`
    plus one message. A receiver whose lane cannot take the next message
    slows its sender instead of closing it. UDP holds the fragment that
    would complete it, unacknowledged and reported held (12): the sender's
    window closes and, once its lane fills, its `send` returns `WouldBlock`.
    The caller-polled endpoint holds until its next `poll`, which takes the
    held messages first; the threaded server polls its endpoint within the
    room its ingress has left, so it holds until its caller's `poll` drains
    the ingress. The UDP endpoint's `global_reliable_inbound_messages`, a
    per-poll ceiling across peers, holds the same way, and held lanes take
    turns at the front of a poll so each progresses. Native WebSocket
    stops reading the connection until `poll` makes room (13). The browser
    cannot stop reading, so there the inbound bounds are a budget per
    caller poll, sized for the poll interval, and a peer that exceeds them
    is closed alone with `DisconnectReason::InboundOverflow`; elsewhere that
    reason means only a peer past the UDP endpoint's
    `global_reliable_inbound_bytes`.
    Received unreliable messages waiting for `poll` past a lane's
    `unreliable_messages` or `unreliable_bytes` (WebSocket and threaded UDP
    ingress) instead shed the oldest: a receiver that is not polled drops
    its oldest unpolled unreliable messages, as a full UDP socket buffer
    does. The threaded server's poll returns at most 32 lane messages per
    peer (`LANE_MESSAGES_PER_PEER_PER_POLL`), shared across its lanes and
    both classes, so that is the sustainable per-poll rate above which a
    peer's reliable messages wait and its unreliable messages are shed.
    Other connections are never affected.
12. UDP datagrams (version 3) are at most `MAX_DATAGRAM_BYTES` (1200). A
    payload datagram's kind byte carries a lane acknowledgement mask in its
    high nibble, and one six-byte acknowledgement follows per set lane:
    `next`, the lane's first fragment not yet consumed, then 32 bits whose
    bits 0–30 mark which of the 31 fragments after it arrived and whose bit
    31 (HELD) says `next` itself arrived and waits for the receiver's caller
    to make room (11). The sender resends neither a marked nor a held
    fragment and never counts a held one toward
    `max_reliable_transmissions`, nor samples its round trip; its window of
    `WINDOW` (32) fragments starts at the oldest fragment before which
    everything is acknowledged, so it never reaches past the receiver's,
    which buffers the same span from `next`. Any number of items fill the
    rest of the datagram. Every payload datagram carries the
    acknowledgement of each lane that has received anything, where it fits,
    and a lane acknowledges each change to its receive state (an arrival,
    or a held fragment consumed) in the next two flushes, in a payload
    datagram or else an acknowledgement datagram, so one lost
    acknowledgement does not stall a sender whose whole window rode one
    datagram until its retransmission timeout. An acknowledgement that does
    not fit beside a latest-state or unreliable item stays owed and rides a
    later datagram of the same flush when the per-peer datagram budget
    allows, or the next flush. Each item's tag
    carries its kind, FIRST and MORE flags and lane, and the first fragment of
    a longer message its total length. Item kinds are 0 reliable, 1 latest
    state and 2 unreliable (3 is reserved); an unreliable item carries no
    fragment flags, and its sequence is its lane's unreliable sequence, which
    the receiver uses only to drop duplicates within a 1,024-message window
    (one reordered behind more than that counts as lost). Reliable fragments
    carry at most `MAX_RELIABLE_FRAGMENT_BYTES` (1150, four fewer on a first
    fragment), so a reliable fragment always fits beside every lane's
    acknowledgement; latest state and unreliable messages carry at most 1168
    beside one. A datagram with the reserved item kind, a lane past
    `RELIABLE_LANES`, latest state or an unreliable message with fragment
    flags, latest state with a lane, a first fragment declaring no more than
    it carries, or mask bits on a control kind is rejected; from a connected
    peer that is `ProtocolViolation`. Handshakes use a keyed cookie
    challenge with a per-prefix challenge budget and a confirm replay cache.
13. WebSocket frames use the 18-byte version-2 envelope: magic, version, flags
    (kind 0 reliable, 1 latest, 2 unreliable; FIRST; MORE), lane, big-endian
    sequence (0 for reliable and unreliable, strictly increasing for latest),
    big-endian length, then the big-endian declared total on the first
    fragment of a longer message. A reliable message leaves in frames of at
    most `WEBSOCKET_FRAGMENT_BYTES` (16 KiB), so a long message holds another
    lane back by at most one fragment. Unreliable frames wait in their lane's
    queue and leave in its schedule under the same pacing as reliable ones
    (the browser's `bufferedAmount` watermark); they are never dropped by the
    sender. Latest state waits only for the lane frames flushed with or
    before it: a flush releases the state last sent before it, which leaves
    as soon as those frames have gone, ahead of every frame still queued; it
    is not a barrier, so meanwhile lane frames flushed later may leave first
    (holding them would let one lane's long message hold every lane back); a
    newer state sent after that flush waits for its own without withholding
    it, and of states whose turn comes together only the newest leaves. Text frames, wrong magic or version, the reserved kind, reserved
    flags, an invalid lane, latest state or an unreliable message with
    fragment flags, a total not above its fragment, and frames over
    `MAX_WEBSOCKET_FRAME_BYTES` are rejected as `ProtocolViolation`. Every
    lane shares the one TCP stream: a lost segment stalls every lane until TCP
    retransmits it, and a frame already written precedes everything after it;
    UDP is the transport for loss-isolated lanes. WebSocket may be slower
    than UDP, never weaker: a native receiver whose lane cannot take a
    completed message holds that frame and stops reading the connection
    until the caller's `poll` makes room, so its kernel buffer fills, TCP
    closes the sender's window, the sender's worker stops writing and its
    game's `send` returns `WouldBlock`. Every lane of that connection waits
    meanwhile. The stall is not the peer's inactivity; the sender's own
    timeout bounds how long it waits for a receiver that never polls. The
    browser WebSocket API cannot stop reading, so a browser receiver keeps
    its inbound bounds: a browser game sizes `inbound_messages` and
    `inbound_bytes` for the most it can receive between two polls, and a
    sender that outruns them is still closed with `InboundOverflow`.
    Neither transport negotiates a version, so builds on different versions
    cannot connect; a game changes its WebSocket subprotocol when the wire
    version changes. Browser reconnect follows `ReconnectPolicy`, driven by
    `poll(now_ms)`, after every close the game did not ask for,
    `InboundOverflow` and `ProtocolViolation` included: at most
    `max_attempts` attempts with doubling delays up to `max_delay_ms`. The
    count restarts only after a connection that stayed up at least
    `max_delay_ms`, so one that keeps failing soon after it opens backs off
    and stops.
14. Lanes share a connection by deficit round robin over the lanes with
    sendable work. Each lane's quantum is `LaneConfig::weight` items
    (`1..=MAX_LANE_WEIGHT`, default 1), an item being a reliable fragment or
    an unreliable message, each counting once whatever its size: a
    backlogged lane sends `weight` items per round, every backlogged lane
    makes progress, and between two items of a lane the other lanes send at
    most the sum of their weights. Weights share items, not bytes, so a lane
    of small messages that competes with bulk for a binding datagram budget
    needs a weight for its message rate. There is no strict priority. A
    lane's unreliable messages share its quantum with its reliable work.
    Within a lane, due UDP retransmissions go first; otherwise a new
    reliable fragment and an unreliable message take turns, so neither waits
    behind more than one of the other (plus, on UDP, the lane's due
    retransmissions, at most `WINDOW` per retransmission timeout).
    A UDP flush packs payload datagrams in that order: the latest state
    queued before the flush first, then lane items while the next fits
    beside the acknowledgement of every lane that has received anything; an
    item that does not fit starts the next datagram, so small items share
    datagrams and packing keeps the schedule and its bound. A peer sends at
    most `max_packets_per_peer_flush` datagrams per flush; an item left over
    is neither taken nor charged and waits, unsent unreliable messages
    included, for a later flush. The memory transport, whose messages cross
    whole, returns lanes interleaved by the same weights.
15. A reliable message longer than one UDP item or WebSocket frame is
    fragmented as it is sent, each fragment a range of the queued message,
    so the sender holds every message once; the first fragment declares the
    message's total. Each lane reassembles one message at a time against
    that total: a middle or last fragment with no first, a first or whole
    fragment while assembling, a declared total above the receiver's
    `max_message_bytes` or not above the first fragment, a middle fragment
    that reaches the total, and a last fragment that misses it are
    `ProtocolViolation`, and nothing from the offending datagram or frame is
    delivered. The receiver's cap governs, so both ends configure the same
    one. The total is checked before anything is buffered, and the buffer
    grows geometrically with what arrives, reserving at most twice what has
    arrived and never past the total, so a lane holds at most one partial
    message of the cap (plus, on UDP, its window of buffered fragments);
    the UDP endpoint's `global_reliable_inbound_bytes` caps the received
    bytes across peers. Partial messages go with their connection whatever
    ends it, a timed-out sender's included; there is no reassembly timer:
    keepalives decide liveness, retransmission progress, and the caps
    memory. A held fragment is not lost progress, so a UDP sender whose
    receiver stalls keeps the connection, its window closed, for as long
    as the receiver's keepalives arrive; the game decides how long to
    wait.

## Acceptance

- The same game-owned encoded payload crosses UDP, WebSocket, and memory adapters.
- The memory adapter runs without threads or sleeps.
- Bounds fail locally and observably without corrupting another connection.
- A producer faster than the link is refused, not disconnected, and the
  retained message lands exactly once.
- A realtime lane keeps its scheduling bound while another lane streams bulk
  data, and on UDP a dropped bulk fragment delays no other lane.
- On UDP small messages of every class share datagrams up to the limit,
  and every lane stays exact and in order over a lossy network.
- A 4 MiB message crosses every transport within the configured memory,
  through a lane whose byte allowance is smaller, while a realtime lane
  keeps its scheduling bound.
- A receiver that polls slowly makes a UDP (caller-polled or threaded) or
  native WebSocket sender slower, not disconnected; so does a threaded UDP
  server whose caller stops polling for longer than the timeout.
- Unreliable messages are never retransmitted and never delivered twice;
  with no network loss and a polled receiver every accepted one arrives,
  and no sending side drops one.
- A seeded `SimulatedNetwork` run replays the same event trace.
- `sgl-net` has no dependency on `sgl-2d` or a game crate.
