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
use std::time::Duration;
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;
use tracing::{debug, error, info, warn};
use kodama_core::{
    Frame, SourceId,
    Command, CommandMessage, CommandResponse, CommandResult, CameraStatus,
    ConfigureParams, RecordParams, UpdateFirmwareParams, NetworkParams, NetworkAction,
    DeleteRecordingParams, SendRecordingParams, StreamParams,
};
use kodama_transport::{Relay, CommandStream};
use kodama_capture::{
    h264,
    AbrConfig, AbrController, AbrDecision, QualityTier, ThroughputTracker,
    AudioCapture, AudioCaptureConfig,
    MotionDetector, TelemetryCapture, TelemetryCaptureConfig,
    VideoCaptureConfig,
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
    /// Enable adaptive bitrate control
    enable_abr: bool,
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

        // ABR enabled by default, disable with KODAMA_ABR=0
        let enable_abr = std::env::var("KODAMA_ABR")
            .map(|v| v != "0")
            .unwrap_or(true);

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

        // Telemetry interval from env or default (1 second with sparse updates)
        let telemetry_interval: u32 = std::env::var("KODAMA_TELEMETRY_INTERVAL")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(1);

        // Telemetry heartbeat interval from env or default (30 seconds)
        let telemetry_heartbeat: u32 = std::env::var("KODAMA_TELEMETRY_HEARTBEAT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(30);

        // GPS position threshold in degrees from env or default (0.0001 ≈ 11m)
        let gps_threshold: f64 = std::env::var("KODAMA_TELEMETRY_GPS_THRESHOLD")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0.0001);

        let mut telemetry_thresholds = kodama_capture::TelemetryThresholds::default();
        telemetry_thresholds.lat_lon = gps_threshold;

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
                heartbeat_interval_secs: telemetry_heartbeat,
                thresholds: telemetry_thresholds,
                ..Default::default()
            },
            test_source,
            enable_audio,
            enable_telemetry,
            enable_abr,
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

    let abr_enabled = config.enable_abr && !config.test_source;

    info!("Kodama Camera starting");
    info!("  Video: {}x{} @ {}fps", config.video.width, config.video.height, config.video.fps);
    info!("  Audio: {}", if config.enable_audio { "enabled" } else { "disabled" });
    info!("  Telemetry: {}", if config.enable_telemetry { "enabled" } else { "disabled" });
    info!("  ABR: {}", if abr_enabled { "enabled" } else { "disabled" });
    info!("  Test source: {}", config.test_source);

    // Initialize relay endpoint
    let relay = Relay::new(Some(&config.key_path)).await?;
    let source_id = SourceId::from_node_id_bytes(relay.public_key().as_bytes());

    info!("Camera PublicKey: {}", relay.public_key());

    // Create a combined channel for all capture sources
    let (combined_tx, mut combined_rx) = mpsc::channel::<CaptureMessage>(128);

    // Start video capture
    // When ABR is enabled, VideoCapture is owned by the bitrate handler task.
    // The bitrate_cmd_tx channel is used to send new bitrate values to it.
    let video_tx = combined_tx.clone();
    let mut _video_capture = None;
    let mut bitrate_cmd_tx: Option<mpsc::Sender<u32>> = None;

    let video_rx = if config.test_source {
        #[cfg(feature = "test-source")]
        {
            info!("Starting test video source");
            kodama_capture::start_test_source(kodama_capture::TestSourceConfig {
                fps: config.video.fps,
            })
        }
        #[cfg(not(feature = "test-source"))]
        {
            anyhow::bail!("Test source not enabled. Rebuild with --features test-source");
        }
    } else {
        info!("Starting video capture");
        let mut video_config = config.video.clone();
        // When ABR is on, start at the High tier bitrate
        if abr_enabled {
            video_config.bitrate = QualityTier::High.bitrate_bps();
            info!("ABR: starting at {} tier ({} bps)", QualityTier::High, video_config.bitrate);
        }
        let (capture, rx) = kodama_capture::VideoCapture::start(video_config)
            .context("Failed to start video capture")?;

        if abr_enabled {
            // Hand VideoCapture to a dedicated bitrate handler task
            let (cmd_tx, mut cmd_rx) = mpsc::channel::<u32>(4);
            bitrate_cmd_tx = Some(cmd_tx);

            tokio::task::spawn_blocking(move || {
                let mut vc = capture;
                while let Some(new_bitrate) = cmd_rx.blocking_recv() {
                    if let Err(e) = vc.update_bitrate(new_bitrate) {
                        error!("ABR: failed to update bitrate: {}", e);
                    }
                }
                debug!("Bitrate handler task ended");
            });
        } else {
            _video_capture = Some(capture);
        }
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
    let mut _audio_capture_handle = None;
    if config.enable_audio {
        let audio_tx = combined_tx.clone();

        let audio_rx = if config.test_source {
            #[cfg(feature = "test-source")]
            {
                info!("Starting test audio source");
                Some(kodama_capture::start_test_audio(kodama_capture::TestAudioConfig::default()))
            }
            #[cfg(not(feature = "test-source"))]
            {
                warn!("Test audio source not enabled, audio disabled");
                None::<mpsc::Receiver<Bytes>>
            }
        } else {
            match AudioCapture::start(config.audio.clone()) {
                Ok((capture, rx)) => {
                    info!("Audio capture started");
                    _audio_capture_handle = Some(capture);
                    Some(rx)
                }
                Err(e) => {
                    warn!("Failed to start audio capture: {}. Continuing without audio.", e);
                    None
                }
            }
        };

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

    }

    // Create motion detector (shares motion level with telemetry)
    let (mut motion_detector, motion_level_shared) = MotionDetector::new();

    // Start telemetry capture
    let mut _telemetry_capture_handle = None;
    if config.enable_telemetry {
        let telemetry_tx = combined_tx.clone();

        let mut telemetry_config = config.telemetry.clone();
        telemetry_config.motion_level = Some(motion_level_shared);

        match TelemetryCapture::start(telemetry_config) {
            Ok((capture, mut rx)) => {
                _telemetry_capture_handle = Some(capture);
                info!(
                    "Telemetry capture started (interval: {}s, heartbeat: {}s)",
                    config.telemetry.interval_secs, config.telemetry.heartbeat_interval_secs
                );

                tokio::spawn(async move {
                    while let Some(payload) = rx.recv().await {
                        if telemetry_tx.send(CaptureMessage::Telemetry(payload)).await.is_err() {
                            break;
                        }
                    }
                    debug!("Telemetry capture task ended");
                });
            }
            Err(e) => {
                warn!("Failed to start telemetry capture: {}. Continuing without telemetry.", e);
            }
        }
    }

    // Drop the original sender so combined_rx closes when all sources end
    drop(combined_tx);

    // ── Network monitor ────────────────────────────────────────────────
    // Polls the default route interface every 500ms. When it changes
    // (e.g. wlan0 → wwan0 on WiFi dropout), immediately notifies iroh
    // to rebind sockets and signals the send loop to break for reconnect.
    let (net_change_tx, net_change_rx) = watch::channel(0u64);
    {
        let endpoint = relay.endpoint().endpoint().clone();
        tokio::spawn(async move {
            network_monitor(endpoint, net_change_tx).await;
        });
    }
    let mut net_change_rx = net_change_rx;

    // ── Reconnection loop ──────────────────────────────────────────────
    // Captures keep running. On connection loss we reconnect and resume
    // reading from combined_rx. Exponential backoff: 1s → 2s → 4s … 30s.
    let mut attempt = 0u32;
    let session_start = Instant::now();
    let mut total_video = 0u64;
    let mut total_audio = 0u64;
    let mut total_telemetry = 0u64;
    let mut total_bytes = 0u64;

    loop {
        attempt += 1;

        // Wait for a default route to exist before trying to connect.
        // After WiFi drops, it can take 10-30s for cellular to come up.
        if get_default_interface().is_none() {
            info!("No default route, waiting for network...");
            let wait_start = Instant::now();
            loop {
                tokio::time::sleep(Duration::from_millis(500)).await;
                if get_default_interface().is_some() {
                    info!("Route appeared after {:.1}s", wait_start.elapsed().as_secs_f64());
                    break;
                }
                // Drain frames to prevent backpressure while waiting
                while combined_rx.try_recv().is_ok() {}
            }
        }

        // Notify iroh of potential network changes (e.g. WiFi → cellular)
        // so it re-scans interfaces, rebinds sockets, and reconnects to relays.
        relay.endpoint().endpoint().network_change().await;
        // Wait for iroh to establish a relay connection on the (possibly new) interface.
        // Without this, connect() fires before sockets are rebound after a WiFi drop.
        match tokio::time::timeout(
            Duration::from_secs(10),
            relay.endpoint().endpoint().online(),
        )
        .await
        {
            Ok(()) => info!("Relay online after network_change"),
            Err(_) => warn!("Timed out waiting for relay (10s), trying connect anyway"),
        }

        info!("Connecting to server (attempt {})...", attempt);

        // ── Connect ────────────────────────────────────────────────────
        // Let connect() run to completion without interrupting on route
        // flaps. On cellular, NM cycles the modem frequently — restarting
        // connect on every flap prevents the camera from ever connecting.
        // The send loop below handles network changes instead.
        let conn = match relay.connect(config.server_key).await {
            Ok(c) => c,
            Err(e) => {
                let delay = reconnect_delay(attempt);
                warn!("Connection failed: {}. Retrying in {:.0}s", e, delay.as_secs_f64());
                drain_during_delay(&mut combined_rx, delay).await;
                continue;
            }
        };

        let sender = match conn.open_frame_stream().await {
            Ok(s) => s,
            Err(e) => {
                let delay = reconnect_delay(attempt);
                warn!("Failed to open stream: {}. Retrying in {:.0}s", e, delay.as_secs_f64());
                drain_during_delay(&mut combined_rx, delay).await;
                continue;
            }
        };

        // Success — reset attempt counter
        attempt = 0;
        info!("Connected to server!");

        // Open command stream (best-effort, non-blocking)
        let cmd_conn = conn.clone_handle();
        tokio::spawn(async move {
            match cmd_conn.open_command_stream().await {
                Ok(stream) => {
                    let ready = CommandMessage::Response(CommandResponse {
                        id: 0,
                        result: CommandResult::Ok,
                    });
                    if let Err(e) = stream.sender.send(&ready).await {
                        warn!(error = %e, "Failed to send command ready signal");
                        return;
                    }
                    info!("Command stream opened to server");
                    handle_commands(stream).await;
                }
                Err(e) => {
                    warn!(error = %e, "Failed to open command stream");
                }
            }
        });

        // Start ABR at Minimum tier — avoids overwhelming a slow link
        // (e.g. cellular). ABR will upgrade once throughput is proven.
        let mut throughput_tracker = ThroughputTracker::new(Duration::from_secs(3));
        let mut abr = AbrController::new_at(AbrConfig::default(), QualityTier::Minimum);
        let mut last_abr_eval = Instant::now();
        if abr_enabled {
            if let Some(ref cmd_tx) = bitrate_cmd_tx {
                let _ = cmd_tx.try_send(QualityTier::Minimum.bitrate_bps());
            }
        }

        // ── Stream frames until error ──────────────────────────────────
        let mut session_video = 0u64;
        let mut session_audio = 0u64;
        let mut session_telemetry = 0u64;
        let mut session_kf = 0u64;
        let mut session_bytes = 0u64;
        let mut last_stats = Instant::now();
        let session_connected = Instant::now();
        let mut connection_lost = false;

        // Mark current network state as seen — only react to NEW changes
        net_change_rx.borrow_and_update();

        loop {
            // Wait for either a frame or a network change signal
            let msg = tokio::select! {
                msg = combined_rx.recv() => {
                    match msg {
                        Some(m) => m,
                        None => break, // channel closed, all captures ended
                    }
                }
                Ok(_) = net_change_rx.changed() => {
                    warn!("Network interface changed, breaking connection for fast reconnect");
                    connection_lost = true;
                    break;
                }
            };
            let timestamp_us = session_start.elapsed().as_micros() as u64;

            let frame = match msg {
                CaptureMessage::Video(payload) => {
                    let is_keyframe = h264::contains_keyframe(&payload);
                    session_video += 1;
                    if is_keyframe { session_kf += 1; }
                    motion_detector.update(is_keyframe, payload.len());
                    Frame::video(source_id, payload, is_keyframe)
                        .with_timestamp(timestamp_us)
                }
                CaptureMessage::Audio(payload) => {
                    session_audio += 1;
                    Frame::audio(source_id, payload)
                        .with_timestamp(timestamp_us)
                }
                CaptureMessage::Telemetry(payload) => {
                    session_telemetry += 1;
                    Frame::telemetry(source_id, payload)
                        .with_timestamp(timestamp_us)
                }
            };

            let frame_bytes = frame.payload.len();
            session_bytes += frame_bytes as u64;

            // Send frame — race against network changes AND a 5s timeout.
            // Dead QUIC connections can hang for 30s+ without a timeout.
            let send_ok = tokio::select! {
                result = sender.send(&frame) => {
                    match result {
                        Ok(()) => true,
                        Err(e) => {
                            warn!("Send failed: {}. Will reconnect.", e);
                            false
                        }
                    }
                }
                Ok(_) = net_change_rx.changed() => {
                    warn!("Network changed during send, breaking for fast reconnect");
                    false
                }
                _ = tokio::time::sleep(Duration::from_secs(5)) => {
                    warn!("Send timed out (5s), connection likely dead");
                    false
                }
            };
            if !send_ok {
                connection_lost = true;
                break;
            }

            // ABR evaluation
            if abr_enabled {
                throughput_tracker.record(frame_bytes);

                if last_abr_eval.elapsed() >= Duration::from_secs(1) {
                    last_abr_eval = Instant::now();
                    let throughput = throughput_tracker.bits_per_second();

                    if let AbrDecision::ChangeTo(new_tier) = abr.evaluate(throughput) {
                        if let Some(ref cmd_tx) = bitrate_cmd_tx {
                            let _ = cmd_tx.try_send(new_tier.bitrate_bps());
                        }
                    }
                }
            }

            // Stats every 5 seconds
            if last_stats.elapsed().as_secs() >= 5 {
                let elapsed = session_connected.elapsed().as_secs_f64();
                let fps = session_video as f64 / elapsed;
                let mbps = (session_bytes as f64 * 8.0) / (elapsed * 1_000_000.0);

                if abr_enabled {
                    let tp = throughput_tracker.bits_per_second();
                    let tier = abr.current_tier();
                    info!(
                        "Stats: video={} ({} kf), audio={}, telemetry={} | {:.1} fps, {:.2} Mbps | ABR: {} ({:.1}Mbps), tp={:.2}Mbps",
                        session_video, session_kf, session_audio, session_telemetry,
                        fps, mbps,
                        tier, tier.bitrate_bps() as f64 / 1_000_000.0,
                        tp / 1_000_000.0,
                    );
                } else {
                    info!(
                        "Stats: video={} ({} kf), audio={}, telemetry={} | {:.1} fps, {:.2} Mbps",
                        session_video, session_kf, session_audio, session_telemetry,
                        fps, mbps,
                    );
                }
                last_stats = Instant::now();
            }
        }

        total_video += session_video;
        total_audio += session_audio;
        total_telemetry += session_telemetry;
        total_bytes += session_bytes;

        if connection_lost {
            warn!(
                "Session ended: {} video, {} audio, {} telemetry, {} bytes",
                session_video, session_audio, session_telemetry, session_bytes
            );
            // Small delay before reconnecting (let iroh rebind to new interface)
            tokio::time::sleep(Duration::from_secs(1)).await;
            continue;
        }

        // combined_rx returned None — all capture sources ended
        break;
    }

    info!(
        "Camera shutting down. Total: {} video, {} audio, {} telemetry frames, {} bytes sent",
        total_video, total_audio, total_telemetry, total_bytes
    );

    Ok(())
}

