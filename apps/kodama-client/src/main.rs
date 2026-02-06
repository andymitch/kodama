//! Kodama Lite Client
//!
//! Connects to server and receives video, audio, and telemetry frames.
//! This is a lightweight CLI viewer without server capability.
//!
//! ## Usage
//!
//! ```bash
//! # Set the server's public key
//! export KODAMA_SERVER_KEY=<base32-public-key>
//!
//! # Run the client
//! kodama-client
//!
//! # Save received frames to disk
//! kodama-client --save-frames /tmp/kodama-frames
//!
//! # With verbose logging
//! RUST_LOG=kodama=debug kodama-client
//! ```

use anyhow::{Context, Result};
use iroh::PublicKey;
use std::collections::HashMap;
use std::path::PathBuf;
use std::str::FromStr;
use tokio::time::{Duration, Instant};
use tracing::{debug, error, info, warn};
use kodama_core::{Channel, Frame, SourceId};
use kodama_relay::Relay;
use kodama_capture::{decode_telemetry, TelemetryData};

/// Client configuration from environment/args
struct Config {
    /// Server's public key to connect to
    server_key: PublicKey,
    /// Optional path to save received frames
    save_frames_path: Option<PathBuf>,
    /// Show telemetry data
    show_telemetry: bool,
}

impl Config {
    fn from_env() -> Result<Self> {
        let server_key_str = std::env::var("KODAMA_SERVER_KEY")
            .context("KODAMA_SERVER_KEY environment variable not set")?;

        let server_key = PublicKey::from_str(&server_key_str)
            .context("Invalid KODAMA_SERVER_KEY format")?;

        let args: Vec<String> = std::env::args().collect();

        let save_frames_path = args.iter()
            .position(|arg| arg == "--save-frames")
            .and_then(|i| args.get(i + 1))
            .map(PathBuf::from);

        let show_telemetry = !args.iter().any(|arg| arg == "--no-telemetry");

        Ok(Self {
            server_key,
            save_frames_path,
            show_telemetry,
        })
    }
}

/// Per-source statistics
#[derive(Debug, Default)]
struct SourceStats {
    video_frames: u64,
    video_bytes: u64,
    video_keyframes: u64,
    audio_frames: u64,
    audio_bytes: u64,
    telemetry_frames: u64,
    last_telemetry: Option<TelemetryData>,
}

/// Frame saver for debugging
struct FrameSaver {
    path: PathBuf,
    frame_count: u64,
}

impl FrameSaver {
    fn new(path: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&path)?;
        Ok(Self { path, frame_count: 0 })
    }

    fn save(&mut self, frame: &Frame) -> Result<()> {
        let channel_name = match frame.channel {
            Channel::Video => "video",
            Channel::Audio => "audio",
            Channel::Telemetry => "telemetry",
        };

        let filename = format!(
            "{:08}_{}_{}_{}.bin",
            self.frame_count,
            frame.source,
            channel_name,
            frame.timestamp_us
        );

        let filepath = self.path.join(filename);
        std::fs::write(&filepath, &frame.payload)?;

        self.frame_count += 1;
        debug!("Saved frame to {:?}", filepath);

        Ok(())
    }
}

