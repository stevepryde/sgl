//! Fixture server for the browser WebSocket tests (`bun scripts/tasks.ts
//! check-browser`): a `NativeWebSocketServer` on a fixed loopback port that
//! echoes reliable frames reliably and latest-state frames as latest state,
//! and closes a connection when it receives the reliable command `close`.
//! The probe page is served by `browser/lane.test.ts` at a pinned address,
//! which is the only admitted Origin.
//!
//! The workspace also builds examples for wasm32; the fixture is native only.

#[cfg(target_arch = "wasm32")]
fn main() {}

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use std::io::Write;
    use std::time::{Duration, Instant};

    use sgl_net::websocket::{
        GAME_PATH, NativeWebSocketServer, NativeWebSocketServerConfig, OriginPolicy,
        WebSocketIdentity,
    };
    use sgl_net::{Delivery, ServerEvent, ServerIo};

    /// Shared with `crates/sgl-net/tests/browser_ws.rs`.
    pub const PORT: u16 = 8801;
    pub const MAGIC: [u8; 3] = *b"BRW";
    pub const SUBPROTOCOL: &str = "sgl-browser-test.v1";
    pub const PAGE_ORIGIN: &str = "http://127.0.0.1:8123";

    pub fn run() {
        let identity = WebSocketIdentity::new(MAGIC, GAME_PATH, SUBPROTOCOL);
        let mut config = NativeWebSocketServerConfig::new(([127, 0, 0, 1], PORT).into(), identity)
            .with_origin_policy(OriginPolicy::exact([PAGE_ORIGIN.to_owned()]).expect("origin"));
        config.ping_interval_ms = 1_000;
        config.timeout_ms = 10_000;
        let mut server = NativeWebSocketServer::bind(config).expect("bind fixture server");
        println!("listening 127.0.0.1:{PORT}");
        std::io::stdout().flush().expect("flush");

        let started = Instant::now();
        loop {
            let now_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
            for event in server.poll(now_ms) {
                match event {
                    ServerEvent::Message {
                        conn,
                        delivery: Delivery::ReliableOrdered,
                        payload,
                    } if payload == b"close" => server.disconnect(conn, now_ms),
                    ServerEvent::Message {
                        conn,
                        delivery,
                        payload,
                    } => {
                        // Echo failures (a peer past its cap) are the peer's problem.
                        let _ = server.send(conn, delivery, &payload);
                    }
                    ServerEvent::Connected { .. } | ServerEvent::Disconnected { .. } => {}
                }
            }
            server.flush(now_ms);
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    native::run();
}
