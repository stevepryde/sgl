//! A server and two predicting clients moving bodies with
//! `sgl_physics::move_and_slide` over `sgl-net`, the model a networked game
//! copies: the server steps every player from its queued inputs; each
//! client predicts its own player with the same step, reconciles on every
//! snapshot by replaying what the server has not acknowledged, draws the
//! other player between snapshots, and compares digests to find a desync.
//!
//! The shared half (level, input, step, messages, server and client) is
//! [`net_sim.rs`](../net_sim.rs); this file runs a scripted session over
//! `sgl-net`'s seeded simulated datagram network, with loss, delay,
//! duplication, reordering and a server stall, and prints what happened.
//! It needs no window and opens no socket: swap the simulated transports
//! for `UdpServer` and `UdpClient` (or WebSocket ones in a browser) and
//! feed real time to run it for real. Everything here also builds for
//! `wasm32-unknown-unknown`.
//!
//! Run with `cargo run -p sgl-physics-examples --example net_mover`.

// The tests use the parts this session does not.
#[allow(dead_code)]
#[path = "../net_sim.rs"]
mod net_sim;

use net_sim::{INPUTS_END_MS, script, simulated_session};
use sgl_core::digest_of;
use sgl_core::prediction::Sampled;
use sgl_net::udp::simulated::SimulatedConfig;
use std::time::Duration;

/// Ticks a client runs ahead of the newest snapshot: about a round trip of
/// the network below (2 × 50 ms, at 60 Hz) and a margin for its jitter.
const LEAD_TICKS: u64 = 12;
const FRAME_MS: u64 = 16;
const SESSION_MS: u64 = 7_000;
/// The server stalls for 250 ms from here; its `CatchUp` clock runs the
/// missed ticks over the following frames.
const STALL_MS: u64 = 2_000;
const STALL_LENGTH_MS: u64 = 250;

fn main() {
    let network = SimulatedConfig::default();
    println!(
        "net_mover: 2 clients over a simulated network: {} ms ± {} one way, \
         {}% lost, {}% duplicated, {}% delayed {} ms more",
        network.one_way_latency_ms,
        network.jitter_ms,
        network.loss_per_10k / 100,
        network.duplicate_per_10k / 100,
        network.reorder_per_10k / 100,
        network.reorder_extra_ms,
    );
    let mut session = simulated_session(2, LEAD_TICKS, network, 0x5eed);
    let mut stalled = 0;
    let mut extrapolated = 0;
    while session.now_ms < SESSION_MS {
        let server_late = (STALL_MS..STALL_MS + STALL_LENGTH_MS).contains(&session.now_ms);
        session.now_ms += FRAME_MS;
        stalled += FRAME_MS;
        // A late server frame is given all the time it missed.
        if !server_late {
            let elapsed = Duration::from_millis(std::mem::take(&mut stalled));
            session.server.frame(session.now_ms, elapsed);
        }
        for (index, client) in session.clients.iter_mut().enumerate() {
            let input = script(index, session.now_ms);
            client.frame(session.now_ms, Duration::from_millis(FRAME_MS), input);
            // A game draws every frame: its own body predicted, the other
            // player between snapshots.
            if let Some(drawn) = client.draw() {
                extrapolated += (drawn.remotes.iter())
                    .filter(|remote| !matches!(remote.position, Sampled::Interpolated(_)))
                    .count();
            }
        }
    }

    let server = &session.server;
    let last = server.tick().expect("the server ran");
    println!(
        "server: {} ticks; inputs stopped at {INPUTS_END_MS} ms; {} frames drew \
         another player past its newest snapshot",
        last + 1,
        extrapolated,
    );
    for client in &session.clients {
        let Some(id) = client.snapshot().map(|snapshot| snapshot.player) else {
            println!("a client never joined");
            continue;
        };
        let player = (server.players().iter())
            .find(|player| player.id == id)
            .expect("joined players stay");
        // The client predicted the server's last tick before the server ran
        // it; after the inputs stopped, the two must agree to the bit.
        let agrees = client.predicted(last) == Some(digest_of(&player.state));
        println!(
            "player {id}: {} snapshots replayed {} inputs; {} corrections, \
             {} desyncs, {} resyncs; the server guessed {} inputs; \
             final digest {}",
            client.snapshots,
            client.replayed,
            client.corrections(),
            client.desyncs,
            client.resyncs,
            player.guessed,
            if agrees { "matches" } else { "DIFFERS" },
        );
    }
}
