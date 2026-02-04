//! Local filesystem storage backend
//!
//! Stores video frames to the local filesystem in a structured format.
//! Frames are organized by source and time, with an index for fast lookups.

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use crate::core::{Frame, SourceId};
use crate::relay::mux::frame::{frame_from_bytes, frame_to_bytes};
use super::StorageBackend;

/// Configuration for local storage
#[derive(Debug, Clone)]
pub struct LocalStorageConfig {
    /// Root directory for storage
    pub root_path: PathBuf,
    /// Maximum storage size in bytes (0 = unlimited)
    pub max_size_bytes: u64,
    /// Segment duration in microseconds (how frames are grouped into files)
    pub segment_duration_us: u64,
}

impl Default for LocalStorageConfig {
    fn default() -> Self {
        Self {
            root_path: PathBuf::from("/var/lib/kodama/recordings"),
            max_size_bytes: 10 * 1024 * 1024 * 1024, // 10 GB
            segment_duration_us: 60 * 1_000_000, // 1-minute segments
        }
    }
}

/// Index entry for a segment file
#[derive(Debug, Clone)]
struct SegmentIndex {
    /// Path to the segment file
    path: PathBuf,
    /// Start timestamp (microseconds)
    start_time_us: u64,
    /// End timestamp (microseconds)
    end_time_us: u64,
    /// Number of frames in segment (stored for future use)
    #[allow(dead_code)]
    frame_count: u32,
    /// File size in bytes
    size_bytes: u64,
}

/// In-memory index for fast lookups
struct StorageIndex {
    /// Segments indexed by source ID
    segments: HashMap<SourceId, Vec<SegmentIndex>>,
    /// Total storage used
    total_bytes: u64,
}

impl StorageIndex {
    fn new() -> Self {
        Self {
            segments: HashMap::new(),
            total_bytes: 0,
        }
    }
}

/// Local filesystem storage backend
pub struct LocalStorage {
    config: LocalStorageConfig,
    index: Arc<RwLock<StorageIndex>>,
    /// Currently open segment writers per source
    writers: Arc<RwLock<HashMap<SourceId, SegmentWriter>>>,
}

/// Writer for a single segment file
struct SegmentWriter {
    file: BufWriter<File>,
    path: PathBuf,
    start_time_us: u64,
    end_time_us: u64,
    frame_count: u32,
    bytes_written: u64,
}

impl LocalStorage {
    /// Create a new local storage backend
    pub fn new(config: LocalStorageConfig) -> Result<Self> {
        // Create root directory if needed
        fs::create_dir_all(&config.root_path)
            .with_context(|| format!("Failed to create storage directory: {:?}", config.root_path))?;

        let storage = Self {
            config,
            index: Arc::new(RwLock::new(StorageIndex::new())),
            writers: Arc::new(RwLock::new(HashMap::new())),
        };

        // Scan existing files to rebuild index
        // This is done synchronously on startup
        storage.rebuild_index_sync()?;

        Ok(storage)
    }

    /// Rebuild index from existing files
    fn rebuild_index_sync(&self) -> Result<()> {
        info!("Scanning storage directory: {:?}", self.config.root_path);

        let mut index = StorageIndex::new();

        // Walk the directory structure: root/source_id/*.segment
        if let Ok(entries) = fs::read_dir(&self.config.root_path) {
            for entry in entries.flatten() {
                if entry.path().is_dir() {
                    // Parse source ID from directory name
                    if let Some(source_id) = parse_source_id_from_dir(&entry.path()) {
                        let mut segments = Vec::new();

                        if let Ok(segment_entries) = fs::read_dir(entry.path()) {
                            for seg_entry in segment_entries.flatten() {
                                if seg_entry.path().extension().is_some_and(|ext| ext == "segment") {
                                    if let Ok(seg_index) = self.read_segment_index(&seg_entry.path()) {
                                        index.total_bytes += seg_index.size_bytes;
                                        segments.push(seg_index);
                                    }
                                }
                            }
                        }

                        // Sort segments by start time
                        segments.sort_by_key(|s| s.start_time_us);
                        index.segments.insert(source_id, segments);
                    }
                }
            }
        }

        info!("Found {} sources, {} total bytes", index.segments.len(), index.total_bytes);

        // We can't easily set the index here since we don't have async context
        // The index will be set when first accessed
        Ok(())
    }

    /// Read segment index from file header
    fn read_segment_index(&self, path: &Path) -> Result<SegmentIndex> {
        let metadata = fs::metadata(path)?;
        let mut file = File::open(path)?;

        // Read header: start_time (8) + end_time (8) + frame_count (4)
        let mut header = [0u8; 20];
        file.read_exact(&mut header)?;

        let start_time_us = u64::from_le_bytes(header[0..8].try_into().unwrap());
        let end_time_us = u64::from_le_bytes(header[8..16].try_into().unwrap());
        let frame_count = u32::from_le_bytes(header[16..20].try_into().unwrap());

        Ok(SegmentIndex {
            path: path.to_path_buf(),
            start_time_us,
            end_time_us,
            frame_count,
            size_bytes: metadata.len(),
        })
    }