/// Exponential backoff: 1s, 2s, 4s, 8s, 16s, 30s (capped).
fn reconnect_delay(attempt: u32) -> Duration {
    let secs = (1u64 << attempt.min(4)).min(30);
    Duration::from_secs(secs)
}

/// Drain frames from the combined channel during a backoff delay
/// so capture tasks don't block on a full channel.
async fn drain_during_delay(rx: &mut mpsc::Receiver<CaptureMessage>, delay: Duration) {
    let deadline = Instant::now() + delay;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Some(_)) => {} // discard
            Ok(None) => break, // channel closed
            Err(_) => break,   // timeout reached
        }
    }
}

/// Handle incoming commands from the server
async fn handle_commands(stream: CommandStream) {
    loop {
        match stream.receiver.recv().await {
            Ok(Some(CommandMessage::Request(req))) => {
                info!(id = req.id, command = ?req.command, "Received command");
                let result = execute_command(req.command).await;
                let resp = CommandMessage::Response(CommandResponse {
                    id: req.id,
                    result,
                });
                if let Err(e) = stream.sender.send(&resp).await {
                    warn!(error = %e, "Failed to send command response");
                    break;
                }
            }
            Ok(Some(CommandMessage::Response(_))) => {
                warn!("Received unexpected response on command stream");
            }
            Ok(None) => {
                info!("Command stream closed by server");
                break;
            }
            Err(e) => {
                warn!(error = %e, "Command stream error");
                break;
            }
        }
    }
}

