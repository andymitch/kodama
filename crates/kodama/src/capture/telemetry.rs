//! Telemetry capture module
//!
//! Collects system metrics (CPU, temperature, disk usage, etc.),
//! GPS position, and video motion level. Encodes as MessagePack with
//! threshold-based sparse updates for efficient streaming.
//! Works on Linux systems.

use anyhow::{Context, Result};
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use std::fs;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use tokio::time::Instant;
use tracing::{debug, warn};

/// GPS position data
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GpsData {
    /// Latitude in degrees (positive = north)
    pub latitude: f64,
    /// Longitude in degrees (positive = east)
    pub longitude: f64,
    /// Altitude in meters above sea level (None if unavailable)
    pub altitude: Option<f64>,
    /// Ground speed in m/s (None if unavailable)
    pub speed: Option<f64>,
    /// Heading/track in degrees from true north (None if unavailable)
    pub heading: Option<f64>,
    /// GPS fix mode: 2 = 2D, 3 = 3D
    pub fix_mode: u8,
}

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
    /// GPS position (None if no GPS available)
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub gps: Option<GpsData>,
    /// Video motion detection level (0.0 = no motion, 1.0 = max motion)
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub motion_level: Option<f32>,
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
            gps: None,
            motion_level: None,
        }
    }
}

/// Thresholds for sparse telemetry updates.
/// A field is only sent when it changes by more than the threshold since the last sent value.
#[derive(Debug, Clone)]
pub struct TelemetryThresholds {
    pub cpu_usage: f32,
    pub cpu_temp: f32,
    pub memory_usage: f32,
    pub disk_usage: f32,
    pub network_bytes: u64,
    pub load_average: f32,
    pub lat_lon: f64,
    pub altitude: f64,
    pub speed: f64,
    pub heading: f64,
    pub motion_level: f32,
}

impl Default for TelemetryThresholds {
    fn default() -> Self {
        Self {
            cpu_usage: 5.0,
            cpu_temp: 1.0,
            memory_usage: 5.0,
            disk_usage: 5.0,
            network_bytes: 10_240,
            load_average: 0.1,
            lat_lon: 0.0001,
            altitude: 5.0,
            speed: 0.5,
            heading: 5.0,
            motion_level: 0.05,
        }
    }
}

/// Sparse GPS data with short field names for wire efficiency
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SparseGps {
    #[serde(rename = "la")]
    pub latitude: f64,
    #[serde(rename = "lo")]
    pub longitude: f64,
    #[serde(rename = "al", skip_serializing_if = "Option::is_none", default)]
    pub altitude: Option<f64>,
    #[serde(rename = "sp", skip_serializing_if = "Option::is_none", default)]
    pub speed: Option<f64>,
    #[serde(rename = "hd", skip_serializing_if = "Option::is_none", default)]
    pub heading: Option<f64>,
    #[serde(rename = "fm")]
    pub fix_mode: u8,
}

/// Sparse telemetry payload with short field names and optional fields.
/// Only changed values are included between full heartbeats.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SparseTelemetry {
    /// Whether this is a full heartbeat (all fields present)
    #[serde(rename = "f", default)]
    pub full: bool,
    #[serde(rename = "cpu", skip_serializing_if = "Option::is_none", default)]
    pub cpu_usage: Option<f32>,
    #[serde(rename = "tmp", skip_serializing_if = "Option::is_none", default)]
    pub cpu_temp: Option<f32>,
    #[serde(rename = "mem", skip_serializing_if = "Option::is_none", default)]
    pub memory_usage: Option<f32>,
    #[serde(rename = "dsk", skip_serializing_if = "Option::is_none", default)]
    pub disk_usage: Option<f32>,
    #[serde(rename = "tx", skip_serializing_if = "Option::is_none", default)]
    pub network_tx_bytes: Option<u64>,
    #[serde(rename = "rx", skip_serializing_if = "Option::is_none", default)]
    pub network_rx_bytes: Option<u64>,
    #[serde(rename = "up", skip_serializing_if = "Option::is_none", default)]
    pub uptime_secs: Option<u64>,
    #[serde(rename = "la", skip_serializing_if = "Option::is_none", default)]
    pub load_average: Option<[f32; 3]>,
    #[serde(rename = "gps", skip_serializing_if = "Option::is_none", default)]
    pub gps: Option<SparseGps>,
    #[serde(rename = "mot", skip_serializing_if = "Option::is_none", default)]
    pub motion_level: Option<f32>,
}

