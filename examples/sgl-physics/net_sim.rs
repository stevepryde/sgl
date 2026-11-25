//! What a networked game's server and client share: the level, the body,
//! the input, the one step function both run, the messages they exchange,
//! and the two halves that run them over any `sgl-net` transport.
//!
//! The server steps every player from an `InputQueue` under a `FixedClock`
//! with `CatchUp`, and sends each one a snapshot on the latest-state class.
//! The client predicts its own player with the same `step` through a
//! `Predictor`, sends its inputs with their ticks on an unreliable lane,
//! reconciles on each snapshot, draws the other players from a
//! `SnapshotBuffer`, and compares a `StateHasher` digest of what it
//! predicted with the server's state for the same tick to find a desync.
//!
//! `sgl-physics`, `sgl-net` and `sgl-core` know nothing of each other: the
//! game joins them here. Time is the caller's; nothing sleeps or opens a
//! socket. This file is included by path by
//! `examples/sgl-physics/examples/net_mover.rs` and
//! `tests/sgl-physics/tests/net.rs`; it is not part of any published crate.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sgl_core::math::Vec3;
use sgl_core::prediction::{
    InputQueue, Playback, Predictor, ReconcileError, Sampled, SnapshotBuffer, TickInput,
};
use sgl_core::time::{CatchUp, FixedClock};
use sgl_core::{Digest, digest_of};
use sgl_net::udp::simulated::{SimulatedConfig, SimulatedNetwork, SimulatedTransport};
use sgl_net::udp::{ClientEndpoint, EndpointConfig, ServerEndpoint};
use sgl_net::{
    ClientEvent, ClientIo, ConnectionId, Delivery, Lane, MAX_LATEST_STATE_BYTES, MemoryClientIo,
    ServerEvent, ServerIo, ServerIoMux, memory_duplex,
};
use sgl_physics::{
    Collider, ColliderSet, Cut, MoverConfig, MoverOptions, MoverSettings, MoverState, Shape,
    move_and_slide,
};

// ---------------------------------------------------------------------------
// The simulation both sides run.

/// The fixed step's rate. Both sides step with [`DT`], never with a
/// measured frame time, so their steps are the same calls.
pub const TICK_HZ: f32 = 60.0;
/// The fixed step, in seconds.
pub const DT: f32 = 1.0 / 60.0;
const GRAVITY: f32 = 25.0;
const RUN: f32 = 5.0;
/// Lifts the body 9² / (2 × 25) = 1.62.
const JUMP: f32 = 9.0;
/// The player: 0.6 wide and deep, 1.8 tall.
pub const BODY: Shape<Vec3> = Shape::Box {
    half: Vec3::new(0.3, 0.9, 0.3),
};
/// Both clocks follow elapsed time: at most 8 steps a frame, and up to 64
/// more carried, so a stall of about a second is caught up, not dropped.
pub const CATCH_UP: CatchUp = CatchUp {
    max_steps_per_frame: 8,
    max_debt_steps: 64,
};

pub const FLOOR: u64 = 1;
pub const WALL: u64 = 2;
/// 0.3 high, under the 0.35 step height: walked onto without a jump.
pub const STEP: u64 = 3;
/// Rises 2 over 4 along x (`normal · up` = 0.894, walkable), then drops 2
/// at its far end.
pub const RAMP: u64 = 4;

/// The level, y up: a floor, a wall at its +x end, a step and a ramp
/// across it.
#[must_use]
pub fn level() -> [Collider<Vec3>; 4] {
    let block = |id, centre, half| Collider::new(id, Shape::Box { half }, centre);
    [
        block(FLOOR, Vec3::new(0.0, -0.5, 0.0), Vec3::new(14.0, 0.5, 6.0)),
        block(WALL, Vec3::new(12.5, 2.0, 0.0), Vec3::new(0.5, 2.0, 6.0)),
        block(STEP, Vec3::new(3.0, 0.15, 0.0), Vec3::new(1.0, 0.15, 6.0)),
        // The box from x = 6 to 10 where y - 1 <= (x - 8) / 2.
        block(RAMP, Vec3::new(8.0, 1.0, 0.0), Vec3::new(2.0, 1.0, 6.0))
            .with_cut(Cut::new(Vec3::new(-1.0, 2.0, 0.0), 0.0)),
    ]
}

/// Where player `id` starts: just above the floor, in one of five lanes.
#[must_use]
pub fn spawn(id: u8) -> MoverState<Vec3> {
    MoverState {
        position: Vec3::new(-8.0, 0.91, -4.0 + 2.0 * f32::from(id % 5)),
        ..MoverState::default()
    }
}

