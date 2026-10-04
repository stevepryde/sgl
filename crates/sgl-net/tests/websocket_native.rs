#![cfg(not(target_arch = "wasm32"))]

use std::net::TcpStream;
use std::thread;
use std::time::{Duration, Instant};

use sgl_net::websocket::{
    ENVELOPE_HEADER_LEN, ENVELOPE_VERSION, GAME_PATH, NativeWebSocketClient,
    NativeWebSocketClientConfig, NativeWebSocketServer, NativeWebSocketServerConfig, OriginPolicy,
    WebSocketIdentity, encode_envelope,
};
use sgl_net::{
    ClientEvent, ClientIo, ConnectionId, Delivery, DisconnectReason, MAX_LATEST_STATE_BYTES,
    RELIABLE_OUTBOUND_MESSAGES, SendError, ServerEvent, ServerIo,
};
use tungstenite::client::IntoClientRequest;
use tungstenite::http::HeaderValue;
use tungstenite::http::header::{ORIGIN, SEC_WEBSOCKET_PROTOCOL};
use tungstenite::protocol::Message;
use tungstenite::{connect, stream::MaybeTlsStream};

const ORIGIN_VALUE: &str = "http://localhost";

fn identity() -> WebSocketIdentity {
    WebSocketIdentity::new(*b"TST", GAME_PATH, "test.v1")
}

fn server(max_connections: usize) -> NativeWebSocketServer {
    let mut config = NativeWebSocketServerConfig::new(([127, 0, 0, 1], 0).into(), identity())
        .with_origin_policy(OriginPolicy::exact([ORIGIN_VALUE.to_owned()]).unwrap());
    config.max_connections = max_connections;
    NativeWebSocketServer::bind(config).unwrap()
}

fn url(server: &NativeWebSocketServer) -> String {
    format!("ws://{}{GAME_PATH}", server.local_addr())
}

fn connect_native(server: &NativeWebSocketServer) -> NativeWebSocketClient {
    NativeWebSocketClient::connect(NativeWebSocketClientConfig::new(
        url(server),
        ORIGIN_VALUE,
        identity(),
    ))
    .unwrap()
}

fn wait_until<T>(timeout: Duration, mut poll: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(value) = poll() {
            return value;
        }
        assert!(Instant::now() < deadline, "timed out");
        thread::sleep(Duration::from_millis(1));
    }
}

fn wait_server_events(server: &mut NativeWebSocketServer) -> Vec<ServerEvent> {
    wait_until(Duration::from_secs(2), || {
        let events = server.poll(0);
        (!events.is_empty()).then_some(events)
    })
}

fn collect_server_events(server: &mut NativeWebSocketServer, count: usize) -> Vec<ServerEvent> {
    let mut collected = Vec::new();
    wait_until(Duration::from_secs(2), || {
        collected.extend(server.poll(0));
        (collected.len() >= count).then_some(collected.clone())
    })
}

fn collect_client_events(client: &mut NativeWebSocketClient, count: usize) -> Vec<ClientEvent> {
    let mut collected = Vec::new();
    wait_until(Duration::from_secs(2), || {
        collected.extend(client.poll(0));
        (collected.len() >= count).then_some(collected.clone())
    })
}

fn connected_id(events: &[ServerEvent]) -> ConnectionId {
    events
        .iter()
        .find_map(|event| match event {
            ServerEvent::Connected { conn } => Some(*conn),
            _ => None,
        })
        .expect("connected event")
}

#[test]
fn native_client_and_server_are_trait_transparent_on_both_lanes() {
    let mut server = server(4);
    let mut client = connect_native(&server);
    assert_eq!(client.poll(0), vec![ClientEvent::Connected]);
    let conn = connected_id(&wait_server_events(&mut server));

    client
        .send(Delivery::ReliableOrdered, b"client event")
        .unwrap();
    client.send(Delivery::LatestState, b"old").unwrap();
    client.send(Delivery::LatestState, b"new").unwrap();
    client.flush(10);

    let received = collect_server_events(&mut server, 2);
    assert_eq!(
        received,
        vec![
            ServerEvent::Message {
                conn,
                delivery: Delivery::ReliableOrdered,
                payload: b"client event".to_vec(),
            },
            ServerEvent::Message {
                conn,
                delivery: Delivery::LatestState,
                payload: b"new".to_vec(),
            },
        ]
    );

    server
        .send(conn, Delivery::ReliableOrdered, b"server event")
        .unwrap();
    for value in 0_u8..100 {
        server.send(conn, Delivery::LatestState, &[value]).unwrap();
    }
    server.flush(20);
    let received = collect_client_events(&mut client, 2);
    assert_eq!(
        received,
        vec![
            ClientEvent::Message {
                delivery: Delivery::ReliableOrdered,
                payload: b"server event".to_vec(),
            },
            ClientEvent::Message {
                delivery: Delivery::LatestState,
                payload: vec![99],
            },
        ]
    );
}

