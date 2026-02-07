//! Video capture module
//!
//! Provides video capture from Pi cameras using libcamera-vid,
//! and a test source for development without hardware.

use anyhow::{Context, Result};
use bytes::Bytes;
use std::io::Read;
use std::process::{Child, Command, Stdio};
use tokio::sync::mpsc;
use tracing::{debug, error, info};

/// Video capture configuration
#[derive(Debug, Clone)]
pub struct VideoCaptureConfig {
    /// Frame width in pixels
    pub width: u32,
    /// Frame height in pixels
    pub height: u32,
    /// Frames per second
    pub fps: u32,
    /// Bitrate in bits per second (0 = auto)
    pub bitrate: u32,
    /// Include SPS/PPS with each keyframe
    pub inline_headers: bool,
    /// Keyframe interval in frames (0 = auto)
    pub keyframe_interval: u32,
}

impl Default for VideoCaptureConfig {
    fn default() -> Self {
        Self {
            width: 1280,
            height: 720,
            fps: 30,
            bitrate: 0, // Auto
            inline_headers: true,
            keyframe_interval: 20, // Every 0.66s at 30fps (balanced)
        }
    }
}

impl VideoCaptureConfig {
    /// 1080p at 30fps
    pub fn hd() -> Self {
        Self {
            width: 1920,
            height: 1080,
            fps: 30,
            ..Default::default()
        }
    }

    /// 720p at 30fps (default)
    pub fn hd_720() -> Self {
        Self::default()
    }

    /// 480p at 30fps (low bandwidth)
    pub fn sd() -> Self {
        Self {
            width: 854,
            height: 480,
            fps: 30,
            ..Default::default()
        }
    }
}

/// Handle to a running video capture process
pub struct VideoCapture {
    child: Option<Child>,
    config: VideoCaptureConfig,
}

impl VideoCapture {
    /// Start video capture using libcamera-vid (Pi camera)
    ///
    /// Returns a receiver for H.264 encoded video chunks.
    /// Each chunk may contain one or more NAL units.
    pub fn start(config: VideoCaptureConfig) -> Result<(Self, mpsc::Receiver<Bytes>)> {
        Self::start_libcamera(config)
    }

    /// Start capture using libcamera-vid
    fn start_libcamera(config: VideoCaptureConfig) -> Result<(Self, mpsc::Receiver<Bytes>)> {
        let (tx, rx) = mpsc::channel(config.fps as usize); // Buffer ~1 second

        let mut args = vec![
            "-t".to_string(),
            "0".to_string(), // Run indefinitely
            "--width".to_string(),
            config.width.to_string(),
            "--height".to_string(),
            config.height.to_string(),
            "--framerate".to_string(),
            config.fps.to_string(),
            "--codec".to_string(),
            "h264".to_string(),
            "-o".to_string(),
            "-".to_string(), // Output to stdout
            "--flush".to_string(), // Flush output immediately
            "-n".to_string(), // No preview window (headless)
        ];

        if config.inline_headers {
            args.push("--inline".to_string());
        }

        if config.bitrate > 0 {
            args.push("--bitrate".to_string());
            args.push(config.bitrate.to_string());
        }

        if config.keyframe_interval > 0 {
            args.push("--intra".to_string());
            args.push(config.keyframe_interval.to_string());
        }

        // Try rpicam-vid first (modern Pi OS), fall back to libcamera-vid (legacy)
        let cmd = if Command::new("rpicam-vid").arg("--help").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok() {
            "rpicam-vid"
        } else {
            "libcamera-vid"
        };

        info!(
            "Starting {}: {}x{} @ {}fps",
            cmd, config.width, config.height, config.fps
        );
        debug!("{} args: {:?}", cmd, args);

        let mut child = Command::new(cmd)
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context(format!("Failed to spawn {}. Is it installed?", cmd))?;

        let stdout = child
            .stdout
            .take()
            .context(format!("Failed to capture stdout from {}", cmd))?;

        // Spawn blocking reader task
        let read_config = config.clone();
        tokio::task::spawn_blocking(move || {
            Self::read_video_stream(stdout, tx, &read_config);
        });

        Ok((
            Self {
                child: Some(child),
                config,
            },
            rx,
        ))
    }

