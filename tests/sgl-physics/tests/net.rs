//! Physics state survives the trip between a server and its predicting
//! clients (physics.md, "pure and plain" and "ready for the network"; #917).
//! The server, clients, level and step are the
//! [network example's](../../../examples/sgl-physics/net_sim.rs), run over
//! `sgl-net`'s in-memory duplex and its seeded simulated datagram network.
//! Runs natively and in a wasm runtime.

#[allow(dead_code)]
#[path = "../../../examples/sgl-physics/net_sim.rs"]
mod net_sim;

use std::collections::BTreeMap;
use std::time::Duration;

use net_sim::{
    Client, INPUTS_END_MS, Input, MAX_EXTRAPOLATION_TICKS, RAMP, STEP, Session, Sim, level,
    memory_session, script, simulated_session,
};
use sgl_core::digest_of;
use sgl_core::math::Vec3;
use sgl_core::prediction::Sampled;
use sgl_net::udp::simulated::SimulatedConfig;
use sgl_net::{ClientIo, RELIABLE_LANES, ServerIo};
use sgl_physics::MoverState;
use wasm_bindgen_test::wasm_bindgen_test;

const FRAME_MS: u64 = 16;
const SESSION_MS: u64 = 7_000;
/// A second after the inputs stop and the last ones have been answered.
const SETTLED_MS: u64 = 6_000;

/// Every player's state by server tick, read after each server frame.
type History = BTreeMap<u64, BTreeMap<u8, MoverState<Vec3>>>;

fn record<S: ServerIo>(history: &mut History, server: &net_sim::Server<S>) {
    if let Some(tick) = server.tick() {
        let players = server.players().iter().map(|p| (p.id, p.state)).collect();
        history.insert(tick, players);
    }
}

/// Runs the scripted session to `until_ms`, recording the server's states,
/// and returns the first server tick run at or after `from_ms`.
fn run<S: ServerIo, C: ClientIo>(
    session: &mut Session<S, C>,
    history: &mut History,
    until_ms: u64,
    from_ms: u64,
) -> u64 {
    let mut from = None;
    while session.now_ms < until_ms {
        session.frame(FRAME_MS);
        record(history, &session.server);
        if session.now_ms >= from_ms && from.is_none() {
            from = session.server.tick();
        }
    }
    from.expect("the server ran")
}

fn player<C: ClientIo>(client: &Client<C>) -> u8 {
    client.snapshot().expect("the client joined").player
}

/// The client's prediction for every server tick from `from` that it still
/// holds is bit-equal to the server's state for that tick. Returns how many
/// ticks were compared.
fn assert_predictions_match<C: ClientIo>(client: &Client<C>, history: &History, from: u64) {
    let id = player(client);
    let mut compared = 0;
    for (&tick, players) in history.range(from..) {
        if let Some(predicted) = client.predicted(tick) {
            assert_eq!(
                predicted,
                digest_of(&players[&id]),
                "player {id} at tick {tick}"
            );
            compared += 1;
        }
    }
    assert!(compared >= 30, "player {id}: compared {compared} ticks");
}

/// No loss and no extra delay beyond a fixed latency.
fn lossless(one_way_latency_ms: u64) -> SimulatedConfig {
    SimulatedConfig {
        one_way_latency_ms,
        jitter_ms: 0,
        loss_per_10k: 0,
        duplicate_per_10k: 0,
        reorder_per_10k: 0,
        reorder_extra_ms: 0,
        max_in_flight_datagrams: 4_096,
        lane_loss_per_10k: [0; RELIABLE_LANES],
    }
}

/// Defect: a client that does not take the server's state (no reconcile,
/// or one that replays from the wrong tick or with the wrong inputs), so a
/// prediction the server overruled stays wrong. Oracle: the server's state:
/// with 20% loss, 5% duplication, 10% reordering and a lead shorter than
/// the worst round trip, the server runs some ticks on a guessed input and
/// the clients are corrected; a second after the inputs stop, each client's
/// prediction for every tick is bit-equal to the server's state then.
#[wasm_bindgen_test(unsupported = test)]
fn under_loss_the_clients_converge_on_the_server() {
    let mut session = simulated_session(2, 6, SimulatedConfig::default(), 0x5eed);
    let mut history = History::new();
    let settled = run(&mut session, &mut history, SESSION_MS, SETTLED_MS);

    // The session did what it is meant to: the players walked, and the
    // network made the server overrule predictions.
    let guessed: u64 = session.server.players().iter().map(|p| p.guessed).sum();
    let corrections: u64 = session.clients.iter().map(Client::corrections).sum();
    assert!(guessed > 0 && corrections > 0, "{guessed} {corrections}");
    for player in session.server.players() {
        assert!(player.state.position.x > 5.0, "{:?}", player.state);
    }
    for client in &session.clients {
        assert_predictions_match(client, &history, settled);
    }
}

