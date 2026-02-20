//! Command protocol types for bidirectional camera control
//!
//! Commands are sent from server (or client via server) to cameras.
//! Each request has a correlation ID for matching responses.
//! Serialization uses MessagePack via rmp-serde.

use serde::{Deserialize, Serialize};
use std::fmt;

/// A string wrapper that redacts its contents in Debug output.
///
/// Use for sensitive values (passwords, tokens) that should not appear in logs.
/// Serializes/deserializes transparently as a plain string.
#[derive(Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RedactedString(pub String);

impl fmt::Debug for RedactedString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "\"***\"")
    }
}

impl From<String> for RedactedString {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for RedactedString {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

/// Maximum command message size (1 MB)
pub const MAX_COMMAND_SIZE: usize = 1024 * 1024;

/// Wire envelope: either a request or a response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CommandMessage {
    Request(CommandRequest),
    Response(CommandResponse),
}

/// A command request with correlation ID
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandRequest {
    pub id: u32,
    pub command: Command,
}

/// A command response matched to a request by ID
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandResponse {
    pub id: u32,
    pub result: CommandResult,
}

/// All supported commands
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Command {
    /// Request current camera status (CPU, memory, temp, uptime, capture state)
    RequestStatus,
    /// Configure camera parameters
    Configure(ConfigureParams),
    /// Start/stop recording
    Record(RecordParams),
    /// Reboot the camera
    Reboot,
    /// Update firmware/binary
    UpdateFirmware(UpdateFirmwareParams),
    /// Network management (WiFi, etc.)
    Network(NetworkParams),
    /// List recordings on camera
    ListRecordings,
    /// Delete a recording
    DeleteRecording(DeleteRecordingParams),
    /// Send a recording to server/cloud
    SendRecording(SendRecordingParams),
    /// Start/stop live stream
    Stream(StreamParams),
}

/// Response to a command
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CommandResult {
    /// Command succeeded
    Ok,
    /// Command failed
    Error(String),
    /// Status response
    Status(CameraStatus),
    /// List of recordings
    RecordingsList(Vec<RecordingInfo>),
    /// Firmware update started (download in progress)
    UpdateStarted { update_id: String },
}

/// Camera status information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CameraStatus {
    /// CPU usage percentage (0-100)
    pub cpu_percent: f32,
    /// Memory usage in bytes
    pub memory_used: u64,
    /// Total memory in bytes
    pub memory_total: u64,
    /// CPU temperature in Celsius
    pub cpu_temp: Option<f32>,
    /// Uptime in seconds
    pub uptime_secs: u64,
    /// Whether video capture is active
    pub video_active: bool,
    /// Whether audio capture is active
    pub audio_active: bool,
    /// Current video resolution
    pub video_resolution: Option<(u32, u32)>,
    /// Current video FPS
    pub video_fps: Option<u32>,
    /// Current bitrate in bps
    pub video_bitrate: Option<u32>,
    /// Disk usage in bytes
    pub disk_used: Option<u64>,
    /// Disk total in bytes
    pub disk_total: Option<u64>,
    /// Firmware version string (e.g. "0.1.0+abc1234")
    #[serde(default)]
    pub version: Option<String>,
}

/// Parameters for camera configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigureParams {
    /// New video width
    pub width: Option<u32>,
    /// New video height
    pub height: Option<u32>,
    /// New FPS
    pub fps: Option<u32>,
    /// New bitrate in bps
    pub bitrate: Option<u32>,
    /// New keyframe interval (in frames)
    pub keyframe_interval: Option<u32>,
}

/// Parameters for recording control
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordParams {
    /// true = start recording, false = stop recording
    pub start: bool,
    /// Optional duration limit in seconds
    pub duration_secs: Option<u64>,
}

/// Parameters for firmware update
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateFirmwareParams {
    /// URL to download the new firmware from
    pub url: String,
    /// Expected SHA256 hash for verification
    pub sha256: String,
    /// Target version string (optional, used to skip if already running)
    #[serde(default)]
    pub version: Option<String>,
}