/// Execute a command and return the result
async fn execute_command(command: Command) -> CommandResult {
    match command {
        Command::RequestStatus => {
            match gather_status().await {
                Ok(status) => CommandResult::Status(status),
                Err(e) => CommandResult::Error(format!("Failed to gather status: {}", e)),
            }
        }
        Command::Reboot => {
            info!("Reboot command received, scheduling reboot in 2 seconds");
            tokio::spawn(async {
                tokio::time::sleep(Duration::from_secs(2)).await;
                info!("Executing reboot...");
                let _ = tokio::process::Command::new("sudo")
                    .args(["reboot"])
                    .status()
                    .await;
            });
            CommandResult::Ok
        }
        Command::Configure(params) => {
            execute_configure(params).await
        }
        Command::Record(params) => {
            execute_record(params).await
        }
        Command::UpdateFirmware(params) => {
            execute_update_firmware(params).await
        }
        Command::Network(params) => {
            execute_network(params).await
        }
        Command::ListRecordings => {
            execute_list_recordings().await
        }
        Command::DeleteRecording(params) => {
            execute_delete_recording(params).await
        }
        Command::SendRecording(params) => {
            execute_send_recording(params).await
        }
        Command::Stream(params) => {
            execute_stream(params).await
        }
    }
}

/// Handle Configure command — log requested changes
async fn execute_configure(params: ConfigureParams) -> CommandResult {
    info!(
        "Configure command: width={:?} height={:?} fps={:?} bitrate={:?} keyframe_interval={:?}",
        params.width, params.height, params.fps, params.bitrate, params.keyframe_interval
    );
    // Phase 2: apply to shared capture pipeline state
    CommandResult::Error("Configure not yet wired to capture pipeline (Phase 2)".into())
}

