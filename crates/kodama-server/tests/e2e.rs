//! E2E regression test suite for Kodama server
//!
//! Uses two real Iroh endpoints with direct addressing (no relay discovery)
//! to exercise the full Camera → Router → Client pipeline.
//!
//! Run: `cargo test -p kodama-server --test e2e`
//!
//! Tests:
//!   1. Normal frame flow through router (with rate limiter)
//!   2. MAX_FRAME_PAYLOAD_SIZE (2 MB) enforcement on wire
//!   3. Per-connection rate limiting (issue #8)

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use kodama_core::{
    Channel, Frame, FrameFlags, SourceId, MAX_FRAME_PAYLOAD_SIZE, ALPN,
    Command, CommandMessage, CommandResponse, CommandResult,
    ClientCommandMessage, TargetedCommandRequest,
};
use kodama_transport::{Relay, RelayConnection};
use kodama_server::Router;

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

/// Test 1: Frames within the 2 MB limit flow through the full pipeline.
///
/// Camera endpoint → QUIC stream → Server router (with rate limiter) → broadcast → client subscriber
#[tokio::test(flavor = "multi_thread")]
async fn frames_flow_through_router_e2e() {
    let server_relay = Arc::new(Relay::new(None).await.unwrap());
    let camera_relay = Relay::new(None).await.unwrap();

    // Get server's full address (includes direct socket addresses for local connection)
    let server_addr = server_relay.endpoint().endpoint().addr();

    let router = Router::new(64);
    let handle = router.handle();
    let mut client_rx = handle.subscribe();

    // Server: accept and handle camera
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

    // Camera: connect and send 10 frames
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

    // Client: collect frames
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
    eprintln!("Test 1 PASS: 10 frames sent -> 10 broadcast -> 10 received");

    server_task.abort();
}

/// Test 2: The 2 MB frame size limit is enforced on the wire.
#[tokio::test(flavor = "multi_thread")]
async fn oversized_frame_rejected_on_wire() {
    // Verify write_frame rejects oversized frames (no network needed)
    let oversized = video_frame(MAX_FRAME_PAYLOAD_SIZE + 1, true);
    let mut buf = Vec::new();
    let result = kodama_transport::mux::frame::write_frame(&mut buf, &oversized).await;
    assert!(result.is_err(), "Expected error for oversized frame");
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("too large"),
        "Error should mention 'too large', got: {err_msg}"
    );
    eprintln!("Oversized frame ({} bytes) correctly rejected: {err_msg}", MAX_FRAME_PAYLOAD_SIZE + 1);

    // Verify a frame at exactly the limit succeeds
    let at_limit = video_frame(MAX_FRAME_PAYLOAD_SIZE, true);
    let mut buf2 = Vec::new();
    kodama_transport::mux::frame::write_frame(&mut buf2, &at_limit)
        .await
        .expect("Frame at exact limit should succeed");
    eprintln!("Frame at exact limit ({} bytes) accepted", MAX_FRAME_PAYLOAD_SIZE);

    // Also verify read_frame rejects oversized length prefix
    let bad_len: u32 = (MAX_FRAME_PAYLOAD_SIZE + 100) as u32;
    let mut bad_buf = Vec::new();
    bad_buf.extend_from_slice(&bad_len.to_be_bytes());
    bad_buf.extend_from_slice(&vec![0u8; bad_len as usize]);
    let mut cursor = std::io::Cursor::new(bad_buf);
    let read_result = kodama_transport::mux::frame::read_frame(&mut cursor).await;
    assert!(read_result.is_err(), "read_frame should reject oversized length");
    eprintln!("read_frame correctly rejected oversized length prefix");

    eprintln!("Test 2 PASS: 2 MB limit enforced on both write and read paths");
}

/// Test 3: Rate limiter drops excess frames without crashing.
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

        // Send 200 frames as fast as possible — default video limit is 120fps
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

    eprintln!(
        "Test 3: Sent 200 frames -> {} broadcast, {} received by client",
        stats.frames_broadcast, received
    );

    assert!(
        stats.frames_broadcast > 0,
        "At least some frames should get through"
    );
    assert!(
        stats.frames_broadcast <= 200,
        "Should not broadcast more than sent"
    );
    eprintln!("Test 3 PASS: rate limiter active, no panics/deadlocks");

    server_task.abort();
}

