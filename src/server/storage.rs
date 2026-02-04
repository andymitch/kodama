//! Storage management for server (stub)
//!
//! Future: Manages storage of video recordings to local or cloud storage.

use anyhow::Result;
use crate::core::{Frame, SourceId};

/// Storage backend trait
#[async_trait::async_trait]
pub trait StorageBackend: Send + Sync {
    /// Store a frame
    async fn store_frame(&self, frame: &Frame) -> Result<()>;

    /// Retrieve frames for a source within a time range
    async fn get_frames(
        &self,
        source: SourceId,
        start_time: u64,
        end_time: u64,
    ) -> Result<Vec<Frame>>;

    /// Delete frames older than the given timestamp
    async fn cleanup(&self, before_timestamp: u64) -> Result<u64>;

    /// Get storage usage in bytes
    async fn usage_bytes(&self) -> Result<u64>;
}

/// Storage configuration
#[derive(Debug, Clone)]
pub struct StorageConfig {
    /// Maximum storage size in bytes (0 = unlimited)
    pub max_size_bytes: u64,
    /// Retention period in seconds (0 = unlimited)
    pub retention_secs: u64,
    /// Whether to store all frames or just keyframes
    pub keyframes_only: bool,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            max_size_bytes: 10 * 1024 * 1024 * 1024, // 10 GB
            retention_secs: 7 * 24 * 60 * 60,         // 7 days
            keyframes_only: false,
        }
    }
}

/// Manages storage of video recordings
pub struct StorageManager {
    _config: StorageConfig,
    // TODO: backend: Box<dyn StorageBackend>,
}

impl StorageManager {
    /// Create a new storage manager
    pub fn new(_config: StorageConfig) -> Self {
        // TODO: Initialize storage backend
        Self { _config }
    }

    /// Store a frame
    pub async fn store(&self, _frame: &Frame) -> Result<()> {
        // TODO: Implement storage
        Ok(())
    }

    /// Start the storage cleanup task
    pub fn start_cleanup_task(&self) {
        // TODO: Spawn periodic cleanup task
    }
}
