//! Capture module for video, audio, and telemetry
//!
//! This module handles sensor capture from cameras:
//! - Video capture via libcamera (Pi camera)
//! - Audio capture via ALSA/cpal (future)
//! - Telemetry collection (CPU, temp, etc.) (future)
//! - H.264 parsing utilities for keyframe detection

pub mod video;
pub mod audio;
pub mod telemetry;
pub mod h264;

// Re-export commonly used types
pub use video::{VideoCapture, VideoCaptureConfig};

#[cfg(feature = "test-source")]
pub use video::{start_test_source, TestSourceConfig};