    /// Read video stream from stdout and send chunks to channel
    fn read_video_stream<R: Read>(mut reader: R, tx: mpsc::Sender<Bytes>, config: &VideoCaptureConfig) {
        // Buffer size: aim for ~1 frame worth of data at estimated bitrate
        // Default H.264 at 720p30 is roughly 2-4 Mbps, so ~10-15KB per frame
        let buf_size = if config.bitrate > 0 {
            (config.bitrate / 8 / config.fps).max(16384) as usize
        } else {
            65536 // 64KB default
        };

        let mut buf = vec![0u8; buf_size];
        let mut total_bytes = 0u64;
        let mut chunk_count = 0u64;

        loop {
            match reader.read(&mut buf) {
                Ok(0) => {
                    info!("Video stream ended (EOF)");
                    break;
                }
                Ok(n) => {
                    total_bytes += n as u64;
                    chunk_count += 1;

                    if chunk_count % 100 == 0 {
                        debug!(
                            "Video capture: {} chunks, {} bytes total",
                            chunk_count, total_bytes
                        );
                    }

                    if tx.blocking_send(Bytes::copy_from_slice(&buf[..n])).is_err() {
                        info!("Video receiver dropped, stopping capture");
                        break;
                    }
                }
                Err(e) => {
                    error!("Error reading video stream: {}", e);
                    break;
                }
            }
        }

        info!(
            "Video capture finished: {} chunks, {} bytes",
            chunk_count, total_bytes
        );
    }

    /// Get the capture configuration
    pub fn config(&self) -> &VideoCaptureConfig {
        &self.config
    }