#[test]
fn reliable_overflow_disconnects_only_the_overflowing_peer() {
    let mut server = server(4);
    let mut first = connect_native(&server);
    let first_conn = connected_id(&wait_server_events(&mut server));
    let mut second = connect_native(&server);
    let second_conn = connected_id(&wait_server_events(&mut server));
    first.poll(0);
    second.poll(0);

    for _ in 0..RELIABLE_OUTBOUND_MESSAGES {
        server
            .send(first_conn, Delivery::ReliableOrdered, b"x")
            .unwrap();
    }
    assert_eq!(
        server.send(first_conn, Delivery::ReliableOrdered, b"overflow"),
        Err(SendError::ReliableOverflow)
    );
    server
        .send(second_conn, Delivery::ReliableOrdered, b"still alive")
        .unwrap();
    server.flush(0);

    let terminal = wait_server_events(&mut server);
    assert!(terminal.contains(&ServerEvent::Disconnected {
        conn: first_conn,
        reason: DisconnectReason::ReliableOverflow,
    }));
    let second_events = collect_client_events(&mut second, 1);
    assert_eq!(
        second_events,
        vec![ClientEvent::Message {
            delivery: Delivery::ReliableOrdered,
            payload: b"still alive".to_vec(),
        }]
    );
}

fn raw_request(url: &str, origins: &[&str]) -> tungstenite::http::Request<()> {
    let mut request = url.into_client_request().unwrap();
    request
        .headers_mut()
        .insert(SEC_WEBSOCKET_PROTOCOL, HeaderValue::from_static("test.v1"));
    for origin in origins {
        request
            .headers_mut()
            .append(ORIGIN, HeaderValue::from_str(origin).unwrap());
    }
    request
}

#[test]
fn origin_admission_rejects_missing_duplicate_null_and_lookalike_values() {
    let server = server(8);
    let endpoint = url(&server);
    for origins in [
        vec![],
        vec![ORIGIN_VALUE, ORIGIN_VALUE],
        vec!["null"],
        vec!["http://localhost.evil"],
        vec!["http://localhost/"],
    ] {
        let result = connect(raw_request(&endpoint, &origins));
        assert!(result.is_err(), "admitted origins {origins:?}");
    }
    assert!(connect(raw_request(&endpoint, &[ORIGIN_VALUE])).is_ok());
}

fn assert_protocol_disconnect(message_sequence: Vec<Message>) {
    let mut server = server(2);
    let (mut socket, _) = connect(raw_request(&url(&server), &[ORIGIN_VALUE])).unwrap();
    if let MaybeTlsStream::Plain(stream) = socket.get_mut() {
        stream.set_nonblocking(false).unwrap();
    }
    let conn = connected_id(&wait_server_events(&mut server));
    for message in message_sequence {
        socket.send(message).unwrap();
    }
    let events = wait_until(Duration::from_secs(2), || {
        let events = server.poll(0);
        events
            .iter()
            .any(|event| {
                matches!(
                    event,
                    ServerEvent::Disconnected {
                        reason: DisconnectReason::ProtocolViolation,
                        ..
                    }
                )
            })
            .then_some(events)
    });
    assert_eq!(
        events
            .iter()
            .filter(
                |event| matches!(event, ServerEvent::Disconnected { conn: id, .. } if *id == conn)
            )
            .count(),
        1
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, ServerEvent::Message { .. }))
    );
}

#[test]
fn text_malformed_oversize_wrong_class_and_stale_frames_fail_closed() {
    assert_protocol_disconnect(vec![Message::Text("no text".into())]);
    assert_protocol_disconnect(vec![Message::Binary(vec![1, 2, 3].into())]);

    let mut oversized = Vec::with_capacity(ENVELOPE_HEADER_LEN + MAX_LATEST_STATE_BYTES + 1);
    oversized.extend_from_slice(b"TST");
    oversized.push(ENVELOPE_VERSION);
    oversized.push(1);
    oversized.extend_from_slice(&1_u64.to_be_bytes());
    oversized.extend_from_slice(
        &u32::try_from(MAX_LATEST_STATE_BYTES + 1)
            .unwrap()
            .to_be_bytes(),
    );
    oversized.resize(ENVELOPE_HEADER_LEN + MAX_LATEST_STATE_BYTES + 1, 0);
    assert_protocol_disconnect(vec![Message::Binary(oversized.into())]);

    let mut wrong_class =
        encode_envelope(*b"TST", Delivery::ReliableOrdered, 0, b"queued").unwrap();
    wrong_class[5..13].copy_from_slice(&1_u64.to_be_bytes());
    assert_protocol_disconnect(vec![
        Message::Binary(
            encode_envelope(*b"TST", Delivery::ReliableOrdered, 0, b"must be purged")
                .unwrap()
                .into(),
        ),
        Message::Binary(wrong_class.into()),
    ]);

    assert_protocol_disconnect(vec![
        Message::Binary(
            encode_envelope(*b"TST", Delivery::LatestState, 2, b"new")
                .unwrap()
                .into(),
        ),
        Message::Binary(
            encode_envelope(*b"TST", Delivery::LatestState, 1, b"stale")
                .unwrap()
                .into(),
        ),
    ]);
}

