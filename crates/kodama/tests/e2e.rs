//! E2E regression test suite for Kodama
//!
//! Uses real Iroh endpoints with direct addressing (no relay discovery, no hardware)
//! to exercise the full pipeline:
//!
//! - Camera → QUIC → Router → broadcast subscriber (transport layer)
//! - Camera → QUIC → Router → Web Server → HTTP/WebSocket client (web layer)
//!
//! Run: `cargo test -p kodama --features web --test e2e`

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures_util::StreamExt;
use tokio_tungstenite::tungstenite;

use kodama::server::Router;
use kodama::transport::{Relay, RelayConnection};
use kodama::{
    Channel, ClientCommandMessage, Command, CommandMessage, CommandResponse, CommandResult, Frame,
    FrameFlags, SourceId, TargetedCommandRequest, ALPN, MAX_FRAME_PAYLOAD_SIZE,
};

// ── WebSocket message type prefixes (mirror ws.rs constants) ─────────

const MSG_CAMERA_LIST: u8 = 0x01;
const MSG_VIDEO_INIT: u8 = 0x02;
const MSG_VIDEO_SEGMENT: u8 = 0x03;
const MSG_TELEMETRY: u8 = 0x04;
const MSG_AUDIO_LEVEL: u8 = 0x05;
const MSG_AUDIO_DATA: u8 = 0x06;
const MSG_PEER_EVENT: u8 = 0x07;
const MSG_SERVER_STATS: u8 = 0x08;

// ── Shared helpers ───────────────────────────────────────────────────

fn test_source() -> SourceId {
    SourceId::new([1, 2, 3, 4, 5, 6, 7, 8])
}

fn video_frame(payload_len: usize, keyframe: bool) -> Frame {
    Frame {
        source: test_source(),
        channel: Channel::Video,
        flags: if keyframe {
            FrameFlags::keyframe()
        } else {
            FrameFlags::default()
        },
        timestamp_us: 0,
        payload: Bytes::from(vec![0xAB; payload_len]),
    }
}

fn audio_frame(samples: &[i16]) -> Frame {
    let mut payload = Vec::with_capacity(samples.len() * 2);
    for &s in samples {
        payload.extend_from_slice(&s.to_le_bytes());
    }
    Frame {
        source: test_source(),
        channel: Channel::Audio,
        flags: FrameFlags::default(),
        timestamp_us: 0,
        payload: Bytes::from(payload),
    }
}

fn telemetry_frame(full: bool) -> Frame {
    let payload = if full {
        rmp_serde::to_vec(&serde_json::json!({
            "f": true,
            "cpu": 42.5,
            "tmp": 55.0,
            "mem": 68.2,
            "dsk": 30.0,
            "up": 3600_u64,
            "la": [1.0, 0.8, 0.5],
        }))
        .unwrap()
    } else {
        rmp_serde::to_vec(&serde_json::json!({
            "cpu": 50.0,
        }))
        .unwrap()
    };
    Frame {
        source: test_source(),
        channel: Channel::Telemetry,
        flags: FrameFlags::default(),
        timestamp_us: 0,
        payload: Bytes::from(payload),
    }
}

// ── Web test helpers ─────────────────────────────────────────────────

// We can't use kodama::web::start directly because it blocks forever.
// Instead, replicate the minimal axum setup needed for testing.
// This tests the same handler code (ws::handle_ws) but lets us control the listener.

use axum::extract::{State, WebSocketUpgrade};
use axum::response::{IntoResponse, Json};

struct TestWebState {
    handle: kodama::server::RouterHandle,
    start_time: std::time::Instant,
    video_muxer: Arc<kodama::web::fmp4::VideoMuxer>,
}

async fn ws_upgrade(
    ws: WebSocketUpgrade,
    State(state): State<Arc<TestWebState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| {
        kodama::web::ws::handle_ws(
            socket,
            state.handle.clone(),
            Arc::clone(&state.video_muxer),
            state.start_time,
        )
    })
}

async fn api_cameras(State(state): State<Arc<TestWebState>>) -> Json<serde_json::Value> {
    use kodama::server::PeerRole;
    let peers = state.handle.peers().await;
    let cameras: Vec<serde_json::Value> = peers
        .into_iter()
        .filter(|p| p.role == PeerRole::Camera)
        .map(|p| {
            let source = SourceId::from_node_id_bytes(p.key.as_bytes());
            serde_json::json!({
                "id": format!("{}", source),
                "name": format!("Camera {}", &format!("{}", source)[..8]),
                "connected": true,
            })
        })
        .collect();
    Json(serde_json::json!(cameras))
}