/// What a player holds during one tick. Plain integers, so the server steps
/// exactly what the client stepped.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Input {
    /// Run along x, -127 to 127.
    pub x: i8,
    /// Run along z, -127 to 127.
    pub z: i8,
    /// Jump when on the floor.
    pub jump: bool,
}

/// The world and its rules: everything `step` reads besides the state.
pub struct Sim {
    world: ColliderSet<Vec3>,
    config: MoverConfig<Vec3>,
    options: MoverOptions,
}

impl Sim {
    /// The level as a `ColliderSet`, built from [`level`].
    #[must_use]
    pub fn new() -> Self {
        Self::from_colliders(level())
    }

    /// The level from colliders given in any order: a set is derived data,
    /// so a client may rebuild it from the level at any time.
    #[must_use]
    pub fn from_colliders(colliders: impl IntoIterator<Item = Collider<Vec3>>) -> Self {
        let mut world = ColliderSet::new(4.0);
        for collider in colliders {
            world.insert(collider);
        }
        Self {
            world,
            config: MoverConfig::new(MoverSettings {
                up: Vec3::Y,
                skin: 0.01,
                snap_distance: 0.1,
                normal_threshold: 0.7,
                block_epsilon: 1e-4,
            }),
            options: MoverOptions {
                step_height: 0.35,
                ..MoverOptions::default()
            },
        }
    }

    /// The one fixed step, on the server and in the client's prediction and
    /// replay alike: velocity from the input and gravity, then the mover.
    /// Its result depends only on its arguments and the level. It takes the
    /// input by reference, as `Predictor` passes it.
    #[allow(clippy::trivially_copy_pass_by_ref)]
    pub fn step(&self, state: &mut MoverState<Vec3>, _tick: u64, input: &Input) {
        let run = |axis: i8| f32::from(axis) / 127.0 * RUN;
        state.velocity.x = run(input.x);
        state.velocity.z = run(input.z);
        if input.jump && state.floor.is_some() {
            state.velocity.y = JUMP;
        }
        state.velocity.y -= GRAVITY * DT;
        move_and_slide(state, &BODY, DT, &self.world, &self.config, &self.options)
            .expect("a set always answers");
    }
}

impl Default for Sim {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Messages, encoded with postcard.

/// Inputs travel unreliably: a lost one is covered by the next message,
/// which repeats the last [`RESEND_TAIL`] inputs not yet acknowledged.
pub const INPUTS: Delivery = Delivery::Unreliable(Lane::DEFAULT);
/// How many unacknowledged inputs each input message carries: each input is
/// sent this many times while the server has not acknowledged it.
pub const RESEND_TAIL: usize = 8;

/// Client to server: the inputs for consecutive ticks from `first_tick`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Inputs {
    pub first_tick: u64,
    pub inputs: Vec<Input>,
}

/// Server to client, on `Delivery::LatestState`: every player's state after
/// `tick`. `tick` also acknowledges the receiver's input for that tick:
/// `you` is its state after the server ran it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub tick: u64,
    /// The receiver's player id.
    pub player: u8,
    pub you: MoverState<Vec3>,
    pub others: Vec<(u8, MoverState<Vec3>)>,
}

/// A snapshot's largest postcard encoding without the others: the tick (a
/// `u64` varint, at most 10 bytes), the player id (1), the receiver's state
/// (`MoverState::<Vec3>::MAX_POSTCARD_BYTES`, 47) and the others' count (a
/// varint, 1 byte below 128): 59 bytes.
const SNAPSHOT_BASE_BYTES: usize = 10 + 1 + MoverState::<Vec3>::MAX_POSTCARD_BYTES + 1;
/// One other player: its id and its state, 48 bytes.
const OTHER_BYTES: usize = 1 + MoverState::<Vec3>::MAX_POSTCARD_BYTES;
/// Players one latest-state payload holds: the receiver and
/// (1168 - 59) / 48 = 23 others, so 24. A game with more sends a nearby
/// subset, or splits the others over several messages on a lane.
pub const MAX_PLAYERS: usize = 1 + (MAX_LATEST_STATE_BYTES - SNAPSHOT_BASE_BYTES) / OTHER_BYTES;
const _: () = assert!(MAX_PLAYERS < 128, "the others' count stays one byte");

// ---------------------------------------------------------------------------
// The server.

/// How many ticks ahead of the server's a client's input may be queued.
const INPUT_QUEUE: usize = 64;

