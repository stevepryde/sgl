//! Browser lane probe (testing.md 5): `BrowserWebSocketClient` exercised
//! inside a real browser page against the native fixture server
//! (`ws_fixture_server`). Built for wasm32 as a `cdylib`, bound with
//! `wasm-bindgen --target web`, and driven by Playwright from
//! `browser/lane.test.ts` via `bun scripts/tasks.ts check-browser`.
//!
//! Each scenario returns `Err(detail)` instead of panicking so the whole
//! report reaches the page. Assertions are what a browser game observes:
//! connection, echoed bytes, coalesced latest state, the cap refusal and the
//! bounded reconnect that follows, a server-initiated close, a rejected
//! subprotocol, and a clean local disconnect.
#![cfg(target_arch = "wasm32")]
#![allow(clippy::future_not_send)]

use sgl_net::websocket::{
    BrowserWebSocketClient, BrowserWebSocketConfig, GAME_PATH, ReconnectPolicy, WebSocketIdentity,
};
use sgl_net::{ClientEvent, ClientIo, Delivery, DisconnectReason, SendError};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

// Shared with `crates/sgl-net/examples/ws_fixture_server.rs`.
const PORT: u16 = 8801;
const MAGIC: [u8; 3] = *b"BRW";
const SUBPROTOCOL: &str = "sgl-browser-test.v1";

macro_rules! ensure {
    ($cond:expr, $($arg:tt)+) => {
        if !$cond {
            return Err(format!($($arg)+));
        }
    };
}

fn url() -> String {
    format!("ws://127.0.0.1:{PORT}{GAME_PATH}")
}

fn identity() -> WebSocketIdentity {
    WebSocketIdentity::new(MAGIC, GAME_PATH, SUBPROTOCOL)
}

fn config() -> BrowserWebSocketConfig {
    let mut config = BrowserWebSocketConfig::new(url(), identity());
    config.reconnect = ReconnectPolicy {
        max_attempts: 3,
        initial_delay_ms: 50,
        max_delay_ms: 200,
    };
    config
}

async fn sleep_ms(ms: i32) {
    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        web_sys::window()
            .expect("window")
            .set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms)
            .expect("setTimeout");
    });
    let _ = JsFuture::from(promise).await;
}

/// A poll-driven clock: real 10 ms sleeps between polls, virtual `now_ms`
/// advanced by the same amount so the client's reconnect timers run.
struct Driver {
    client: BrowserWebSocketClient,
    now_ms: u64,
    events: Vec<ClientEvent>,
}

impl Driver {
    fn new(config: BrowserWebSocketConfig) -> Result<Self, String> {
        let client = BrowserWebSocketClient::connect(config)
            .map_err(|e| format!("client construction: {e:?}"))?;
        Ok(Self {
            client,
            now_ms: 0,
            events: Vec::new(),
        })
    }

    async fn connect(config: BrowserWebSocketConfig) -> Result<Self, String> {
        let mut driver = Self::new(config)?;
        driver
            .settle(|events| events.contains(&ClientEvent::Connected))
            .await?;
        Ok(driver)
    }

    /// Poll and flush until `done` sees what it wants in the accumulated
    /// events, or five seconds pass.
    async fn settle(&mut self, done: impl Fn(&[ClientEvent]) -> bool) -> Result<(), String> {
        for _ in 0..500 {
            self.now_ms += 10;
            self.events.extend(self.client.poll(self.now_ms));
            self.client.flush(self.now_ms);
            if done(&self.events) {
                return Ok(());
            }
            sleep_ms(10).await;
        }
        Err(format!("timed out; events so far: {:?}", self.events))
    }

    fn payloads(&self, delivery: Delivery) -> Vec<Vec<u8>> {
        self.events
            .iter()
            .filter_map(|event| match event {
                ClientEvent::Message {
                    delivery: d,
                    payload,
                } if *d == delivery => Some(payload.clone()),
                _ => None,
            })
            .collect()
    }

    fn count(&self, wanted: &ClientEvent) -> usize {
        self.events.iter().filter(|e| *e == wanted).count()
    }
}