fn format_bytes(bytes: u64) -> String {
    if bytes >= 1024 * 1024 * 1024 {
        format!("{:.2} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    } else if bytes >= 1024 * 1024 {
        format!("{:.2} MB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.2} KB", bytes as f64 / 1024.0)
    } else {
        format!("{} B", bytes)
    }
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

    let config = Config::from_env()?;

    info!("Kodama Lite Client starting");
    info!("Connecting to server: {}", config.server_key);

    if let Some(ref path) = config.save_frames_path {
        info!("Saving frames to: {:?}", path);
    }

    // Initialize relay endpoint (ephemeral key for client)
    let relay = Relay::new(None).await?;
    info!("Client PublicKey: {}", relay.public_key());

    // Connect to server
    let conn = relay.connect(config.server_key).await
        .context("Failed to connect to server")?;
    info!("Connected to server!");

    // Wait for the server to open a frame stream to us
    info!("Waiting for frame stream from server...");

    // Give server time to detect us as a client and open stream
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Accept the frame stream from the server
    let receiver = conn.accept_frame_stream().await
        .context("Failed to accept frame stream")?;
    info!("Frame stream opened, receiving frames...");
    info!("");

    // Initialize frame saver if path provided
    let mut frame_saver = config.save_frames_path
        .as_ref()
        .map(|p| FrameSaver::new(p.clone()))
        .transpose()?;

    let start = Instant::now();
    let mut sources: HashMap<SourceId, SourceStats> = HashMap::new();
    let mut total_frames = 0u64;
    let mut total_bytes = 0u64;
    let mut last_stats = Instant::now();
    let mut last_telemetry_display = Instant::now();

    // Receive frames from server
    loop {
        match receiver.recv().await {
            Ok(Some(frame)) => {
                total_frames += 1;
                total_bytes += frame.payload.len() as u64;

                // Update per-source stats
                let stats = sources.entry(frame.source).or_default();

                match frame.channel {
                    Channel::Video => {
                        stats.video_frames += 1;
                        stats.video_bytes += frame.payload.len() as u64;
                        if frame.flags.is_keyframe() {
                            stats.video_keyframes += 1;
                        }
                    }
                    Channel::Audio => {
                        stats.audio_frames += 1;
                        stats.audio_bytes += frame.payload.len() as u64;
                    }
                    Channel::Telemetry => {
                        stats.telemetry_frames += 1;
                        // Try to decode telemetry
                        if let Ok(telemetry) = decode_telemetry(&frame.payload) {
                            stats.last_telemetry = Some(telemetry);
                        }
                    }
                }

                // Save frame if configured
                if let Some(ref mut saver) = frame_saver {
                    if let Err(e) = saver.save(&frame) {
                        warn!("Failed to save frame: {}", e);
                    }
                }

                // Display telemetry every 5 seconds if enabled
                if config.show_telemetry && last_telemetry_display.elapsed().as_secs() >= 5 {
                    for (source, s) in &sources {
                        if let Some(ref telemetry) = s.last_telemetry {
                            info!(
                                "Telemetry [{}]: CPU={:.1}%{}, Mem={:.1}%, Load=[{:.2}, {:.2}, {:.2}], Up={}s",
                                source,
                                telemetry.cpu_usage,
                                telemetry.cpu_temp.map(|t| format!(", Temp={:.1}C", t)).unwrap_or_default(),
                                telemetry.memory_usage,
                                telemetry.load_average[0],
                                telemetry.load_average[1],
                                telemetry.load_average[2],
                                telemetry.uptime_secs
                            );
                        }
                    }
                    last_telemetry_display = Instant::now();
                }

                // Log stats every 5 seconds
                if last_stats.elapsed().as_secs() >= 5 {
                    let elapsed = start.elapsed().as_secs_f64();

                    info!("=== Stream Statistics ({:.0}s) ===", elapsed);

                    for (source, stats) in &sources {
                        let video_fps = stats.video_frames as f64 / elapsed;
                        let video_bitrate = (stats.video_bytes as f64 * 8.0) / (elapsed * 1_000_000.0);

                        info!(
                            "  Source {}: video={} frames ({} kf), {:.1} fps, {:.2} Mbps",
                            source,
                            stats.video_frames,
                            stats.video_keyframes,
                            video_fps,
                            video_bitrate
                        );

                        if stats.audio_frames > 0 {
                            let audio_rate = stats.audio_frames as f64 / elapsed;
                            info!(
                                "           audio={} frames ({:.1}/s), {}",
                                stats.audio_frames,
                                audio_rate,
                                format_bytes(stats.audio_bytes)
                            );
                        }

                        if stats.telemetry_frames > 0 {
                            info!(
                                "           telemetry={} updates",
                                stats.telemetry_frames
                            );
                        }
                    }

                    let total_fps = total_frames as f64 / elapsed;
                    let total_bitrate = (total_bytes as f64 * 8.0) / (elapsed * 1_000_000.0);
                    info!(
                        "  Total: {} frames, {:.1} fps, {:.2} Mbps, {}",
                        total_frames, total_fps, total_bitrate, format_bytes(total_bytes)
                    );
                    info!("");

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

    // Final summary
    let elapsed = start.elapsed().as_secs_f64();
    info!("");
    info!("=== Session Summary ===");
    info!("Duration: {:.1}s", elapsed);

    for (source, stats) in &sources {
        info!(
            "Source {}: {} video ({} keyframes), {} audio, {} telemetry",
            source,
            stats.video_frames,
            stats.video_keyframes,
            stats.audio_frames,
            stats.telemetry_frames
        );
    }

    info!(
        "Total: {} frames, {}, {:.1} fps average",
        total_frames,
        format_bytes(total_bytes),
        if elapsed > 0.0 { total_frames as f64 / elapsed } else { 0.0 }
    );

    Ok(())
}