    /// Stop capture and clean up
    pub fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            info!("Stopping video capture");
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for VideoCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Configuration for test video source
#[cfg(feature = "test-source")]
#[derive(Debug, Clone)]
pub struct TestSourceConfig {
    /// Frames per second
    pub fps: u32,
    /// Simulated frame size in bytes (ignored - real H.264 sizes used)
    pub frame_size: usize,
    /// Simulate keyframes every N frames (ignored - real H.264 keyframe interval used)
    pub keyframe_interval: u32,
}

#[cfg(feature = "test-source")]
impl Default for TestSourceConfig {
    fn default() -> Self {
        Self {
            fps: 30,
            frame_size: 15000,
            keyframe_interval: 30,
        }
    }
}

/// Embedded H.264 Annex B test stream (320x240 baseline, 30fps, 2 seconds)
#[cfg(feature = "test-source")]
static TEST_H264_DATA: &[u8] = include_bytes!("test_video.h264");

/// Split embedded H.264 Annex B stream into per-frame chunks.
/// Each keyframe chunk includes SPS+PPS+IDR NALs.
/// Each P-frame chunk includes a single Non-IDR slice NAL.
#[cfg(feature = "test-source")]
fn split_h264_frames(data: &[u8]) -> Vec<(Vec<u8>, bool)> {
    let mut frames = Vec::new();
    let mut current_frame = Vec::new();
    let mut is_keyframe = false;

    // Parse NAL units by finding start codes
    let mut i = 0;
    let mut nal_starts: Vec<(usize, usize)> = Vec::new(); // (offset_after_sc, sc_len)
    while i < data.len().saturating_sub(3) {
        if i + 4 <= data.len() && data[i..i + 4] == [0, 0, 0, 1] {
            nal_starts.push((i + 4, 4));
            i += 4;
        } else if data[i..i + 3] == [0, 0, 1] {
            nal_starts.push((i + 3, 3));
            i += 3;
        } else {
            i += 1;
        }
    }

    for (idx, &(nal_body_start, sc_len)) in nal_starts.iter().enumerate() {
        let nal_start = nal_body_start - sc_len;
        let nal_end = if idx + 1 < nal_starts.len() {
            nal_starts[idx + 1].0 - nal_starts[idx + 1].1
        } else {
            data.len()
        };
        let nal_type = data[nal_body_start] & 0x1F;
        let nal_data = &data[nal_start..nal_end];

        match nal_type {
            7 | 8 | 6 => {
                // SPS, PPS, SEI - accumulate into keyframe
                current_frame.extend_from_slice(nal_data);
                is_keyframe = true;
            }
            5 => {
                // IDR slice - complete the keyframe
                current_frame.extend_from_slice(nal_data);
                frames.push((current_frame.clone(), true));
                current_frame.clear();
                is_keyframe = false;
            }
            1 => {
                // Non-IDR P-frame
                if !current_frame.is_empty() {
                    // Flush any accumulated non-slice NALs
                    frames.push((current_frame.clone(), is_keyframe));
                    current_frame.clear();
                    is_keyframe = false;
                }
                frames.push((nal_data.to_vec(), false));
            }
            _ => {
                current_frame.extend_from_slice(nal_data);
            }
        }
    }

    frames
}

/// Build an H.264 filler data NAL unit of the given size.
/// Filler NAL (type 12) is ignored by decoders, used to pad bitstreams.
#[cfg(feature = "test-source")]
fn h264_filler_nal(size: usize) -> Vec<u8> {
    let mut nal = Vec::with_capacity(size + 5);
    nal.extend_from_slice(&[0x00, 0x00, 0x00, 0x01, 0x0C]); // start code + filler type
    // Filler bytes (0xFF per spec), then RBSP stop bit (0x80)
    let payload_len = size.saturating_sub(1);
    nal.extend(std::iter::repeat(0xFF).take(payload_len));
    nal.push(0x80);
    nal
}

/// Start a test video source that generates real H.264 Annex B frames.
/// Simulates periodic motion bursts by padding P-frames with varying amounts
/// of filler NAL units so the motion detector sees realistic frame-size variance.
#[cfg(feature = "test-source")]
pub fn start_test_source(config: TestSourceConfig) -> mpsc::Receiver<Bytes> {
    use tokio::time::{interval, Duration};

    let (tx, rx) = mpsc::channel(config.fps as usize);

    tokio::spawn(async move {
        let mut interval = interval(Duration::from_micros(1_000_000 / config.fps as u64));
        let frames = split_h264_frames(TEST_H264_DATA);
        let mut frame_idx = 0usize;
        let mut total_sent = 0u64;

        // Motion simulation: 5s calm, 3s motion, repeating
        let calm_frames = config.fps as u64 * 5;
        let motion_frames = config.fps as u64 * 3;
        let cycle_len = calm_frames + motion_frames;

        // Simple LCG for deterministic pseudo-random padding sizes
        let mut rng_state: u32 = 42;

        info!(
            "Test video source started: {}fps, {} H.264 frames (looping), motion bursts every {}s",
            config.fps,
            frames.len(),
            cycle_len / config.fps as u64,
        );

        loop {
            interval.tick().await;

            let (ref data, is_keyframe) = frames[frame_idx];
            frame_idx = (frame_idx + 1) % frames.len();
            total_sent += 1;

            // During motion bursts, pad P-frames with varying filler NALs
            // (1x-5x multiplier, simulating variable motion intensity per-frame)
            let cycle_pos = total_sent % cycle_len;
            let in_motion = cycle_pos >= calm_frames;

            let frame_data = if in_motion && !is_keyframe {
                // LCG: state = state * 1103515245 + 12345
                rng_state = rng_state.wrapping_mul(1103515245).wrapping_add(12345);
                let multiplier = 1 + (rng_state >> 16) % 5; // 1-5x
                let pad_size = data.len() * multiplier as usize;
                let mut padded = data.clone();
                padded.extend_from_slice(&h264_filler_nal(pad_size));
                padded
            } else {
                data.clone()
            };

            if tx.send(Bytes::from(frame_data)).await.is_err() {
                info!("Test source receiver dropped");
                break;
            }

            if total_sent % 300 == 0 {
                debug!("Test source: {} frames sent", total_sent);
            }
        }

        info!("Test video source stopped after {} frames", total_sent);
    });

    rx
}
