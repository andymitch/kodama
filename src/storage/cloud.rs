//! Cloud storage backend
//!
//! Stores video frames to S3-compatible cloud object storage (AWS S3, Cloudflare R2, etc.).
//!
//! # Implementation Notes
//!
//! This module provides a cloud storage backend that buffers frames locally
//! and uploads them as segments to object storage. For a production deployment,
//! you would add an S3 client dependency like `aws-sdk-s3` or `rusoto_s3`.
//!
//! The current implementation uses the AWS CLI for uploads, which works for
//! development and small deployments. For production, replace with native SDK calls.

use anyhow::{Context, Result};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use crate::core::{Frame, SourceId};
use crate::relay::mux::frame::{frame_from_bytes, frame_to_bytes};
use super::StorageBackend;

/// Configuration for cloud storage
#[derive(Debug, Clone)]
pub struct CloudStorageConfig {
    /// S3-compatible endpoint URL (empty for AWS S3)
    pub endpoint_url: Option<String>,
    /// Bucket name
    pub bucket: String,
    /// AWS region
    pub region: String,
    /// Key prefix for all objects
    pub prefix: String,
    /// Local cache directory for buffering
    pub cache_dir: PathBuf,
    /// Segment duration in microseconds
    pub segment_duration_us: u64,
    /// Maximum local cache size in bytes
    pub max_cache_bytes: u64,
}

impl Default for CloudStorageConfig {
    fn default() -> Self {
        Self {
            endpoint_url: None,
            bucket: String::from("kodama-recordings"),
            region: String::from("us-east-1"),
            prefix: String::new(),
            cache_dir: PathBuf::from("/tmp/kodama-cache"),
            segment_duration_us: 60 * 1_000_000, // 1-minute segments
            max_cache_bytes: 1024 * 1024 * 1024, // 1 GB cache
        }
    }
}

impl CloudStorageConfig {
    /// Create config for Cloudflare R2
    pub fn r2(account_id: &str, bucket: &str) -> Self {
        Self {
            endpoint_url: Some(format!("https://{}.r2.cloudflarestorage.com", account_id)),
            bucket: bucket.to_string(),
            region: String::from("auto"),
            ..Default::default()
        }
    }

    /// Create config for AWS S3
    pub fn s3(bucket: &str, region: &str) -> Self {
        Self {
            endpoint_url: None,
            bucket: bucket.to_string(),
            region: region.to_string(),
            ..Default::default()
        }
    }
}

/// Segment being buffered locally
struct LocalSegment {
    file: BufWriter<File>,
    path: PathBuf,
    start_time_us: u64,
    end_time_us: u64,
    frame_count: u32,
    bytes_written: u64,
}

/// Index of uploaded segments
#[derive(Debug, Clone)]
struct SegmentInfo {
    key: String,
    start_time_us: u64,
    end_time_us: u64,
    #[allow(dead_code)]
    frame_count: u32,
    size_bytes: u64,
}

/// In-memory index
struct CloudIndex {
    segments: HashMap<SourceId, Vec<SegmentInfo>>,
    total_bytes: u64,
}

impl CloudIndex {
    fn new() -> Self {
        Self {
            segments: HashMap::new(),
            total_bytes: 0,
        }
    }
}

/// Cloud storage backend (S3/R2)
pub struct CloudStorage {
    config: CloudStorageConfig,
    index: Arc<RwLock<CloudIndex>>,
    local_segments: Arc<RwLock<HashMap<SourceId, LocalSegment>>>,
}