/// One connected player, as the server keeps it.
pub struct Player {
    pub conn: ConnectionId,
    pub id: u8,
    pub state: MoverState<Vec3>,
    queue: InputQueue<Input>,
    /// Ticks run on a repeated input because this tick's had not arrived:
    /// where the client's prediction may have to be corrected.
    pub guessed: u64,
}

/// The authoritative half: steps every player once a tick.
pub struct Server<S> {
    io: S,
    sim: Sim,
    clock: FixedClock,
    next_tick: u64,
    players: Vec<Player>,
}

impl<S: ServerIo> Server<S> {
    #[must_use]
    pub fn new(io: S) -> Self {
        Self {
            io,
            sim: Sim::new(),
            clock: FixedClock::with_catch_up(TICK_HZ, CATCH_UP),
            next_tick: 0,
            players: Vec::new(),
        }
    }

    /// One server frame of `elapsed` time ending at `now_ms`: take what
    /// arrived, run the ticks due, send each player one snapshot of the
    /// newest tick. Returns the ticks run.
    pub fn frame(&mut self, now_ms: u64, elapsed: Duration) -> u32 {
        for event in self.io.poll(now_ms) {
            match event {
                ServerEvent::Connected { conn } => self.join(conn, now_ms),
                ServerEvent::Message {
                    conn,
                    delivery,
                    payload,
                } if delivery == INPUTS => self.receive(conn, &payload),
                ServerEvent::Disconnected { conn, .. } => {
                    self.players.retain(|player| player.conn != conn);
                }
                ServerEvent::Message { .. } => {}
            }
        }
        self.clock.begin_frame(elapsed);
        let mut ran = 0;
        while self.clock.step() {
            self.run_tick();
            ran += 1;
        }
        self.clock.finish();
        if ran > 0 {
            self.send_snapshots();
        }
        self.io.flush(now_ms);
        ran
    }

    /// The last tick run.
    #[must_use]
    pub fn tick(&self) -> Option<u64> {
        self.next_tick.checked_sub(1)
    }

    #[must_use]
    pub fn players(&self) -> &[Player] {
        &self.players
    }

    fn join(&mut self, conn: ConnectionId, now_ms: u64) {
        let free = (0..=u8::MAX).find(|&id| self.players.iter().all(|p| p.id != id));
        match free {
            Some(id) if self.players.len() < MAX_PLAYERS => self.players.push(Player {
                conn,
                id,
                state: spawn(id),
                // Its first tick is the next one the server runs.
                queue: InputQueue::new(self.next_tick, INPUT_QUEUE),
                guessed: 0,
            }),
            _ => self.io.disconnect(conn, now_ms),
        }
    }

    fn receive(&mut self, conn: ConnectionId, payload: &[u8]) {
        // A message that does not decode is dropped, like a lost one.
        let Ok(message) = postcard::from_bytes::<Inputs>(payload) else {
            return;
        };
        let Some(player) = self.players.iter_mut().find(|p| p.conn == conn) else {
            return;
        };
        for (tick, input) in (0..).zip(message.inputs) {
            let Some(tick) = message.first_tick.checked_add(tick) else {
                break;
            };
            // Late, duplicate and too-far-ahead inputs are dropped: the
            // queue counts them.
            let _ = player.queue.push(tick, input);
        }
    }

    fn run_tick(&mut self) {
        let tick = self.next_tick;
        for player in &mut self.players {
            let (_, given) = player.queue.take();
            player.guessed += u64::from(matches!(given, TickInput::Repeated(_)));
            // Before its first input arrives a player stands still, as the
            // client predicted when it joined.
            let input = given.input().copied().unwrap_or_default();
            self.sim.step(&mut player.state, tick, &input);
        }
        self.next_tick += 1;
    }

    fn send_snapshots(&mut self) {
        let tick = self.next_tick - 1;
        for player in &self.players {
            let snapshot = Snapshot {
                tick,
                player: player.id,
                you: player.state,
                others: (self.players.iter())
                    .filter(|other| other.id != player.id)
                    .map(|other| (other.id, other.state))
                    .collect(),
            };
            let payload = postcard::to_allocvec(&snapshot).expect("a snapshot encodes");
            // Only a connection that has just gone refuses: MAX_PLAYERS
            // keeps every snapshot within MAX_LATEST_STATE_BYTES.
            let _ = self.io.send(player.conn, Delivery::LatestState, &payload);
        }
    }
}

// ---------------------------------------------------------------------------
// The client.