#[test]
fn ping_timeout_advances_only_from_the_caller_clock() {
    let mut config = NativeWebSocketServerConfig::new(([127, 0, 0, 1], 0).into(), identity())
        .with_origin_policy(OriginPolicy::allow_any());
    config.ping_interval_ms = 10;
    config.timeout_ms = 30;
    let mut server = NativeWebSocketServer::bind(config).unwrap();
    let (_socket, _) = connect(raw_request(&url(&server), &[ORIGIN_VALUE])).unwrap();
    let conn = connected_id(&wait_server_events(&mut server));

    server.poll(10);
    thread::sleep(Duration::from_millis(5));
    assert!(server.poll(10).is_empty());
    let events = server.poll(30);
    assert_eq!(
        events,
        vec![ServerEvent::Disconnected {
            conn,
            reason: DisconnectReason::TimedOut,
        }]
    );
    assert!(server.poll(u64::MAX).is_empty());
}

#[test]
fn disconnect_flushes_accepted_reliable_before_close() {
    let mut server = server(4);
    let mut client = connect_native(&server);
    client.poll(0);
    let conn = connected_id(&wait_server_events(&mut server));

    client
        .send(Delivery::ReliableOrdered, b"last client event")
        .unwrap();
    client.disconnect(10);
    let events = collect_server_events(&mut server, 2);
    assert_eq!(
        events[0],
        ServerEvent::Message {
            conn,
            delivery: Delivery::ReliableOrdered,
            payload: b"last client event".to_vec(),
        }
    );
    assert!(matches!(events[1], ServerEvent::Disconnected { conn: id, .. } if id == conn));

    let mut client = connect_native(&server);
    client.poll(0);
    let conn = connected_id(&wait_server_events(&mut server));
    server
        .send(conn, Delivery::ReliableOrdered, b"last server event")
        .unwrap();
    server.disconnect(conn, 20);
    let events = collect_client_events(&mut client, 2);
    assert_eq!(
        events[0],
        ClientEvent::Message {
            delivery: Delivery::ReliableOrdered,
            payload: b"last server event".to_vec(),
        }
    );
    assert!(matches!(events[1], ClientEvent::Disconnected { .. }));
}

