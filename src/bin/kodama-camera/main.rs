//! Kodama Camera Binary
//!
//! Captures video, audio, and telemetry from Pi camera and streams to server via Iroh P2P.
//!
//! ## Usage
//!
//! ```bash
//! # Set the server's public key
//! export KODAMA_SERVER_KEY=<base32-public-key>
//!
//! # Run with real camera (Pi)
//! kodama-camera
//!
//! # Run with test source (development)
//! kodama-camera --test-source
//!
//! # Disable audio or telemetry
//! kodama-camera --no-audio --no-telemetry
//! ```

use anyhow::{Context, Result};
use bytes::Bytes;
use iroh::PublicKey;
use std::path::PathBuf;
use std::str::FromStr;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tracing::{debug, error, info, warn};
use kodama::{
    capture::{
        h264,
        AudioCapture, AudioCaptureConfig,
        TelemetryCapture, TelemetryCaptureConfig,
    },
    Frame, Relay, SourceId, VideoCaptureConfig,
};

/// Camera configuration from environment/args
struct Config {
    /// Server's public key to connect to
    server_key: PublicKey,
    /// Path to store our secret key
    key_path: PathBuf,
    /// Video capture settings
    video: VideoCaptureConfig,
    /// Audio capture settings
    audio: AudioCaptureConfig,
    /// Telemetry capture settings
    telemetry: TelemetryCaptureConfig,
    /// Use test source instead of real camera
    test_source: bool,
    /// Enable audio capture
    enable_audio: bool,
    /// Enable telemetry capture
    enable_telemetry: bool,
}

impl Config {
    fn from_env() -> Result<Self> {
        let server_key_str = std::env::var("KODAMA_SERVER_KEY")
            .context("KODAMA_SERVER_KEY environment variable not set")?;

        let server_key = PublicKey::from_str(&server_key_str)
            .context("Invalid KODAMA_SERVER_KEY format")?;

        let key_path = std::env::var("KODAMA_KEY_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/var/lib/kodama/camera.key"));

        let args: Vec<String> = std::env::args().collect();
        let test_source = args.iter().any(|arg| arg == "--test-source");
        let enable_audio = !args.iter().any(|arg| arg == "--no-audio");
        let enable_telemetry = !args.iter().any(|arg| arg == "--no-telemetry");

        // Video config from env or defaults
        let width: u32 = std::env::var("KODAMA_WIDTH")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(1280);

        let height: u32 = std::env::var("KODAMA_HEIGHT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(720);

        let fps: u32 = std::env::var("KODAMA_FPS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(30);

        // Telemetry interval from env or default (5 seconds)
        let telemetry_interval: u32 = std::env::var("KODAMA_TELEMETRY_INTERVAL")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(5);

        Ok(Self {
            server_key,
            key_path,
            video: VideoCaptureConfig {
                width,
                height,
                fps,
                ..Default::default()
            },
            audio: AudioCaptureConfig::default(),
            telemetry: TelemetryCaptureConfig {
                interval_secs: telemetry_interval,
                ..Default::default()
            },
            test_source,
            enable_audio,
            enable_telemetry,
        })
    }
}

/// Combined channel message for multiplexing
enum CaptureMessage {
    Video(Bytes),
    Audio(Bytes),
    Telemetry(Bytes),
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("kodama=info".parse().unwrap()),
        )
        .init();

    // Load configuration
    let config = Config::from_env()?;

    info!("Kodama Camera starting");
    info!("  Video: {}x{} @ {}fps", config.video.width, config.video.height, config.video.fps);
    info!("  Audio: {}", if config.enable_audio { "enabled" } else { "disabled" });
    info!("  Telemetry: {}", if config.enable_telemetry { "enabled" } else { "disabled" });
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

    // Create a combined channel for all capture sources
    let (combined_tx, mut combined_rx) = mpsc::channel::<CaptureMessage>(128);