/// Ticks of prediction a client keeps: unacknowledged inputs, and the
/// digests of what it predicted, by tick.
const HISTORY: usize = 128;
/// How far behind the newest snapshot other players are drawn, in ticks:
/// room for one late snapshot.
pub const INTERP_TICKS: u64 = 3;
/// How far past the newest snapshot other players are extrapolated before
/// they hold still.
pub const MAX_EXTRAPOLATION_TICKS: u64 = 6;

/// The digest of the state predicted for each recent tick.
struct Predicted {
    slots: Vec<Option<(u64, Digest)>>,
}

impl Predicted {
    fn new() -> Self {
        Self {
            slots: vec![None; HISTORY],
        }
    }

    fn slot(tick: u64) -> usize {
        usize::try_from(tick % HISTORY as u64).expect("below HISTORY")
    }

    fn record(&mut self, tick: u64, state: &MoverState<Vec3>) {
        self.slots[Self::slot(tick)] = Some((tick, digest_of(state)));
    }

    fn get(&self, tick: u64) -> Option<Digest> {
        self.slots[Self::slot(tick)].and_then(|(at, digest)| (at == tick).then_some(digest))
    }
}

/// One other player as the client draws it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Remote {
    pub player: u8,
    /// The newest snapshot tick held for it when drawn.
    pub newest_tick: Option<u64>,
    pub position: Sampled<Vec3>,
}

/// What the client draws this frame.
#[derive(Debug, Clone, PartialEq)]
pub struct Drawn {
    /// The client's last predicted tick, and the clock's `alpha` past it.
    pub tick: u64,
    pub alpha: f32,
    /// The player's own body, between its last two predicted ticks.
    pub own: Vec3,
    pub remotes: Vec<Remote>,
}

/// The predicting half.
pub struct Client<C> {
    io: C,
    sim: Sim,
    lead_ticks: u64,
    clock: FixedClock,
    /// `None` until the first snapshot.
    predictor: Option<Predictor<Input, MoverState<Vec3>>>,
    predicted: Predicted,
    newest: Option<Snapshot>,
    remotes: BTreeMap<u8, SnapshotBuffer<Vec3>>,
    /// Snapshots whose state differed from the client's prediction for the
    /// same tick.
    pub desyncs: u64,
    /// Snapshots the client was not ahead of: it started again from them.
    pub resyncs: u64,
    /// Snapshots taken (late and duplicated ones are not).
    pub snapshots: u64,
    /// Inputs replayed by reconciles.
    pub replayed: u64,
}

impl<C: ClientIo> Client<C> {
    /// `lead_ticks` is how far the client runs ahead of the newest snapshot
    /// when it joins, so its inputs reach the server before their ticks
    /// run: the round trip in ticks plus a margin for jitter. A game
    /// measures it with `ClientIo::rtt`.
    #[must_use]
    pub fn new(io: C, lead_ticks: u64) -> Self {
        assert!(
            usize::try_from(lead_ticks).is_ok_and(|lead| lead < HISTORY),
            "the lead fits the history"
        );
        Self {
            io,
            sim: Sim::new(),
            lead_ticks,
            clock: FixedClock::with_catch_up(TICK_HZ, CATCH_UP),
            predictor: None,
            predicted: Predicted::new(),
            newest: None,
            remotes: BTreeMap::new(),
            desyncs: 0,
            resyncs: 0,
            snapshots: 0,
            replayed: 0,
        }
    }

    /// How other players are drawn: `lead_ticks + INTERP_TICKS` behind the
    /// client's own tick, which is about `INTERP_TICKS` behind the newest
    /// snapshot.
    #[must_use]
    pub fn playback(&self) -> Playback {
        Playback {
            delay_ticks: self.lead_ticks + INTERP_TICKS,
            max_extrapolation_ticks: MAX_EXTRAPOLATION_TICKS,
        }
    }

    /// One client frame of `elapsed` time ending at `now_ms`, with the
    /// input the player holds: take snapshots, predict the ticks due, send
    /// the inputs not yet acknowledged.
    pub fn frame(&mut self, now_ms: u64, elapsed: Duration, input: Input) {
        for event in self.io.poll(now_ms) {
            if let ClientEvent::Message {
                delivery: Delivery::LatestState,
                payload,
            } = event
                && let Ok(snapshot) = postcard::from_bytes::<Snapshot>(&payload)
            {
                self.receive(snapshot);
            }
        }
        if self.predictor.is_some() {
            self.clock.begin_frame(elapsed);
            while self.clock.step() {
                self.predict(input);
            }
            self.clock.finish();
            self.send_inputs();
        }
        self.io.flush(now_ms);
    }

