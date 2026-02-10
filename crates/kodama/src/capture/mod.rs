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
pub mod throughput;
pub mod abr;

// Re-export commonly used types
pub use video::{VideoCapture, VideoCaptureConfig};
pub use audio::{AudioCapture, AudioCaptureConfig};
pub use telemetry::{TelemetryCapture, TelemetryCaptureConfig, TelemetryData, GpsData, MotionDetector, SparseTelemetry, TelemetryThresholds, encode_telemetry, decode_telemetry};
pub use throughput::ThroughputTracker;
pub use abr::{AbrController, AbrConfig, AbrDecision, QualityTier};

#[cfg(feature = "test-source")]
pub use video::{start_test_source, TestSourceConfig};
#[cfg(feature = "test-source")]
pub use audio::{start_test_audio, TestAudioConfig};
