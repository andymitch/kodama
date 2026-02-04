//! Telemetry capture module (stub)
//!
//! Future: Collect system metrics (CPU, temperature, disk usage, etc.)
//! and encode as MessagePack for streaming.

use anyhow::Result;
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

/// Telemetry data from a camera
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetryData {
    /// CPU usage percentage (0-100)
    pub cpu_usage: f32,
    /// CPU temperature in Celsius
    pub cpu_temp: f32,
    /// Memory usage percentage (0-100)
    pub memory_usage: f32,
    /// Disk usage percentage (0-100)
    pub disk_usage: f32,
    /// Network bytes sent since boot
    pub network_tx_bytes: u64,
    /// Network bytes received since boot
    pub network_rx_bytes: u64,
    /// Uptime in seconds
    pub uptime_secs: u64,
}

/// Telemetry capture configuration
#[derive(Debug, Clone)]
pub struct TelemetryCaptureConfig {
    /// How often to collect telemetry (in seconds)
    pub interval_secs: u32,
}

impl Default for TelemetryCaptureConfig {
    fn default() -> Self {
        Self {
            interval_secs: 5, // Every 5 seconds
        }
    }
}

/// Handle to running telemetry collection
pub struct TelemetryCapture {
    _config: TelemetryCaptureConfig,
}

impl TelemetryCapture {
    /// Start telemetry collection
    ///
    /// Returns a receiver for MessagePack-encoded telemetry packets.
    pub fn start(_config: TelemetryCaptureConfig) -> Result<(Self, mpsc::Receiver<Bytes>)> {
        // TODO: Implement telemetry collection
        // - Read /proc/stat for CPU
        // - Read /sys/class/thermal for temperature
        // - Read /proc/meminfo for memory
        // - Read /proc/net/dev for network
        anyhow::bail!("Telemetry capture not yet implemented")
    }

    /// Stop collection
    pub fn stop(&mut self) {
        // TODO: Clean up
    }
}

/// Encode telemetry data to MessagePack bytes
pub fn encode_telemetry(data: &TelemetryData) -> Result<Bytes> {
    // TODO: Use rmp-serde for MessagePack encoding
    let _ = data;
    anyhow::bail!("Telemetry encoding not yet implemented")
}

/// Decode telemetry data from MessagePack bytes
pub fn decode_telemetry(_data: &[u8]) -> Result<TelemetryData> {
    // TODO: Use rmp-serde for MessagePack decoding
    anyhow::bail!("Telemetry decoding not yet implemented")
}