/// Handle Record command — start/stop local recording
async fn execute_record(params: RecordParams) -> CommandResult {
    if params.start {
        info!("Record START requested, duration={:?}s", params.duration_secs);
    } else {
        info!("Record STOP requested");
    }
    // Phase 2: needs local recording infrastructure
    CommandResult::Error("Recording not yet implemented (Phase 2)".into())
}

/// Handle UpdateFirmware command — download and verify binary
async fn execute_update_firmware(params: UpdateFirmwareParams) -> CommandResult {
    info!("Firmware update requested from url={}, sha256={}", params.url, params.sha256);
    // Phase 2: download, verify hash, self-replace
    CommandResult::Error("Firmware update not yet implemented (Phase 2)".into())
}

/// Handle Network command — WiFi management
async fn execute_network(params: NetworkParams) -> CommandResult {
    match params.action {
        NetworkAction::ScanWifi => {
            info!("WiFi scan requested");
            CommandResult::Error("WiFi scan not yet implemented (Phase 2)".into())
        }
        NetworkAction::ConnectWifi { ssid, password: _ } => {
            info!("WiFi connect requested: ssid={}", ssid);
            CommandResult::Error("WiFi connect not yet implemented (Phase 2)".into())
        }
        NetworkAction::DisconnectWifi => {
            info!("WiFi disconnect requested");
            CommandResult::Error("WiFi disconnect not yet implemented (Phase 2)".into())
        }
        NetworkAction::GetStatus => {
            info!("Network status requested");
            // Return basic network info from ip/hostname
            match gather_network_status().await {
                Ok(_info) => CommandResult::Ok,
                Err(e) => CommandResult::Error(format!("Failed to get network status: {}", e)),
            }
        }
    }
}