/// Defect: inputs lost on the way, so the server guesses: no resend tail,
/// or a tail that skips ticks. Oracle: the server's input queues: under
/// 20% loss with a lead that covers the round trip, the server never runs
/// a tick on a guessed input.
#[wasm_bindgen_test(unsupported = test)]
fn the_resend_tail_carries_every_input_through_loss() {
    let mut session = simulated_session(2, 16, SimulatedConfig::default(), 0x1055);
    let mut history = History::new();
    run(&mut session, &mut history, INPUTS_END_MS + 500, 0);

    let ticks = session.server.tick().expect("the server ran");
    for client in &session.clients {
        // The network lost snapshots too: the loss was real.
        assert!(client.snapshots * 10 < ticks * 9, "{}", client.snapshots);
    }
    for player in session.server.players() {
        assert_eq!(player.guessed, 0, "player {}", player.id);
        assert!(player.state.position.x > 5.0, "{:?}", player.state);
    }
}

/// Defect: a step that differs between server and client (another gravity,
/// another level, an input read differently), or state that loses bits on
/// the wire. Oracle: the server's state: with no loss, in memory and over a
/// network with a fixed 40 ms latency, every snapshot equals what the
/// client predicted for its tick, so no reconcile ever changes the
/// prediction, and the client's last predictions equal the server's states.
#[wasm_bindgen_test(unsupported = test)]
fn without_loss_every_prediction_is_the_servers_state() {
    fn check<S: ServerIo, C: ClientIo>(mut session: Session<S, C>, what: &str) {
        let mut history = History::new();
        run(&mut session, &mut history, SESSION_MS, 0);
        let from = *history.keys().next().expect("the server ran");
        for client in &session.clients {
            // Snapshots were reconciled against pending inputs throughout.
            assert!(
                client.snapshots > 300 && client.replayed > client.snapshots,
                "{what}: {} {}",
                client.snapshots,
                client.replayed
            );
            assert_eq!(
                (client.corrections(), client.desyncs, client.resyncs),
                (0, 0, 0),
                "{what}"
            );
            assert_predictions_match(client, &history, from);
        }
    }
    check(memory_session(2, 3), "memory");
    check(simulated_session(2, 7, lossless(40), 7), "simulated");
}

/// Defect: a desync that goes unnoticed, or one reconciliation does not
/// repair. Oracle: the perturbation: the client's prediction is replayed
/// from the last snapshot's state with the lowest bit of x flipped; the
/// next snapshot reports exactly one desync, and from then on every
/// prediction is the server's state again.
#[wasm_bindgen_test(unsupported = test)]
fn a_one_bit_desync_is_reported_at_the_next_snapshot_and_repaired() {
    let mut session = memory_session(1, 3);
    let mut history = History::new();
    // Mid-run on flat floor, after the first jump.
    run(&mut session, &mut history, 2_400, 0);
    let client = &mut session.clients[0];
    assert_eq!(client.desyncs, 0);
    let snapshot = client.snapshot().expect("joined").clone();
    let mut perturbed = snapshot.you;
    perturbed.position.x = f32::from_bits(perturbed.position.x.to_bits() ^ 1);
    client.reconcile(snapshot.tick, perturbed);

    while session.clients[0].snapshot().map(|s| s.tick) == Some(snapshot.tick) {
        session.frame(FRAME_MS);
        record(&mut history, &session.server);
    }
    assert_eq!(
        session.clients[0].desyncs, 1,
        "reported at the next snapshot"
    );
    let repaired = session.server.tick().expect("ran");
    run(&mut session, &mut history, SESSION_MS, 0);
    assert_eq!(session.clients[0].desyncs, 1, "and never again");
    assert_predictions_match(&session.clients[0], &history, repaired + 1);
}