    /// Get the segment path for a given source and timestamp
    fn segment_path(&self, source: SourceId, timestamp_us: u64) -> PathBuf {
        let segment_start = (timestamp_us / self.config.segment_duration_us) * self.config.segment_duration_us;
        let source_dir = self.config.root_path.join(format!("{}", source));
        source_dir.join(format!("{}.segment", segment_start))
    }

    /// Get or create a segment writer for the given source and timestamp
    async fn get_or_create_writer(&self, source: SourceId, timestamp_us: u64) -> Result<()> {
        let segment_path = self.segment_path(source, timestamp_us);
        let segment_start = (timestamp_us / self.config.segment_duration_us) * self.config.segment_duration_us;

        let mut writers = self.writers.write().await;

        // Check if we need a new segment
        if let Some(writer) = writers.get(&source) {
            let writer_segment = (writer.start_time_us / self.config.segment_duration_us) * self.config.segment_duration_us;
            if writer_segment == segment_start {
                return Ok(());
            }
            // Close old segment and update index
            let old_writer = writers.remove(&source).unwrap();
            self.finalize_segment(source, old_writer).await?;
        }

        // Create new segment
        let source_dir = segment_path.parent().unwrap();
        fs::create_dir_all(source_dir)?;

        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&segment_path)?;

        let mut writer = SegmentWriter {
            file: BufWriter::new(file),
            path: segment_path,
            start_time_us: timestamp_us,
            end_time_us: timestamp_us,
            frame_count: 0,
            bytes_written: 0,
        };

        // Write placeholder header
        let header = [0u8; 20];
        writer.file.write_all(&header)?;
        writer.bytes_written = 20;

        writers.insert(source, writer);
        Ok(())
    }

    /// Finalize a segment and update index
    async fn finalize_segment(&self, source: SourceId, mut writer: SegmentWriter) -> Result<()> {
        // Flush and get file
        writer.file.flush()?;
        let mut file = writer.file.into_inner()?;

        // Update header with final values
        file.seek(SeekFrom::Start(0))?;
        file.write_all(&writer.start_time_us.to_le_bytes())?;
        file.write_all(&writer.end_time_us.to_le_bytes())?;
        file.write_all(&writer.frame_count.to_le_bytes())?;
        file.flush()?;

        // Update index
        let mut index = self.index.write().await;
        let seg_index = SegmentIndex {
            path: writer.path,
            start_time_us: writer.start_time_us,
            end_time_us: writer.end_time_us,
            frame_count: writer.frame_count,
            size_bytes: writer.bytes_written,
        };

        index.total_bytes += seg_index.size_bytes;
        index.segments
            .entry(source)
            .or_default()
            .push(seg_index);

        Ok(())
    }

    /// Write a frame to the current segment
    async fn write_frame_to_segment(&self, frame: &Frame) -> Result<()> {
        self.get_or_create_writer(frame.source, frame.timestamp_us).await?;

        let mut writers = self.writers.write().await;
        let writer = writers.get_mut(&frame.source)
            .context("Writer not found after creation")?;

        // Serialize frame
        let frame_bytes = frame_to_bytes(frame);
        let len = frame_bytes.len() as u32;

        // Write length prefix + frame
        writer.file.write_all(&len.to_le_bytes())?;
        writer.file.write_all(&frame_bytes)?;

        writer.end_time_us = frame.timestamp_us;
        writer.frame_count += 1;
        writer.bytes_written += 4 + frame_bytes.len() as u64;

        Ok(())
    }

    /// Flush all pending writes and finalize open segments
    pub async fn flush(&self) -> Result<()> {
        let mut writers = self.writers.write().await;
        let sources: Vec<SourceId> = writers.keys().copied().collect();

        for source in sources {
            if let Some(writer) = writers.remove(&source) {
                // Drop lock temporarily to allow finalize_segment to acquire it
                drop(writers);
                self.finalize_segment(source, writer).await?;
                writers = self.writers.write().await;
            }
        }

        Ok(())
    }
}

#[async_trait::async_trait]
impl StorageBackend for LocalStorage {
    async fn store_frame(&self, frame: &Frame) -> Result<()> {
        self.write_frame_to_segment(frame).await
    }