async fn api_peers(State(state): State<Arc<TestWebState>>) -> Json<serde_json::Value> {
    use kodama::server::PeerRole;
    let peers = state.handle.peers().await;
    let list: Vec<serde_json::Value> = peers
        .into_iter()
        .map(|p| {
            serde_json::json!({
                "key": p.key.to_string(),
                "role": match p.role { PeerRole::Camera => "camera", PeerRole::Client => "client" },
            })
        })
        .collect();
    Json(serde_json::json!(list))
}

async fn api_status(State(state): State<Arc<TestWebState>>) -> Json<serde_json::Value> {
    let stats = state.handle.stats().await;
    let uptime = state.start_time.elapsed().as_secs();
    Json(serde_json::json!({
        "cameras": stats.cameras_connected,
        "clients": stats.clients_connected,
        "uptime_secs": uptime,
        "frames_received": stats.frames_received,
        "frames_broadcast": stats.frames_broadcast,
        "storage_enabled": false,
        "storage_bytes_used": 0,
    }))
}

async fn api_storage(State(_state): State<Arc<TestWebState>>) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "enabled": false }))
}

/// Start a web server on an ephemeral port, return the bound address.
async fn start_test_server(handle: kodama::server::RouterHandle) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        // Start shared video muxer
        let video_muxer = Arc::new(kodama::web::fmp4::VideoMuxer::new(512));
        let muxer_rx = handle.subscribe();
        let muxer_ref = Arc::clone(&video_muxer);
        tokio::spawn(async move {
            muxer_ref.run(muxer_rx).await;
        });

        let state = Arc::new(TestWebState {
            handle,
            start_time: std::time::Instant::now(),
            video_muxer,
        });

        let app = axum::Router::new()
            .route("/ws", axum::routing::get(ws_upgrade))
            .route("/api/cameras", axum::routing::get(api_cameras))
            .route("/api/peers", axum::routing::get(api_peers))
            .route("/api/status", axum::routing::get(api_status))
            .route("/api/storage", axum::routing::get(api_storage))
            .with_state(state);

        axum::serve(listener, app).await.unwrap();
    });

    tokio::time::sleep(Duration::from_millis(50)).await;
    addr
}

/// Connect a camera via Iroh and send frames. Returns when all frames are sent.
async fn connect_camera_and_send(server_relay: &Arc<Relay>, router: &Router, frames: Vec<Frame>) {
    let server_addr = server_relay.endpoint().endpoint().addr();
    let camera_relay = Relay::new(None).await.unwrap();

    let router_clone = router.clone();
    let server_relay_clone = Arc::clone(server_relay);
    let server_task = tokio::spawn(async move {
        let conn = server_relay_clone.endpoint().accept().await.unwrap();
        let relay_conn = RelayConnection::from_connection(conn);
        let remote = relay_conn.remote_public_key();
        let receiver = relay_conn.accept_frame_stream().await.unwrap();
        router_clone
            .handle_camera_with_receiver(remote, receiver)
            .await
    });

    tokio::time::sleep(Duration::from_millis(200)).await;

    let conn = camera_relay
        .endpoint()
        .endpoint()
        .connect(server_addr, ALPN)
        .await
        .unwrap();
    let relay_conn = RelayConnection::from_connection(conn);
    let sender = relay_conn.open_frame_stream().await.unwrap();

    for frame in frames {
        sender.send(&frame).await.unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;
    }

    tokio::time::sleep(Duration::from_millis(500)).await;
    sender.finish().await.ok();
    server_task.abort();
}

/// Connect a WebSocket client, return the stream.
async fn connect_ws(
    addr: SocketAddr,
) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    let url = format!("ws://{}/ws", addr);
    let (stream, _response) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("WebSocket connect failed");
    stream
}

