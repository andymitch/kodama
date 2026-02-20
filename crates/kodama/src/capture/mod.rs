//! Capture module for video, audio, and telemetry
//!
//! This module handles sensor capture from cameras:
//! - Video capture via libcamera (Pi camera)
//! - Audio capture via ALSA/cpal (future)
//! - Telemetry collection (CPU, temp, etc.) (future)
//! - H.264 parsing utilities for keyframe detection

pub mod abr;
pub mod audio;
pub mod h264;
pub mod telemetry;
pub mod throughput;
pub mod video;

// Re-export commonly used types
pub use abr::{AbrConfig, AbrController, AbrDecision, QualityTier};
pub use audio::{AudioCapture, AudioCaptureConfig};
pub use telemetry::{
    decode_telemetry, encode_telemetry, GpsData, MotionDetector, SparseTelemetry, TelemetryCapture,
    TelemetryCaptureConfig, TelemetryData, TelemetryThresholds,
};
pub use throughput::ThroughputTracker;
pub use video::{VideoCapture, VideoCaptureConfig};

#[cfg(feature = "test-source")]
pub use audio::{start_test_audio, TestAudioConfig};
#[cfg(feature = "test-source")]
pub use video::{start_test_source, TestSourceConfig};