impl SparseTelemetry {
    /// Create a full heartbeat payload with all fields set
    pub fn from_full(data: &TelemetryData) -> Self {
        Self {
            full: true,
            cpu_usage: Some(data.cpu_usage),
            cpu_temp: data.cpu_temp,
            memory_usage: Some(data.memory_usage),
            disk_usage: Some(data.disk_usage),
            network_tx_bytes: Some(data.network_tx_bytes),
            network_rx_bytes: Some(data.network_rx_bytes),
            uptime_secs: Some(data.uptime_secs),
            load_average: Some(data.load_average),
            gps: data.gps.as_ref().map(|g| SparseGps {
                latitude: g.latitude,
                longitude: g.longitude,
                altitude: g.altitude,
                speed: g.speed,
                heading: g.heading,
                fix_mode: g.fix_mode,
            }),
            motion_level: data.motion_level,
        }
    }

    /// Create a diff payload containing only fields that exceed thresholds.
    /// Returns `None` if nothing changed enough to warrant sending.
    pub fn diff(
        current: &TelemetryData,
        previous: &TelemetryData,
        thresholds: &TelemetryThresholds,
    ) -> Option<Self> {
        let mut sparse = Self {
            full: false,
            cpu_usage: None,
            cpu_temp: None,
            memory_usage: None,
            disk_usage: None,
            network_tx_bytes: None,
            network_rx_bytes: None,
            uptime_secs: None,
            load_average: None,
            gps: None,
            motion_level: None,
        };

        let mut has_changes = false;

        if (current.cpu_usage - previous.cpu_usage).abs() >= thresholds.cpu_usage {
            sparse.cpu_usage = Some(current.cpu_usage);
            has_changes = true;
        }

        match (current.cpu_temp, previous.cpu_temp) {
            (Some(c), Some(p)) if (c - p).abs() >= thresholds.cpu_temp => {
                sparse.cpu_temp = Some(c);
                has_changes = true;
            }
            (Some(c), None) => {
                sparse.cpu_temp = Some(c);
                has_changes = true;
            }
            _ => {}
        }

        if (current.memory_usage - previous.memory_usage).abs() >= thresholds.memory_usage {
            sparse.memory_usage = Some(current.memory_usage);
            has_changes = true;
        }

        if (current.disk_usage - previous.disk_usage).abs() >= thresholds.disk_usage {
            sparse.disk_usage = Some(current.disk_usage);
            has_changes = true;
        }

        if current.network_tx_bytes.abs_diff(previous.network_tx_bytes) >= thresholds.network_bytes
        {
            sparse.network_tx_bytes = Some(current.network_tx_bytes);
            has_changes = true;
        }

        if current.network_rx_bytes.abs_diff(previous.network_rx_bytes) >= thresholds.network_bytes
        {
            sparse.network_rx_bytes = Some(current.network_rx_bytes);
            has_changes = true;
        }

        // Always send uptime (cheap, useful for liveness)
        if current.uptime_secs != previous.uptime_secs {
            sparse.uptime_secs = Some(current.uptime_secs);
            has_changes = true;
        }

        let la_changed = current
            .load_average
            .iter()
            .zip(previous.load_average.iter())
            .any(|(c, p)| (c - p).abs() >= thresholds.load_average);
        if la_changed {
            sparse.load_average = Some(current.load_average);
            has_changes = true;
        }

        // GPS diff
        match (&current.gps, &previous.gps) {
            (Some(cg), Some(pg)) => {
                if (cg.latitude - pg.latitude).abs() >= thresholds.lat_lon
                    || (cg.longitude - pg.longitude).abs() >= thresholds.lat_lon
                    || opt_diff_f64(cg.altitude, pg.altitude, thresholds.altitude)
                    || opt_diff_f64(cg.speed, pg.speed, thresholds.speed)
                    || opt_diff_f64(cg.heading, pg.heading, thresholds.heading)
                {
                    sparse.gps = Some(SparseGps {
                        latitude: cg.latitude,
                        longitude: cg.longitude,
                        altitude: cg.altitude,
                        speed: cg.speed,
                        heading: cg.heading,
                        fix_mode: cg.fix_mode,
                    });
                    has_changes = true;
                }
            }
            (Some(cg), None) => {
                sparse.gps = Some(SparseGps {
                    latitude: cg.latitude,
                    longitude: cg.longitude,
                    altitude: cg.altitude,
                    speed: cg.speed,
                    heading: cg.heading,
                    fix_mode: cg.fix_mode,
                });
                has_changes = true;
            }
            _ => {}
        }

        match (current.motion_level, previous.motion_level) {
            (Some(c), Some(p)) if (c - p).abs() >= thresholds.motion_level => {
                sparse.motion_level = Some(c);
                has_changes = true;
            }
            (Some(c), None) => {
                sparse.motion_level = Some(c);
                has_changes = true;
            }
            _ => {}
        }

        if has_changes {
            Some(sparse)
        } else {
            None
        }
    }

