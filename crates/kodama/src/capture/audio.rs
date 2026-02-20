//! Audio capture module
//!
//! Captures audio from the default ALSA input device using cpal (native ALSA bindings).
//! Produces 20ms buffers of mono 48kHz S16_LE PCM, matching the format expected by
//! the frame transport and fMP4 muxer.

use anyhow::{Context, Result};
use bytes::Bytes;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
#[cfg(feature = "test-source")]
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

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

/// Check whether an audio input device is available without starting capture.
pub fn audio_available() -> bool {
    let host = cpal::default_host();
    host.default_input_device()
        .and_then(|d| d.default_input_config().ok())
        .is_some()
}

/// Handle to a running audio capture
pub struct AudioCapture {
    _config: AudioCaptureConfig,
}

impl AudioCapture {
    /// Start audio capture using cpal (native ALSA on Linux).
    ///
    /// Returns a receiver for raw S16_LE mono 48kHz PCM packets.
    /// Each packet contains `buffer_duration_ms` worth of audio (default 20ms = 1920 bytes).
    pub fn start(config: AudioCaptureConfig) -> Result<(Self, mpsc::Receiver<Bytes>)> {
        // Pre-check: verify audio device exists before spawning thread
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .context("No audio input device available")?;
        let supported_config = device
            .default_input_config()
            .context("Failed to get default input config")?;

        info!(
            "Audio device: {:?} ({} Hz, {} ch, {:?})",
            device.name().unwrap_or_default(),
            supported_config.sample_rate().0,
            supported_config.channels(),
            supported_config.sample_format(),
        );

        let (tx, rx) = mpsc::channel(32);
        let target_rate = config.sample_rate;
        let target_samples = config.buffer_size_samples(); // 960 at 48kHz/20ms

        // cpal streams are !Send, so we run capture in a std::thread
        std::thread::Builder::new()
            .name("audio-capture".into())
            .spawn(move || {
                run_audio_capture(tx, device, supported_config, target_rate, target_samples);
            })
            .context("Failed to spawn audio capture thread")?;

        Ok((Self { _config: config }, rx))
    }
}

/// Run audio capture loop in a std::thread (cpal streams are !Send).
fn run_audio_capture(
    tx: mpsc::Sender<Bytes>,
    device: cpal::Device,
    supported_config: cpal::SupportedStreamConfig,
    target_rate: u32,
    target_samples: usize,
) {
    let source_rate = supported_config.sample_rate().0;
    let channels = supported_config.channels();

    // Channel from cpal callback → processing loop
    let (std_tx, std_rx) = std::sync::mpsc::channel::<Vec<f32>>();

    let stream = match supported_config.sample_format() {
        cpal::SampleFormat::F32 => {
            let sender = std_tx.clone();
            device.build_input_stream(
                &supported_config.into(),
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    let _ = sender.send(data.to_vec());
                },
                |err| error!("Audio stream error: {}", err),
                None,
            )
        }
        cpal::SampleFormat::I16 => {
            let sender = std_tx.clone();
            device.build_input_stream(
                &supported_config.into(),
                move |data: &[i16], _: &cpal::InputCallbackInfo| {
                    let samples: Vec<f32> =
                        data.iter().map(|&s| s as f32 / i16::MAX as f32).collect();
                    let _ = sender.send(samples);
                },
                |err| error!("Audio stream error: {}", err),
                None,
            )
        }
        cpal::SampleFormat::I32 => {
            let sender = std_tx.clone();
            device.build_input_stream(
                &supported_config.into(),
                move |data: &[i32], _: &cpal::InputCallbackInfo| {
                    let samples: Vec<f32> =
                        data.iter().map(|&s| s as f32 / i32::MAX as f32).collect();
                    let _ = sender.send(samples);
                },
                |err| error!("Audio stream error: {}", err),
                None,
            )
        }
        fmt => {
            warn!("Unsupported audio sample format: {:?}", fmt);
            return;
        }
    };

    // Drop the extra sender so the channel closes if the stream stops
    drop(std_tx);

    let stream = match stream {
        Ok(s) => s,
        Err(e) => {
            error!("Failed to build audio input stream: {}", e);
            return;
        }
    };

    if let Err(e) = stream.play() {
        error!("Failed to start audio stream: {}", e);
        return;
    }

    info!(
        "Audio capture started: {}Hz {}ch → {}Hz mono, {}ms buffers",
        source_rate,
        channels,
        target_rate,
        target_samples * 1000 / target_rate as usize,
    );

    // Accumulator for fixed-size output buffers
    let mut accumulator: Vec<f32> = Vec::with_capacity(target_samples * 2);
    let mut packet_count = 0u64;

    loop {
        match std_rx.recv() {
            Ok(raw_samples) => {
                // Stereo → mono
                let mono = stereo_to_mono(&raw_samples, channels);
                // Resample to target rate
                let resampled = resample(&mono, source_rate, target_rate);

                accumulator.extend_from_slice(&resampled);

                // Emit complete buffers
                while accumulator.len() >= target_samples {
                    let buffer: Vec<f32> = accumulator.drain(..target_samples).collect();
                    let pcm = f32_to_s16le(&buffer);

                    packet_count += 1;
                    if packet_count.is_multiple_of(500) {
                        let duration_secs =
                            packet_count * (target_samples as u64) / target_rate as u64;
                        debug!(
                            "Audio capture: {} packets, {}s",
                            packet_count, duration_secs,
                        );
                    }

                    if tx.blocking_send(Bytes::from(pcm)).is_err() {
                        info!("Audio receiver dropped, stopping capture");
                        return;
                    }
                }
            }
            Err(_) => {
                // cpal stream stopped or channel disconnected
                info!("Audio stream ended");
                break;
            }
        }
    }

    info!("Audio capture finished: {} packets", packet_count);
}

