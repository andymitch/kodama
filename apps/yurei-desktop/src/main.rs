//! Yurei Desktop Client
//!
//! Connects to server and receives video frames.
//!
//! ## Usage
//!
//! ```bash
//! # Set the server's public key
//! export YUREI_SERVER_KEY=<base32-public-key>
//!
//! # Run the client
//! yurei-desktop
//!
//! # With verbose logging
//! RUST_LOG=yurei=debug yurei-desktop
//! ```
//!
//! The client connects to the specified server and starts receiving frames.
//! For POC 1, this is a CLI that logs frame statistics.
//! A full Tauri GUI will be implemented in Phase 1.6b.

use anyhow::{Context, Result};
use iroh::PublicKey;
use std::str::FromStr;
use tokio::time::{Duration, Instant};
use tracing::{error, info};
use yurei_relay::Relay;

/// Client configuration from environment
struct Config {
    /// Server's public key to connect to
    server_key: PublicKey,
}

impl Config {
    fn from_env() -> Result<Self> {
        let server_key_str = std::env::var("YUREI_SERVER_KEY")
            .context("YUREI_SERVER_KEY environment variable not set")?;

        let server_key = PublicKey::from_str(&server_key_str)
            .context("Invalid YUREI_SERVER_KEY format")?;

        Ok(Self { server_key })
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

    let config = Config::from_env()?;

    info!("Yurei Desktop Client starting");
    info!("Connecting to server: {}", config.server_key);

    // Initialize relay endpoint (ephemeral key for client)
    let relay = Relay::new(None).await?;
    info!("Client PublicKey: {}", relay.public_key());

    // Connect to server
    let conn = relay.connect(config.server_key).await
        .context("Failed to connect to server")?;
    info!("Connected to server!");

    // Wait for the server to open a frame stream to us
    // The server detects us as a client (we don't open a stream first)
    // and opens a stream to send us frames.
    info!("Waiting for frame stream from server...");

    // Give server time to detect us as a client and open stream
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Accept the frame stream from the server
    let receiver = conn.accept_frame_stream().await
        .context("Failed to accept frame stream")?;
    info!("Frame stream opened, receiving frames...");

    let start = Instant::now();
    let mut frame_count = 0u64;
    let mut keyframe_count = 0u64;
    let mut bytes_received = 0u64;
    let mut last_stats = Instant::now();

    // Receive frames from server
    loop {
        match receiver.recv().await {
            Ok(Some(frame)) => {
                frame_count += 1;
                bytes_received += frame.payload.len() as u64;
                if frame.flags.is_keyframe() {
                    keyframe_count += 1;
                }

                // Log stats every 5 seconds
                if last_stats.elapsed().as_secs() >= 5 {
                    let elapsed = start.elapsed().as_secs_f64();
                    let fps = frame_count as f64 / elapsed;
                    let bitrate_mbps = (bytes_received as f64 * 8.0) / (elapsed * 1_000_000.0);

                    info!(
                        "Stats: {} frames ({} keyframes), {:.1} fps, {:.2} Mbps",
                        frame_count, keyframe_count, fps, bitrate_mbps
                    );
                    last_stats = Instant::now();
                }
            }
            Ok(None) => {
                info!("Frame stream closed by server");
                break;
            }
            Err(e) => {
                error!("Frame stream error: {}", e);
                break;
            }
        }
    }

    let elapsed = start.elapsed().as_secs_f64();
    info!(
        "Session complete: {} frames, {} bytes, {:.1} fps average",
        frame_count,
        bytes_received,
        if elapsed > 0.0 { frame_count as f64 / elapsed } else { 0.0 }
    );

    Ok(())
}