/// Defect: a `Closure` dropped early (events stop), the wrong `binaryType`,
/// or reliable frames reordered through the browser buffer. Oracle: the
/// fixture echoes exactly what it received, in order.
async fn reliable_binary_payloads_echo_in_order() -> Result<(), String> {
    let mut driver = Driver::connect(config()).await?;
    let corpus: Vec<Vec<u8>> = vec![
        vec![],
        vec![0, 0xFF, 0x80, b'\n', 0xC0, 0xAF],
        (0..1500u32).map(|i| (i * 7 % 256) as u8).collect(),
        b"last".to_vec(),
    ];
    for payload in &corpus {
        driver
            .client
            .send(Delivery::ReliableOrdered, payload)
            .map_err(|e| format!("send: {e:?}"))?;
    }
    let want = corpus.len();
    driver
        .settle(|events| {
            events
                .iter()
                .filter(|e| {
                    matches!(
                        e,
                        ClientEvent::Message {
                            delivery: Delivery::ReliableOrdered,
                            ..
                        }
                    )
                })
                .count()
                >= want
        })
        .await?;
    let got = driver.payloads(Delivery::ReliableOrdered);
    ensure!(got == corpus, "echo mismatch: {got:?}");
    Ok(())
}

/// Defect: latest-state frames sent one per value instead of coalesced, or
/// the newest value lost. Oracle: the newest value arrives and values never
/// go backwards.
async fn latest_state_coalesces_to_the_newest_value() -> Result<(), String> {
    let mut driver = Driver::connect(config()).await?;
    for i in 0..50u8 {
        driver
            .client
            .send(Delivery::LatestState, &[b'L', i])
            .map_err(|e| format!("send: {e:?}"))?;
    }
    driver
        .settle(|events| {
            events.iter().any(|e| {
                matches!(e, ClientEvent::Message { delivery: Delivery::LatestState, payload } if payload == &[b'L', 49])
            })
        })
        .await?;
    let seen = driver.payloads(Delivery::LatestState);
    ensure!(
        seen.windows(2).all(|w| w[0][1] < w[1][1]),
        "latest regressed: {seen:?}"
    );
    ensure!(
        seen.len() < 50,
        "every value was sent separately: {}",
        seen.len()
    );
    Ok(())
}

/// Defect: `bufferedAmount` miscounted so the reliable cap never trips, or a
/// tripped cap that neither reports nor recovers. Oracle: a payload past the
/// configured watermark is refused with `ReliableOverflow`, the connection
/// is reported lost for that reason, and the bounded policy reconnects.
async fn a_reliable_send_past_the_watermark_is_refused_then_reconnects() -> Result<(), String> {
    let mut config = config();
    config.reliable_buffered_bytes = 128;
    let mut driver = Driver::connect(config).await?;
    let refused = driver.client.send(Delivery::ReliableOrdered, &[1; 200]);
    ensure!(
        refused == Err(SendError::ReliableOverflow),
        "expected ReliableOverflow, got {refused:?}"
    );
    let lost = ClientEvent::Disconnected {
        reason: DisconnectReason::ReliableOverflow,
    };
    driver
        .settle(|events| {
            events.contains(&lost)
                && events
                    .iter()
                    .filter(|e| **e == ClientEvent::Connected)
                    .count()
                    >= 2
        })
        .await?;
    ensure!(
        driver
            .events
            .iter()
            .any(|e| matches!(e, ClientEvent::Reconnecting { attempt: 1 })),
        "no reconnect attempt: {:?}",
        driver.events
    );
    driver.events.clear();
    driver
        .client
        .send(Delivery::ReliableOrdered, b"after")
        .map_err(|e| format!("send after reconnect: {e:?}"))?;
    driver
        .settle(|events| {
            events
                .iter()
                .any(|e| matches!(e, ClientEvent::Message { payload, .. } if payload == b"after"))
        })
        .await
}

