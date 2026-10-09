#![cfg(not(target_arch = "wasm32"))]

use std::net::TcpStream;
use std::thread;
use std::time::{Duration, Instant};

use sgl_net::websocket::{
    ENVELOPE_HEADER_LEN, ENVELOPE_VERSION, Envelope, Fragment, GAME_PATH,
    MAX_WEBSOCKET_FRAME_BYTES, NativeWebSocketClient, NativeWebSocketClientConfig,
    NativeWebSocketServer, NativeWebSocketServerConfig, OriginPolicy, WebSocketIdentity,
    encode_envelope,
};
use sgl_net::{
    ClientEvent, ClientIo, ConnectionId, DEFAULT_LANE_OUTBOUND_MESSAGES, Delivery,
    DisconnectReason, Lane, MAX_LATEST_STATE_BYTES, ReliableConfig, SendError, ServerEvent,
    ServerIo,
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

/// One whole-message frame.
fn frame(delivery: Delivery, sequence: u64, payload: &[u8]) -> Vec<u8> {
    let envelope = Envelope {
        delivery,
        sequence,
        fragment: Fragment::Whole,
        payload,
    };
    encode_envelope(*b"TST", &envelope).unwrap()
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
        .send(Delivery::RELIABLE_ORDERED, b"client event")
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
                delivery: Delivery::RELIABLE_ORDERED,
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
        .send(conn, Delivery::RELIABLE_ORDERED, b"server event")
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
                delivery: Delivery::RELIABLE_ORDERED,
                payload: b"server event".to_vec(),
            },
            ClientEvent::Message {
                delivery: Delivery::LatestState,
                payload: vec![99],
            },
        ]
    );
}

/// Defect (#267): a full reliable queue that closes the peer, or a refused
/// message that is lost or duplicated. Oracle: the client's received list —
/// every accepted message, then the retried one, each once and in order —
/// while another client on the same server is unaffected.
#[test]
fn a_saturated_lane_would_block_and_delivers_everything_after_the_drain() {
    let mut server = server(4);
    let mut first = connect_native(&server);
    let first_conn = connected_id(&wait_server_events(&mut server));
    let mut second = connect_native(&server);
    let second_conn = connected_id(&wait_server_events(&mut server));
    first.poll(0);
    second.poll(0);

    let message = |index: usize| index.to_le_bytes().to_vec();
    for index in 0..DEFAULT_LANE_OUTBOUND_MESSAGES {
        server
            .send(first_conn, Delivery::RELIABLE_ORDERED, &message(index))
            .unwrap();
    }
    let retained = message(DEFAULT_LANE_OUTBOUND_MESSAGES);
    assert_eq!(
        server.send(first_conn, Delivery::RELIABLE_ORDERED, &retained),
        Err(SendError::WouldBlock)
    );
    assert_eq!(server.capacity(first_conn, Lane::DEFAULT).messages, 0);
    server
        .send(second_conn, Delivery::RELIABLE_ORDERED, b"still alive")
        .unwrap();
    server.flush(0);

    let mut received = Vec::new();
    let mut retried = false;
    wait_until(Duration::from_secs(5), || {
        for event in first.poll(0) {
            match event {
                ClientEvent::Message { payload, .. } => received.push(payload),
                other => panic!("unexpected {other:?}"),
            }
        }
        for event in server.poll(0) {
            assert!(
                !matches!(event, ServerEvent::Disconnected { .. }),
                "{event:?}"
            );
        }
        if !retried
            && server
                .send(first_conn, Delivery::RELIABLE_ORDERED, &retained)
                .is_ok()
        {
            retried = true;
            server.flush(0);
        }
        (received.len() > DEFAULT_LANE_OUTBOUND_MESSAGES).then_some(())
    });
    assert_eq!(
        received,
        (0..=DEFAULT_LANE_OUTBOUND_MESSAGES)
            .map(message)
            .collect::<Vec<_>>()
    );
    let second_events = collect_client_events(&mut second, 1);
    assert_eq!(
        second_events,
        vec![ClientEvent::Message {
            delivery: Delivery::RELIABLE_ORDERED,
            payload: b"still alive".to_vec(),
        }]
    );
}