/// Defect: a step that depends on more than the state and the level's
/// contents: on the world a client happened to keep, or on the order its
/// collider set was built in. Oracle: a client that kept its world: every
/// snapshot it took, restored into a world rebuilt from the level in
/// reverse order and stepped with each input, gives the same next state,
/// bit for bit, as in the kept world.
#[wasm_bindgen_test(unsupported = test)]
fn a_snapshot_steps_the_same_in_a_rebuilt_world() {
    let inputs = [
        Input::default(),
        Input {
            x: 127,
            z: -40,
            jump: true,
        },
        Input {
            x: -90,
            z: 127,
            jump: false,
        },
    ];
    let mut session = memory_session(2, 3);
    let mut taken = [None; 2];
    let mut floors = Vec::new();
    while session.now_ms < SESSION_MS {
        session.frame(FRAME_MS);
        for (client, taken) in session.clients.iter().zip(&mut taken) {
            let Some(snapshot) = client.snapshot() else {
                continue;
            };
            if taken.replace(snapshot.tick) == Some(snapshot.tick) {
                continue;
            }
            let rebuilt = Sim::from_colliders(level().into_iter().rev());
            for input in &inputs {
                let (mut kept, mut fresh) = (snapshot.you, snapshot.you);
                client.sim().step(&mut kept, snapshot.tick + 1, input);
                rebuilt.step(&mut fresh, snapshot.tick + 1, input);
                assert_eq!(
                    digest_of(&kept),
                    digest_of(&fresh),
                    "tick {}",
                    snapshot.tick
                );
                floors.extend(kept.floor.map(|floor| floor.id));
            }
        }
    }
    // The restored bodies stood on the step and the ramp, not only the floor.
    assert!(floors.contains(&STEP) && floors.contains(&RAMP));
}

/// Defect: another player drawn where the server never had it: ahead of
/// its snapshots, outside the two around the instant drawn, or carried on
/// without bound when snapshots stop. Oracle: the server's states: drawn
/// between snapshots, a player lies between the server's states at the two
/// ticks around the instant drawn; while the server stalls, it moves past
/// the newest snapshot by at most `MAX_EXTRAPOLATION_TICKS` times its last
/// step there.
#[wasm_bindgen_test(unsupported = test)]
fn other_players_are_drawn_between_the_servers_states() {
    const FRAME_MS: u64 = 10;
    const STALL_MS: u64 = 2_500;
    const END_MS: u64 = 2_800;
    // 10 ms frames: the server runs at most one tick a frame, so every
    // tick's snapshot reaches the clients.
    let mut session = memory_session(2, 3);
    let mut history = History::new();
    let (mut between, mut beyond, mut moving) = (0, 0, 0);
    while session.now_ms < END_MS {
        session.now_ms += FRAME_MS;
        let elapsed = Duration::from_millis(FRAME_MS);
        if session.now_ms < STALL_MS {
            session.server.frame(session.now_ms, elapsed);
            record(&mut history, &session.server);
        }
        for (index, client) in session.clients.iter_mut().enumerate() {
            client.frame(session.now_ms, elapsed, script(index, session.now_ms));
            let delay = client.playback().delay_ticks;
            let Some(drawn) = client.draw() else {
                continue;
            };
            // The instant drawn is `before + alpha`.
            let Some(before) = drawn.tick.checked_sub(delay) else {
                continue;
            };
            let alpha = drawn.alpha;
            for remote in drawn.remotes {
                let at = |tick: u64| history.get(&tick).map(|p| p[&remote.player].position);
                match remote.position {
                    Sampled::Interpolated(drawn) => {
                        let a = at(before).expect("recorded");
                        let b = at(before + 1).unwrap_or(a);
                        let (lo, hi) = (a.min(b), a.max(b));
                        let slack = Vec3::splat(1e-5) * (Vec3::ONE + lo.abs().max(hi.abs()));
                        assert!(
                            drawn.cmpge(lo - slack).all() && drawn.cmple(hi + slack).all(),
                            "{drawn} outside {a} {b} at {before} + {alpha}"
                        );
                        between += 1;
                    }
                    Sampled::Extrapolated(drawn) | Sampled::Held(drawn) => {
                        let newest = remote.newest_tick.expect("held");
                        let last = at(newest).expect("recorded");
                        let step = last - at(newest - 1).expect("recorded");
                        let ticks = u16::try_from(MAX_EXTRAPOLATION_TICKS).expect("a few");
                        let reach = step.abs() * f32::from(ticks) + 1e-4;
                        assert!(
                            (drawn - last).abs().cmple(reach).all(),
                            "{drawn} beyond {last} by more than {reach}"
                        );
                        beyond += 1;
                        moving += usize::from(step.x != 0.0);
                    }
                    Sampled::Early(_) => {}
                }
            }
        }
    }
    assert!(
        between > 300 && beyond > 20 && moving > 0,
        "{between} {beyond} {moving}"
    );
}
