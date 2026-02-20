//! WebSocket handler: subscribes to router broadcast, muxes fMP4, pushes to clients.
//!
//! Binary protocol (client ← server):
//!   0x01 + JSON           → camera list update
//!   0x02 + src(8) + codec_len(1) + codec + w(2LE) + h(2LE) + init_segment
//!   0x03 + src(8) + media_segment
//!   0x04 + src(8) + JSON  → telemetry
//!   0x05 + src(8) + f32LE → audio level
//!   0x06 + src(8) + sr(4LE) + ch(1) + pcm_data → audio data
//!   0x07 + JSON           → peer event (connect/disconnect)
//!   0x08 + JSON           → server stats

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio::time::{interval, Duration};
use tracing::{debug, warn};

use crate::{Channel, Frame, SourceId};
use crate::server::{PeerRole, RouterHandle};
use super::fmp4::{MuxedVideo, VideoMuxer};

// ── Wire types for decoding camera's MessagePack telemetry ────────────

/// Sparse telemetry as sent by camera (MessagePack, short field names)
#[derive(Deserialize)]
struct WireTelemetry {
    #[serde(rename = "f", default)]
    full: bool,
    #[serde(rename = "cpu", default)]
    cpu_usage: Option<f32>,
    #[serde(rename = "tmp", default)]
    cpu_temp: Option<f32>,
    #[serde(rename = "mem", default)]
    memory_usage: Option<f32>,
    #[serde(rename = "dsk", default)]
    disk_usage: Option<f32>,
    #[serde(rename = "up", default)]
    uptime_secs: Option<u64>,
    #[serde(rename = "la", default)]
    load_average: Option<[f32; 3]>,
    #[serde(rename = "gps", default)]
    gps: Option<WireGps>,
    #[serde(rename = "mot", default)]
    motion_level: Option<f32>,
}

#[derive(Deserialize)]
struct WireGps {
    #[serde(rename = "la")]
    latitude: f64,
    #[serde(rename = "lo")]
    longitude: f64,
    #[serde(rename = "al", default)]
    altitude: Option<f64>,
    #[serde(rename = "sp", default)]
    speed: Option<f64>,
    #[serde(rename = "hd", default)]
    heading: Option<f64>,
    #[serde(rename = "fm", default)]
    fix_mode: u8,
}

/// Accumulated telemetry state, serialized as JSON for the browser
#[derive(Default, Clone, Serialize)]
struct TelemetryState {
    cpu_usage: f32,
    cpu_temp: Option<f32>,
    memory_usage: f32,
    disk_usage: f32,
    uptime_secs: u64,
    load_average: [f32; 3],
    #[serde(skip_serializing_if = "Option::is_none")]
    gps: Option<GpsState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    motion_level: Option<f32>,
    /// True after first full heartbeat (has all fields)
    #[serde(skip)]
    ready: bool,
}

#[derive(Clone, Serialize)]
struct GpsState {
    latitude: f64,
    longitude: f64,
    altitude: Option<f64>,
    speed: Option<f64>,
    heading: Option<f64>,
    fix_mode: u8,
}

impl TelemetryState {
    fn merge(&mut self, wire: WireTelemetry) {
        if let Some(v) = wire.cpu_usage { self.cpu_usage = v; }
        if let Some(v) = wire.cpu_temp { self.cpu_temp = Some(v); }
        if let Some(v) = wire.memory_usage { self.memory_usage = v; }
        if let Some(v) = wire.disk_usage { self.disk_usage = v; }
        if let Some(v) = wire.uptime_secs { self.uptime_secs = v; }
        if let Some(v) = wire.load_average { self.load_average = v; }
        if let Some(ref g) = wire.gps {
            self.gps = Some(GpsState {
                latitude: g.latitude,
                longitude: g.longitude,
                altitude: g.altitude,
                speed: g.speed,
                heading: g.heading,
                fix_mode: g.fix_mode,
            });
        }
        if let Some(v) = wire.motion_level { self.motion_level = Some(v); }
        if wire.full { self.ready = true; }
        // Also mark ready on first update (camera sends full heartbeat first)
        if !self.ready && wire.cpu_usage.is_some() && wire.memory_usage.is_some() {
            self.ready = true;
        }
    }
}

