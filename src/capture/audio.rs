//! Audio capture module
//!
//! Captures audio from the default input device and provides raw PCM samples.
//! For a full implementation with Opus encoding, enable the `audio` feature
//! and add the `cpal` and `opus` dependencies.
//!
//! This module provides a basic implementation that works without external
//! audio dependencies by reading from stdin or a file (useful for testing).

use anyhow::{Context, Result};
use bytes::Bytes;
use std::io::Read;
use std::process::{Child, Command, Stdio};
#[cfg(feature = "test-source")]
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{debug, error, info};

/// Audio capture configuration
#[derive(Debug, Clone)]
pub struct AudioCaptureConfig {
    /// Sample rate in Hz
    pub sample_rate: u32,
    /// Number of channels (1 = mono, 2 = stereo)
    pub channels: u8,
    /// Bits per sample (16 or 32)
    pub bits_per_sample: u8,
    /// Buffer duration in milliseconds
    pub buffer_duration_ms: u32,
    /// Audio device name (None = default)
    pub device: Option<String>,
}

impl Default for AudioCaptureConfig {
    fn default() -> Self {
        Self {
            sample_rate: 48000,
            channels: 1,
            bits_per_sample: 16,
            buffer_duration_ms: 20, // 20ms buffers (960 samples at 48kHz)
            device: None,
        }
    }
}

impl AudioCaptureConfig {
    /// Calculate the buffer size in bytes
    pub fn buffer_size_bytes(&self) -> usize {
        let samples_per_buffer = (self.sample_rate * self.buffer_duration_ms) / 1000;
        let bytes_per_sample = (self.bits_per_sample / 8) as usize;
        samples_per_buffer as usize * self.channels as usize * bytes_per_sample
    }

    /// Calculate the buffer size in samples
    pub fn buffer_size_samples(&self) -> usize {
        ((self.sample_rate * self.buffer_duration_ms) / 1000) as usize
    }
}

/// Handle to a running audio capture
pub struct AudioCapture {
    child: Option<Child>,
    shutdown_tx: Option<mpsc::Sender<()>>,
    config: AudioCaptureConfig,
}

impl AudioCapture {
    /// Start audio capture using arecord (ALSA) on Linux
    ///
    /// Returns a receiver for raw PCM audio packets.
    /// Each packet contains `buffer_duration_ms` worth of audio.
    pub fn start(config: AudioCaptureConfig) -> Result<(Self, mpsc::Receiver<Bytes>)> {
        Self::start_arecord(config)
    }

    /// Start capture using arecord (Linux ALSA)
    fn start_arecord(config: AudioCaptureConfig) -> Result<(Self, mpsc::Receiver<Bytes>)> {
        let (tx, rx) = mpsc::channel(32);
        let (shutdown_tx, _shutdown_rx) = mpsc::channel::<()>(1);

        let format = match config.bits_per_sample {
            16 => "S16_LE",
            32 => "S32_LE",
            _ => anyhow::bail!("Unsupported bits per sample: {}", config.bits_per_sample),
        };

        let mut args = vec![
            "-f".to_string(), format.to_string(),
            "-r".to_string(), config.sample_rate.to_string(),
            "-c".to_string(), config.channels.to_string(),
            "-t".to_string(), "raw".to_string(),
            "--buffer-time".to_string(), (config.buffer_duration_ms * 1000).to_string(),
            "-q".to_string(), // Quiet mode
        ];

        if let Some(ref device) = config.device {
            args.push("-D".to_string());
            args.push(device.clone());
        }

        // Output to stdout
        args.push("-".to_string());

        info!(
            "Starting arecord: {}Hz, {} channels, {} bits",
            config.sample_rate, config.channels, config.bits_per_sample
        );
        debug!("arecord args: {:?}", args);

        let mut child = Command::new("arecord")
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("Failed to spawn arecord. Is ALSA installed?")?;

        let stdout = child
            .stdout
            .take()
            .context("Failed to capture stdout from arecord")?;

        let buffer_size = config.buffer_size_bytes();
        let read_config = config.clone();

        // Spawn blocking reader task
        tokio::task::spawn_blocking(move || {
            Self::read_audio_stream(stdout, tx, buffer_size, &read_config);
        });

        Ok((
            Self {
                child: Some(child),
                shutdown_tx: Some(shutdown_tx),
                config,
            },
            rx,
        ))
    }