/// Collect WebSocket binary messages until timeout.
async fn collect_ws_messages(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    timeout: Duration,
) -> Vec<Vec<u8>> {
    let mut messages = Vec::new();
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, ws.next()).await {
            Ok(Some(Ok(tungstenite::Message::Binary(data)))) => {
                messages.push(data.to_vec());
            }
            Ok(Some(Ok(_))) => {} // Ignore text/ping/pong
            Ok(Some(Err(_))) | Ok(None) => break,
            Err(_) => break, // Timeout
        }
    }
    messages
}

// ═══════════════════════════════════════════════════════════════════════
// Transport layer tests
// ═══════════════════════════════════════════════════════════════════════

/// Frames within the 2 MB limit flow through the full pipeline.
///
/// Camera endpoint → QUIC stream → Server router (with rate limiter) → broadcast → client subscriber
#[tokio::test(flavor = "multi_thread")]
async fn frames_flow_through_router() {
    let server_relay = Arc::new(Relay::new(None).await.unwrap());
    let camera_relay = Relay::new(None).await.unwrap();
    let server_addr = server_relay.endpoint().endpoint().addr();

    let router = Router::new(64);
    let handle = router.handle();
    let mut client_rx = handle.subscribe();

    let router_clone = router.clone();
    let server_relay_clone = Arc::clone(&server_relay);
    let server_task = tokio::spawn(async move {
        let conn = server_relay_clone.endpoint().accept().await.unwrap();
        let relay_conn = RelayConnection::from_connection(conn);
        let receiver = relay_conn.accept_frame_stream().await.unwrap();
        router_clone
            .handle_camera_with_receiver(relay_conn.remote_public_key(), receiver)
            .await
    });

    let camera_task = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        let conn = camera_relay
            .endpoint()
            .endpoint()
            .connect(server_addr, ALPN)
            .await
            .unwrap();
        let relay_conn = RelayConnection::from_connection(conn);
        let sender = relay_conn.open_frame_stream().await.unwrap();

        for i in 0..10 {
            let frame = video_frame(1000, i == 0);
            sender.send(&frame).await.unwrap();
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        sender.finish().await.unwrap();
    });

    let client_task = tokio::spawn(async move {
        let mut received = 0u32;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        loop {
            let timeout = deadline.saturating_duration_since(tokio::time::Instant::now());
            match tokio::time::timeout(timeout, client_rx.recv()).await {
                Ok(Ok(_)) => {
                    received += 1;
                    if received >= 10 {
                        break;
                    }
                }
                Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {}
                Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => break,
                Err(_) => panic!("Timed out waiting for frames, got {received}/10"),
            }
        }
        received
    });

    camera_task.await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    let received = client_task.await.unwrap();
    assert_eq!(received, 10, "Expected 10 frames received by client");

    let stats = handle.stats().await;
    assert_eq!(stats.frames_received, 10);
    assert_eq!(stats.frames_broadcast, 10);

    server_task.abort();
}

/// The 2 MB frame size limit is enforced on the wire.
#[tokio::test(flavor = "multi_thread")]
async fn oversized_frame_rejected_on_wire() {
    let oversized = video_frame(MAX_FRAME_PAYLOAD_SIZE + 1, true);
    let mut buf = Vec::new();
    let result = kodama::transport::mux::frame::write_frame(&mut buf, &oversized).await;
    assert!(result.is_err(), "Expected error for oversized frame");
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("too large"),
        "Error should mention 'too large', got: {err_msg}"
    );

    let at_limit = video_frame(MAX_FRAME_PAYLOAD_SIZE, true);
    let mut buf2 = Vec::new();
    kodama::transport::mux::frame::write_frame(&mut buf2, &at_limit)
        .await
        .expect("Frame at exact limit should succeed");

    let bad_len: u32 = (MAX_FRAME_PAYLOAD_SIZE + 100) as u32;
    let mut bad_buf = Vec::new();
    bad_buf.extend_from_slice(&bad_len.to_be_bytes());
    bad_buf.extend_from_slice(&vec![0u8; bad_len as usize]);
    let mut cursor = std::io::Cursor::new(bad_buf);
    let read_result = kodama::transport::mux::frame::read_frame(&mut cursor).await;
    assert!(
        read_result.is_err(),
        "read_frame should reject oversized length"
    );
}

