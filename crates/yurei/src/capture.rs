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
            keyframe_interval: 60, // Every 2 seconds at 30fps
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

        info!(
            "Starting libcamera-vid: {}x{} @ {}fps",
            config.width, config.height, config.fps
        );
        debug!("libcamera-vid args: {:?}", args);

        let mut child = Command::new("libcamera-vid")
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("Failed to spawn libcamera-vid. Is it installed?")?;

        let stdout = child
            .stdout
            .take()
            .context("Failed to capture stdout from libcamera-vid")?;

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
    /// Simulated frame size in bytes
    pub frame_size: usize,
    /// Simulate keyframes every N frames
    pub keyframe_interval: u32,
}

#[cfg(feature = "test-source")]
impl Default for TestSourceConfig {
    fn default() -> Self {
        Self {
            fps: 30,
            frame_size: 10000, // ~10KB per frame
            keyframe_interval: 30,
        }
    }
}

/// Start a test video source that generates fake H.264-like frames
#[cfg(feature = "test-source")]
pub fn start_test_source(config: TestSourceConfig) -> mpsc::Receiver<Bytes> {
    use tokio::time::{interval, Duration, Instant};

    let (tx, rx) = mpsc::channel(config.fps as usize);

    tokio::spawn(async move {
        let mut interval = interval(Duration::from_micros(1_000_000 / config.fps as u64));
        let mut frame_num = 0u32;
        let start = Instant::now();

        info!(
            "Test video source started: {}fps, {}B frames",
            config.fps, config.frame_size
        );

        loop {
            interval.tick().await;

            let is_keyframe = frame_num % config.keyframe_interval == 0;
            let timestamp_us = start.elapsed().as_micros() as u64;

            // Build fake frame
            let mut data = Vec::with_capacity(config.frame_size);

            // Frame header
            data.extend_from_slice(&frame_num.to_be_bytes());
            data.push(if is_keyframe { 0x01 } else { 0x00 });
            data.extend_from_slice(&timestamp_us.to_be_bytes());

            // Padding with pattern (simulates compressed video data)
            while data.len() < config.frame_size {
                data.push((frame_num & 0xFF) as u8);
            }
            data.truncate(config.frame_size);

            if tx.send(Bytes::from(data)).await.is_err() {
                info!("Test source receiver dropped");
                break;
            }

            frame_num = frame_num.wrapping_add(1);

            if frame_num % 300 == 0 {
                debug!("Test source: {} frames generated", frame_num);
            }
        }

        info!("Test video source stopped after {} frames", frame_num);
    });

    rx
}
