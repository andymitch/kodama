//! Audio capture module (stub)
//!
//! Future: Capture audio via ALSA/cpal and encode to Opus.

use anyhow::Result;
use bytes::Bytes;
use tokio::sync::mpsc;

/// Audio capture configuration
#[derive(Debug, Clone)]
pub struct AudioCaptureConfig {
    /// Sample rate in Hz
    pub sample_rate: u32,
    /// Number of channels (1 = mono, 2 = stereo)
    pub channels: u8,
    /// Opus bitrate in bits per second
    pub bitrate: u32,
}

impl Default for AudioCaptureConfig {
    fn default() -> Self {
        Self {
            sample_rate: 48000,
            channels: 1, // Mono for security cameras
            bitrate: 32000, // 32kbps Opus
        }
    }
}

/// Handle to a running audio capture
pub struct AudioCapture {
    _config: AudioCaptureConfig,
}

impl AudioCapture {
    /// Start audio capture
    ///
    /// Returns a receiver for Opus-encoded audio packets.
    pub fn start(_config: AudioCaptureConfig) -> Result<(Self, mpsc::Receiver<Bytes>)> {
        // TODO: Implement audio capture using cpal + opus
        anyhow::bail!("Audio capture not yet implemented")
    }

    /// Stop capture
    pub fn stop(&mut self) {
        // TODO: Clean up audio capture
    }
}