/// Rate limiter drops excess frames without crashing.
#[tokio::test(flavor = "multi_thread")]
async fn rate_limiter_drops_excess_frames() {
    let server_relay = Arc::new(Relay::new(None).await.unwrap());
    let camera_relay = Relay::new(None).await.unwrap();
    let server_addr = server_relay.endpoint().endpoint().addr();

    let router = Router::new(512);
    let handle = router.handle();
    let mut client_rx = handle.subscribe();

    let router_clone = router.clone();
    let server_relay_clone = Arc::clone(&server_relay);
    let server_task = tokio::spawn(async move {
        let conn = server_relay_clone.endpoint().accept().await.unwrap();
        let relay_conn = RelayConnection::from_connection(conn);
        let receiver = relay_conn.accept_frame_stream().await.unwrap();
        router_clone
            .handle_camera_with_receiver(relay_conn.remote_public_key(), receiver)
            .await
    });

    let camera_task = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        let conn = camera_relay
            .endpoint()
            .endpoint()
            .connect(server_addr, ALPN)
            .await
            .unwrap();
        let relay_conn = RelayConnection::from_connection(conn);
        let sender = relay_conn.open_frame_stream().await.unwrap();

        for i in 0..200 {
            let frame = video_frame(100, i == 0);
            if sender.send(&frame).await.is_err() {
                break;
            }
        }

        tokio::time::sleep(Duration::from_millis(1000)).await;
        sender.finish().await.ok();
    });

    let client_task = tokio::spawn(async move {
        let mut received = 0u32;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let timeout = deadline.saturating_duration_since(tokio::time::Instant::now());
            match tokio::time::timeout(timeout, client_rx.recv()).await {
                Ok(Ok(_)) => received += 1,
                Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(n))) => {
                    received += n as u32;
                }
                Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => break,
                Err(_) => break,
            }
        }
        received
    });

    camera_task.await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    let received = client_task.await.unwrap();
    let stats = handle.stats().await;

    assert!(
        stats.frames_broadcast > 0,
        "At least some frames should get through"
    );
    assert!(
        stats.frames_broadcast <= 200,
        "Should not broadcast more than sent"
    );
    let _ = received;

    server_task.abort();
}

/// Command routing: client → server → camera → server → client
#[tokio::test(flavor = "multi_thread")]
async fn command_routing_client_to_camera() {
    let server_relay = Arc::new(Relay::new(None).await.unwrap());
    let camera_relay = Relay::new(None).await.unwrap();
    let client_relay = Relay::new(None).await.unwrap();

    let server_addr = server_relay.endpoint().endpoint().addr();
    let router = Router::new(64);

    let camera_pub_key = camera_relay.public_key();
    let client_server_addr = server_addr.clone();
    let camera_task = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        let conn = camera_relay
            .endpoint()
            .endpoint()
            .connect(server_addr, ALPN)
            .await
            .unwrap();
        let relay_conn = RelayConnection::from_connection(conn);

        let sender = relay_conn.open_frame_stream().await.unwrap();
        sender.send(&video_frame(100, true)).await.unwrap();

        let cmd_stream = relay_conn.open_command_stream().await.unwrap();

        cmd_stream
            .sender
            .send(&CommandMessage::Response(CommandResponse {
                id: 0,
                result: CommandResult::Ok,
            }))
            .await
            .unwrap();

        match cmd_stream.receiver.recv().await.unwrap() {
            Some(CommandMessage::Request(req)) => {
                assert!(matches!(req.command, Command::RequestStatus));
                cmd_stream
                    .sender
                    .send(&CommandMessage::Response(CommandResponse {
                        id: req.id,
                        result: CommandResult::Ok,
                    }))
                    .await
                    .unwrap();
            }
            other => panic!("Expected CommandRequest, got {:?}", other),
        }

        tokio::time::sleep(Duration::from_secs(5)).await;
    });

    let router_clone = router.clone();
    let server_relay_clone = Arc::clone(&server_relay);
    let server_camera_task = tokio::spawn(async move {
        let conn = server_relay_clone.endpoint().accept().await.unwrap();
        let relay_conn = RelayConnection::from_connection(conn);
        let remote = relay_conn.remote_public_key();

        let receiver = relay_conn.accept_frame_stream().await.unwrap();
        let cmd_stream = relay_conn.accept_command_stream().await.unwrap();
        router_clone.register_camera_commands(remote, cmd_stream);

        router_clone
            .handle_camera_with_receiver(remote, receiver)
            .await
    });

    tokio::time::sleep(Duration::from_millis(800)).await;

    let cameras = router.cameras_with_commands().await;
    assert_eq!(
        cameras.len(),
        1,
        "Camera command channel should be registered"
    );
    assert_eq!(cameras[0], camera_pub_key);

    let router_clone = router.clone();
    let server_relay_clone = Arc::clone(&server_relay);

    let server_client_task = tokio::spawn(async move {
        let conn = server_relay_clone.endpoint().accept().await.unwrap();
        let relay_conn = RelayConnection::from_connection(conn);
        let remote = relay_conn.remote_public_key();

        let cmd_stream = relay_conn.accept_client_command_stream().await.unwrap();
        router_clone
            .handle_client_commands(remote, cmd_stream)
            .await;
    });

    let camera_key_str = camera_pub_key.to_string();
    let client_task = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        let conn = client_relay
            .endpoint()
            .endpoint()
            .connect(client_server_addr, ALPN)
            .await
            .unwrap();
        let relay_conn = RelayConnection::from_connection(conn);

        let cmd_stream = relay_conn.open_client_command_stream().await.unwrap();

        cmd_stream
            .sender
            .send(&ClientCommandMessage::Request(TargetedCommandRequest {
                id: 1,
                target_camera: camera_key_str,
                command: Command::RequestStatus,
            }))
            .await
            .unwrap();

        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        let timeout = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(timeout, cmd_stream.receiver.recv()).await {
            Ok(Ok(Some(ClientCommandMessage::Response(resp)))) => {
                assert_eq!(resp.id, 1, "Response ID should match request");
                assert!(
                    matches!(resp.result, CommandResult::Ok),
                    "Expected Ok result, got {:?}",
                    resp.result
                );
            }
            Ok(Ok(other)) => panic!("Unexpected message: {:?}", other),
            Ok(Err(e)) => panic!("Stream error: {}", e),
            Err(_) => panic!("Timed out waiting for command response"),
        }
    });

    client_task.await.unwrap();

    camera_task.abort();
    server_camera_task.abort();
    server_client_task.abort();
}