/// Defect (design §12): WebSocket dropping, duplicating, reordering or
/// fragmenting accepted unreliable messages while another lane streams bulk
/// data. Oracle: SGL never drops an accepted message and TCP loses nothing,
/// so every accepted lane-0 unreliable message arrives once, in send order,
/// and every bulk message arrives intact and in order.
#[test]
fn unreliable_messages_all_arrive_in_order_beside_reliable_bulk() {
    const BULK: u32 = 20;
    const UNRELIABLE: u32 = 300;
    let mut reliable = ReliableConfig::DEFAULT;
    reliable.lanes[0].weight = 8;
    // The test's own polling must not be what limits the bulk stream.
    reliable.lanes[1].inbound_messages = 1_024;
    reliable.lanes[1].inbound_bytes = 64 * 1024 * 1024;
    let mut config = NativeWebSocketServerConfig::new(([127, 0, 0, 1], 0).into(), identity())
        .with_origin_policy(OriginPolicy::exact([ORIGIN_VALUE.to_owned()]).unwrap());
    config.reliable = reliable.clone();
    let mut server = NativeWebSocketServer::bind(config).unwrap();
    let mut client_config =
        NativeWebSocketClientConfig::new(url(&server), ORIGIN_VALUE, identity());
    client_config.reliable = reliable;
    let mut client = NativeWebSocketClient::connect(client_config).unwrap();
    client.poll(0);
    connected_id(&wait_server_events(&mut server));

    let bulk_lane = Delivery::Reliable(Lane::new(1).unwrap());
    let unreliable_lane = Delivery::Unreliable(Lane::DEFAULT);
    let bulk = |index: u32| vec![u8::try_from(index).unwrap(); 60 * 1024];
    let (mut bulk_sent, mut unreliable_sent) = (0, 0);
    let (mut bulk_got, mut unreliable_got) = (Vec::new(), Vec::new());
    wait_until(Duration::from_secs(10), || {
        if bulk_sent < BULK && client.send(bulk_lane, &bulk(bulk_sent)).is_ok() {
            bulk_sent += 1;
        }
        if unreliable_sent < UNRELIABLE
            && client
                .send(unreliable_lane, &unreliable_sent.to_le_bytes())
                .is_ok()
        {
            unreliable_sent += 1;
        }
        client.flush(0);
        for event in server.poll(0) {
            match event {
                ServerEvent::Message {
                    delivery, payload, ..
                } if delivery == unreliable_lane => {
                    unreliable_got.push(u32::from_le_bytes(payload.try_into().unwrap()));
                }
                ServerEvent::Message {
                    delivery, payload, ..
                } if delivery == bulk_lane => bulk_got.push(payload),
                other => panic!("unexpected {other:?}"),
            }
        }
        (unreliable_got.len() >= UNRELIABLE as usize && bulk_got.len() >= BULK as usize)
            .then_some(())
    });
    assert_eq!(unreliable_got, (0..UNRELIABLE).collect::<Vec<_>>());
    assert_eq!(bulk_got, (0..BULK).map(bulk).collect::<Vec<_>>());
}

