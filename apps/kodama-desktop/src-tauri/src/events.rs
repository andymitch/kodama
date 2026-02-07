//! Tauri event payload types for frontend communication

use serde::Serialize;

/// Camera connected/disconnected event
#[derive(Debug, Clone, Serialize)]
pub struct CameraEvent {
    pub source_id: String,
    pub connected: bool,
}

/// Video init segment (ftyp+moov) - sent once per camera when SPS/PPS is found
#[derive(Debug, Clone, Serialize)]
pub struct VideoInitEvent {
    pub source_id: String,
    /// avcC codec string e.g. "avc1.42001e"
    pub codec: String,
    pub width: u32,
    pub height: u32,
    /// Base64-encoded fMP4 init segment
    pub init_segment: String,
}

/// Video media segment (moof+mdat) - sent per frame
#[derive(Debug, Clone, Serialize)]
pub struct VideoSegmentEvent {
    pub source_id: String,
    /// Base64-encoded fMP4 media segment
    pub data: String,
}

/// Audio level event - computed RMS from PCM
#[derive(Debug, Clone, Serialize)]
pub struct AudioLevelEvent {
    pub source_id: String,
    /// RMS level in dB (typically -60 to 0)
    pub level_db: f32,
}

/// Audio data event - actual PCM samples for playback
#[derive(Debug, Clone, Serialize)]
pub struct AudioDataEvent {
    pub source_id: String,
    /// Base64-encoded PCM s16le data (48000 Hz, mono, 16-bit)
    pub data: String,
    /// Sample rate in Hz
    pub sample_rate: u32,
    /// Number of channels (1 = mono, 2 = stereo)
    pub channels: u8,
}

/// GPS position data for frontend
#[derive(Debug, Clone, Serialize)]
pub struct GpsEvent {
    pub latitude: f64,
    pub longitude: f64,
    pub altitude: Option<f64>,
    pub speed: Option<f64>,
    pub heading: Option<f64>,
    pub fix_mode: u8,
}

/// Telemetry data event
#[derive(Debug, Clone, Serialize)]
pub struct TelemetryEvent {
    pub source_id: String,
    pub cpu_usage: f32,
    pub cpu_temp: Option<f32>,
    pub memory_usage: f32,
    pub disk_usage: f32,
    pub uptime_secs: u64,
    pub load_average: [f32; 3],
    pub gps: Option<GpsEvent>,
    pub motion_level: Option<f32>,
}