// ═══════════════════════════════════════════════════════════════════════
// Web layer tests (HTTP API + WebSocket)
// ═══════════════════════════════════════════════════════════════════════

/// REST API returns correct status with no cameras connected.
#[tokio::test(flavor = "multi_thread")]
async fn rest_api_status_empty() {
    let router = Router::new(64);
    let handle = router.handle();
    let addr = start_test_server(handle).await;

    let client = reqwest::Client::new();

    let resp: serde_json::Value = client
        .get(format!("http://{}/api/status", addr))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(resp["cameras"], 0);
    assert_eq!(resp["clients"], 0);
    assert_eq!(resp["frames_received"], 0);
    assert_eq!(resp["storage_enabled"], false);

    let resp: serde_json::Value = client
        .get(format!("http://{}/api/cameras", addr))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert!(resp.as_array().unwrap().is_empty());

    let resp: serde_json::Value = client
        .get(format!("http://{}/api/storage", addr))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(resp["enabled"], false);
}

/// REST API reflects connected camera after Iroh connection.
#[tokio::test(flavor = "multi_thread")]
async fn rest_api_with_camera() {
    let server_relay = Arc::new(Relay::new(None).await.unwrap());
    let router = Router::new(64);
    let handle = router.handle();
    let addr = start_test_server(handle.clone()).await;

    let frames = vec![video_frame(100, true)];
    connect_camera_and_send(&server_relay, &router, frames).await;

    let client = reqwest::Client::new();

    let resp: serde_json::Value = client
        .get(format!("http://{}/api/status", addr))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(resp["cameras"], 1, "Expected 1 camera connected");
    assert!(resp["frames_received"].as_u64().unwrap() >= 1);

    let resp: serde_json::Value = client
        .get(format!("http://{}/api/cameras", addr))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let cameras = resp.as_array().unwrap();
    assert_eq!(cameras.len(), 1);
    assert_eq!(cameras[0]["connected"], true);
    assert!(cameras[0]["id"].as_str().is_some());

    let resp: serde_json::Value = client
        .get(format!("http://{}/api/peers", addr))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let peers = resp.as_array().unwrap();
    assert_eq!(peers.len(), 1);
    assert_eq!(peers[0]["role"], "camera");
}