impl CloudStorage {
    /// Create a new cloud storage backend
    pub fn new(config: CloudStorageConfig) -> Result<Self> {
        // Create cache directory
        fs::create_dir_all(&config.cache_dir)
            .with_context(|| format!("Failed to create cache dir: {:?}", config.cache_dir))?;

        Ok(Self {
            config,
            index: Arc::new(RwLock::new(CloudIndex::new())),
            local_segments: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    /// Get the object key for a segment
    fn object_key(&self, source: SourceId, start_time_us: u64) -> String {
        let prefix = if self.config.prefix.is_empty() {
            String::new()
        } else {
            format!("{}/", self.config.prefix)
        };
        format!("{}{}/{}.segment", prefix, source, start_time_us)
    }

    /// Get the local cache path for a segment
    fn cache_path(&self, source: SourceId, start_time_us: u64) -> PathBuf {
        self.config.cache_dir.join(format!("{}_{}.segment", source, start_time_us))
    }

    /// Upload a file to S3 using AWS CLI
    fn upload_to_s3(&self, local_path: &Path, key: &str) -> Result<()> {
        let s3_url = format!("s3://{}/{}", self.config.bucket, key);

        let mut cmd = Command::new("aws");
        cmd.args(["s3", "cp", local_path.to_str().unwrap(), &s3_url]);

        if let Some(ref endpoint) = self.config.endpoint_url {
            cmd.args(["--endpoint-url", endpoint]);
        }

        cmd.args(["--region", &self.config.region]);

        let output = cmd.output()
            .context("Failed to run aws s3 cp")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("S3 upload failed: {}", stderr);
        }

        Ok(())
    }

    /// Download a file from S3 using AWS CLI
    fn download_from_s3(&self, key: &str, local_path: &Path) -> Result<()> {
        let s3_url = format!("s3://{}/{}", self.config.bucket, key);

        let mut cmd = Command::new("aws");
        cmd.args(["s3", "cp", &s3_url, local_path.to_str().unwrap()]);

        if let Some(ref endpoint) = self.config.endpoint_url {
            cmd.args(["--endpoint-url", endpoint]);
        }

        cmd.args(["--region", &self.config.region]);

        let output = cmd.output()
            .context("Failed to run aws s3 cp")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("S3 download failed: {}", stderr);
        }

        Ok(())
    }

    /// Delete an object from S3 using AWS CLI
    fn delete_from_s3(&self, key: &str) -> Result<()> {
        let s3_url = format!("s3://{}/{}", self.config.bucket, key);

        let mut cmd = Command::new("aws");
        cmd.args(["s3", "rm", &s3_url]);

        if let Some(ref endpoint) = self.config.endpoint_url {
            cmd.args(["--endpoint-url", endpoint]);
        }

        cmd.args(["--region", &self.config.region]);

        let output = cmd.output()
            .context("Failed to run aws s3 rm")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("S3 delete failed: {}", stderr);
        }

        Ok(())
    }

    /// Finalize and upload a local segment
    async fn finalize_segment(&self, source: SourceId, mut segment: LocalSegment) -> Result<()> {
        // Flush and close file
        segment.file.flush()?;
        drop(segment.file);

        // Upload to S3
        let key = self.object_key(source, segment.start_time_us);
        info!("Uploading segment to s3://{}/{}", self.config.bucket, key);

        self.upload_to_s3(&segment.path, &key)?;

        // Update index
        let mut index = self.index.write().await;
        let info = SegmentInfo {
            key,
            start_time_us: segment.start_time_us,
            end_time_us: segment.end_time_us,
            frame_count: segment.frame_count,
            size_bytes: segment.bytes_written,
        };
        index.total_bytes += info.size_bytes;
        index.segments.entry(source).or_default().push(info);

        // Remove local file
        let _ = fs::remove_file(&segment.path);

        Ok(())
    }

    /// Get or create a local segment for buffering
    async fn get_or_create_segment(&self, source: SourceId, timestamp_us: u64) -> Result<()> {
        let segment_start = (timestamp_us / self.config.segment_duration_us) * self.config.segment_duration_us;

        let mut segments = self.local_segments.write().await;

        // Check if we need a new segment
        if let Some(seg) = segments.get(&source) {
            let seg_start = (seg.start_time_us / self.config.segment_duration_us) * self.config.segment_duration_us;
            if seg_start == segment_start {
                return Ok(());
            }
            // Finalize old segment
            let old_seg = segments.remove(&source).unwrap();
            drop(segments); // Release lock before async operation
            self.finalize_segment(source, old_seg).await?;
            segments = self.local_segments.write().await;
        }

        // Create new local segment
        let path = self.cache_path(source, segment_start);
        let file = File::create(&path)?;
        let segment = LocalSegment {
            file: BufWriter::new(file),
            path,
            start_time_us: timestamp_us,
            end_time_us: timestamp_us,
            frame_count: 0,
            bytes_written: 0,
        };

        segments.insert(source, segment);
        Ok(())
    }