    /// Read audio stream and send packets to channel
    fn read_audio_stream<R: Read>(
        mut reader: R,
        tx: mpsc::Sender<Bytes>,
        buffer_size: usize,
        config: &AudioCaptureConfig,
    ) {
        let mut buf = vec![0u8; buffer_size];
        let mut total_bytes = 0u64;
        let mut packet_count = 0u64;

        loop {
            match reader.read_exact(&mut buf) {
                Ok(()) => {
                    total_bytes += buffer_size as u64;
                    packet_count += 1;

                    if packet_count % 500 == 0 {
                        let duration_secs = (packet_count * config.buffer_duration_ms as u64) / 1000;
                        debug!(
                            "Audio capture: {} packets, {} bytes, {}s",
                            packet_count, total_bytes, duration_secs
                        );
                    }

                    if tx.blocking_send(Bytes::copy_from_slice(&buf)).is_err() {
                        info!("Audio receiver dropped, stopping capture");
                        break;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    info!("Audio stream ended (EOF)");
                    break;
                }
                Err(e) => {
                    error!("Error reading audio stream: {}", e);
                    break;
                }
            }
        }

        info!(
            "Audio capture finished: {} packets, {} bytes",
            packet_count, total_bytes
        );
    }

    /// Get the capture configuration
    pub fn config(&self) -> &AudioCaptureConfig {
        &self.config
    }

    /// Stop capture and clean up
    pub fn stop(&mut self) {
        self.shutdown_tx.take();
        if let Some(mut child) = self.child.take() {
            info!("Stopping audio capture");
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for AudioCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Configuration for test audio source
#[cfg(feature = "test-source")]
#[derive(Debug, Clone)]
pub struct TestAudioConfig {
    /// Sample rate in Hz
    pub sample_rate: u32,
    /// Number of channels
    pub channels: u8,
    /// Frequency of test tone in Hz
    pub tone_frequency: f32,
    /// Buffer duration in milliseconds
    pub buffer_duration_ms: u32,
}

#[cfg(feature = "test-source")]
impl Default for TestAudioConfig {
    fn default() -> Self {
        Self {
            sample_rate: 48000,
            channels: 1,
            tone_frequency: 440.0, // A4 note
            buffer_duration_ms: 20,
        }
    }
}

/// Start a test audio source that generates a sine wave
#[cfg(feature = "test-source")]
pub fn start_test_audio(config: TestAudioConfig) -> mpsc::Receiver<Bytes> {
    use std::f32::consts::PI;

    let (tx, rx) = mpsc::channel(32);

    tokio::spawn(async move {
        let samples_per_buffer = (config.sample_rate * config.buffer_duration_ms) / 1000;
        let buffer_size = samples_per_buffer as usize * config.channels as usize * 2; // 16-bit
        let buffer_duration = Duration::from_millis(config.buffer_duration_ms as u64);

        let mut interval = tokio::time::interval(buffer_duration);
        let mut sample_idx: u64 = 0;

        info!(
            "Test audio source started: {}Hz, {} channels, {}Hz tone",
            config.sample_rate, config.channels, config.tone_frequency
        );

        loop {
            interval.tick().await;

            let mut buffer = Vec::with_capacity(buffer_size);

            for _ in 0..samples_per_buffer {
                let t = sample_idx as f32 / config.sample_rate as f32;
                let sample = (2.0 * PI * config.tone_frequency * t).sin();
                let sample_i16 = (sample * 32767.0) as i16;

                for _ in 0..config.channels {
                    buffer.extend_from_slice(&sample_i16.to_le_bytes());
                }

                sample_idx += 1;
            }

            if tx.send(Bytes::from(buffer)).await.is_err() {
                info!("Test audio receiver dropped");
                break;
            }
        }
    });

    rx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_audio_config_buffer_size() {
        let config = AudioCaptureConfig {
            sample_rate: 48000,
            channels: 1,
            bits_per_sample: 16,
            buffer_duration_ms: 20,
            device: None,
        };

        // 48000 * 20 / 1000 = 960 samples
        // 960 * 1 channel * 2 bytes = 1920 bytes
        assert_eq!(config.buffer_size_bytes(), 1920);
        assert_eq!(config.buffer_size_samples(), 960);
    }

    #[test]
    fn test_audio_config_stereo() {
        let config = AudioCaptureConfig {
            sample_rate: 44100,
            channels: 2,
            bits_per_sample: 16,
            buffer_duration_ms: 10,
            device: None,
        };

        // 44100 * 10 / 1000 = 441 samples
        // 441 * 2 channels * 2 bytes = 1764 bytes
        assert_eq!(config.buffer_size_bytes(), 1764);
    }

    #[cfg(feature = "test-source")]
    #[tokio::test]
    async fn test_audio_source() {
        let config = TestAudioConfig {
            sample_rate: 48000,
            channels: 1,
            tone_frequency: 440.0,
            buffer_duration_ms: 20,
        };

        let mut rx = start_test_audio(config);

        // Receive a few buffers
        for _ in 0..3 {
            let buffer = tokio::time::timeout(
                Duration::from_millis(100),
                rx.recv()
            ).await.unwrap().unwrap();

            // 960 samples * 2 bytes = 1920
            assert_eq!(buffer.len(), 1920);
        }
    }
}