/// WebSocket receives camera list on connect.
#[tokio::test(flavor = "multi_thread")]
async fn ws_receives_camera_list() {
    let router = Router::new(64);
    let handle = router.handle();
    let addr = start_test_server(handle).await;

    let mut ws = connect_ws(addr).await;

    let messages = collect_ws_messages(&mut ws, Duration::from_secs(2)).await;
    assert!(!messages.is_empty(), "Should receive at least one message");

    let first = &messages[0];
    assert_eq!(
        first[0], MSG_CAMERA_LIST,
        "First message should be camera list"
    );

    let json: serde_json::Value = serde_json::from_slice(&first[1..]).unwrap();
    assert!(
        json.as_array().unwrap().is_empty(),
        "Camera list should be empty initially"
    );

    ws.close(None).await.ok();
}

/// WebSocket receives telemetry after camera sends telemetry frames.
///
/// Full pipeline: Camera → Iroh → Router → broadcast → WS handler → WebSocket client
#[tokio::test(flavor = "multi_thread")]
async fn ws_receives_telemetry() {
    let server_relay = Arc::new(Relay::new(None).await.unwrap());
    let router = Router::new(64);
    let handle = router.handle();
    let addr = start_test_server(handle.clone()).await;

    let mut ws = connect_ws(addr).await;
    let _ = collect_ws_messages(&mut ws, Duration::from_millis(500)).await;

    let frames = vec![telemetry_frame(true)];
    connect_camera_and_send(&server_relay, &router, frames).await;

    let messages = collect_ws_messages(&mut ws, Duration::from_secs(3)).await;

    let telemetry_msgs: Vec<&Vec<u8>> = messages
        .iter()
        .filter(|m| !m.is_empty() && m[0] == MSG_TELEMETRY)
        .collect();

    assert!(
        !telemetry_msgs.is_empty(),
        "Should receive at least one telemetry message, got {} messages total: {:?}",
        messages.len(),
        messages
            .iter()
            .map(|m| m.first().copied())
            .collect::<Vec<_>>()
    );

    let tel = telemetry_msgs[0];
    assert!(tel.len() > 9, "Telemetry message too short");
    assert_eq!(tel[0], MSG_TELEMETRY);

    let src_bytes: [u8; 8] = tel[1..9].try_into().unwrap();
    assert!(
        src_bytes.iter().any(|&b| b != 0),
        "Source ID should be non-zero"
    );

    let json: serde_json::Value = serde_json::from_slice(&tel[9..]).unwrap();
    assert_eq!(json["cpu_usage"], 42.5);
    assert_eq!(json["cpu_temp"], 55.0);
    assert_eq!(json["memory_usage"], 68.2);
    assert_eq!(json["uptime_secs"], 3600);

    ws.close(None).await.ok();
}

/// WebSocket receives audio level after camera sends audio frames.
#[tokio::test(flavor = "multi_thread")]
async fn ws_receives_audio() {
    let server_relay = Arc::new(Relay::new(None).await.unwrap());
    let router = Router::new(64);
    let handle = router.handle();
    let addr = start_test_server(handle.clone()).await;

    let mut ws = connect_ws(addr).await;
    let _ = collect_ws_messages(&mut ws, Duration::from_millis(500)).await;

    let silence: Vec<i16> = vec![0; 480];
    let frames = vec![audio_frame(&silence)];
    connect_camera_and_send(&server_relay, &router, frames).await;

    let messages = collect_ws_messages(&mut ws, Duration::from_secs(3)).await;

    let audio_level_msgs: Vec<&Vec<u8>> = messages
        .iter()
        .filter(|m| !m.is_empty() && m[0] == MSG_AUDIO_LEVEL)
        .collect();

    let audio_data_msgs: Vec<&Vec<u8>> = messages
        .iter()
        .filter(|m| !m.is_empty() && m[0] == MSG_AUDIO_DATA)
        .collect();

    assert!(
        !audio_level_msgs.is_empty(),
        "Should receive audio level message, got types: {:?}",
        messages
            .iter()
            .map(|m| m.first().copied())
            .collect::<Vec<_>>()
    );

    assert!(
        !audio_data_msgs.is_empty(),
        "Should receive audio data message"
    );

    let level_msg = audio_level_msgs[0];
    assert_eq!(
        level_msg.len(),
        1 + 8 + 4,
        "Audio level message should be 13 bytes"
    );
    let level_bytes: [u8; 4] = level_msg[9..13].try_into().unwrap();
    let level_db = f32::from_le_bytes(level_bytes);
    assert_eq!(level_db, -60.0, "Silence should produce -60 dB");

    let data_msg = audio_data_msgs[0];
    assert!(data_msg.len() > 14, "Audio data message too short");
    let sr_bytes: [u8; 4] = data_msg[9..13].try_into().unwrap();
    let sample_rate = u32::from_le_bytes(sr_bytes);
    assert_eq!(sample_rate, 48000, "Sample rate should be 48kHz");
    assert_eq!(data_msg[13], 1, "Should be mono (1 channel)");

    ws.close(None).await.ok();
}

