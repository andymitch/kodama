//! yurei-capture: Video, audio, and telemetry capture for Yurei cameras
//!
//! This crate handles sensor capture on camera devices:
//! - Video capture via libcamera-vid (H.264 encoded)
//! - H.264 NAL unit parsing and keyframe detection
//! - Audio capture via ALSA/cpal (Opus encoded) [future]
//! - Telemetry collection (CPU, temp, etc.) [future]
//!
//! # Example
//!
//! ```ignore
//! use yurei_capture::{VideoCapture, VideoCaptureConfig, h264};
//!
//! // Start capture with default 720p30 settings
//! let (capture, mut rx) = VideoCapture::start(VideoCaptureConfig::default())?;
//!
//! // Receive H.264 chunks and detect keyframes
//! while let Some(chunk) = rx.recv().await {
//!     let is_keyframe = h264::contains_keyframe(&chunk);
//!     // Process video data...
//! }
//! ```
//!
//! # Test Source
//!
//! For development without camera hardware, enable the `test-source` feature:
//!
//! ```ignore
//! use yurei_capture::test::{start_test_source, TestSourceConfig};
//!
//! let mut rx = start_test_source(TestSourceConfig::default());
//! while let Some(frame) = rx.recv().await {
//!     // Process fake frame...
//! }
//! ```

pub mod h264;
mod video;

pub use video::{VideoCapture, VideoCaptureConfig};

#[cfg(feature = "test-source")]
pub use video::test::{self, parse_test_frame, start_test_source, TestSourceConfig};