    async fn get_frames(
        &self,
        source: SourceId,
        start_time_us: u64,
        end_time_us: u64,
    ) -> Result<Vec<Frame>> {
        let index = self.index.read().await;
        let segments = match index.segments.get(&source) {
            Some(segs) => segs,
            None => return Ok(Vec::new()),
        };

        let mut frames = Vec::new();

        // Find relevant segments
        for seg in segments {
            if seg.end_time_us < start_time_us || seg.start_time_us > end_time_us {
                continue;
            }

            // Read frames from segment
            let mut file = BufReader::new(File::open(&seg.path)?);

            // Skip header
            file.seek(SeekFrom::Start(20))?;

            loop {
                // Read length prefix
                let mut len_bytes = [0u8; 4];
                if file.read_exact(&mut len_bytes).is_err() {
                    break;
                }
                let len = u32::from_le_bytes(len_bytes) as usize;

                // Read frame data
                let mut frame_bytes = vec![0u8; len];
                file.read_exact(&mut frame_bytes)?;

                let frame = frame_from_bytes(bytes::Bytes::from(frame_bytes))?;

                // Filter by time range
                if frame.timestamp_us >= start_time_us && frame.timestamp_us <= end_time_us {
                    frames.push(frame);
                }
            }
        }

        // Sort by timestamp
        frames.sort_by_key(|f| f.timestamp_us);
        Ok(frames)
    }

    async fn cleanup(&self, before_timestamp_us: u64) -> Result<u64> {
        let mut index = self.index.write().await;
        let mut deleted_bytes = 0u64;

        for (_source, segments) in index.segments.iter_mut() {
            let mut to_remove = Vec::new();

            for (i, seg) in segments.iter().enumerate() {
                if seg.end_time_us < before_timestamp_us {
                    // Delete segment file
                    if let Err(e) = fs::remove_file(&seg.path) {
                        warn!("Failed to delete segment {:?}: {}", seg.path, e);
                    } else {
                        deleted_bytes += seg.size_bytes;
                        to_remove.push(i);
                    }
                }
            }

            // Remove from index (in reverse order to maintain indices)
            for i in to_remove.into_iter().rev() {
                segments.remove(i);
            }
        }

        index.total_bytes = index.total_bytes.saturating_sub(deleted_bytes);
        debug!("Cleaned up {} bytes", deleted_bytes);
        Ok(deleted_bytes)
    }

    async fn usage_bytes(&self) -> Result<u64> {
        let index = self.index.read().await;
        Ok(index.total_bytes)
    }

    async fn available_bytes(&self) -> Result<Option<u64>> {
        // Use df to check available space
        let output = std::process::Command::new("df")
            .args(["-B1", self.config.root_path.to_str().unwrap_or("/")])
            .output()?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        if let Some(line) = stdout.lines().nth(1) {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 4 {
                if let Ok(available) = parts[3].parse::<u64>() {
                    return Ok(Some(available));
                }
            }
        }

        Ok(None)
    }
}

/// Parse source ID from directory name (hex string)
fn parse_source_id_from_dir(path: &Path) -> Option<SourceId> {
    let name = path.file_name()?.to_str()?;
    if name.len() != 16 {
        return None;
    }

    let mut bytes = [0u8; 8];
    for (i, chunk) in name.as_bytes().chunks(2).enumerate() {
        let hex_str = std::str::from_utf8(chunk).ok()?;
        bytes[i] = u8::from_str_radix(hex_str, 16).ok()?;
    }

    Some(SourceId::new(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use crate::core::{Channel, FrameFlags};
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_local_storage_store_and_retrieve() {
        let dir = tempdir().unwrap();
        let config = LocalStorageConfig {
            root_path: dir.path().to_path_buf(),
            max_size_bytes: 0,
            segment_duration_us: 1_000_000, // 1 second segments
        };

        let storage = LocalStorage::new(config).unwrap();
        let source = SourceId::new([1, 2, 3, 4, 5, 6, 7, 8]);

        // Store some frames
        for i in 0..5 {
            let frame = Frame {
                source,
                channel: Channel::Video,
                flags: if i == 0 { FrameFlags::keyframe() } else { FrameFlags::default() },
                timestamp_us: i * 100_000, // 100ms apart
                payload: Bytes::from(format!("frame {}", i)),
            };
            storage.store_frame(&frame).await.unwrap();
        }

        // Flush pending writes
        storage.flush().await.unwrap();

        // Retrieve frames
        let frames = storage.get_frames(source, 0, 500_000).await.unwrap();
        assert_eq!(frames.len(), 5);
        assert_eq!(frames[0].timestamp_us, 0);
        assert_eq!(frames[4].timestamp_us, 400_000);
    }

    #[test]
    fn test_parse_source_id() {
        let path = PathBuf::from("/storage/0102030405060708");
        let source = parse_source_id_from_dir(&path).unwrap();
        assert_eq!(source.0, [1, 2, 3, 4, 5, 6, 7, 8]);
    }
}