/// Parameters for network management
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkParams {
    /// Action to perform
    pub action: NetworkAction,
}

/// Network actions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NetworkAction {
    /// List available WiFi networks
    ScanWifi,
    /// Connect to a WiFi network
    ConnectWifi {
        ssid: String,
        password: RedactedString,
    },
    /// Disconnect from WiFi
    DisconnectWifi,
    /// Get current network info
    GetStatus,
}

/// Parameters for deleting a recording
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeleteRecordingParams {
    /// Recording ID to delete
    pub recording_id: String,
}

/// Parameters for sending a recording
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendRecordingParams {
    /// Recording ID to send
    pub recording_id: String,
    /// Destination URL or identifier
    pub destination: String,
}

/// Parameters for stream control
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamParams {
    /// true = start streaming, false = stop streaming
    pub start: bool,
}

/// Recording information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordingInfo {
    /// Unique recording identifier
    pub id: String,
    /// Start timestamp (unix seconds)
    pub start_time: u64,
    /// Duration in seconds
    pub duration_secs: u64,
    /// File size in bytes
    pub size_bytes: u64,
}

/// A command request from a client that targets a specific camera.
/// Used for client -> server -> camera routing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetedCommandRequest {
    /// Correlation ID (assigned by client)
    pub id: u32,
    /// Target camera public key (base32 string)
    pub target_camera: String,
    /// The command to send
    pub command: Command,
}

/// Wire envelope for client <-> server command channel
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClientCommandMessage {
    Request(TargetedCommandRequest),
    Response(CommandResponse),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_command_roundtrip() {
        let msg = CommandMessage::Request(CommandRequest {
            id: 42,
            command: Command::RequestStatus,
        });

        let bytes = rmp_serde::to_vec(&msg).unwrap();
        let decoded: CommandMessage = rmp_serde::from_slice(&bytes).unwrap();

        match decoded {
            CommandMessage::Request(req) => {
                assert_eq!(req.id, 42);
                assert!(matches!(req.command, Command::RequestStatus));
            }
            _ => panic!("Expected Request"),
        }
    }

    #[test]
    fn test_response_roundtrip() {
        let msg = CommandMessage::Response(CommandResponse {
            id: 42,
            result: CommandResult::Status(CameraStatus {
                cpu_percent: 45.2,
                memory_used: 1024 * 1024 * 100,
                memory_total: 1024 * 1024 * 512,
                cpu_temp: Some(55.0),
                uptime_secs: 3600,
                video_active: true,
                audio_active: true,
                video_resolution: Some((1280, 720)),
                video_fps: Some(30),
                video_bitrate: Some(5_000_000),
                disk_used: None,
                disk_total: None,
                version: Some("0.1.0+abc1234".into()),
            }),
        });

        let bytes = rmp_serde::to_vec(&msg).unwrap();
        let decoded: CommandMessage = rmp_serde::from_slice(&bytes).unwrap();

        match decoded {
            CommandMessage::Response(resp) => {
                assert_eq!(resp.id, 42);
                assert!(matches!(resp.result, CommandResult::Status(_)));
            }
            _ => panic!("Expected Response"),
        }
    }

    #[test]
    fn test_targeted_command_roundtrip() {
        let msg = ClientCommandMessage::Request(TargetedCommandRequest {
            id: 1,
            target_camera: "abc123".into(),
            command: Command::Reboot,
        });

        let bytes = rmp_serde::to_vec(&msg).unwrap();
        let decoded: ClientCommandMessage = rmp_serde::from_slice(&bytes).unwrap();

        match decoded {
            ClientCommandMessage::Request(req) => {
                assert_eq!(req.id, 1);
                assert_eq!(req.target_camera, "abc123");
            }
            _ => panic!("Expected Request"),
        }
    }
}
