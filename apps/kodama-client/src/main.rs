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
use kodama_core::{
    Channel, Frame, SourceId,
    Command, ClientCommandMessage, TargetedCommandRequest, CommandResult,
    ConfigureParams, RecordParams, UpdateFirmwareParams, NetworkParams, NetworkAction,
    ShellParams, DeleteRecordingParams, SendRecordingParams, StreamParams,
};
use kodama_relay::Relay;
use kodama_capture::{decode_telemetry, SparseTelemetry};

/// Client configuration from environment/args
struct Config {
    /// Server's public key to connect to
    server_key: PublicKey,
    /// Optional path to save received frames
    save_frames_path: Option<PathBuf>,
    /// Show telemetry data
    show_telemetry: bool,
    /// Send a command to a camera and exit (format: "camera_key:command")
    send_command: Option<(String, String)>,
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

        // --send-command CAMERA_KEY COMMAND (e.g. --send-command <key> status)
        let send_command = args.iter()
            .position(|arg| arg == "--send-command")
            .and_then(|i| {
                let camera = args.get(i + 1)?.clone();
                let cmd = args.get(i + 2).cloned().unwrap_or_else(|| "status".into());
                Some((camera, cmd))
            });

        Ok(Self {
            server_key,
            save_frames_path,
            show_telemetry,
            send_command,
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
    last_telemetry: Option<SparseTelemetry>,
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

    // If --send-command was specified, send command and exit
    if let Some((camera_key, cmd_name)) = config.send_command {
        return send_command_and_exit(&conn, &camera_key, &cmd_name).await;
    }

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
                            let motion_str = telemetry.motion_level
                                .map(|m| format!(", Motion={:.2}", m))
                                .unwrap_or_default();
                            let gps_str = telemetry.gps.as_ref()
                                .map(|g| format!(", GPS={:.5},{:.5} fix={}D{}{}",
                                    g.latitude, g.longitude, g.fix_mode,
                                    g.speed.map(|s| format!(" {:.1}m/s", s)).unwrap_or_default(),
                                    g.altitude.map(|a| format!(" {:.1}m", a)).unwrap_or_default(),
                                ))
                                .unwrap_or_default();
                            let load = telemetry.load_average.unwrap_or([0.0; 3]);
                            info!(
                                "Telemetry [{}]: CPU={:.1}%{}, Mem={:.1}%, Load=[{:.2}, {:.2}, {:.2}], Up={}s{}{}",
                                source,
                                telemetry.cpu_usage.unwrap_or(0.0),
                                telemetry.cpu_temp.map(|t| format!(", Temp={:.1}C", t)).unwrap_or_default(),
                                telemetry.memory_usage.unwrap_or(0.0),
                                load[0], load[1], load[2],
                                telemetry.uptime_secs.unwrap_or(0),
                                motion_str,
                                gps_str,
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

/// Send a command to a camera via the server and print the response
async fn send_command_and_exit(
    conn: &kodama_relay::RelayConnection,
    camera_key: &str,
    cmd_name: &str,
) -> Result<()> {
    // Wait for server to detect us as client (timeout-based)
    info!("Waiting for server role detection...");
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Open client command stream
    info!("Opening command stream...");
    let cmd_stream = conn.open_client_command_stream().await
        .context("Failed to open client command stream")?;

    // Parse command — supports all Command variants
    let command = parse_command(cmd_name)?;

    // Send targeted command
    let request = ClientCommandMessage::Request(TargetedCommandRequest {
        id: 1,
        target_camera: camera_key.to_string(),
        command,
    });

    info!("Sending {} command to camera {}...", cmd_name, camera_key);
    cmd_stream.sender.send(&request).await
        .context("Failed to send command")?;

    // Wait for response
    info!("Waiting for response...");
    match tokio::time::timeout(Duration::from_secs(15), cmd_stream.receiver.recv()).await {
        Ok(Ok(Some(ClientCommandMessage::Response(resp)))) => {
            print_command_result(&resp.result);
        }
        Ok(Ok(Some(other))) => {
            anyhow::bail!("Unexpected message: {:?}", other);
        }
        Ok(Ok(None)) => {
            anyhow::bail!("Command stream closed before response");
        }
        Ok(Err(e)) => {
            anyhow::bail!("Command stream error: {}", e);
        }
        Err(_) => {
            anyhow::bail!("Timeout waiting for command response (15s)");
        }
    }

    Ok(())
}

/// Parse a command name (with optional inline args) into a Command
///
/// Supported formats:
///   status                          → RequestStatus
///   reboot                          → Reboot
///   configure:fps=15,bitrate=2000000 → Configure
///   record:start                    → Record { start: true }
///   record:stop                     → Record { start: false }
///   update-firmware:url=X,sha256=Y  → UpdateFirmware
///   network:scan                    → Network(ScanWifi)
///   network:status                  → Network(GetStatus)
///   network:disconnect              → Network(DisconnectWifi)
///   shell:COMMAND                   → Shell { command: COMMAND }
///   list-recordings                 → ListRecordings
///   delete-recording:ID             → DeleteRecording
///   send-recording:ID,DEST          → SendRecording
///   stream:start                    → Stream { start: true }
///   stream:stop                     → Stream { start: false }
fn parse_command(input: &str) -> Result<Command> {
    let (name, args) = input.split_once(':').unwrap_or((input, ""));

    match name {
        "status" => Ok(Command::RequestStatus),
        "reboot" => Ok(Command::Reboot),
        "configure" => {
            let mut params = ConfigureParams {
                width: None,
                height: None,
                fps: None,
                bitrate: None,
                keyframe_interval: None,
            };
            for pair in args.split(',') {
                if let Some((k, v)) = pair.split_once('=') {
                    match k {
                        "width" => params.width = v.parse().ok(),
                        "height" => params.height = v.parse().ok(),
                        "fps" => params.fps = v.parse().ok(),
                        "bitrate" => params.bitrate = v.parse().ok(),
                        "keyframe_interval" => params.keyframe_interval = v.parse().ok(),
                        _ => {}
                    }
                }
            }
            Ok(Command::Configure(params))
        }
        "record" => {
            let start = args != "stop";
            let duration_secs = args.strip_prefix("start,")
                .and_then(|rest| rest.parse().ok());
            Ok(Command::Record(RecordParams { start, duration_secs }))
        }
        "update-firmware" => {
            let mut url = String::new();
            let mut sha256 = String::new();
            for pair in args.split(',') {
                if let Some((k, v)) = pair.split_once('=') {
                    match k {
                        "url" => url = v.to_string(),
                        "sha256" => sha256 = v.to_string(),
                        _ => {}
                    }
                }
            }
            Ok(Command::UpdateFirmware(UpdateFirmwareParams { url, sha256 }))
        }
        "network" => {
            let action = match args {
                "scan" => NetworkAction::ScanWifi,
                "status" => NetworkAction::GetStatus,
                "disconnect" => NetworkAction::DisconnectWifi,
                _ => anyhow::bail!(
                    "Unknown network action: '{}'. Available: scan, status, disconnect",
                    args
                ),
            };
            Ok(Command::Network(NetworkParams { action }))
        }
        "shell" => {
            if args.is_empty() {
                anyhow::bail!("Shell command required. Usage: shell:echo hello");
            }
            Ok(Command::Shell(ShellParams {
                command: args.to_string(),
                timeout_secs: Some(30),
            }))
        }
        "list-recordings" => Ok(Command::ListRecordings),
        "delete-recording" => {
            if args.is_empty() {
                anyhow::bail!("Recording ID required. Usage: delete-recording:ID");
            }
            Ok(Command::DeleteRecording(DeleteRecordingParams {
                recording_id: args.to_string(),
            }))
        }
        "send-recording" => {
            let (id, dest) = args.split_once(',')
                .context("Usage: send-recording:ID,DESTINATION")?;
            Ok(Command::SendRecording(SendRecordingParams {
                recording_id: id.to_string(),
                destination: dest.to_string(),
            }))
        }
        "stream" => {
            let start = args != "stop";
            Ok(Command::Stream(StreamParams { start }))
        }
        _ => {
            anyhow::bail!(
                "Unknown command: '{}'. Available: status, reboot, configure, record, \
                 update-firmware, network, shell, list-recordings, delete-recording, \
                 send-recording, stream",
                name
            );
        }
    }
}

/// Print a CommandResult in a human-readable format
fn print_command_result(result: &CommandResult) {
    match result {
        CommandResult::Ok => {
            println!("OK");
        }
        CommandResult::Error(e) => {
            println!("ERROR: {}", e);
        }
        CommandResult::Status(status) => {
            println!("Camera Status:");
            println!("  CPU:        {:.1}%", status.cpu_percent);
            println!("  Memory:     {} / {} MB",
                status.memory_used / (1024 * 1024),
                status.memory_total / (1024 * 1024));
            if let Some(temp) = status.cpu_temp {
                println!("  CPU Temp:   {:.1}C", temp);
            }
            println!("  Uptime:     {}s", status.uptime_secs);
            println!("  Video:      {}", if status.video_active { "active" } else { "inactive" });
            println!("  Audio:      {}", if status.audio_active { "active" } else { "inactive" });
            if let Some((w, h)) = status.video_resolution {
                println!("  Resolution: {}x{}", w, h);
            }
            if let Some(fps) = status.video_fps {
                println!("  FPS:        {}", fps);
            }
            if let Some(br) = status.video_bitrate {
                println!("  Bitrate:    {} bps", br);
            }
        }
        CommandResult::RecordingsList(recordings) => {
            if recordings.is_empty() {
                println!("No recordings found.");
            } else {
                println!("Recordings ({}):", recordings.len());
                for r in recordings {
                    println!("  {} | {}s | {} bytes | started {}", r.id, r.duration_secs, r.size_bytes, r.start_time);
                }
            }
        }
        CommandResult::ShellOutput(output) => {
            if !output.stdout.is_empty() {
                print!("{}", output.stdout);
            }
            if !output.stderr.is_empty() {
                eprint!("{}", output.stderr);
            }
            if output.exit_code != 0 {
                println!("(exit code: {})", output.exit_code);
            }
        }
    }
}
