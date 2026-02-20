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

use super::StorageBackend;
use crate::transport::mux::frame::{frame_from_bytes, frame_to_bytes};
use crate::{Frame, SourceId};

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
            segment_duration_us: 60 * 1_000_000,     // 1-minute segments
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
        fs::create_dir_all(&config.root_path).with_context(|| {
            format!("Failed to create storage directory: {:?}", config.root_path)
        })?;

        // Scan existing files to rebuild index
        let index = Self::rebuild_index_sync(&config)?;

        let storage = Self {
            config,
            index: Arc::new(RwLock::new(index)),
            writers: Arc::new(RwLock::new(HashMap::new())),
        };

        Ok(storage)
    }

    /// Rebuild index from existing files
    fn rebuild_index_sync(config: &LocalStorageConfig) -> Result<StorageIndex> {
        info!("Scanning storage directory: {:?}", config.root_path);

        let mut index = StorageIndex::new();

        // Walk the directory structure: root/source_id/*.segment
        if let Ok(entries) = fs::read_dir(&config.root_path) {
            for entry in entries.flatten() {
                if entry.path().is_dir() {
                    // Parse source ID from directory name
                    if let Some(source_id) = parse_source_id_from_dir(&entry.path()) {
                        let mut segments = Vec::new();

                        if let Ok(segment_entries) = fs::read_dir(entry.path()) {
                            for seg_entry in segment_entries.flatten() {
                                if seg_entry
                                    .path()
                                    .extension()
                                    .is_some_and(|ext| ext == "segment")
                                {
                                    if let Ok(seg_index) = read_segment_index(&seg_entry.path()) {
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

        info!(
            "Found {} sources, {} total bytes",
            index.segments.len(),
            index.total_bytes
        );

        Ok(index)
    }

    /// Get the segment path for a given source and timestamp
    fn segment_path(&self, source: SourceId, timestamp_us: u64) -> PathBuf {
        let segment_start =
            (timestamp_us / self.config.segment_duration_us) * self.config.segment_duration_us;
        let source_dir = self.config.root_path.join(format!("{}", source));
        source_dir.join(format!("{}.segment", segment_start))
    }

    /// Get or create a segment writer for the given source and timestamp
    async fn get_or_create_writer(&self, source: SourceId, timestamp_us: u64) -> Result<()> {
        let segment_path = self.segment_path(source, timestamp_us);
        let segment_start =
            (timestamp_us / self.config.segment_duration_us) * self.config.segment_duration_us;

        let mut writers = self.writers.write().await;

        // Check if we need a new segment
        if let Some(writer) = writers.get(&source) {
            let writer_segment = (writer.start_time_us / self.config.segment_duration_us)
                * self.config.segment_duration_us;
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
        file.sync_data()?;

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
        index.segments.entry(source).or_default().push(seg_index);

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
        self.get_or_create_writer(frame.source, frame.timestamp_us)
            .await?;

        let mut writers = self.writers.write().await;
        let writer = writers
            .get_mut(&frame.source)
            .context("Writer not found after creation")?;

        // Serialize frame data in async context
        let frame_bytes = frame_to_bytes(frame);
        let len = frame_bytes.len() as u32;

        // Perform the blocking file write via block_in_place
        tokio::task::block_in_place(|| {
            writer.file.write_all(&len.to_le_bytes())?;
            writer.file.write_all(&frame_bytes)?;
            Ok::<(), anyhow::Error>(())
        })?;

        writer.end_time_us = frame.timestamp_us;
        writer.frame_count += 1;
        writer.bytes_written += 4 + frame_bytes.len() as u64;

        Ok(())
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
        #[cfg(unix)]
        {
            let path = self.config.root_path.clone();
            let result = tokio::task::spawn_blocking(move || {
                use std::ffi::CString;
                let c_path = CString::new(path.to_str().unwrap_or("/"))
                    .map_err(|e| anyhow::anyhow!("Invalid path: {}", e))?;
                unsafe {
                    let mut stat: libc::statvfs = std::mem::zeroed();
                    if libc::statvfs(c_path.as_ptr(), &mut stat) != 0 {
                        anyhow::bail!("statvfs failed: {}", std::io::Error::last_os_error());
                    }
                    #[allow(clippy::unnecessary_cast)]
                    let available = stat.f_bavail as u64 * stat.f_frsize as u64;
                    Ok(Some(available))
                }
            })
            .await??;
            Ok(result)
        }
        #[cfg(not(unix))]
        {
            Ok(None)
        }
    }
}

/// Read segment index from file header
fn read_segment_index(path: &Path) -> Result<SegmentIndex> {
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
    use crate::{Channel, FrameFlags};
    use bytes::Bytes;
    use tempfile::tempdir;

    #[tokio::test(flavor = "multi_thread")]
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
                flags: if i == 0 {
                    FrameFlags::keyframe()
                } else {
                    FrameFlags::default()
                },
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

    #[test]
    fn test_parse_source_id_rejects_wrong_length() {
        assert!(parse_source_id_from_dir(&PathBuf::from("/storage/0102")).is_none());
        assert!(
            parse_source_id_from_dir(&PathBuf::from("/storage/01020304050607080910")).is_none()
        );
    }

    #[test]
    fn test_parse_source_id_rejects_non_hex() {
        assert!(parse_source_id_from_dir(&PathBuf::from("/storage/ghijklmnopqrstuv")).is_none());
    }

    fn make_frame(source: SourceId, timestamp_us: u64, data: &str) -> Frame {
        Frame {
            source,
            channel: Channel::Video,
            flags: FrameFlags::default(),
            timestamp_us,
            payload: Bytes::from(data.to_string()),
        }
    }

    fn test_config(dir: &Path) -> LocalStorageConfig {
        LocalStorageConfig {
            root_path: dir.to_path_buf(),
            max_size_bytes: 0,
            segment_duration_us: 1_000_000, // 1-second segments
        }
    }

    // ========== Cleanup cycle ==========

    #[tokio::test(flavor = "multi_thread")]
    async fn cleanup_deletes_old_segments() {
        let dir = tempdir().unwrap();
        let storage = LocalStorage::new(test_config(dir.path())).unwrap();
        let source = SourceId::new([1, 2, 3, 4, 5, 6, 7, 8]);

        // Store frames in two different segments (1s apart)
        storage
            .store_frame(&make_frame(source, 100_000, "old"))
            .await
            .unwrap();
        storage
            .store_frame(&make_frame(source, 1_500_000, "new"))
            .await
            .unwrap();
        storage.flush().await.unwrap();

        let usage_before = storage.usage_bytes().await.unwrap();
        assert!(usage_before > 0);

        // Cleanup everything before the second segment
        let deleted = storage.cleanup(1_000_000).await.unwrap();
        assert!(deleted > 0);

        // Only the second segment should remain
        let frames = storage.get_frames(source, 0, 2_000_000).await.unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].timestamp_us, 1_500_000);

        let usage_after = storage.usage_bytes().await.unwrap();
        assert!(usage_after < usage_before);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cleanup_with_nothing_to_delete() {
        let dir = tempdir().unwrap();
        let storage = LocalStorage::new(test_config(dir.path())).unwrap();
        let source = SourceId::new([1, 2, 3, 4, 5, 6, 7, 8]);

        storage
            .store_frame(&make_frame(source, 1_000_000, "recent"))
            .await
            .unwrap();
        storage.flush().await.unwrap();

        // Cleanup before the earliest frame — nothing to delete
        let deleted = storage.cleanup(500_000).await.unwrap();
        assert_eq!(deleted, 0);

        let frames = storage.get_frames(source, 0, 2_000_000).await.unwrap();
        assert_eq!(frames.len(), 1);
    }

    // ========== Index rebuild on startup ==========

    #[tokio::test(flavor = "multi_thread")]
    async fn index_rebuilds_from_existing_files() {
        let dir = tempdir().unwrap();
        let source = SourceId::new([1, 2, 3, 4, 5, 6, 7, 8]);

        // Phase 1: write data and drop storage
        {
            let storage = LocalStorage::new(test_config(dir.path())).unwrap();
            storage
                .store_frame(&make_frame(source, 100_000, "frame1"))
                .await
                .unwrap();
            storage
                .store_frame(&make_frame(source, 200_000, "frame2"))
                .await
                .unwrap();
            storage.flush().await.unwrap();
        }

        // Phase 2: create new storage instance — should rebuild index from disk
        let storage = LocalStorage::new(test_config(dir.path())).unwrap();

        let usage = storage.usage_bytes().await.unwrap();
        assert!(usage > 0, "Index should have been rebuilt from disk");

        let frames = storage.get_frames(source, 0, 500_000).await.unwrap();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].timestamp_us, 100_000);
        assert_eq!(frames[1].timestamp_us, 200_000);
    }

    // ========== Multiple sources ==========

    #[tokio::test(flavor = "multi_thread")]
    async fn concurrent_writes_to_different_sources() {
        let dir = tempdir().unwrap();
        let storage = LocalStorage::new(test_config(dir.path())).unwrap();
        let source1 = SourceId::new([1, 0, 0, 0, 0, 0, 0, 0]);
        let source2 = SourceId::new([2, 0, 0, 0, 0, 0, 0, 0]);

        // Interleave writes from two sources
        storage
            .store_frame(&make_frame(source1, 100_000, "s1-f1"))
            .await
            .unwrap();
        storage
            .store_frame(&make_frame(source2, 100_000, "s2-f1"))
            .await
            .unwrap();
        storage
            .store_frame(&make_frame(source1, 200_000, "s1-f2"))
            .await
            .unwrap();
        storage
            .store_frame(&make_frame(source2, 200_000, "s2-f2"))
            .await
            .unwrap();
        storage.flush().await.unwrap();

        let frames1 = storage.get_frames(source1, 0, 500_000).await.unwrap();
        let frames2 = storage.get_frames(source2, 0, 500_000).await.unwrap();

        assert_eq!(frames1.len(), 2);
        assert_eq!(frames2.len(), 2);
        assert_eq!(frames1[0].payload, Bytes::from("s1-f1"));
        assert_eq!(frames2[0].payload, Bytes::from("s2-f1"));
    }

    // ========== Segment boundaries ==========

    #[tokio::test(flavor = "multi_thread")]
    async fn frames_spanning_multiple_segments() {
        let dir = tempdir().unwrap();
        let storage = LocalStorage::new(test_config(dir.path())).unwrap();
        let source = SourceId::new([1, 2, 3, 4, 5, 6, 7, 8]);

        // Write frames across 3 segments (1s each)
        for i in 0..3 {
            let timestamp = i * 1_000_000 + 500_000; // 0.5s, 1.5s, 2.5s
            storage
                .store_frame(&make_frame(source, timestamp, &format!("seg{}", i)))
                .await
                .unwrap();
        }
        storage.flush().await.unwrap();

        // Query full range
        let all = storage.get_frames(source, 0, 3_000_000).await.unwrap();
        assert_eq!(all.len(), 3);

        // Query partial range (only middle segment)
        let mid = storage
            .get_frames(source, 1_000_000, 2_000_000)
            .await
            .unwrap();
        assert_eq!(mid.len(), 1);
        assert_eq!(mid[0].timestamp_us, 1_500_000);
    }

    // ========== Edge cases ==========

    #[tokio::test(flavor = "multi_thread")]
    async fn get_frames_unknown_source_returns_empty() {
        let dir = tempdir().unwrap();
        let storage = LocalStorage::new(test_config(dir.path())).unwrap();
        let unknown = SourceId::new([99, 99, 99, 99, 99, 99, 99, 99]);

        let frames = storage.get_frames(unknown, 0, u64::MAX).await.unwrap();
        assert!(frames.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn empty_storage_reports_zero_usage() {
        let dir = tempdir().unwrap();
        let storage = LocalStorage::new(test_config(dir.path())).unwrap();

        assert_eq!(storage.usage_bytes().await.unwrap(), 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn available_bytes_returns_some() {
        let dir = tempdir().unwrap();
        let storage = LocalStorage::new(test_config(dir.path())).unwrap();

        let available = storage.available_bytes().await.unwrap();
        assert!(available.is_some());
        assert!(available.unwrap() > 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cleanup_updates_total_bytes_correctly() {
        let dir = tempdir().unwrap();
        let storage = LocalStorage::new(test_config(dir.path())).unwrap();
        let source = SourceId::new([1, 2, 3, 4, 5, 6, 7, 8]);

        storage
            .store_frame(&make_frame(source, 100_000, "data"))
            .await
            .unwrap();
        storage.flush().await.unwrap();

        let before = storage.usage_bytes().await.unwrap();
        assert!(before > 0);

        let deleted = storage.cleanup(u64::MAX).await.unwrap();
        assert_eq!(deleted, before);

        let after = storage.usage_bytes().await.unwrap();
        assert_eq!(after, 0);
    }
}
