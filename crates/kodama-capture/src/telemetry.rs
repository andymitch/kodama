//! Telemetry capture module
//!
//! Collects system metrics (CPU, temperature, disk usage, etc.)
//! and encodes as JSON for streaming. Works on Linux systems.

use anyhow::{Context, Result};
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use std::fs;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{debug, warn};

/// Telemetry data from a camera
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetryData {
    /// CPU usage percentage (0-100)
    pub cpu_usage: f32,
    /// CPU temperature in Celsius (None if unavailable)
    pub cpu_temp: Option<f32>,
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
    /// Load average (1, 5, 15 minutes)
    pub load_average: [f32; 3],
}

impl Default for TelemetryData {
    fn default() -> Self {
        Self {
            cpu_usage: 0.0,
            cpu_temp: None,
            memory_usage: 0.0,
            disk_usage: 0.0,
            network_tx_bytes: 0,
            network_rx_bytes: 0,
            uptime_secs: 0,
            load_average: [0.0, 0.0, 0.0],
        }
    }
}

/// Telemetry capture configuration
#[derive(Debug, Clone)]
pub struct TelemetryCaptureConfig {
    /// How often to collect telemetry (in seconds)
    pub interval_secs: u32,
    /// Network interface to monitor (None = sum all)
    pub network_interface: Option<String>,
    /// Disk path to monitor for usage
    pub disk_path: String,
}

impl Default for TelemetryCaptureConfig {
    fn default() -> Self {
        Self {
            interval_secs: 5,
            network_interface: None,
            disk_path: "/".to_string(),
        }
    }
}

/// Handle to running telemetry collection
pub struct TelemetryCapture {
    shutdown_tx: Option<mpsc::Sender<()>>,
}

impl TelemetryCapture {
    /// Start telemetry collection
    ///
    /// Returns a receiver for JSON-encoded telemetry packets.
    pub fn start(config: TelemetryCaptureConfig) -> Result<(Self, mpsc::Receiver<Bytes>)> {
        let (tx, rx) = mpsc::channel(16);
        let (shutdown_tx, mut shutdown_rx) = mpsc::channel(1);

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(config.interval_secs as u64));
            let mut prev_cpu_stats: Option<CpuStats> = None;

            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        match collect_telemetry(&config, &mut prev_cpu_stats) {
                            Ok(data) => {
                                match encode_telemetry(&data) {
                                    Ok(bytes) => {
                                        if tx.send(bytes).await.is_err() {
                                            debug!("Telemetry receiver dropped");
                                            break;
                                        }
                                    }
                                    Err(e) => {
                                        warn!("Failed to encode telemetry: {}", e);
                                    }
                                }
                            }
                            Err(e) => {
                                warn!("Failed to collect telemetry: {}", e);
                            }
                        }
                    }
                    _ = shutdown_rx.recv() => {
                        debug!("Telemetry capture shutdown requested");
                        break;
                    }
                }
            }
        });

        Ok((Self { shutdown_tx: Some(shutdown_tx) }, rx))
    }

    /// Stop collection
    pub fn stop(&mut self) {
        self.shutdown_tx.take();
    }
}

impl Drop for TelemetryCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Raw CPU stats from /proc/stat
#[derive(Debug, Clone)]
struct CpuStats {
    user: u64,
    nice: u64,
    system: u64,
    idle: u64,
    iowait: u64,
    irq: u64,
    softirq: u64,
}

impl CpuStats {
    fn total(&self) -> u64 {
        self.user + self.nice + self.system + self.idle + self.iowait + self.irq + self.softirq
    }

    fn idle_total(&self) -> u64 {
        self.idle + self.iowait
    }
}

/// Collect all telemetry data
fn collect_telemetry(config: &TelemetryCaptureConfig, prev_cpu: &mut Option<CpuStats>) -> Result<TelemetryData> {
    let mut data = TelemetryData::default();

    // CPU usage (compare with previous sample)
    if let Ok(cpu_stats) = read_cpu_stats() {
        if let Some(prev) = prev_cpu.take() {
            let total_delta = cpu_stats.total().saturating_sub(prev.total());
            let idle_delta = cpu_stats.idle_total().saturating_sub(prev.idle_total());
            if total_delta > 0 {
                data.cpu_usage = ((total_delta - idle_delta) as f32 / total_delta as f32) * 100.0;
            }
        }
        *prev_cpu = Some(cpu_stats);
    }

    // CPU temperature
    data.cpu_temp = read_cpu_temp().ok();

    // Memory usage
    if let Ok((used, total)) = read_memory_info() {
        if total > 0 {
            data.memory_usage = (used as f32 / total as f32) * 100.0;
        }
    }

    // Disk usage
    if let Ok((used, total)) = read_disk_usage(&config.disk_path) {
        if total > 0 {
            data.disk_usage = (used as f32 / total as f32) * 100.0;
        }
    }

    // Network stats
    if let Ok((rx, tx)) = read_network_stats(config.network_interface.as_deref()) {
        data.network_rx_bytes = rx;
        data.network_tx_bytes = tx;
    }

    // Uptime
    if let Ok(uptime) = read_uptime() {
        data.uptime_secs = uptime;
    }

    // Load average
    if let Ok(load) = read_load_average() {
        data.load_average = load;
    }

    Ok(data)
}

