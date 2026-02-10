//! fMP4 muxer using FFmpeg subprocess
//!
//! Converts H.264 Annex B to fMP4 using FFmpeg — battle-tested and reliable.

use anyhow::{Context, Result};
use bytes::Bytes;
use std::io::{BufReader, BufWriter, Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

/// Result of muxing a frame
pub struct MuxResult {
    /// Init segment (ftyp+moov) — only present first time
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
    needs_keyframe: bool,
    pending_init: Vec<u8>,
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
            needs_keyframe: false,
            pending_init: Vec::new(),
        }
    }

    fn start_ffmpeg(&mut self) -> Result<()> {
        let mut child = Command::new("ffmpeg")
            .args([
                "-f", "h264",
                "-probesize", "32",
                "-analyzeduration", "0",
                "-fflags", "+genpts",
                "-i", "pipe:0",
                "-c:v", "copy",
                "-f", "mp4",
                "-avoid_negative_ts", "make_zero",
                "-start_at_zero",
                "-movflags",
                "frag_keyframe+empty_moov+default_base_moof+frag_every_frame+dash",
                "-fflags", "+flush_packets+nobuffer",
                "-flush_packets", "1",
                "-loglevel", "error",
                "pipe:1",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("Failed to spawn ffmpeg. Is it installed?")?;

        let stdin = child.stdin.take().context("Failed to get ffmpeg stdin")?;
        let stdout = child.stdout.take().context("Failed to get ffmpeg stdout")?;
        let stderr = child.stderr.take();

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

        // Stderr drain thread
        if let Some(stderr) = stderr {
            thread::spawn(move || {
                let mut reader = BufReader::new(stderr);
                let mut buffer = vec![0u8; 4096];
                loop {
                    match reader.read(&mut buffer) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            let msg = String::from_utf8_lossy(&buffer[..n]);
                            for line in msg.lines() {
                                if !line.is_empty() {
                                    tracing::warn!("FFmpeg stderr: {}", line);
                                }
                            }
                        }
                    }
                }
            });
        }

        // Stdout reader thread
        let (stdout_tx, stdout_rx) = mpsc::channel::<Bytes>();
        thread::spawn(move || {
            let mut reader = BufReader::with_capacity(4096, stdout);
            let mut buffer = vec![0u8; 8192];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => break,
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

    /// Kill the FFmpeg process and reset state.
    pub fn reset(&mut self) {
        if let Some(mut child) = self.ffmpeg.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.stdin_tx = None;
        self.stdout_rx = None;
        self.init_sent = false;
        self.needs_keyframe = true;
        self.pending_init.clear();
        tracing::info!("FFmpeg muxer reset");
    }

    pub fn mux_frame(&mut self, payload: &[u8], is_keyframe: bool, _timestamp_us: u64) -> MuxResult {
        let mut result = MuxResult {
            init_segment: None,
            codec: Some("avc1.640028".to_string()),
            width: Some(self.width),
            height: Some(self.height),
            media_segment: None,
        };

        if self.needs_keyframe {
            if !is_keyframe {
                return result;
            }
            tracing::info!("FFmpeg got keyframe after reset, starting new muxer");
            self.needs_keyframe = false;
        }

        if self.ffmpeg.is_none() {
            if let Err(e) = self.start_ffmpeg() {
                tracing::error!("Failed to start ffmpeg: {}", e);
                return result;
            }
        }

        if let Some(ref tx) = self.stdin_tx {
            if tx.send(payload.to_vec()).is_err() {
                tracing::warn!("Failed to send data to ffmpeg");
            }
        }

        // Check if FFmpeg process has exited
        if let Some(ref mut child) = self.ffmpeg {
            match child.try_wait() {
                Ok(Some(status)) => {
                    tracing::warn!("FFmpeg process exited with status: {}", status);
                    self.ffmpeg = None;
                    self.stdin_tx = None;
                }
                Ok(None) => {}
                Err(e) => tracing::warn!("Failed to check FFmpeg status: {}", e),
            }
        }

        // Read all available fMP4 output from ffmpeg
        if let Some(ref rx) = self.stdout_rx {
            if !self.init_sent {
                while let Ok(data) = rx.try_recv() {
                    self.pending_init.extend_from_slice(&data);
                }

                if !self.pending_init.is_empty() {
                    if let Some(moov_end) = find_box_end(&self.pending_init, b"moov") {
                        result.init_segment = Some(self.pending_init[..moov_end].to_vec());
                        self.init_sent = true;
                        tracing::info!("FFmpeg init segment: {} bytes", moov_end);

                        if moov_end < self.pending_init.len() {
                            tracing::info!(
                                "Discarding {} bytes of initial media data",
                                self.pending_init.len() - moov_end
                            );
                        }
                        self.pending_init.clear();
                    }
                }
            } else {
                let mut combined = Vec::new();
                loop {
                    match rx.try_recv() {
                        Ok(data) => combined.extend_from_slice(&data),
                        Err(mpsc::TryRecvError::Empty) => break,
                        Err(mpsc::TryRecvError::Disconnected) => {
                            tracing::warn!("FFmpeg stdout disconnected");
                            self.ffmpeg = None;
                            self.stdin_tx = None;
                            self.stdout_rx = None;
                            self.init_sent = false;
                            self.needs_keyframe = true;
                            break;
                        }
                    }
                }
                if !combined.is_empty() {
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