/// Convert interleaved multi-channel samples to mono by averaging channels.
fn stereo_to_mono(samples: &[f32], channels: u16) -> Vec<f32> {
    if channels == 1 {
        return samples.to_vec();
    }

    samples
        .chunks(channels as usize)
        .map(|chunk| chunk.iter().sum::<f32>() / channels as f32)
        .collect()
}

/// Linear interpolation resampler.
fn resample(samples: &[f32], source_rate: u32, target_rate: u32) -> Vec<f32> {
    if source_rate == target_rate {
        return samples.to_vec();
    }

    let ratio = source_rate as f64 / target_rate as f64;
    let output_len = ((samples.len() as f64) / ratio).ceil() as usize;
    let mut output = Vec::with_capacity(output_len);

    for i in 0..output_len {
        let src_idx = i as f64 * ratio;
        let idx0 = src_idx.floor() as usize;
        let idx1 = (idx0 + 1).min(samples.len().saturating_sub(1));
        let frac = (src_idx - idx0 as f64) as f32;

        if idx0 < samples.len() {
            let sample = samples[idx0] * (1.0 - frac) + samples[idx1] * frac;
            output.push(sample);
        }
    }

    output
}

/// Convert f32 samples (range -1.0..1.0) to S16_LE PCM bytes.
fn f32_to_s16le(samples: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(samples.len() * 2);
    for &s in samples {
        let clamped = s.clamp(-1.0, 1.0);
        let i16_val = (clamped * 32767.0) as i16;
        out.extend_from_slice(&i16_val.to_le_bytes());
    }
    out
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

    #[test]
    fn test_stereo_to_mono_passthrough() {
        let samples = vec![0.5, -0.3, 0.8];
        let result = stereo_to_mono(&samples, 1);
        assert_eq!(result, samples);
    }

    #[test]
    fn test_stereo_to_mono_two_channels() {
        let samples = vec![0.4, 0.6, -0.2, 0.8];
        let result = stereo_to_mono(&samples, 2);
        assert_eq!(result.len(), 2);
        assert!((result[0] - 0.5).abs() < 1e-6);
        assert!((result[1] - 0.3).abs() < 1e-6);
    }

    #[test]
    fn test_resample_same_rate() {
        let samples = vec![0.1, 0.2, 0.3, 0.4];
        let result = resample(&samples, 48000, 48000);
        assert_eq!(result, samples);
    }

    #[test]
    fn test_resample_downsample() {
        // 96kHz → 48kHz should roughly halve the number of samples
        let samples: Vec<f32> = (0..960).map(|i| (i as f32) / 960.0).collect();
        let result = resample(&samples, 96000, 48000);
        assert!((result.len() as f64 - 480.0).abs() <= 1.0);
    }

    #[test]
    fn test_resample_upsample() {
        // 24kHz → 48kHz should roughly double the number of samples
        let samples: Vec<f32> = (0..240).map(|i| (i as f32) / 240.0).collect();
        let result = resample(&samples, 24000, 48000);
        assert!((result.len() as f64 - 480.0).abs() <= 1.0);
    }

    #[test]
    fn test_f32_to_s16le() {
        let samples = vec![0.0, 1.0, -1.0, 0.5];
        let pcm = f32_to_s16le(&samples);
        assert_eq!(pcm.len(), 8); // 4 samples * 2 bytes

        // 0.0 → 0
        assert_eq!(i16::from_le_bytes([pcm[0], pcm[1]]), 0);
        // 1.0 → 32767
        assert_eq!(i16::from_le_bytes([pcm[2], pcm[3]]), 32767);
        // -1.0 → -32767
        assert_eq!(i16::from_le_bytes([pcm[4], pcm[5]]), -32767);
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
            let buffer = tokio::time::timeout(Duration::from_millis(100), rx.recv())
                .await
                .unwrap()
                .unwrap();

            // 960 samples * 2 bytes = 1920
            assert_eq!(buffer.len(), 1920);
        }
    }
}
