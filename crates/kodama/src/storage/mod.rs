//! Storage module for persisting video recordings
//!
//! Provides storage backends for saving and retrieving video frames:
//! - Local filesystem storage
//! - Cloud storage (S3/R2)

pub mod cloud;
pub mod local;

use crate::{Frame, SourceId};
use anyhow::Result;

/// Storage backend trait
#[async_trait::async_trait]
pub trait StorageBackend: Send + Sync {
    /// Store a frame
    async fn store_frame(&self, frame: &Frame) -> Result<()>;

    /// Retrieve frames for a source within a time range
    async fn get_frames(
        &self,
        source: SourceId,
        start_time_us: u64,
        end_time_us: u64,
    ) -> Result<Vec<Frame>>;

    /// Delete frames older than the given timestamp
    async fn cleanup(&self, before_timestamp_us: u64) -> Result<u64>;

    /// Get storage usage in bytes
    async fn usage_bytes(&self) -> Result<u64>;

    /// Get available capacity in bytes (None if unlimited)
    async fn available_bytes(&self) -> Result<Option<u64>>;
}

pub use cloud::{CloudStorage, CloudStorageConfig};
pub use local::{LocalStorage, LocalStorageConfig};