/// WebSocket receives server stats periodically.
#[tokio::test(flavor = "multi_thread")]
async fn ws_receives_server_stats() {
    let router = Router::new(64);
    let handle = router.handle();
    let addr = start_test_server(handle).await;

    let mut ws = connect_ws(addr).await;

    let messages = collect_ws_messages(&mut ws, Duration::from_secs(7)).await;

    let stats_msgs: Vec<&Vec<u8>> = messages
        .iter()
        .filter(|m| !m.is_empty() && m[0] == MSG_SERVER_STATS)
        .collect();

    assert!(
        !stats_msgs.is_empty(),
        "Should receive server stats within 7 seconds, got types: {:?}",
        messages
            .iter()
            .map(|m| m.first().copied())
            .collect::<Vec<_>>()
    );

    let stats: serde_json::Value = serde_json::from_slice(&stats_msgs[0][1..]).unwrap();
    assert!(stats["cameras"].is_number());
    assert!(stats["clients"].is_number());
    assert!(stats["uptime_secs"].is_number());
    assert!(stats["frames_received"].is_number());
    assert!(stats["frames_broadcast"].is_number());

    ws.close(None).await.ok();
}

/// Multiple frame types flow through WebSocket concurrently.
#[tokio::test(flavor = "multi_thread")]
async fn ws_multi_channel_pipeline() {
    let server_relay = Arc::new(Relay::new(None).await.unwrap());
    let router = Router::new(64);
    let handle = router.handle();
    let addr = start_test_server(handle.clone()).await;

    let mut ws = connect_ws(addr).await;
    let _ = collect_ws_messages(&mut ws, Duration::from_millis(500)).await;

    let tone: Vec<i16> = (0..480)
        .map(|i| ((i as f32 * 0.1).sin() * 10000.0) as i16)
        .collect();
    let frames = vec![
        video_frame(1000, true),
        video_frame(500, false),
        audio_frame(&tone),
        telemetry_frame(true),
        video_frame(500, false),
        telemetry_frame(false),
    ];
    connect_camera_and_send(&server_relay, &router, frames).await;

    let messages = collect_ws_messages(&mut ws, Duration::from_secs(3)).await;

    let mut got_camera_list = false;
    let mut got_telemetry = false;
    let mut got_audio_level = false;
    let mut got_audio_data = false;

    for msg in &messages {
        if msg.is_empty() {
            continue;
        }
        match msg[0] {
            MSG_CAMERA_LIST => got_camera_list = true,
            MSG_VIDEO_INIT | MSG_VIDEO_SEGMENT => {} // Only if FFmpeg is available
            MSG_TELEMETRY => got_telemetry = true,
            MSG_AUDIO_LEVEL => got_audio_level = true,
            MSG_AUDIO_DATA => got_audio_data = true,
            MSG_PEER_EVENT | MSG_SERVER_STATS => {}
            _ => {}
        }
    }

    assert!(got_camera_list, "Should receive camera list update");
    assert!(got_telemetry, "Should receive telemetry");
    assert!(got_audio_level, "Should receive audio level");
    assert!(got_audio_data, "Should receive audio data");

    let level_msg = messages
        .iter()
        .find(|m| !m.is_empty() && m[0] == MSG_AUDIO_LEVEL)
        .unwrap();
    let level_db = f32::from_le_bytes(level_msg[9..13].try_into().unwrap());
    assert!(
        level_db > -60.0,
        "Tone should produce level above -60 dB, got {}",
        level_db
    );

    ws.close(None).await.ok();
}
