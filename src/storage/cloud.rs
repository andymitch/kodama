//! Cloud storage backend (stub)
//!
//! Stores video frames to cloud object storage (S3/R2).

use anyhow::Result;

use crate::core::{Frame, SourceId};
use super::StorageBackend;

/// Configuration for cloud storage
#[derive(Debug, Clone)]
pub struct CloudStorageConfig {
    /// S3-compatible endpoint URL
    pub endpoint_url: String,
    /// Bucket name
    pub bucket: String,
    /// Access key ID
    pub access_key_id: String,
    /// Secret access key
    pub secret_access_key: String,
    /// Region
    pub region: String,
    /// Key prefix for all objects
    pub prefix: String,
}

impl Default for CloudStorageConfig {
    fn default() -> Self {
        Self {
            endpoint_url: String::new(),
            bucket: String::from("yurei-recordings"),
            access_key_id: String::new(),
            secret_access_key: String::new(),
            region: String::from("auto"),
            prefix: String::new(),
        }
    }
}

/// Cloud storage backend (S3/R2)
pub struct CloudStorage {
    _config: CloudStorageConfig,
}

impl CloudStorage {
    /// Create a new cloud storage backend
    pub fn new(config: CloudStorageConfig) -> Result<Self> {
        // TODO: Initialize S3 client
        // TODO: Verify bucket access
        Ok(Self { _config: config })
    }

    /// Get the object key for a segment
    fn _object_key(&self, _source: SourceId, _timestamp_us: u64) -> String {
        // TODO: Implement key generation
        // Format: {prefix}/{source_id}/{date}/{hour}/{minute}.mp4
        String::new()
    }
}

#[async_trait::async_trait]
impl StorageBackend for CloudStorage {
    async fn store_frame(&self, _frame: &Frame) -> Result<()> {
        // TODO: Implement frame storage
        // - Buffer frames locally
        // - Upload segments to S3 when complete
        Ok(())
    }

    async fn get_frames(
        &self,
        _source: SourceId,
        _start_time_us: u64,
        _end_time_us: u64,
    ) -> Result<Vec<Frame>> {
        // TODO: Implement frame retrieval
        // - List objects in time range
        // - Download and decode segments
        Ok(Vec::new())
    }

    async fn cleanup(&self, _before_timestamp_us: u64) -> Result<u64> {
        // TODO: Implement cleanup
        // - List objects older than timestamp
        // - Delete objects
        Ok(0)
    }

    async fn usage_bytes(&self) -> Result<u64> {
        // TODO: Query bucket usage
        Ok(0)
    }

    async fn available_bytes(&self) -> Result<Option<u64>> {
        // Cloud storage is effectively unlimited
        Ok(None)
    }
}
