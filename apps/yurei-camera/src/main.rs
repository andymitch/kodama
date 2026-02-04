//! Yurei Camera Binary
//!
//! Captures video from Pi camera and streams to server via Iroh P2P.
//!
//! ## Usage
//!
//! ```bash
//! # Set the server's public key
//! export YUREI_SERVER_KEY=<base32-public-key>
//!
//! # Run with real camera (Pi)
//! yurei-camera
//!
//! # Run with test source (development)
//! yurei-camera --test-source
//! ```

use anyhow::{Context, Result};
use iroh::PublicKey;
use std::path::PathBuf;
use std::str::FromStr;
use tokio::time::Instant;
use tracing::{error, info, warn};
use yurei::{capture::h264, Frame, Relay, SourceId, VideoCaptureConfig};

/// Camera configuration from environment/args
struct Config {
    /// Server's public key to connect to
    server_key: PublicKey,
    /// Path to store our secret key
    key_path: PathBuf,
    /// Video capture settings
    video: VideoCaptureConfig,
    /// Use test source instead of real camera
    test_source: bool,
}

impl Config {
    fn from_env() -> Result<Self> {
        let server_key_str = std::env::var("YUREI_SERVER_KEY")
            .context("YUREI_SERVER_KEY environment variable not set")?;

        let server_key = PublicKey::from_str(&server_key_str)
            .context("Invalid YUREI_SERVER_KEY format")?;

        let key_path = std::env::var("YUREI_KEY_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/var/lib/yurei/camera.key"));

        let test_source = std::env::args().any(|arg| arg == "--test-source");

        // Video config from env or defaults
        let width: u32 = std::env::var("YUREI_WIDTH")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(1280);

        let height: u32 = std::env::var("YUREI_HEIGHT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(720);

        let fps: u32 = std::env::var("YUREI_FPS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(30);

        Ok(Self {
            server_key,
            key_path,
            video: VideoCaptureConfig {
                width,
                height,
                fps,
                ..Default::default()
            },
            test_source,
        })
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

    // Load configuration
    let config = Config::from_env()?;

    info!("Yurei Camera starting");
    info!("  Video: {}x{} @ {}fps", config.video.width, config.video.height, config.video.fps);
    info!("  Test source: {}", config.test_source);

    // Initialize relay endpoint
    let relay = Relay::new(Some(&config.key_path)).await?;
    let source_id = SourceId::from_node_id_bytes(relay.public_key().as_bytes());

    info!("Camera PublicKey: {}", relay.public_key());
    info!("Connecting to server: {}", config.server_key);

    // Connect to server
    let conn = relay.connect(config.server_key).await
        .context("Failed to connect to server")?;
    info!("Connected to server!");

    // Open persistent frame stream for efficient streaming
    let sender = conn.open_frame_stream().await
        .context("Failed to open frame stream")?;

    // Start video capture
    let mut video_rx = if config.test_source {
        #[cfg(feature = "test-source")]
        {
            info!("Starting test video source");
            yurei::start_test_source(yurei::TestSourceConfig {
                fps: config.video.fps,
                frame_size: 15000, // ~15KB simulated frames
                keyframe_interval: config.video.fps, // Keyframe every second
            })
        }
        #[cfg(not(feature = "test-source"))]
        {
            anyhow::bail!("Test source not enabled. Rebuild with --features test-source");
        }
    } else {
        info!("Starting video capture");
        let (_capture, rx) = yurei::VideoCapture::start(config.video.clone())
            .context("Failed to start video capture")?;
        rx
    };

    info!("Streaming video to server...");

    let start = Instant::now();
    let mut frame_count = 0u64;
    let mut keyframe_count = 0u64;
    let mut bytes_sent = 0u64;
    let mut last_stats = Instant::now();

    // Stream frames to server
    while let Some(payload) = video_rx.recv().await {
        let timestamp_us = start.elapsed().as_micros() as u64;

        // Detect keyframe using H.264 NAL parsing
        let is_keyframe = h264::contains_keyframe(&payload);

        // Build frame
        let frame = Frame::video(source_id, payload.clone(), is_keyframe)
            .with_timestamp(timestamp_us);

        // Send frame
        if let Err(e) = sender.send(&frame).await {
            error!("Failed to send frame: {}", e);
            break;
        }

        // Update stats
        frame_count += 1;
        bytes_sent += frame.payload.len() as u64;
        if is_keyframe {
            keyframe_count += 1;
        }

        // Log stats every 5 seconds
        if last_stats.elapsed().as_secs() >= 5 {
            let elapsed = start.elapsed().as_secs_f64();
            let fps = frame_count as f64 / elapsed;
            let bitrate_mbps = (bytes_sent as f64 * 8.0) / (elapsed * 1_000_000.0);

            info!(
                "Stats: {} frames ({} keyframes), {:.1} fps, {:.2} Mbps",
                frame_count, keyframe_count, fps, bitrate_mbps
            );
            last_stats = Instant::now();
        }
    }

    warn!("Video stream ended");
    info!(
        "Total: {} frames, {} bytes sent",
        frame_count, bytes_sent
    );

    Ok(())
}