    /// Write a frame to the local buffer
    async fn write_frame(&self, frame: &Frame) -> Result<()> {
        self.get_or_create_segment(frame.source, frame.timestamp_us).await?;

        let mut segments = self.local_segments.write().await;
        let segment = segments.get_mut(&frame.source)
            .context("Segment not found after creation")?;

        let frame_bytes = frame_to_bytes(frame);
        let len = frame_bytes.len() as u32;

        segment.file.write_all(&len.to_le_bytes())?;
        segment.file.write_all(&frame_bytes)?;

        segment.end_time_us = frame.timestamp_us;
        segment.frame_count += 1;
        segment.bytes_written += 4 + frame_bytes.len() as u64;

        Ok(())
    }
}

#[async_trait::async_trait]
impl StorageBackend for CloudStorage {
    async fn store_frame(&self, frame: &Frame) -> Result<()> {
        self.write_frame(frame).await
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

        for seg in segments {
            if seg.end_time_us < start_time_us || seg.start_time_us > end_time_us {
                continue;
            }

            // Download segment to cache
            let cache_path = self.cache_path(source, seg.start_time_us);
            if !cache_path.exists() {
                self.download_from_s3(&seg.key, &cache_path)?;
            }

            // Read frames from cached file
            let file_data = fs::read(&cache_path)?;
            let mut offset = 0;

            while offset + 4 <= file_data.len() {
                let len = u32::from_le_bytes(file_data[offset..offset+4].try_into().unwrap()) as usize;
                offset += 4;

                if offset + len > file_data.len() {
                    break;
                }

                let frame_bytes = bytes::Bytes::copy_from_slice(&file_data[offset..offset+len]);
                offset += len;

                let frame = frame_from_bytes(frame_bytes)?;
                if frame.timestamp_us >= start_time_us && frame.timestamp_us <= end_time_us {
                    frames.push(frame);
                }
            }
        }

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
                    if let Err(e) = self.delete_from_s3(&seg.key) {
                        warn!("Failed to delete segment {}: {}", seg.key, e);
                    } else {
                        deleted_bytes += seg.size_bytes;
                        to_remove.push(i);
                    }
                }
            }

            for i in to_remove.into_iter().rev() {
                segments.remove(i);
            }
        }

        index.total_bytes = index.total_bytes.saturating_sub(deleted_bytes);
        debug!("Cleaned up {} bytes from cloud storage", deleted_bytes);
        Ok(deleted_bytes)
    }

    async fn usage_bytes(&self) -> Result<u64> {
        let index = self.index.read().await;
        Ok(index.total_bytes)
    }

    async fn available_bytes(&self) -> Result<Option<u64>> {
        // Cloud storage is effectively unlimited
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cloud_config_r2() {
        let config = CloudStorageConfig::r2("my-account", "my-bucket");
        assert_eq!(config.bucket, "my-bucket");
        assert!(config.endpoint_url.unwrap().contains("my-account"));
    }

    #[test]
    fn test_cloud_config_s3() {
        let config = CloudStorageConfig::s3("my-bucket", "eu-west-1");
        assert_eq!(config.bucket, "my-bucket");
        assert_eq!(config.region, "eu-west-1");
        assert!(config.endpoint_url.is_none());
    }

    #[test]
    fn test_object_key_generation() {
        let config = CloudStorageConfig {
            prefix: "cameras".to_string(),
            ..Default::default()
        };
        let storage = CloudStorage::new(config).unwrap();

        let source = SourceId::new([1, 2, 3, 4, 5, 6, 7, 8]);
        let key = storage.object_key(source, 1000000);

        assert!(key.starts_with("cameras/"));
        assert!(key.contains("0102030405060708"));
        assert!(key.ends_with(".segment"));
    }
}