/// Defect: a server-initiated close surfacing as a transport error, or the
/// reconnect loop ignoring `poll`'s clock. Oracle: `Disconnected { Peer }`,
/// then `Reconnecting`, then `Connected` again, all driven by polls.
async fn a_server_close_is_reported_as_peer_and_reconnects() -> Result<(), String> {
    let mut driver = Driver::connect(config()).await?;
    driver
        .client
        .send(Delivery::ReliableOrdered, b"close")
        .map_err(|e| format!("send: {e:?}"))?;
    driver
        .settle(|events| {
            events
                .iter()
                .filter(|e| **e == ClientEvent::Connected)
                .count()
                >= 2
        })
        .await?;
    let closed = driver.events.iter().position(|e| {
        *e == ClientEvent::Disconnected {
            reason: DisconnectReason::Peer,
        }
    });
    let reconnecting = driver
        .events
        .iter()
        .position(|e| matches!(e, ClientEvent::Reconnecting { attempt: 1 }));
    ensure!(
        matches!((closed, reconnecting), (Some(c), Some(r)) if c < r),
        "expected Peer close then reconnect: {:?}",
        driver.events
    );
    Ok(())
}

/// Defect: a handshake that ignores the subprotocol. Oracle: the fixture
/// refuses another subprotocol, so the client never connects and reports
/// the loss instead.
async fn a_wrong_subprotocol_never_connects() -> Result<(), String> {
    let mut config = config();
    config.identity = WebSocketIdentity::new(MAGIC, GAME_PATH, "other.v9");
    let mut driver = Driver::new(config)?;
    driver
        .settle(|events| {
            events
                .iter()
                .any(|e| matches!(e, ClientEvent::Disconnected { .. }))
        })
        .await?;
    ensure!(
        driver.count(&ClientEvent::Connected) == 0,
        "connected with the wrong subprotocol: {:?}",
        driver.events
    );
    Ok(())
}

/// Defect: a local disconnect that triggers the reconnect policy. Oracle:
/// `Disconnected { Local }` and no `Reconnecting`.
async fn a_local_disconnect_is_clean_and_final() -> Result<(), String> {
    let mut driver = Driver::connect(config()).await?;
    let now = driver.now_ms;
    driver.client.disconnect(now);
    let local = ClientEvent::Disconnected {
        reason: DisconnectReason::Local,
    };
    driver.settle(|events| events.contains(&local)).await?;
    for _ in 0..30 {
        driver.now_ms += 10;
        driver.events.extend(driver.client.poll(driver.now_ms));
        sleep_ms(10).await;
    }
    ensure!(
        !driver
            .events
            .iter()
            .any(|e| matches!(e, ClientEvent::Reconnecting { .. })),
        "reconnected after a local close: {:?}",
        driver.events
    );
    Ok(())
}

fn record(report: &mut String, name: &str, result: Result<(), String>) {
    match result {
        Ok(()) => report.push_str(&format!("ok {name}\n")),
        Err(detail) => report.push_str(&format!("FAIL {name}: {detail}\n")),
    }
}

/// Run every scenario and return one line per scenario: `ok <name>` or
/// `FAIL <name>: <detail>`.
#[wasm_bindgen]
pub async fn run() -> String {
    let mut report = String::new();
    record(
        &mut report,
        "reliable_binary_payloads_echo_in_order",
        reliable_binary_payloads_echo_in_order().await,
    );
    record(
        &mut report,
        "latest_state_coalesces_to_the_newest_value",
        latest_state_coalesces_to_the_newest_value().await,
    );
    record(
        &mut report,
        "a_reliable_send_past_the_watermark_is_refused_then_reconnects",
        a_reliable_send_past_the_watermark_is_refused_then_reconnects().await,
    );
    record(
        &mut report,
        "a_server_close_is_reported_as_peer_and_reconnects",
        a_server_close_is_reported_as_peer_and_reconnects().await,
    );
    record(
        &mut report,
        "a_wrong_subprotocol_never_connects",
        a_wrong_subprotocol_never_connects().await,
    );
    record(
        &mut report,
        "a_local_disconnect_is_clean_and_final",
        a_local_disconnect_is_clean_and_final().await,
    );
    report
}