    // Start video capture
    let video_tx = combined_tx.clone();
    let video_rx = if config.test_source {
        #[cfg(feature = "test-source")]
        {
            info!("Starting test video source");
            kodama::start_test_source(kodama::TestSourceConfig {
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
        let (_capture, rx) = kodama::VideoCapture::start(config.video.clone())
            .context("Failed to start video capture")?;
        rx
    };

    // Spawn video forwarding task
    tokio::spawn(async move {
        let mut rx = video_rx;
        while let Some(payload) = rx.recv().await {
            if video_tx.send(CaptureMessage::Video(payload)).await.is_err() {
                break;
            }
        }
        debug!("Video capture task ended");
    });

    // Start audio capture
    let _audio_capture = if config.enable_audio {
        let audio_tx = combined_tx.clone();

        let audio_rx = if config.test_source {
            #[cfg(feature = "test-source")]
            {
                info!("Starting test audio source");
                kodama::capture::start_test_audio(kodama::capture::TestAudioConfig::default())
            }
            #[cfg(not(feature = "test-source"))]
            {
                warn!("Test audio source not enabled, audio disabled");
                None::<mpsc::Receiver<Bytes>>
            }
        } else {
            match AudioCapture::start(config.audio.clone()) {
                Ok((_capture, rx)) => {
                    info!("Audio capture started");
                    Some(rx)
                }
                Err(e) => {
                    warn!("Failed to start audio capture: {}. Continuing without audio.", e);
                    None
                }
            }
        };

        #[cfg(feature = "test-source")]
        if let Some(mut rx) = audio_rx {
            tokio::spawn(async move {
                while let Some(payload) = rx.recv().await {
                    if audio_tx.send(CaptureMessage::Audio(payload)).await.is_err() {
                        break;
                    }
                }
                debug!("Audio capture task ended");
            });
        }

        #[cfg(not(feature = "test-source"))]
        if let Some(mut rx) = audio_rx {
            tokio::spawn(async move {
                while let Some(payload) = rx.recv().await {
                    if audio_tx.send(CaptureMessage::Audio(payload)).await.is_err() {
                        break;
                    }
                }
                debug!("Audio capture task ended");
            });
        }

        true
    } else {
        false
    };

    // Start telemetry capture
    let _telemetry_capture = if config.enable_telemetry {
        let telemetry_tx = combined_tx.clone();

        match TelemetryCapture::start(config.telemetry.clone()) {
            Ok((_capture, mut rx)) => {
                info!("Telemetry capture started (interval: {}s)", config.telemetry.interval_secs);

                tokio::spawn(async move {
                    while let Some(payload) = rx.recv().await {
                        if telemetry_tx.send(CaptureMessage::Telemetry(payload)).await.is_err() {
                            break;
                        }
                    }
                    debug!("Telemetry capture task ended");
                });

                true
            }
            Err(e) => {
                warn!("Failed to start telemetry capture: {}. Continuing without telemetry.", e);
                false
            }
        }
    } else {
        false
    };

    // Drop the original sender so combined_rx closes when all sources end
    drop(combined_tx);

    info!("Streaming to server...");

    let start = Instant::now();
    let mut video_frame_count = 0u64;
    let mut audio_frame_count = 0u64;
    let mut telemetry_count = 0u64;
    let mut keyframe_count = 0u64;
    let mut bytes_sent = 0u64;
    let mut last_stats = Instant::now();

    // Process and send frames from all sources
    while let Some(msg) = combined_rx.recv().await {
        let timestamp_us = start.elapsed().as_micros() as u64;

        let frame = match msg {
            CaptureMessage::Video(payload) => {
                let is_keyframe = h264::contains_keyframe(&payload);
                video_frame_count += 1;
                if is_keyframe {
                    keyframe_count += 1;
                }
                Frame::video(source_id, payload, is_keyframe)
                    .with_timestamp(timestamp_us)
            }
            CaptureMessage::Audio(payload) => {
                audio_frame_count += 1;
                Frame::audio(source_id, payload)
                    .with_timestamp(timestamp_us)
            }
            CaptureMessage::Telemetry(payload) => {
                telemetry_count += 1;
                Frame::telemetry(source_id, payload)
                    .with_timestamp(timestamp_us)
            }
        };

        bytes_sent += frame.payload.len() as u64;

        // Send frame
        if let Err(e) = sender.send(&frame).await {
            error!("Failed to send frame: {}", e);
            break;
        }

        // Log stats every 5 seconds
        if last_stats.elapsed().as_secs() >= 5 {
            let elapsed = start.elapsed().as_secs_f64();
            let video_fps = video_frame_count as f64 / elapsed;
            let bitrate_mbps = (bytes_sent as f64 * 8.0) / (elapsed * 1_000_000.0);

            info!(
                "Stats: video={} ({} kf), audio={}, telemetry={} | {:.1} fps, {:.2} Mbps",
                video_frame_count, keyframe_count, audio_frame_count, telemetry_count,
                video_fps, bitrate_mbps
            );
            last_stats = Instant::now();
        }
    }

    warn!("Capture streams ended");
    info!(
        "Total: {} video, {} audio, {} telemetry frames, {} bytes sent",
        video_frame_count, audio_frame_count, telemetry_count, bytes_sent
    );

    Ok(())
}