    /// Merge this sparse update into a full TelemetryData state
    pub fn merge_into(&self, state: &mut TelemetryData) {
        if let Some(v) = self.cpu_usage {
            state.cpu_usage = v;
        }
        if let Some(v) = self.cpu_temp {
            state.cpu_temp = Some(v);
        }
        if let Some(v) = self.memory_usage {
            state.memory_usage = v;
        }
        if let Some(v) = self.disk_usage {
            state.disk_usage = v;
        }
        if let Some(v) = self.network_tx_bytes {
            state.network_tx_bytes = v;
        }
        if let Some(v) = self.network_rx_bytes {
            state.network_rx_bytes = v;
        }
        if let Some(v) = self.uptime_secs {
            state.uptime_secs = v;
        }
        if let Some(v) = self.load_average {
            state.load_average = v;
        }
        if let Some(ref g) = self.gps {
            state.gps = Some(GpsData {
                latitude: g.latitude,
                longitude: g.longitude,
                altitude: g.altitude,
                speed: g.speed,
                heading: g.heading,
                fix_mode: g.fix_mode,
            });
        }
        if let Some(v) = self.motion_level {
            state.motion_level = Some(v);
        }
    }

    /// Serialize to msgpack bytes
    pub fn to_bytes(&self) -> Result<Bytes> {
        let data = rmp_serde::to_vec_named(self).context("Failed to serialize sparse telemetry")?;
        Ok(Bytes::from(data))
    }

    /// Deserialize from msgpack bytes
    pub fn from_bytes(data: &[u8]) -> Result<Self> {
        rmp_serde::from_slice(data).context("Failed to deserialize sparse telemetry")
    }
}

fn opt_diff_f64(a: Option<f64>, b: Option<f64>, threshold: f64) -> bool {
    match (a, b) {
        (Some(av), Some(bv)) => (av - bv).abs() >= threshold,
        (Some(_), None) | (None, Some(_)) => true,
        _ => false,
    }
}

/// Telemetry capture configuration
#[derive(Debug, Clone)]
pub struct TelemetryCaptureConfig {
    /// How often to collect telemetry (in seconds)
    pub interval_secs: u32,
    /// How often to send a full heartbeat (in seconds)
    pub heartbeat_interval_secs: u32,
    /// Thresholds for sparse updates
    pub thresholds: TelemetryThresholds,
    /// Network interface to monitor (None = sum all)
    pub network_interface: Option<String>,
    /// Disk path to monitor for usage
    pub disk_path: String,
    /// Enable GPS reading from gpsd (localhost:2947)
    pub enable_gps: bool,
    /// Shared motion level from video motion detector (None if not connected)
    pub motion_level: Option<Arc<AtomicU32>>,
}