/// Read CPU stats from /proc/stat
fn read_cpu_stats() -> Result<CpuStats> {
    let content = fs::read_to_string("/proc/stat")
        .context("Failed to read /proc/stat")?;

    let cpu_line = content.lines()
        .find(|line| line.starts_with("cpu "))
        .context("No cpu line in /proc/stat")?;

    let parts: Vec<u64> = cpu_line
        .split_whitespace()
        .skip(1)
        .take(7)
        .filter_map(|s| s.parse().ok())
        .collect();

    if parts.len() < 7 {
        anyhow::bail!("Invalid /proc/stat format");
    }

    Ok(CpuStats {
        user: parts[0],
        nice: parts[1],
        system: parts[2],
        idle: parts[3],
        iowait: parts[4],
        irq: parts[5],
        softirq: parts[6],
    })
}

/// Read CPU temperature from thermal zone
fn read_cpu_temp() -> Result<f32> {
    // Try common thermal zone paths
    let paths = [
        "/sys/class/thermal/thermal_zone0/temp",
        "/sys/class/hwmon/hwmon0/temp1_input",
    ];

    for path in paths {
        if let Ok(content) = fs::read_to_string(path) {
            if let Ok(millidegrees) = content.trim().parse::<i64>() {
                return Ok(millidegrees as f32 / 1000.0);
            }
        }
    }

    anyhow::bail!("No temperature sensor found")
}

/// Read memory info from /proc/meminfo
fn read_memory_info() -> Result<(u64, u64)> {
    let content = fs::read_to_string("/proc/meminfo")
        .context("Failed to read /proc/meminfo")?;

    let mut total: u64 = 0;
    let mut available: u64 = 0;

    for line in content.lines() {
        if line.starts_with("MemTotal:") {
            total = parse_meminfo_value(line)?;
        } else if line.starts_with("MemAvailable:") {
            available = parse_meminfo_value(line)?;
        }
    }

    let used = total.saturating_sub(available);
    Ok((used, total))
}

fn parse_meminfo_value(line: &str) -> Result<u64> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() >= 2 {
        parts[1].parse().context("Invalid meminfo value")
    } else {
        anyhow::bail!("Invalid meminfo line")
    }
}

/// Read disk usage using statvfs
fn read_disk_usage(path: &str) -> Result<(u64, u64)> {
    // Use df command as a fallback since statvfs requires libc
    let output = std::process::Command::new("df")
        .args(["-B1", path])
        .output()
        .context("Failed to run df")?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout.lines().nth(1).context("No df output")?;
    let parts: Vec<&str> = line.split_whitespace().collect();

    if parts.len() >= 4 {
        let total: u64 = parts[1].parse().unwrap_or(0);
        let used: u64 = parts[2].parse().unwrap_or(0);
        Ok((used, total))
    } else {
        anyhow::bail!("Invalid df output")
    }
}

/// Read network stats from /proc/net/dev
fn read_network_stats(interface: Option<&str>) -> Result<(u64, u64)> {
    let content = fs::read_to_string("/proc/net/dev")
        .context("Failed to read /proc/net/dev")?;

    let mut total_rx: u64 = 0;
    let mut total_tx: u64 = 0;

    for line in content.lines().skip(2) {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 10 {
            continue;
        }

        let iface = parts[0].trim_end_matches(':');

        // Skip loopback
        if iface == "lo" {
            continue;
        }

        // Filter by interface if specified
        if let Some(target) = interface {
            if iface != target {
                continue;
            }
        }

        let rx: u64 = parts[1].parse().unwrap_or(0);
        let tx: u64 = parts[9].parse().unwrap_or(0);

        total_rx += rx;
        total_tx += tx;
    }

    Ok((total_rx, total_tx))
}

/// Read uptime from /proc/uptime
fn read_uptime() -> Result<u64> {
    let content = fs::read_to_string("/proc/uptime")
        .context("Failed to read /proc/uptime")?;

    let uptime_str = content.split_whitespace().next()
        .context("Empty /proc/uptime")?;

    let uptime_secs: f64 = uptime_str.parse()
        .context("Invalid uptime value")?;

    Ok(uptime_secs as u64)
}

/// Read load average from /proc/loadavg
fn read_load_average() -> Result<[f32; 3]> {
    let content = fs::read_to_string("/proc/loadavg")
        .context("Failed to read /proc/loadavg")?;

    let parts: Vec<f32> = content
        .split_whitespace()
        .take(3)
        .filter_map(|s| s.parse().ok())
        .collect();

    if parts.len() >= 3 {
        Ok([parts[0], parts[1], parts[2]])
    } else {
        anyhow::bail!("Invalid loadavg format")
    }
}

/// Encode telemetry data to JSON bytes
pub fn encode_telemetry(data: &TelemetryData) -> Result<Bytes> {
    let json = serde_json::to_vec(data)
        .context("Failed to serialize telemetry")?;
    Ok(Bytes::from(json))
}

/// Decode telemetry data from JSON bytes
pub fn decode_telemetry(data: &[u8]) -> Result<TelemetryData> {
    serde_json::from_slice(data)
        .context("Failed to deserialize telemetry")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_telemetry_encode_decode() {
        let data = TelemetryData {
            cpu_usage: 25.5,
            cpu_temp: Some(45.0),
            memory_usage: 60.0,
            disk_usage: 30.0,
            network_tx_bytes: 1000,
            network_rx_bytes: 2000,
            uptime_secs: 3600,
            load_average: [0.5, 0.6, 0.7],
        };

        let encoded = encode_telemetry(&data).unwrap();
        let decoded = decode_telemetry(&encoded).unwrap();

        assert!((decoded.cpu_usage - 25.5).abs() < 0.01);
        assert_eq!(decoded.cpu_temp, Some(45.0));
        assert_eq!(decoded.uptime_secs, 3600);
    }

    #[test]
    fn test_default_telemetry() {
        let data = TelemetryData::default();
        assert_eq!(data.cpu_usage, 0.0);
        assert_eq!(data.cpu_temp, None);
    }
}