const MSG_CAMERA_LIST: u8 = 0x01;
const MSG_VIDEO_INIT: u8 = 0x02;
const MSG_VIDEO_SEGMENT: u8 = 0x03;
const MSG_TELEMETRY: u8 = 0x04;
const MSG_AUDIO_LEVEL: u8 = 0x05;
const MSG_AUDIO_DATA: u8 = 0x06;
const MSG_PEER_EVENT: u8 = 0x07;
const MSG_SERVER_STATS: u8 = 0x08;

/// Handle a single WebSocket connection.
///
/// Video is received from the shared `VideoMuxer` (one FFmpeg per source),
/// while telemetry and audio come directly from the router broadcast.
pub async fn handle_ws(
    socket: WebSocket,
    handle: RouterHandle,
    video_muxer: Arc<VideoMuxer>,
    server_start: Instant,
) {
    let (mut ws_tx, mut ws_rx) = socket.split();
    let mut rx = handle.subscribe();
    let mut video_rx = video_muxer.subscribe();

    // Send initial camera list
    let cameras = get_camera_ids(&handle).await;
    if let Ok(msg) = encode_camera_list(&cameras) {
        let _ = ws_tx.send(Message::Binary(msg.into())).await;
    }

    // Send cached video inits for all known sources
    let mut video_init_sent: HashSet<SourceId> = HashSet::new();
    let cached_inits = video_muxer.cached_inits().await;
    for (src, init) in &cached_inits {
        let msg = encode_video_init(*src, &init.codec, init.width, init.height, &init.data);
        if ws_tx.send(Message::Binary(msg.into())).await.is_err() {
            return;
        }
        video_init_sent.insert(*src);
    }

    // Per-source accumulated telemetry state (sparse msgpack → full JSON)
    let mut telemetry_states: HashMap<SourceId, TelemetryState> = HashMap::new();
    let mut known_cameras: HashSet<SourceId> = cameras.into_iter().collect();

    // Track known peer keys for connect/disconnect detection
    let mut known_peers: HashMap<iroh::PublicKey, PeerRole> = {
        let peers = handle.peers().await;
        peers.into_iter().map(|p| (p.key, p.role)).collect()
    };

    // Periodic stats push every 5 seconds
    let mut stats_interval = interval(Duration::from_secs(5));

    loop {
        tokio::select! {
            // Non-video frames from router broadcast
            result = rx.recv() => {
                match result {
                    Ok(frame) => {
                        // Detect new camera
                        if !known_cameras.contains(&frame.source) {
                            known_cameras.insert(frame.source);
                            let list: Vec<SourceId> = known_cameras.iter().copied().collect();
                            if let Ok(msg) = encode_camera_list_from_sources(&list) {
                                if ws_tx.send(Message::Binary(msg.into())).await.is_err() {
                                    break;
                                }
                            }
                        }

                        // Skip video — handled by video_rx from shared muxer
                        if matches!(frame.channel, Channel::Video) {
                            continue;
                        }

                        let messages = process_frame(&frame, &mut telemetry_states);
                        for msg in messages {
                            if ws_tx.send(Message::Binary(msg.into())).await.is_err() {
                                break;
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!("WebSocket client lagged on frame broadcast, missed {} frames", n);
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            // Muxed video from shared VideoMuxer
            video = video_rx.recv() => {
                match video {
                    Ok(MuxedVideo::Init { source, init }) => {
                        let msg = encode_video_init(source, &init.codec, init.width, init.height, &init.data);
                        if ws_tx.send(Message::Binary(msg.into())).await.is_err() {
                            break;
                        }
                        video_init_sent.insert(source);
                    }
                    Ok(MuxedVideo::Segment { source, data }) => {
                        // Only send segments after init has been sent for this source
                        if !video_init_sent.contains(&source) {
                            // Lazy-fetch cached init
                            if let Some(init) = video_muxer.cached_init(&source).await {
                                let msg = encode_video_init(source, &init.codec, init.width, init.height, &init.data);
                                if ws_tx.send(Message::Binary(msg.into())).await.is_err() {
                                    break;
                                }
                                video_init_sent.insert(source);
                            } else {
                                continue; // No init cached yet, skip segment
                            }
                        }
                        let msg = encode_video_segment(source, &data);
                        if ws_tx.send(Message::Binary(msg.into())).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!("WebSocket client lagged on video broadcast, missed {} frames", n);
                        // Clear tracking so we re-send inits from cache on next segment
                        video_init_sent.clear();
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            // Handle incoming messages from client (ping/pong, close)
            msg = ws_rx.next() => {
                match msg {
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(Message::Ping(data))) => {
                        let _ = ws_tx.send(Message::Pong(data)).await;
                    }
                    Some(Err(_)) => break,
                    _ => {} // Ignore text/binary from client for now
                }
            }
            // Periodic stats + peer change detection
            _ = stats_interval.tick() => {
                // Detect peer changes
                let current_peers = handle.peers().await;
                let current_keys: HashSet<iroh::PublicKey> = current_peers.iter().map(|p| p.key).collect();

                // New peers (connected)
                for p in &current_peers {
                    if !known_peers.contains_key(&p.key) {
                        if let Ok(msg) = encode_peer_event(p.key, p.role, "connected") {
                            if ws_tx.send(Message::Binary(msg.into())).await.is_err() {
                                break;
                            }
                        }
                    }
                }

                // Removed peers (disconnected)
                let mut camera_disconnected = false;
                for (key, role) in &known_peers {
                    if !current_keys.contains(key) {
                        if let Ok(msg) = encode_peer_event(*key, *role, "disconnected") {
                            if ws_tx.send(Message::Binary(msg.into())).await.is_err() {
                                break;
                            }
                        }
                        if *role == PeerRole::Camera {
                            let source = SourceId::from_node_id_bytes(key.as_bytes());
                            known_cameras.remove(&source);
                            camera_disconnected = true;
                        }
                    }
                }

                // Send updated camera list if any camera disconnected
                if camera_disconnected {
                    let list: Vec<SourceId> = known_cameras.iter().copied().collect();
                    if let Ok(msg) = encode_camera_list_from_sources(&list) {
                        if ws_tx.send(Message::Binary(msg.into())).await.is_err() {
                            break;
                        }
                    }
                }

                known_peers = current_peers.iter().map(|p| (p.key, p.role)).collect();

                // Push stats
                let stats = handle.stats().await;
                let uptime = server_start.elapsed().as_secs();
                if let Ok(msg) = encode_server_stats(&stats, uptime) {
                    if ws_tx.send(Message::Binary(msg.into())).await.is_err() {
                        break;
                    }
                }
            }
        }
    }

    debug!("WebSocket client disconnected");
}

/// Process a non-video frame (audio or telemetry) into WebSocket messages.
/// Video is handled separately by the shared VideoMuxer.
fn process_frame(
    frame: &Frame,
    telemetry_states: &mut HashMap<SourceId, TelemetryState>,
) -> Vec<Vec<u8>> {
    let mut messages = Vec::new();
    let src = frame.source;

    match frame.channel {
        Channel::Audio => {
            // Compute audio level from PCM data
            let level_db = compute_audio_rms(&frame.payload);
            messages.push(encode_audio_level(src, level_db));

            // Forward PCM data
            // 48000 Hz mono matches AudioCaptureConfig::default() and TestAudioConfig::default()
            messages.push(encode_audio_data(src, 48000, 1, &frame.payload));
        }
        Channel::Telemetry => {
            // Decode MessagePack sparse telemetry, accumulate state, send as JSON
            match rmp_serde::from_slice::<WireTelemetry>(&frame.payload) {
                Ok(wire) => {
                    let state = telemetry_states.entry(src).or_default();
                    state.merge(wire);
                    if state.ready {
                        if let Ok(json) = serde_json::to_vec(state) {
                            messages.push(encode_telemetry(src, &json));
                        }
                    }
                }
                Err(e) => {
                    warn!("Failed to decode telemetry msgpack: {}", e);
                }
            }
        }
        _ => {} // Video handled by VideoMuxer, unknown channels ignored
    }

    messages
}

async fn get_camera_ids(handle: &RouterHandle) -> Vec<SourceId> {
    let peers = handle.peers().await;
    peers
        .into_iter()
        .filter(|p| p.role == PeerRole::Camera)
        .map(|p| SourceId::from_node_id_bytes(p.key.as_bytes()))
        .collect()
}

fn encode_camera_list(cameras: &[SourceId]) -> Result<Vec<u8>, serde_json::Error> {
    let list: Vec<serde_json::Value> = cameras
        .iter()
        .map(|id| {
            serde_json::json!({
                "id": format!("{}", id),
                "name": format!("Camera {}", &format!("{}", id)[..8.min(format!("{}", id).len())]),
                "connected": true,
            })
        })
        .collect();
    let json = serde_json::to_vec(&list)?;
    let mut buf = Vec::with_capacity(1 + json.len());
    buf.push(MSG_CAMERA_LIST);
    buf.extend_from_slice(&json);
    Ok(buf)
}

fn encode_camera_list_from_sources(cameras: &[SourceId]) -> Result<Vec<u8>, serde_json::Error> {
    encode_camera_list(cameras)
}

fn encode_video_init(src: SourceId, codec: &str, width: u32, height: u32, init_segment: &[u8]) -> Vec<u8> {
    let codec_bytes = codec.as_bytes();
    let codec_len = codec_bytes.len().min(255) as u8;
    let mut buf = Vec::with_capacity(1 + 8 + 1 + codec_len as usize + 4 + init_segment.len());
    buf.push(MSG_VIDEO_INIT);
    buf.extend_from_slice(src.as_bytes());
    buf.push(codec_len);
    buf.extend_from_slice(&codec_bytes[..codec_len as usize]);
    buf.extend_from_slice(&(width as u16).to_le_bytes());
    buf.extend_from_slice(&(height as u16).to_le_bytes());
    buf.extend_from_slice(init_segment);
    buf
}

fn encode_video_segment(src: SourceId, segment: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(1 + 8 + segment.len());
    buf.push(MSG_VIDEO_SEGMENT);
    buf.extend_from_slice(src.as_bytes());
    buf.extend_from_slice(segment);
    buf
}

fn encode_telemetry(src: SourceId, payload: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(1 + 8 + payload.len());
    buf.push(MSG_TELEMETRY);
    buf.extend_from_slice(src.as_bytes());
    buf.extend_from_slice(payload);
    buf
}

fn encode_audio_level(src: SourceId, level_db: f32) -> Vec<u8> {
    let mut buf = Vec::with_capacity(1 + 8 + 4);
    buf.push(MSG_AUDIO_LEVEL);
    buf.extend_from_slice(src.as_bytes());
    buf.extend_from_slice(&level_db.to_le_bytes());
    buf
}

fn encode_audio_data(src: SourceId, sample_rate: u32, channels: u8, pcm_data: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(1 + 8 + 4 + 1 + pcm_data.len());
    buf.push(MSG_AUDIO_DATA);
    buf.extend_from_slice(src.as_bytes());
    buf.extend_from_slice(&sample_rate.to_le_bytes());
    buf.push(channels);
    buf.extend_from_slice(pcm_data);
    buf
}

fn encode_peer_event(key: iroh::PublicKey, role: PeerRole, event_type: &str) -> Result<Vec<u8>, serde_json::Error> {
    let mut obj = serde_json::json!({
        "type": event_type,
        "key": key.to_string(),
        "role": match role {
            PeerRole::Camera => "camera",
            PeerRole::Client => "client",
        },
    });
    if role == PeerRole::Camera {
        let source = SourceId::from_node_id_bytes(key.as_bytes());
        obj["source_id"] = serde_json::json!(format!("{}", source));
    }
    let json = serde_json::to_vec(&obj)?;
    let mut buf = Vec::with_capacity(1 + json.len());
    buf.push(MSG_PEER_EVENT);
    buf.extend_from_slice(&json);
    Ok(buf)
}

fn encode_server_stats(stats: &crate::server::RouterStats, uptime_secs: u64) -> Result<Vec<u8>, serde_json::Error> {
    let obj = serde_json::json!({
        "cameras": stats.cameras_connected,
        "clients": stats.clients_connected,
        "uptime_secs": uptime_secs,
        "frames_received": stats.frames_received,
        "frames_broadcast": stats.frames_broadcast,
    });
    let json = serde_json::to_vec(&obj)?;
    let mut buf = Vec::with_capacity(1 + json.len());
    buf.push(MSG_SERVER_STATS);
    buf.extend_from_slice(&json);
    Ok(buf)
}

/// Compute RMS audio level in dB from PCM s16le data
fn compute_audio_rms(data: &[u8]) -> f32 {
    if data.len() < 2 {
        return -60.0;
    }

    let mut sum_sq = 0.0f64;
    let sample_count = data.len() / 2;

    for chunk in data.chunks_exact(2) {
        let sample = i16::from_le_bytes([chunk[0], chunk[1]]) as f64;
        sum_sq += sample * sample;
    }

    if sample_count == 0 {
        return -60.0;
    }

    let rms = (sum_sq / sample_count as f64).sqrt();
    let db = 20.0 * (rms / 32768.0).log10();
    db.max(-60.0) as f32
}