impl Default for TelemetryCaptureConfig {
    fn default() -> Self {
        Self {
            interval_secs: 1,
            heartbeat_interval_secs: 30,
            thresholds: TelemetryThresholds::default(),
            network_interface: None,
            disk_path: "/".to_string(),
            enable_gps: true,
            motion_level: None,
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
    /// Returns a receiver for msgpack-encoded sparse telemetry packets.
    /// Sends full heartbeats every `heartbeat_interval_secs` and sparse
    /// diffs (only changed fields) in between.
    pub fn start(config: TelemetryCaptureConfig) -> Result<(Self, mpsc::Receiver<Bytes>)> {
        let (tx, rx) = mpsc::channel(16);
        let (shutdown_tx, mut shutdown_rx) = mpsc::channel(1);

        let motion_level = config.motion_level.clone();
        let thresholds = config.thresholds.clone();
        let heartbeat_secs = config.heartbeat_interval_secs;

        tokio::spawn(async move {
            let mut interval =
                tokio::time::interval(Duration::from_secs(config.interval_secs as u64));
            let mut prev_cpu_stats: Option<CpuStats> = None;
            let mut gps_reader = if config.enable_gps {
                GpsdReader::new().await
            } else {
                None
            };
            let mut last_sent: Option<TelemetryData> = None;
            let mut last_heartbeat =
                Instant::now() - Duration::from_secs(heartbeat_secs as u64 + 1);

            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        let config_clone = config.clone();
                        let prev = prev_cpu_stats.take();
                        match tokio::task::spawn_blocking(move || {
                            let mut prev = prev;
                            let result = collect_telemetry(&config_clone, &mut prev);
                            (result, prev)
                        }).await {
                            Ok((result, prev)) => {
                                prev_cpu_stats = prev;
                                match result {
                                    Ok(mut data) => {
                                        // Read GPS if available
                                        if let Some(ref mut reader) = gps_reader {
                                            data.gps = reader.latest_fix().await;
                                        }

                                        // Read motion level if available
                                        if let Some(ref ml) = motion_level {
                                            let bits = ml.load(Ordering::Relaxed);
                                            data.motion_level = Some(f32::from_bits(bits));
                                        }

                                        let is_heartbeat = last_heartbeat.elapsed() >= Duration::from_secs(heartbeat_secs as u64);

                                        let sparse = match last_sent.as_ref() {
                                            None => Some(SparseTelemetry::from_full(&data)),
                                            _ if is_heartbeat => Some(SparseTelemetry::from_full(&data)),
                                            Some(prev) => SparseTelemetry::diff(&data, prev, &thresholds),
                                        };

                                        if let Some(payload) = sparse {
                                            match payload.to_bytes() {
                                                Ok(bytes) => {
                                                    if tx.send(bytes).await.is_err() {
                                                        debug!("Telemetry receiver dropped");
                                                        break;
                                                    }
                                                    if is_heartbeat || last_sent.is_none() {
                                                        last_heartbeat = Instant::now();
                                                    }
                                                    last_sent = Some(data);
                                                }
                                                Err(e) => {
                                                    warn!("Failed to encode telemetry: {}", e);
                                                }
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        warn!("Failed to collect telemetry: {}", e);
                                    }
                                }
                            }
                            Err(e) => {
                                warn!("spawn_blocking for telemetry failed: {}", e);
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

        Ok((
            Self {
                shutdown_tx: Some(shutdown_tx),
            },
            rx,
        ))
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
fn collect_telemetry(
    config: &TelemetryCaptureConfig,
    prev_cpu: &mut Option<CpuStats>,
) -> Result<TelemetryData> {
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
    let content = fs::read_to_string("/proc/stat").context("Failed to read /proc/stat")?;

    let cpu_line = content
        .lines()
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
    let content = fs::read_to_string("/proc/meminfo").context("Failed to read /proc/meminfo")?;

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

/// Read disk usage using statvfs (Unix only)
#[cfg(unix)]
fn read_disk_usage(path: &str) -> Result<(u64, u64)> {
    use std::ffi::CString;
    let c_path = CString::new(path).context("Invalid path")?;
    unsafe {
        let mut stat: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(c_path.as_ptr(), &mut stat) != 0 {
            anyhow::bail!("statvfs failed: {}", std::io::Error::last_os_error());
        }
        let total = stat.f_blocks as u64 * stat.f_frsize as u64;
        let available = stat.f_bavail as u64 * stat.f_frsize as u64;
        let used = total - available;
        Ok((used, total))
    }
}

#[cfg(not(unix))]
fn read_disk_usage(_path: &str) -> Result<(u64, u64)> {
    anyhow::bail!("Disk usage not supported on this platform")
}

/// Read network stats from /proc/net/dev
fn read_network_stats(interface: Option<&str>) -> Result<(u64, u64)> {
    let content = fs::read_to_string("/proc/net/dev").context("Failed to read /proc/net/dev")?;

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
    let content = fs::read_to_string("/proc/uptime").context("Failed to read /proc/uptime")?;

    let uptime_str = content
        .split_whitespace()
        .next()
        .context("Empty /proc/uptime")?;

    let uptime_secs: f64 = uptime_str.parse().context("Invalid uptime value")?;

    Ok(uptime_secs as u64)
}

/// Read load average from /proc/loadavg
fn read_load_average() -> Result<[f32; 3]> {
    let content = fs::read_to_string("/proc/loadavg").context("Failed to read /proc/loadavg")?;

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

/// Encode full telemetry data to msgpack bytes
pub fn encode_telemetry(data: &TelemetryData) -> Result<Bytes> {
    let sparse = SparseTelemetry::from_full(data);
    sparse.to_bytes()
}

/// Decode telemetry data from msgpack bytes (SparseTelemetry envelope)
pub fn decode_telemetry(data: &[u8]) -> Result<SparseTelemetry> {
    SparseTelemetry::from_bytes(data)
}

// --- GPS reader via gpsd JSON protocol ---

/// Reads GPS data from gpsd daemon (localhost:2947)
struct GpsdReader {
    latest: Option<GpsData>,
    rx: mpsc::Receiver<GpsData>,
}

impl GpsdReader {
    /// Try to connect to gpsd. Returns None if gpsd is not available.
    async fn new() -> Option<Self> {
        let (tx, rx) = mpsc::channel(4);

        let stream = match tokio::net::TcpStream::connect("127.0.0.1:2947").await {
            Ok(s) => s,
            Err(_) => {
                debug!("gpsd not available at 127.0.0.1:2947, GPS disabled");
                return None;
            }
        };

        debug!("Connected to gpsd");

        tokio::spawn(async move {
            if let Err(e) = gpsd_read_loop(stream, tx).await {
                debug!("gpsd reader ended: {}", e);
            }
        });

        Some(Self { latest: None, rx })
    }

    /// Get the latest GPS fix, draining any queued updates
    async fn latest_fix(&mut self) -> Option<GpsData> {
        // Drain all pending updates, keep the latest
        while let Ok(fix) = self.rx.try_recv() {
            self.latest = Some(fix);
        }
        self.latest.clone()
    }
}

/// Background loop that reads JSON lines from gpsd
async fn gpsd_read_loop(stream: tokio::net::TcpStream, tx: mpsc::Sender<GpsData>) -> Result<()> {
    let (reader, mut writer) = stream.into_split();

    // Enable JSON watch mode
    writer
        .write_all(b"?WATCH={\"enable\":true,\"json\":true}\n")
        .await
        .context("Failed to send WATCH to gpsd")?;

    let mut lines = BufReader::new(reader).lines();
    while let Some(line) = lines.next_line().await.context("gpsd read error")? {
        // Parse TPV (Time-Position-Velocity) reports
        if let Some(gps) = parse_gpsd_tpv(&line) {
            if tx.send(gps).await.is_err() {
                break;
            }
        }
    }

    Ok(())
}

/// Parse a gpsd TPV JSON report into GpsData
fn parse_gpsd_tpv(line: &str) -> Option<GpsData> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;

    if v.get("class")?.as_str()? != "TPV" {
        return None;
    }

    let mode = v.get("mode")?.as_u64()? as u8;
    // mode 0/1 = no fix
    if mode < 2 {
        return None;
    }

    let lat = v.get("lat")?.as_f64()?;
    let lon = v.get("lon")?.as_f64()?;

    Some(GpsData {
        latitude: lat,
        longitude: lon,
        altitude: v.get("alt").and_then(|v| v.as_f64()),
        speed: v.get("speed").and_then(|v| v.as_f64()),
        heading: v.get("track").and_then(|v| v.as_f64()),
        fix_mode: mode,
    })
}

// --- Video motion detector ---

/// Estimates motion level from H.264 frame-size variance with auto-calibration.
///
/// Computes a coefficient of variation (CV = std_dev / mean) from P-frame sizes,
/// then compares the current CV against a slow-adapting baseline CV. Only
/// deviations *above* the baseline register as motion — this auto-calibrates
/// against the camera's natural frame-size variance (which varies by codec
/// settings, resolution, and I/O chunking).
pub struct MotionDetector {
    /// Shared motion level (f32 stored as u32 bits) for telemetry to read
    level: Arc<AtomicU32>,
    /// Fast EMA of frame size (alpha=0.03, ~33 frames)
    ema_size: f64,
    /// Fast EMA of frame size squared (for variance)
    ema_size_sq: f64,
    /// Slow-adapting baseline CV (alpha=0.002, ~500 frames ≈ 8-17s)
    baseline_cv: f64,
    /// Current smoothed motion level (fast rise, slow decay)
    smoothed: f64,
    /// Number of P-frames seen (for warmup)
    frame_count: u64,
}

impl MotionDetector {
    /// Create a new motion detector. Returns the detector and a shared atomic
    /// that the telemetry collector reads from.
    pub fn new() -> (Self, Arc<AtomicU32>) {
        let level = Arc::new(AtomicU32::new(0));
        let detector = Self {
            level: level.clone(),
            ema_size: 0.0,
            ema_size_sq: 0.0,
            baseline_cv: 0.0,
            smoothed: 0.0,
            frame_count: 0,
        };
        (detector, level)
    }

    /// Feed a video frame to update the motion estimate.
    /// `is_keyframe`: true for I-frames, false for P-frames.
    /// `payload_size`: size of the H.264 frame payload in bytes.
    pub fn update(&mut self, is_keyframe: bool, payload_size: usize) {
        // Skip keyframes — always large regardless of motion
        if is_keyframe {
            return;
        }

        let size = payload_size as f64;
        self.frame_count += 1;

        if self.frame_count == 1 {
            self.ema_size = size;
            self.ema_size_sq = size * size;
            self.level.store(0f32.to_bits(), Ordering::Relaxed);
            return;
        }

        // Fast EMA of size and size² (alpha=0.03, ~33 frames ≈ 0.5-1s)
        let alpha = 0.03;
        self.ema_size = self.ema_size * (1.0 - alpha) + size * alpha;
        self.ema_size_sq = self.ema_size_sq * (1.0 - alpha) + (size * size) * alpha;

        // Current CV from fast EMAs
        let variance = (self.ema_size_sq - self.ema_size * self.ema_size).max(0.0);
        let cv = if self.ema_size > 0.0 {
            variance.sqrt() / self.ema_size
        } else {
            0.0
        };

        // Slow-adapting baseline CV (alpha=0.002, ~500 frames ≈ 8-17s)
        // During warmup (<100 frames), adapt faster to establish initial baseline
        let baseline_alpha = if self.frame_count < 100 { 0.05 } else { 0.002 };
        self.baseline_cv = self.baseline_cv * (1.0 - baseline_alpha) + cv * baseline_alpha;

        // Motion = how much current CV exceeds baseline
        let excess = (cv - self.baseline_cv).max(0.0);
        // Normalize: excess of 0.3+ above baseline = strong motion
        let instant = (excess / 0.3).min(1.0);

        // Fast rise (alpha=0.3), slow decay (alpha=0.005 ≈ 200 frames / ~3-7s)
        if instant > self.smoothed {
            self.smoothed = self.smoothed * 0.7 + instant * 0.3;
        } else {
            self.smoothed = self.smoothed * 0.995 + instant * 0.005;
        }

        let motion = (self.smoothed as f32).clamp(0.0, 1.0);
        self.level.store(motion.to_bits(), Ordering::Relaxed);
    }
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
            gps: None,
            motion_level: None,
        };

        let encoded = encode_telemetry(&data).unwrap();
        let decoded = decode_telemetry(&encoded).unwrap();

        assert!(decoded.full);
        assert!((decoded.cpu_usage.unwrap() - 25.5).abs() < 0.01);
        assert_eq!(decoded.cpu_temp, Some(45.0));
        assert_eq!(decoded.uptime_secs, Some(3600));
        assert!(decoded.gps.is_none());
        assert!(decoded.motion_level.is_none());
    }

    #[test]
    fn test_telemetry_with_gps_and_motion() {
        let data = TelemetryData {
            gps: Some(GpsData {
                latitude: 37.7749,
                longitude: -122.4194,
                altitude: Some(10.0),
                speed: Some(0.5),
                heading: Some(180.0),
                fix_mode: 3,
            }),
            motion_level: Some(0.42),
            ..Default::default()
        };

        let encoded = encode_telemetry(&data).unwrap();
        let decoded = decode_telemetry(&encoded).unwrap();

        let gps = decoded.gps.unwrap();
        assert!((gps.latitude - 37.7749).abs() < 0.0001);
        assert!((gps.longitude - (-122.4194)).abs() < 0.0001);
        assert_eq!(gps.fix_mode, 3);
        assert!((decoded.motion_level.unwrap() - 0.42).abs() < 0.01);
    }

    #[test]
    fn test_sparse_telemetry_full() {
        let data = TelemetryData {
            cpu_usage: 50.0,
            cpu_temp: Some(60.0),
            memory_usage: 70.0,
            disk_usage: 80.0,
            network_tx_bytes: 100_000,
            network_rx_bytes: 200_000,
            uptime_secs: 7200,
            load_average: [1.0, 2.0, 3.0],
            gps: None,
            motion_level: Some(0.3),
        };

        let sparse = SparseTelemetry::from_full(&data);
        assert!(sparse.full);
        assert_eq!(sparse.cpu_usage, Some(50.0));
        assert_eq!(sparse.memory_usage, Some(70.0));
        assert_eq!(sparse.motion_level, Some(0.3));

        // Round-trip through msgpack
        let bytes = sparse.to_bytes().unwrap();
        let decoded = SparseTelemetry::from_bytes(&bytes).unwrap();
        assert!(decoded.full);
        assert!((decoded.cpu_usage.unwrap() - 50.0).abs() < 0.01);
    }

    #[test]
    fn test_sparse_telemetry_diff() {
        let thresholds = TelemetryThresholds::default();

        let prev = TelemetryData {
            cpu_usage: 50.0,
            cpu_temp: Some(60.0),
            memory_usage: 70.0,
            disk_usage: 80.0,
            network_tx_bytes: 100_000,
            network_rx_bytes: 200_000,
            uptime_secs: 7200,
            load_average: [1.0, 2.0, 3.0],
            gps: None,
            motion_level: Some(0.3),
        };

        // CPU changed by 10% (above 5% threshold), memory by 1% (below 5%)
        let current = TelemetryData {
            cpu_usage: 60.0,
            cpu_temp: Some(60.5), // below 1.0 threshold
            memory_usage: 71.0,   // below 5.0 threshold
            disk_usage: 80.0,
            network_tx_bytes: 100_500, // below 10KB threshold
            network_rx_bytes: 200_000,
            uptime_secs: 7201,
            load_average: [1.0, 2.0, 3.0],
            gps: None,
            motion_level: Some(0.3),
        };

        let diff = SparseTelemetry::diff(&current, &prev, &thresholds).unwrap();
        assert!(!diff.full);
        assert_eq!(diff.cpu_usage, Some(60.0)); // changed
        assert!(diff.cpu_temp.is_none()); // below threshold
        assert!(diff.memory_usage.is_none()); // below threshold
        assert!(diff.disk_usage.is_none()); // no change
        assert!(diff.network_tx_bytes.is_none()); // below threshold
        assert_eq!(diff.uptime_secs, Some(7201)); // changed
    }

    #[test]
    fn test_sparse_telemetry_no_changes() {
        let thresholds = TelemetryThresholds::default();

        let data = TelemetryData {
            cpu_usage: 50.0,
            cpu_temp: Some(60.0),
            memory_usage: 70.0,
            disk_usage: 80.0,
            network_tx_bytes: 100_000,
            network_rx_bytes: 200_000,
            uptime_secs: 7200,
            load_average: [1.0, 2.0, 3.0],
            gps: None,
            motion_level: Some(0.3),
        };

        // Identical data should produce None
        let diff = SparseTelemetry::diff(&data, &data, &thresholds);
        assert!(diff.is_none());
    }

    #[test]
    fn test_sparse_telemetry_merge() {
        let mut state = TelemetryData::default();

        // Apply a full heartbeat
        let full = SparseTelemetry::from_full(&TelemetryData {
            cpu_usage: 50.0,
            cpu_temp: Some(60.0),
            memory_usage: 70.0,
            disk_usage: 80.0,
            network_tx_bytes: 100_000,
            network_rx_bytes: 200_000,
            uptime_secs: 7200,
            load_average: [1.0, 2.0, 3.0],
            gps: None,
            motion_level: None,
        });
        full.merge_into(&mut state);
        assert_eq!(state.cpu_usage, 50.0);
        assert_eq!(state.memory_usage, 70.0);

        // Apply a sparse update (only CPU changed)
        let sparse = SparseTelemetry {
            full: false,
            cpu_usage: Some(75.0),
            cpu_temp: None,
            memory_usage: None,
            disk_usage: None,
            network_tx_bytes: None,
            network_rx_bytes: None,
            uptime_secs: None,
            load_average: None,
            gps: None,
            motion_level: None,
        };
        sparse.merge_into(&mut state);
        assert_eq!(state.cpu_usage, 75.0);
        assert_eq!(state.memory_usage, 70.0); // unchanged
    }

    #[test]
    fn test_default_telemetry() {
        let data = TelemetryData::default();
        assert_eq!(data.cpu_usage, 0.0);
        assert_eq!(data.cpu_temp, None);
        assert_eq!(data.gps, None);
        assert_eq!(data.motion_level, None);
    }

    #[test]
    fn test_parse_gpsd_tpv() {
        let tpv = r#"{"class":"TPV","mode":3,"lat":37.7749,"lon":-122.4194,"alt":10.5,"speed":1.2,"track":90.0}"#;
        let gps = parse_gpsd_tpv(tpv).unwrap();
        assert!((gps.latitude - 37.7749).abs() < 0.0001);
        assert!((gps.longitude - (-122.4194)).abs() < 0.0001);
        assert_eq!(gps.altitude, Some(10.5));
        assert_eq!(gps.speed, Some(1.2));
        assert_eq!(gps.heading, Some(90.0));
        assert_eq!(gps.fix_mode, 3);
    }

    #[test]
    fn test_parse_gpsd_tpv_no_fix() {
        let tpv = r#"{"class":"TPV","mode":1}"#;
        assert!(parse_gpsd_tpv(tpv).is_none());
    }

    #[test]
    fn test_parse_gpsd_non_tpv() {
        let sky = r#"{"class":"SKY","satellites":[]}"#;
        assert!(parse_gpsd_tpv(sky).is_none());
    }

    #[test]
    fn test_motion_detector_steady() {
        let (mut detector, level) = MotionDetector::new();

        // Feed steady P-frames - should converge to low motion
        for _ in 0..100 {
            detector.update(false, 5000);
        }
        let steady = f32::from_bits(level.load(Ordering::Relaxed));
        assert!(
            steady < 0.05,
            "Steady frames should have very low motion: {}",
            steady
        );
    }

    #[test]
    fn test_motion_detector_burst() {
        let (mut detector, level) = MotionDetector::new();

        // Establish baseline with steady frames
        for _ in 0..100 {
            detector.update(false, 5000);
        }

        // Feed large P-frames (motion burst)
        for _ in 0..30 {
            detector.update(false, 15000);
        }
        let motion = f32::from_bits(level.load(Ordering::Relaxed));
        assert!(
            motion > 0.2,
            "Large frames should indicate motion: {}",
            motion
        );

        // Motion should persist (slow decay) even after a few normal frames
        for _ in 0..10 {
            detector.update(false, 5000);
        }
        let after = f32::from_bits(level.load(Ordering::Relaxed));
        assert!(after > 0.1, "Motion should decay slowly: {}", after);
    }

    #[test]
    fn test_motion_detector_ignores_keyframes() {
        let (mut detector, level) = MotionDetector::new();

        for _ in 0..50 {
            detector.update(false, 5000);
        }
        // Keyframes should not affect motion level
        for _ in 0..10 {
            detector.update(true, 50000);
        }
        let motion = f32::from_bits(level.load(Ordering::Relaxed));
        assert!(
            motion < 0.05,
            "Keyframes should not trigger motion: {}",
            motion
        );
    }
}