/// Handle ListRecordings command
async fn execute_list_recordings() -> CommandResult {
    info!("List recordings requested");
    // Return empty list — no local recording infrastructure yet
    CommandResult::RecordingsList(vec![])
}

/// Handle DeleteRecording command
async fn execute_delete_recording(params: DeleteRecordingParams) -> CommandResult {
    info!("Delete recording requested: id={}", params.recording_id);
    CommandResult::Error(format!("Recording '{}' not found", params.recording_id))
}

/// Handle SendRecording command
async fn execute_send_recording(params: SendRecordingParams) -> CommandResult {
    info!(
        "Send recording requested: id={} destination={}",
        params.recording_id, params.destination
    );
    CommandResult::Error(format!("Recording '{}' not found", params.recording_id))
}

/// Handle Stream command — start/stop live stream
async fn execute_stream(params: StreamParams) -> CommandResult {
    if params.start {
        info!("Stream START requested");
    } else {
        info!("Stream STOP requested");
    }
    // The camera is always streaming when connected, so this is a no-op for now
    CommandResult::Ok
}

/// Gather basic network status
async fn gather_network_status() -> Result<String> {
    let output = tokio::process::Command::new("hostname")
        .arg("-I")
        .output()
        .await?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Gather camera status information from the system
async fn gather_status() -> Result<CameraStatus> {
    // Read /proc/uptime
    let uptime_secs = tokio::fs::read_to_string("/proc/uptime")
        .await
        .ok()
        .and_then(|s| s.split_whitespace().next().and_then(|v| v.parse::<f64>().ok()))
        .map(|v| v as u64)
        .unwrap_or(0);

    // Read /proc/meminfo
    let (memory_used, memory_total) = tokio::fs::read_to_string("/proc/meminfo")
        .await
        .ok()
        .map(|s| {
            let mut total = 0u64;
            let mut available = 0u64;
            for line in s.lines() {
                if line.starts_with("MemTotal:") {
                    total = line.split_whitespace().nth(1).and_then(|v| v.parse().ok()).unwrap_or(0) * 1024;
                } else if line.starts_with("MemAvailable:") {
                    available = line.split_whitespace().nth(1).and_then(|v| v.parse().ok()).unwrap_or(0) * 1024;
                }
            }
            (total.saturating_sub(available), total)
        })
        .unwrap_or((0, 0));

    // Read CPU temperature (Pi-specific path)
    let cpu_temp = tokio::fs::read_to_string("/sys/class/thermal/thermal_zone0/temp")
        .await
        .ok()
        .and_then(|s| s.trim().parse::<f32>().ok())
        .map(|t| t / 1000.0);

    // Read /proc/stat for CPU usage (simple snapshot — not averaged)
    let cpu_percent = tokio::fs::read_to_string("/proc/stat")
        .await
        .ok()
        .and_then(|s| {
            let line = s.lines().next()?;
            let parts: Vec<u64> = line.split_whitespace().skip(1).filter_map(|v| v.parse().ok()).collect();
            if parts.len() >= 4 {
                let total: u64 = parts.iter().sum();
                let idle = parts[3];
                if total > 0 {
                    return Some(((total - idle) as f32 / total as f32) * 100.0);
                }
            }
            None
        })
        .unwrap_or(0.0);

    Ok(CameraStatus {
        cpu_percent,
        memory_used,
        memory_total,
        cpu_temp,
        uptime_secs,
        video_active: true, // We're running if we're here
        audio_active: true,
        video_resolution: None, // Would need shared state with capture pipeline
        video_fps: None,
        video_bitrate: None,
        disk_used: None,
        disk_total: None,
    })
}

/// Monitor the default network interface and notify on changes.
/// Reads /proc/net/route every 500ms (Linux-only; no-op on other platforms).
///
/// Only signals when the route settles on a DIFFERENT real interface
/// (e.g. wlan0→wwan0 or wwan0→wlan0). Never signals for drops to None —
/// the send loop's 5s timeout handles dead connections instead.
/// This prevents NM's cellular route cycling from breaking active connections.
async fn network_monitor(_endpoint: iroh::Endpoint, change_tx: watch::Sender<u64>) {
    let mut last_signaled = get_default_interface();
    let mut generation = 0u64;
    info!("Network monitor started, default interface: {:?}", last_signaled);

    loop {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let current = get_default_interface();

        if current == last_signaled {
            continue;
        }

        // Route changed — only care about changes to a DIFFERENT real interface.
        // Drops to None are handled by the send loop's timeout.
        if current.is_none() {
            // Route dropped to None. Don't signal — it may come back (NM cycling).
            // Just update tracking so we detect when a NEW interface appears.
            continue;
        }

        // A real interface appeared that differs from last_signaled.
        // Quick 1s debounce to avoid acting on transient flaps.
        warn!("Default route change: {:?} → {:?}, debouncing 1s...", last_signaled, current);
        tokio::time::sleep(Duration::from_secs(1)).await;
        let settled = get_default_interface();

        if settled == last_signaled {
            debug!("Route settled back to {:?}, ignoring", last_signaled);
            continue;
        }

        if settled.is_none() {
            // Flapped away during debounce
            debug!("Route went None during debounce, ignoring");
            continue;
        }

        // Definitive change to a different real interface
        warn!("Default route settled: {:?} → {:?}", last_signaled, settled);
        last_signaled = settled;
        generation += 1;
        let _ = change_tx.send(generation);
    }
}

/// Read the default route interface from /proc/net/route (Linux).
/// Returns None on non-Linux or if no default route exists.
fn get_default_interface() -> Option<String> {
    let content = std::fs::read_to_string("/proc/net/route").ok()?;
    for line in content.lines().skip(1) {
        let fields: Vec<&str> = line.split_whitespace().collect();
        // Destination 00000000 = default route
        if fields.len() >= 2 && fields[1] == "00000000" {
            return Some(fields[0].to_string());
        }
    }
    None
}
