//! WebSocket handler: subscribes to router broadcast, muxes fMP4, pushes to clients.
//!
//! Binary protocol (client ← server):
//!   0x01 + JSON           → camera list update
//!   0x02 + src(8) + codec_len(1) + codec + w(2LE) + h(2LE) + init_segment
//!   0x03 + src(8) + media_segment
//!   0x04 + src(8) + JSON  → telemetry
//!   0x05 + src(8) + f32LE → audio level
//!   0x06 + src(8) + sr(4LE) + ch(1) + pcm_data → audio data

use std::collections::{HashMap, HashSet};

use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::broadcast;
use tracing::{debug, warn};

use crate::{Channel, Frame, SourceId};
use crate::server::{PeerRole, RouterHandle};
use super::fmp4::Fmp4Muxer;

const MSG_CAMERA_LIST: u8 = 0x01;
const MSG_VIDEO_INIT: u8 = 0x02;
const MSG_VIDEO_SEGMENT: u8 = 0x03;
const MSG_TELEMETRY: u8 = 0x04;
const MSG_AUDIO_LEVEL: u8 = 0x05;
const MSG_AUDIO_DATA: u8 = 0x06;

/// Handle a single WebSocket connection.
pub async fn handle_ws(socket: WebSocket, handle: RouterHandle) {
    let (mut ws_tx, mut ws_rx) = socket.split();
    let mut rx = handle.subscribe();

    // Send initial camera list
    let cameras = get_camera_ids(&handle).await;
    if let Ok(msg) = encode_camera_list(&cameras) {
        let _ = ws_tx.send(Message::Binary(msg.into())).await;
    }

    // Per-source fMP4 muxers (run on blocking thread pool)
    let mut muxers: HashMap<SourceId, Fmp4Muxer> = HashMap::new();
    let mut known_cameras: HashSet<SourceId> = cameras.into_iter().collect();

    loop {
        tokio::select! {
            // Forward frames from router to WebSocket client
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

                        let messages = process_frame(&frame, &mut muxers);
                        for msg in messages {
                            if ws_tx.send(Message::Binary(msg.into())).await.is_err() {
                                break;
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!("WebSocket client lagged, missed {} frames", n);
                        // Reset muxers since we missed frames
                        for muxer in muxers.values_mut() {
                            muxer.reset();
                        }
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
        }
    }

    debug!("WebSocket client disconnected");
}

fn process_frame(frame: &Frame, muxers: &mut HashMap<SourceId, Fmp4Muxer>) -> Vec<Vec<u8>> {
    let mut messages = Vec::new();
    let src = frame.source;

    match frame.channel {
        Channel::Video => {
            let muxer = muxers.entry(src).or_insert_with(Fmp4Muxer::new);
            let result = muxer.mux_frame(&frame.payload, frame.flags.is_keyframe(), frame.timestamp_us);

            if let (Some(init), Some(codec), Some(w), Some(h)) =
                (&result.init_segment, &result.codec, result.width, result.height)
            {
                messages.push(encode_video_init(src, codec, w, h, init));
            }

            if let Some(segment) = &result.media_segment {
                messages.push(encode_video_segment(src, segment));
            }
        }
        Channel::Audio => {
            // Compute audio level from PCM data
            let level_db = compute_audio_rms(&frame.payload);
            messages.push(encode_audio_level(src, level_db));

            // Forward PCM data
            // Default: 16000 Hz mono (matching camera capture defaults)
            messages.push(encode_audio_data(src, 16000, 1, &frame.payload));
        }
        Channel::Telemetry => {
            messages.push(encode_telemetry(src, &frame.payload));
        }
        _ => {} // Unknown channels ignored
    }

    messages
}

async fn get_camera_ids(handle: &RouterHandle) -> Vec<SourceId> {
    let peers = handle.peers().await;
    peers
        .into_iter()
        .filter(|(_, role)| *role == PeerRole::Camera)
        .map(|(key, _)| SourceId::from_node_id_bytes(key.as_bytes()))
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