/// Defect (#269, design §13): a receiver whose inbound lane fills between
/// polls disconnecting a healthy sender (`InboundOverflow`) instead of
/// pushing back through TCP, or losing, duplicating or reordering messages
/// while it stops and resumes reading. Oracle: netcode.md 13 — a WebSocket
/// receiver that polls slowly makes a fast sender slower, never
/// disconnected. The server's lane holds 16 messages, so it takes at most
/// 64 KiB a poll, while the client offers 256 KiB a turn; the client's own
/// lane holds 1 MiB, four turns' worth, and its 24 MiB stream outgrows both
/// TCP buffers several times over, so its `send` is refused only once the
/// stalled receiver has filled them. Every message arrives once and in
/// order, and neither side reports a disconnect.
#[test]
fn a_slowly_polled_receiver_paces_a_fast_sender_instead_of_disconnecting_it() {
    const MESSAGES: u32 = 6_144;
    const MESSAGE_BYTES: usize = 4 * 1024;
    const PER_TURN: usize = 64;
    let mut reliable = ReliableConfig::DEFAULT;
    reliable.lanes[0].inbound_messages = 16;
    let mut config = NativeWebSocketServerConfig::new(([127, 0, 0, 1], 0).into(), identity())
        .with_origin_policy(OriginPolicy::exact([ORIGIN_VALUE.to_owned()]).unwrap());
    config.reliable = reliable;
    let mut server = NativeWebSocketServer::bind(config).unwrap();
    let mut client_config =
        NativeWebSocketClientConfig::new(url(&server), ORIGIN_VALUE, identity());
    client_config.reliable.lanes[0].outbound_messages = 4_096;
    client_config.reliable.lanes[0].outbound_bytes = 1 << 20;
    let mut client = NativeWebSocketClient::connect(client_config).unwrap();
    client.poll(0);
    connected_id(&wait_server_events(&mut server));

    let message = |index: u32| {
        let mut payload = vec![0xC3; MESSAGE_BYTES];
        payload[..4].copy_from_slice(&index.to_le_bytes());
        payload
    };
    let (mut sent, mut refused) = (0, 0);
    let mut received = Vec::new();
    wait_until(Duration::from_secs(30), || {
        for _ in 0..PER_TURN {
            if sent == MESSAGES {
                break;
            }
            match client.send(Delivery::RELIABLE_ORDERED, &message(sent)) {
                Ok(()) => sent += 1,
                Err(SendError::WouldBlock) => {
                    refused += 1;
                    break;
                }
                Err(error) => panic!("send refused with {error:?}"),
            }
        }
        client.flush(0);
        let client_events = client.poll(0);
        assert!(client_events.is_empty(), "the sender saw {client_events:?}");
        thread::sleep(Duration::from_millis(1));
        for event in server.poll(0) {
            match event {
                ServerEvent::Message { payload, .. } => {
                    assert_eq!(payload[4..], message(0)[4..]);
                    received.push(u32::from_le_bytes(payload[..4].try_into().unwrap()));
                }
                other => panic!("the receiver saw {other:?}"),
            }
        }
        (received.len() == MESSAGES as usize).then_some(())
    });
    assert_eq!(received, (0..MESSAGES).collect::<Vec<_>>());
    assert!(refused > 0, "the stalled receiver never pushed back");
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
    oversized.extend_from_slice(&[1, 0]);
    oversized.extend_from_slice(&1_u64.to_be_bytes());
    oversized.extend_from_slice(
        &u32::try_from(MAX_LATEST_STATE_BYTES + 1)
            .unwrap()
            .to_be_bytes(),
    );
    oversized.resize(ENVELOPE_HEADER_LEN + MAX_LATEST_STATE_BYTES + 1, 0);
    assert_protocol_disconnect(vec![Message::Binary(oversized.into())]);

    let mut wrong_class = frame(Delivery::RELIABLE_ORDERED, 0, b"queued");
    wrong_class[6..14].copy_from_slice(&1_u64.to_be_bytes());
    assert_protocol_disconnect(vec![
        Message::Binary(frame(Delivery::RELIABLE_ORDERED, 0, b"must be purged").into()),
        Message::Binary(wrong_class.into()),
    ]);

    assert_protocol_disconnect(vec![
        Message::Binary(frame(Delivery::LatestState, 2, b"new").into()),
        Message::Binary(frame(Delivery::LatestState, 1, b"stale").into()),
    ]);

    // Past the frame cap the socket itself refuses the frame (#268).
    assert_protocol_disconnect(vec![Message::Binary(
        vec![0; MAX_WEBSOCKET_FRAME_BYTES + 1].into(),
    )]);

    // A fragment the reassembly table refuses (#268): a last fragment with
    // no first, after a whole message that must not be delivered.
    let mut orphan = frame(Delivery::Reliable(Lane::new(1).unwrap()), 0, b"orphan");
    orphan[4] = 0;
    assert_protocol_disconnect(vec![
        Message::Binary(frame(Delivery::RELIABLE_ORDERED, 0, b"must be purged").into()),
        Message::Binary(orphan.into()),
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
        .send(Delivery::RELIABLE_ORDERED, b"last client event")
        .unwrap();
    client.disconnect(10);
    let events = collect_server_events(&mut server, 2);
    assert_eq!(
        events[0],
        ServerEvent::Message {
            conn,
            delivery: Delivery::RELIABLE_ORDERED,
            payload: b"last client event".to_vec(),
        }
    );
    assert!(matches!(events[1], ServerEvent::Disconnected { conn: id, .. } if id == conn));

    let mut client = connect_native(&server);
    client.poll(0);
    let conn = connected_id(&wait_server_events(&mut server));
    server
        .send(conn, Delivery::RELIABLE_ORDERED, b"last server event")
        .unwrap();
    server.disconnect(conn, 20);
    let events = collect_client_events(&mut client, 2);
    assert_eq!(
        events[0],
        ClientEvent::Message {
            delivery: Delivery::RELIABLE_ORDERED,
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
        let envelope = frame(Delivery::RELIABLE_ORDERED, 0, b"with the upgrade");
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
                delivery: Delivery::RELIABLE_ORDERED,
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