    /// Replay from the server's `state` after `tick`: what a snapshot does.
    /// Public so a test can reconcile from a state of its own.
    pub fn reconcile(&mut self, tick: u64, state: MoverState<Vec3>) {
        let Some(predictor) = &mut self.predictor else {
            return self.start(tick, state);
        };
        let (sim, predicted) = (&self.sim, &mut self.predicted);
        let replay = |state: &mut MoverState<Vec3>, tick: u64, input: &Input| {
            sim.step(state, tick, input);
            predicted.record(tick, state);
        };
        match predictor.reconcile(tick, state, replay) {
            Ok(reconciled) => self.replayed += reconciled.replayed as u64,
            // The server has run ticks the client has not predicted: start
            // again, `lead_ticks` ahead.
            Err((ReconcileError::Ahead { .. }, state)) => {
                self.resyncs += 1;
                self.start(tick, state);
            }
            // Older than the inputs kept: a newer snapshot was applied.
            Err((ReconcileError::Behind { .. }, _)) => {}
        }
    }

    /// The digest of the state the client last predicted for `tick`, while
    /// it is within the history.
    #[must_use]
    pub fn predicted(&self, tick: u64) -> Option<Digest> {
        self.predicted.get(tick)
    }

    /// Reconciles that changed the predicted state.
    #[must_use]
    pub fn corrections(&self) -> u64 {
        self.predictor.as_ref().map_or(0, Predictor::corrections)
    }

    /// The newest snapshot taken.
    #[must_use]
    pub fn snapshot(&self) -> Option<&Snapshot> {
        self.newest.as_ref()
    }

    /// The level as this client keeps it.
    #[must_use]
    pub fn sim(&self) -> &Sim {
        &self.sim
    }

    /// What to draw now: the own body between its last two ticks, the
    /// others from their snapshots.
    pub fn draw(&mut self) -> Option<Drawn> {
        let predictor = self.predictor.as_ref()?;
        let (tick, alpha) = (predictor.next_tick() - 1, self.clock.alpha);
        let lerp = |a: &Vec3, b: &Vec3, t: f32| *a + (*b - *a) * t;
        let own = (predictor.sample()).blend(alpha, |a, b, t| lerp(&a.position, &b.position, t));
        let remotes = (self.remotes.iter_mut())
            .filter_map(|(&player, buffer)| {
                let newest_tick = buffer.newest_tick();
                let position = buffer.sample(tick, alpha, lerp)?;
                Some(Remote {
                    player,
                    newest_tick,
                    position,
                })
            })
            .collect();
        Some(Drawn {
            tick,
            alpha,
            own,
            remotes,
        })
    }

    fn receive(&mut self, snapshot: Snapshot) {
        // Late and duplicated snapshots are older than one already taken.
        if self
            .newest
            .as_ref()
            .is_some_and(|n| snapshot.tick <= n.tick)
        {
            return;
        }
        self.snapshots += 1;
        let playback = self.playback();
        self.remotes
            .retain(|id, _| snapshot.others.iter().any(|(other, _)| other == id));
        for &(id, state) in &snapshot.others {
            let buffer = (self.remotes.entry(id)).or_insert_with(|| {
                let delay = usize::try_from(playback.delay_ticks).expect("below HISTORY");
                SnapshotBuffer::new(delay + 4, playback)
            });
            // A full buffer (the game has not drawn for a while) refuses.
            let _ = buffer.push(snapshot.tick, state.position);
        }
        if let Some(predicted) = self.predicted.get(snapshot.tick)
            && predicted != digest_of(&snapshot.you)
        {
            self.desyncs += 1;
        }
        let (tick, you) = (snapshot.tick, snapshot.you);
        self.newest = Some(snapshot);
        self.reconcile(tick, you);
    }

    /// Start predicting from the server's `state` after `tick`: the ticks up
    /// to `lead_ticks` ahead are predicted idle, as the server runs them
    /// while this client's inputs are on their way.
    fn start(&mut self, tick: u64, state: MoverState<Vec3>) {
        let Some(next) = tick.checked_add(1) else {
            return;
        };
        match &mut self.predictor {
            Some(predictor) => predictor.reset(state, next),
            None => self.predictor = Some(Predictor::new(state, next, HISTORY)),
        }
        for _ in 0..self.lead_ticks {
            self.predict(Input::default());
        }
    }