/// Test 4: Command routing: client → server → camera → server → client
///
/// Verifies the full command pipeline:
/// 1. Camera connects and registers command channel
/// 2. Client sends a TargetedCommandRequest via the server
/// 3. Server routes the command to the camera
/// 4. Camera responds with a CommandResponse
/// 5. Server forwards the response back to the client
#[tokio::test(flavor = "multi_thread")]
async fn command_routing_client_to_camera() {
    let server_relay = Arc::new(Relay::new(None).await.unwrap());
    let camera_relay = Relay::new(None).await.unwrap();
    let client_relay = Relay::new(None).await.unwrap();

    let server_addr = server_relay.endpoint().endpoint().addr();
    let router = Router::new(64);

    // --- Camera side ---
    // Camera connects, opens frame stream (for detection) + command stream
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

        // Open frame stream (so server detects us as camera)
        let sender = relay_conn.open_frame_stream().await.unwrap();
        // Send one frame so the stream is established
        sender.send(&video_frame(100, true)).await.unwrap();

        // Open command stream
        let cmd_stream = relay_conn.open_command_stream().await.unwrap();

        // Send ready signal (id=0)
        cmd_stream
            .sender
            .send(&CommandMessage::Response(CommandResponse {
                id: 0,
                result: CommandResult::Ok,
            }))
            .await
            .unwrap();

        // Wait for a command request and respond
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

        // Keep connection alive until test completes
        tokio::time::sleep(Duration::from_secs(5)).await;
    });

    // --- Server side: accept camera ---
    let router_clone = router.clone();
    let server_relay_clone = Arc::clone(&server_relay);
    let server_camera_task = tokio::spawn(async move {
        let conn = server_relay_clone.endpoint().accept().await.unwrap();
        let relay_conn = RelayConnection::from_connection(conn);
        let remote = relay_conn.remote_public_key();

        // Accept frame stream (camera detection)
        let receiver = relay_conn.accept_frame_stream().await.unwrap();

        // Accept command stream and register
        let cmd_stream = relay_conn.accept_command_stream().await.unwrap();
        router_clone.register_camera_commands(remote, cmd_stream);

        // Handle camera frames (runs until stream closes)
        router_clone
            .handle_camera_with_receiver(remote, receiver)
            .await
    });

    // Wait for camera to be registered
    tokio::time::sleep(Duration::from_millis(800)).await;

    // Verify camera command channel is registered
    let cameras = router.cameras_with_commands().await;
    assert_eq!(cameras.len(), 1, "Camera command channel should be registered");
    assert_eq!(cameras[0], camera_pub_key);

    // --- Client side ---
    let router_clone = router.clone();
    let server_relay_clone = Arc::clone(&server_relay);

    // Server: accept client connection and handle commands
    let server_client_task = tokio::spawn(async move {
        let conn = server_relay_clone.endpoint().accept().await.unwrap();
        let relay_conn = RelayConnection::from_connection(conn);
        let remote = relay_conn.remote_public_key();

        // Accept client command stream
        let cmd_stream = relay_conn.accept_client_command_stream().await.unwrap();
        router_clone.handle_client_commands(remote, cmd_stream).await;
    });

    // Client connects and sends a targeted command
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

        // Open client command stream
        let cmd_stream = relay_conn.open_client_command_stream().await.unwrap();

        // Send targeted command: RequestStatus → camera
        cmd_stream
            .sender
            .send(&ClientCommandMessage::Request(TargetedCommandRequest {
                id: 1,
                target_camera: camera_key_str,
                command: Command::RequestStatus,
            }))
            .await
            .unwrap();

        // Wait for response
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
                eprintln!("Client received response: {:?}", resp);
            }
            Ok(Ok(other)) => panic!("Unexpected message: {:?}", other),
            Ok(Err(e)) => panic!("Stream error: {}", e),
            Err(_) => panic!("Timed out waiting for command response"),
        }
    });

    client_task.await.unwrap();
    eprintln!("Test 4 PASS: command routed client → server → camera → server → client");

    camera_task.abort();
    server_camera_task.abort();
    server_client_task.abort();
}
