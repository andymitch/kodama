//! Yurei Server Binary
//!
//! Accepts camera connections and routes frames to clients.
//!
//! ## Usage
//!
//! ```bash
//! # Run the server
//! yurei-server-bin
//!
//! # With custom key path
//! YUREI_KEY_PATH=./my-server.key yurei-server-bin
//!
//! # With verbose logging
//! RUST_LOG=yurei=debug yurei-server-bin
//! ```
//!
//! The server will print its PublicKey on startup. Share this key with:
//! - Cameras: set YUREI_SERVER_KEY to connect
//! - Clients: use to connect from the desktop app

use anyhow::Result;
use std::path::PathBuf;
use tokio::time::{interval, Duration};
use tracing::{error, info, warn};
use yurei_relay::Relay;
use yurei_server::Router;

/// Server configuration from environment
struct Config {
    /// Path to store our secret key
    key_path: PathBuf,
    /// Frame buffer capacity for broadcast
    buffer_capacity: usize,
}

impl Config {
    fn from_env() -> Self {
        let key_path = std::env::var("YUREI_KEY_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("./server.key"));

        let buffer_capacity: usize = std::env::var("YUREI_BUFFER_SIZE")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(64);

        Self {
            key_path,
            buffer_capacity,
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("yurei=info".parse().unwrap()),
        )
        .init();

    let config = Config::from_env();

    info!("Yurei Server starting");
    info!("  Key path: {:?}", config.key_path);
    info!("  Buffer capacity: {}", config.buffer_capacity);

    // Create parent directory for key if needed
    if let Some(parent) = config.key_path.parent() {
        if !parent.exists() && !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }

    // Initialize relay endpoint
    let relay = Relay::new(Some(&config.key_path)).await?;
    info!("Server PublicKey: {}", relay.public_key());
    info!("");
    info!("Share this key with cameras (YUREI_SERVER_KEY)");
    info!("and clients to connect.");
    info!("");

    // Create router
    let router = Router::new(config.buffer_capacity);
    let handle = router.handle();

    // Spawn stats logging task
    tokio::spawn(async move {
        let mut stats_interval = interval(Duration::from_secs(30));
        loop {
            stats_interval.tick().await;
            let stats = handle.stats().await;
            info!(
                "Stats: {} cameras, {} clients, {} frames received, {} broadcast",
                stats.cameras_connected,
                stats.clients_connected,
                stats.frames_received,
                stats.frames_broadcast
            );
        }
    });

    info!("Waiting for connections...");

    // Accept connections
    loop {
        match relay.accept().await {
            Some(conn) => {
                let remote = conn.remote_public_key();
                info!("New connection from: {}", remote);

                let router = router.clone();

                // For POC: we need a way to determine if this is a camera or client.
                // Simple approach: first stream direction determines role.
                // Camera opens a stream to send frames -> we handle as camera
                // Client expects server to open stream -> we handle as client
                //
                // We'll use a bidirectional handshake stream for role negotiation.
                // For now, let's use a simple heuristic: try to accept a stream,
                // if we get one, it's a camera; if timeout, it's a client.

                tokio::spawn(async move {
                    // Try to detect role by waiting for incoming stream
                    // Camera will open a frame stream immediately
                    // Client will wait for us to send frames
                    let detect_timeout = Duration::from_secs(2);

                    tokio::select! {
                        // Try to accept a frame stream (camera behavior)
                        result = conn.accept_frame_stream() => {
                            match result {
                                Ok(receiver) => {
                                    info!(peer = %remote, "Detected as camera (opened stream)");
                                    // We already accepted the stream, need to handle it
                                    // Re-wrap into a connection handler
                                    if let Err(e) = handle_camera_with_receiver(&router, conn, receiver, remote).await {
                                        warn!(peer = %remote, error = %e, "Camera handler error");
                                    }
                                }
                                Err(e) => {
                                    warn!(peer = %remote, error = %e, "Failed to accept stream");
                                }
                            }
                        }
                        // Timeout - assume it's a client waiting for frames
                        _ = tokio::time::sleep(detect_timeout) => {
                            info!(peer = %remote, "Detected as client (no stream opened)");
                            if let Err(e) = router.handle_client(conn).await {
                                warn!(peer = %remote, error = %e, "Client handler error");
                            }
                        }
                    }
                });
            }
            None => {
                error!("Relay accept returned None, shutting down");
                break;
            }
        }
    }

    Ok(())
}

/// Handle a camera connection when we've already accepted the frame receiver
async fn handle_camera_with_receiver(
    router: &Router,
    _conn: yurei_relay::RelayConnection,
    receiver: yurei_relay::FrameReceiver,
    remote: iroh::PublicKey,
) -> Result<()> {
    router.handle_camera_with_receiver(remote, receiver).await
}