    fn predict(&mut self, input: Input) {
        let Some(predictor) = &mut self.predictor else {
            return;
        };
        let (sim, predicted) = (&self.sim, &mut self.predicted);
        // A full history (nothing acknowledged for HISTORY ticks: the
        // server has stalled or gone) skips the tick; the next snapshot
        // the client is not ahead of starts it again.
        let _ = predictor.predict(input, |state, tick, input| {
            sim.step(state, tick, input);
            predicted.record(tick, state);
        });
    }

    fn send_inputs(&mut self) {
        let Some(predictor) = &self.predictor else {
            return;
        };
        let pending = predictor.pending();
        let tail = pending.len().saturating_sub(RESEND_TAIL);
        let mut pending = pending.skip(tail).peekable();
        let Some(&(first_tick, _)) = pending.peek() else {
            return;
        };
        let message = Inputs {
            first_tick,
            inputs: pending.map(|(_, input)| *input).collect(),
        };
        let payload = postcard::to_allocvec(&message).expect("inputs encode");
        // A refused send is covered by the next frame's.
        let _ = self.io.send(INPUTS, &payload);
    }
}

// ---------------------------------------------------------------------------
// A scripted session: one server and its clients on one virtual clock.

/// When the scripted players let go of everything.
pub const INPUTS_END_MS: u64 = 4_600;

/// What client `index` holds at `now_ms`: idle while it joins; then a run
/// along +x over the step, up the ramp, off its end and into the wall,
/// with two jumps, odd clients weaving across their lane; then idle.
#[must_use]
pub fn script(index: usize, now_ms: u64) -> Input {
    if !(600..INPUTS_END_MS).contains(&now_ms) {
        return Input::default();
    }
    let weave = if (now_ms / 500).is_multiple_of(2) {
        40
    } else {
        -40
    };
    Input {
        x: 127,
        z: if index.is_multiple_of(2) { 0 } else { weave },
        jump: (1_500..1_650).contains(&now_ms) || (3_000..3_150).contains(&now_ms),
    }
}

/// A server and its clients, stepped together.
pub struct Session<S, C> {
    pub now_ms: u64,
    pub server: Server<S>,
    pub clients: Vec<Client<C>>,
}

impl<S: ServerIo, C: ClientIo> Session<S, C> {
    /// One frame of `elapsed_ms` for the server, then every client playing
    /// the [`script`].
    pub fn frame(&mut self, elapsed_ms: u64) {
        self.now_ms += elapsed_ms;
        let elapsed = Duration::from_millis(elapsed_ms);
        self.server.frame(self.now_ms, elapsed);
        for (index, client) in self.clients.iter_mut().enumerate() {
            client.frame(self.now_ms, elapsed, script(index, self.now_ms));
        }
    }
}

/// Clients joined to the server in memory: no loss and no delay.
#[must_use]
pub fn memory_session(clients: usize, lead_ticks: u64) -> Session<ServerIoMux, MemoryClientIo> {
    let mut server = ServerIoMux::new();
    let clients = (0..clients)
        .map(|_| {
            let (client, end) = memory_duplex();
            server.push(end);
            Client::new(client, lead_ticks)
        })
        .collect();
    Session {
        now_ms: 0,
        server: Server::new(server),
        clients,
    }
}

/// Clients joined over `sgl-net`'s seeded simulated datagram network, with
/// its loss, delay, duplication and reordering.
#[must_use]
pub fn simulated_session(
    clients: u8,
    lead_ticks: u64,
    network: SimulatedConfig,
    seed: u64,
) -> Session<ServerEndpoint<SimulatedTransport>, ClientEndpoint<SimulatedTransport>> {
    let network = SimulatedNetwork::new(network, seed).expect("valid network bounds");
    let address = SocketAddr::from(([10, 0, 0, 1], 4000));
    // A real server draws its cookie key, and a client its nonce, from a
    // random source.
    let server = ServerEndpoint::new(
        network.transport(address),
        EndpointConfig::default(),
        [7; 32],
    )
    .expect("a valid endpoint");
    let clients = (1..=clients)
        .map(|n| {
            let local = network.transport(SocketAddr::from(([10, 0, 1, n], 5000)));
            let io =
                ClientEndpoint::connect(local, address, EndpointConfig::default(), 0, u64::from(n))
                    .expect("a valid endpoint");
            Client::new(io, lead_ticks)
        })
        .collect();
    Session {
        now_ms: 0,
        server: Server::new(server),
        clients,
    }
}