#[test]
fn handshake_in_flight_when_admission_stops_is_not_admitted() {
    use std::io::Write;

    let mut server = server(4);
    // Open the TCP connection so the I/O worker accepts it and starts the
    // upgrade, then stop admission before completing the HTTP upgrade.
    let mut stream = TcpStream::connect(server.local_addr()).unwrap();
    thread::sleep(Duration::from_millis(20));
    server.stop_admission();
    let request = format!(
        "GET {GAME_PATH} HTTP/1.1\r\n\
         Host: {}\r\n\
         Upgrade: websocket\r\n\
         Connection: Upgrade\r\n\
         Sec-WebSocket-Key: AAAAAAAAAAAAAAAAAAAAAA==\r\n\
         Sec-WebSocket-Version: 13\r\n\
         Sec-WebSocket-Protocol: test.v1\r\n\
         Origin: {ORIGIN_VALUE}\r\n\r\n",
        server.local_addr()
    );
    stream.write_all(request.as_bytes()).unwrap();

    let deadline = Instant::now() + Duration::from_millis(300);
    while Instant::now() < deadline {
        assert!(
            server.poll(0).is_empty(),
            "peer admitted after stop_admission"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn native_client_handshake_timeout_is_bounded() {
    let listener =
        std::net::TcpListener::bind(std::net::SocketAddr::from(([127, 0, 0, 1], 0))).unwrap();
    let address = listener.local_addr().unwrap();
    let stalled = thread::spawn(move || {
        let (_stream, _) = listener.accept().unwrap();
        thread::sleep(Duration::from_secs(1));
    });
    let mut config = NativeWebSocketClientConfig::new(
        format!("ws://{address}{GAME_PATH}"),
        ORIGIN_VALUE,
        identity(),
    );
    config.handshake_timeout = Duration::from_millis(40);
    let result = NativeWebSocketClient::connect(config);
    match result {
        Ok(_) => panic!("handshake against a silent TCP peer must fail"),
        Err(error) => assert_eq!(error.kind(), std::io::ErrorKind::TimedOut, "{error}"),
    }
    let _ = stalled.join();
}

/// Defect: the client's I/O worker waits for socket readiness before its
/// first turn, so a frame that arrived with the upgrade response — read into
/// tungstenite's buffer during the handshake — is never delivered. Oracle: a
/// raw server that sends the frame in the same write as its 101 response and
/// then stays silent, so no later readiness event can rescue it.
#[test]
fn native_client_delivers_a_frame_that_came_with_the_upgrade_response() {
    use std::io::{Read, Write};

    let listener =
        std::net::TcpListener::bind(std::net::SocketAddr::from(([127, 0, 0, 1], 0))).unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = stream.read(&mut buffer).unwrap();
            assert!(read > 0, "client closed during the upgrade");
            request.extend_from_slice(&buffer[..read]);
        }
        let request = String::from_utf8(request).unwrap();
        let key = request
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("sec-websocket-key")
                    .then(|| value.trim().to_owned())
            })
            .unwrap();
        let envelope =
            encode_envelope(*b"TST", Delivery::ReliableOrdered, 0, b"with the upgrade").unwrap();
        let mut reply = format!(
            "HTTP/1.1 101 Switching Protocols\r\n\
             Connection: Upgrade\r\n\
             Upgrade: websocket\r\n\
             Sec-WebSocket-Accept: {}\r\n\
             Sec-WebSocket-Protocol: test.v1\r\n\r\n",
            tungstenite::handshake::derive_accept_key(key.as_bytes())
        )
        .into_bytes();
        // One unmasked binary frame, short enough for a one-byte length.
        reply.push(0x82);
        reply.push(u8::try_from(envelope.len()).unwrap());
        reply.extend_from_slice(&envelope);
        stream.write_all(&reply).unwrap();
        // Silent until the client closes.
        let _ = stream.read_to_end(&mut Vec::new());
    });

    let mut client = NativeWebSocketClient::connect(NativeWebSocketClientConfig::new(
        format!("ws://{address}{GAME_PATH}"),
        ORIGIN_VALUE,
        identity(),
    ))
    .unwrap();
    assert_eq!(
        collect_client_events(&mut client, 2),
        vec![
            ClientEvent::Connected,
            ClientEvent::Message {
                delivery: Delivery::ReliableOrdered,
                payload: b"with the upgrade".to_vec(),
            },
        ]
    );
    drop(client);
    server.join().unwrap();
}

#[test]
fn slow_handshake_does_not_block_other_admissions() {
    let mut config = NativeWebSocketServerConfig::new(([127, 0, 0, 1], 0).into(), identity())
        .with_origin_policy(OriginPolicy::allow_any());
    config.max_connections = 2;
    config.handshake_timeout = Duration::from_millis(60);
    let server = NativeWebSocketServer::bind(config).unwrap();

    let stalled = TcpStream::connect(server.local_addr()).unwrap();
    thread::sleep(Duration::from_millis(5));
    let admitted = connect_native(&server);
    drop(admitted);
    drop(stalled);
    thread::sleep(Duration::from_millis(80));
    assert!(
        NativeWebSocketClient::connect(NativeWebSocketClientConfig::new(
            url(&server),
            ORIGIN_VALUE,
            identity(),
        ))
        .is_ok()
    );
}

/// Running out of file descriptors must not end admission. The test re-runs
/// itself in a child process with a low descriptor limit, so using every
/// descriptor starves nothing else.
#[cfg(unix)]
#[test]
fn a_server_that_ran_out_of_descriptors_admits_later_clients() {
    const CHILD: &str = "SGL_NET_DESCRIPTOR_LIMIT_CHILD";
    // The harness exits 0 when the filter matches no test; only the child
    // body exits with this.
    const CHILD_PASSED: i32 = 42;
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new("sh")
            .args([
                "-c",
                r#"ulimit -n 64 && exec "$0" --exact "$1" --nocapture"#,
            ])
            .arg(std::env::current_exe().unwrap())
            .arg("a_server_that_ran_out_of_descriptors_admits_later_clients")
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(CHILD_PASSED),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }

    let mut server = server(4);
    let mut files = Vec::new();
    while let Ok(file) = std::fs::File::open("/dev/null") {
        files.push(file);
    }
    // One descriptor for a client; the server has none to accept it with.
    files.pop();
    let _stranded = TcpStream::connect(server.local_addr()).unwrap();
    thread::sleep(Duration::from_millis(300));
    files.clear();

    let mut client = connect_native(&server);
    assert_eq!(client.poll(0), vec![ClientEvent::Connected]);
    connected_id(&wait_server_events(&mut server));
    std::process::exit(CHILD_PASSED);
}
