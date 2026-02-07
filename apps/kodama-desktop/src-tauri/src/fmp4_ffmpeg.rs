//! fMP4 muxer using FFmpeg subprocess
//!
//! Converts H.264 Annex B to fMP4 using FFmpeg - battle-tested and reliable.
//! This replaces our custom muxer with a proven solution.

use anyhow::{Context, Result};
use bytes::Bytes;
use std::io::{BufReader, BufWriter, Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

/// Result of muxing a frame
pub struct MuxResult {
    /// Init segment (ftyp+moov) - only present first time
    pub init_segment: Option<Vec<u8>>,
    /// Codec string
    pub codec: Option<String>,
    /// Video width
    pub width: Option<u32>,
    /// Video height
    pub height: Option<u32>,
    /// Media segment (moof+mdat)
    pub media_segment: Option<Vec<u8>>,
}

/// FFmpeg-based fMP4 muxer
pub struct Fmp4Muxer {
    width: u32,
    height: u32,
    ffmpeg: Option<Child>,
    stdin_tx: Option<Sender<Vec<u8>>>,
    stdout_rx: Option<Receiver<Bytes>>,
    init_sent: bool,
}

impl Fmp4Muxer {
    pub fn new() -> Self {
        Self {
            width: 1280,
            height: 720,
            ffmpeg: None,
            stdin_tx: None,
            stdout_rx: None,
            init_sent: false,
        }
    }

    fn start_ffmpeg(&mut self) -> Result<()> {
        let mut child = Command::new("ffmpeg")
            .args(&[
                "-f", "h264",           // Input format: raw H.264
                "-probesize", "32",     // Minimal probing
                "-analyzeduration", "0", // Skip analysis
                "-fflags", "+genpts",   // Generate presentation timestamps
                "-i", "pipe:0",         // Read from stdin
                "-c:v", "copy",         // Copy video (no re-encode)
                "-f", "mp4",            // Output format: MP4
                "-avoid_negative_ts", "make_zero", // Start timestamps at 0
                "-start_at_zero",       // Force zero start
                "-movflags",
                "frag_keyframe+empty_moov+default_base_moof+frag_every_frame+dash",
                "-fflags", "+flush_packets+nobuffer", // Zero buffering
                "-flush_packets", "1",  // Flush after each packet
                "-loglevel", "error",   // Quiet unless error
                "pipe:1",               // Write to stdout
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("Failed to spawn ffmpeg. Is it installed?")?;

        let stdin = child.stdin.take().context("Failed to get ffmpeg stdin")?;
        let stdout = child.stdout.take().context("Failed to get ffmpeg stdout")?;

        // Stdin writer thread
        let (stdin_tx, stdin_rx) = mpsc::channel::<Vec<u8>>();
        thread::spawn(move || {
            let mut writer = BufWriter::new(stdin);
            while let Ok(data) = stdin_rx.recv() {
                if writer.write_all(&data).is_err() || writer.flush().is_err() {
                    break;
                }
            }
        });

        // Stdout reader thread with minimal buffering
        let (stdout_tx, stdout_rx) = mpsc::channel::<Bytes>();
        thread::spawn(move || {
            let mut reader = BufReader::with_capacity(4096, stdout); // Small buffer for low latency
            let mut buffer = vec![0u8; 8192]; // Small read chunks
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => break, // EOF
                    Ok(n) => {
                        if stdout_tx.send(Bytes::copy_from_slice(&buffer[..n])).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        self.ffmpeg = Some(child);
        self.stdin_tx = Some(stdin_tx);
        self.stdout_rx = Some(stdout_rx);
        tracing::info!("FFmpeg muxer started for {}x{}", self.width, self.height);

        Ok(())
    }

    pub fn mux_frame(&mut self, payload: &[u8], _is_keyframe: bool, _timestamp_us: u64) -> MuxResult {
        let mut result = MuxResult {
            init_segment: None,
            codec: Some("avc1.640028".to_string()), // High profile placeholder
            width: Some(self.width),
            height: Some(self.height),
            media_segment: None,
        };

        // Start ffmpeg on first frame
        if self.ffmpeg.is_none() {
            if let Err(e) = self.start_ffmpeg() {
                tracing::error!("Failed to start ffmpeg: {}", e);
                return result;
            }
        }

        // Send H.264 data to ffmpeg
        if let Some(ref tx) = self.stdin_tx {
            if tx.send(payload.to_vec()).is_err() {
                tracing::warn!("Failed to send data to ffmpeg");
            }
        }

        // Read all available fMP4 output from ffmpeg
        if let Some(ref rx) = self.stdout_rx {
            let mut combined = Vec::new();
            // Drain all available data
            while let Ok(data) = rx.try_recv() {
                combined.extend_from_slice(&data);
            }

            if !combined.is_empty() {
                // Parse fMP4 boxes to separate init from media segments
                if !self.init_sent {
                    // Look for moov box end to split init segment
                    if let Some(moov_end) = find_box_end(&combined, b"moov") {
                        result.init_segment = Some(combined[..moov_end].to_vec());
                        self.init_sent = true;
                        tracing::info!("FFmpeg init segment: {} bytes", moov_end);

                        // Remaining data is media segment(s)
                        if moov_end < combined.len() {
                            result.media_segment = Some(combined[moov_end..].to_vec());
                        }
                    }
                } else {
                    // All data is media segments
                    result.media_segment = Some(combined);
                }
            }
        }

        result
    }
}

impl Drop for Fmp4Muxer {
    fn drop(&mut self) {
        if let Some(mut child) = self.ffmpeg.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Find the end offset of a box in MP4 data
fn find_box_end(data: &[u8], box_type: &[u8; 4]) -> Option<usize> {
    let mut pos = 0;
    while pos + 8 <= data.len() {
        let size = u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]) as usize;
        if size < 8 || pos + size > data.len() {
            return None;
        }
        if &data[pos + 4..pos + 8] == box_type {
            return Some(pos + size);
        }
        pos += size;
    }
    None
}
