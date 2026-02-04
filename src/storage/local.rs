//! Local filesystem storage backend (stub)
//!
//! Stores video frames to the local filesystem in a structured format.

use anyhow::Result;
use std::path::PathBuf;

use crate::core::{Frame, SourceId};
use super::StorageBackend;

/// Configuration for local storage
#[derive(Debug, Clone)]
pub struct LocalStorageConfig {
    /// Root directory for storage
    pub root_path: PathBuf,
    /// Maximum storage size in bytes (0 = unlimited)
    pub max_size_bytes: u64,
    /// Segment duration in seconds (how frames are grouped into files)
    pub segment_duration_secs: u32,
}

impl Default for LocalStorageConfig {
    fn default() -> Self {
        Self {
            root_path: PathBuf::from("/var/lib/yurei/recordings"),
            max_size_bytes: 10 * 1024 * 1024 * 1024, // 10 GB
            segment_duration_secs: 60, // 1-minute segments
        }
    }
}

/// Local filesystem storage backend
pub struct LocalStorage {
    _config: LocalStorageConfig,
}

impl LocalStorage {
    /// Create a new local storage backend
    pub fn new(config: LocalStorageConfig) -> Result<Self> {
        // TODO: Create directories if needed
        // TODO: Initialize index
        Ok(Self { _config: config })
    }

    /// Get the path for a segment file
    fn _segment_path(&self, _source: SourceId, _timestamp_us: u64) -> PathBuf {
        // TODO: Implement path generation
        // Format: {root}/{source_id}/{date}/{hour}/{minute}.mp4
        PathBuf::new()
    }
}

#[async_trait::async_trait]
impl StorageBackend for LocalStorage {
    async fn store_frame(&self, _frame: &Frame) -> Result<()> {
        // TODO: Implement frame storage
        // - Determine which segment file the frame belongs to
        // - Append frame to segment (or create new segment)
        // - Update index
        Ok(())
    }

    async fn get_frames(
        &self,
        _source: SourceId,
        _start_time_us: u64,
        _end_time_us: u64,
    ) -> Result<Vec<Frame>> {
        // TODO: Implement frame retrieval
        // - Find relevant segment files
        // - Read and decode frames within time range
        Ok(Vec::new())
    }

    async fn cleanup(&self, _before_timestamp_us: u64) -> Result<u64> {
        // TODO: Implement cleanup
        // - Find segment files older than timestamp
        // - Delete files and update index
        Ok(0)
    }

    async fn usage_bytes(&self) -> Result<u64> {
        // TODO: Calculate actual disk usage
        Ok(0)
    }

    async fn available_bytes(&self) -> Result<Option<u64>> {
        // TODO: Check filesystem available space
        Ok(None)
    }
}
