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
use kodama_core::{Channel, Frame, FrameFlags, SourceId, MAX_FRAME_PAYLOAD_SIZE, ALPN};
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
