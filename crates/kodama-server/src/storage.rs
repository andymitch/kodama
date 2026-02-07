//! Storage management for server
//!
//! Coordinates storage of video recordings, handles retention policies,
//! and manages storage backends.

use anyhow::Result;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use kodama_core::{Frame, SourceId};
use kodama_storage::StorageBackend;

/// Storage configuration
#[derive(Debug, Clone)]
pub struct StorageConfig {
    /// Maximum storage size in bytes (0 = unlimited)
    pub max_size_bytes: u64,
    /// Retention period in seconds (0 = unlimited)
    pub retention_secs: u64,
    /// Whether to store all frames or just keyframes
    pub keyframes_only: bool,
    /// Cleanup interval in seconds
    pub cleanup_interval_secs: u64,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            max_size_bytes: 10 * 1024 * 1024 * 1024, // 10 GB
            retention_secs: 7 * 24 * 60 * 60,         // 7 days
            keyframes_only: false,
            cleanup_interval_secs: 3600, // 1 hour
        }
    }
}

/// Statistics about storage usage
#[derive(Debug, Clone, Default)]
pub struct StorageStats {
    /// Total bytes used
    pub bytes_used: u64,
    /// Total frames stored
    pub frames_stored: u64,
    /// Frames dropped due to keyframes_only setting
    pub frames_skipped: u64,
    /// Bytes cleaned up
    pub bytes_cleaned: u64,
}

/// Manages storage of video recordings
pub struct StorageManager {
    config: StorageConfig,
    backend: Arc<dyn StorageBackend>,
    stats: Arc<RwLock<StorageStats>>,
    shutdown_tx: Option<tokio::sync::mpsc::Sender<()>>,
}

impl StorageManager {
    /// Create a new storage manager with the given backend
    pub fn new(config: StorageConfig, backend: Arc<dyn StorageBackend>) -> Self {
        Self {
            config,
            backend,
            stats: Arc::new(RwLock::new(StorageStats::default())),
            shutdown_tx: None,
        }
    }

    /// Store a frame
    ///
    /// Returns true if the frame was stored, false if it was skipped.
    pub async fn store(&self, frame: &Frame) -> Result<bool> {
        // Check keyframes_only setting
        if self.config.keyframes_only && !frame.flags.is_keyframe() {
            let mut stats = self.stats.write().await;
            stats.frames_skipped += 1;
            return Ok(false);
        }

        // Check storage limits
        let current_usage = self.backend.usage_bytes().await?;
        if self.config.max_size_bytes > 0 && current_usage >= self.config.max_size_bytes {
            warn!("Storage limit reached, skipping frame");
            return Ok(false);
        }

        // Store the frame
        self.backend.store_frame(frame).await?;

        // Update stats
        let mut stats = self.stats.write().await;
        stats.frames_stored += 1;
        stats.bytes_used = current_usage + frame.payload.len() as u64;

        Ok(true)
    }

    /// Retrieve frames for a source within a time range
    pub async fn get_frames(
        &self,
        source: SourceId,
        start_time_us: u64,
        end_time_us: u64,
    ) -> Result<Vec<Frame>> {
        self.backend.get_frames(source, start_time_us, end_time_us).await
    }

    /// Get storage statistics
    pub async fn stats(&self) -> StorageStats {
        let mut stats = self.stats.read().await.clone();
        // Update bytes_used from backend
        if let Ok(bytes) = self.backend.usage_bytes().await {
            stats.bytes_used = bytes;
        }
        stats
    }

    /// Get current storage usage in bytes
    pub async fn usage_bytes(&self) -> Result<u64> {
        self.backend.usage_bytes().await
    }

    /// Get available storage capacity
    pub async fn available_bytes(&self) -> Result<Option<u64>> {
        self.backend.available_bytes().await
    }

    /// Run cleanup based on retention policy
    pub async fn cleanup(&self) -> Result<u64> {
        if self.config.retention_secs == 0 {
            return Ok(0);
        }

        let cutoff_time_us = {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_micros() as u64;
            now.saturating_sub(self.config.retention_secs * 1_000_000)
        };

        let deleted = self.backend.cleanup(cutoff_time_us).await?;

        if deleted > 0 {
            let mut stats = self.stats.write().await;
            stats.bytes_cleaned += deleted;
            info!("Cleaned up {} bytes of old recordings", deleted);
        }

        Ok(deleted)
    }

    /// Start the periodic cleanup task
    pub fn start_cleanup_task(&mut self) {
        let (shutdown_tx, mut shutdown_rx) = tokio::sync::mpsc::channel::<()>(1);
        self.shutdown_tx = Some(shutdown_tx);

        let backend = Arc::clone(&self.backend);
        let stats = Arc::clone(&self.stats);
        let config = self.config.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(config.cleanup_interval_secs));

            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        if config.retention_secs == 0 {
                            continue;
                        }

                        let cutoff_time_us = {
                            let now = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap()
                                .as_micros() as u64;
                            now.saturating_sub(config.retention_secs * 1_000_000)
                        };

                        match backend.cleanup(cutoff_time_us).await {
                            Ok(deleted) if deleted > 0 => {
                                let mut s = stats.write().await;
                                s.bytes_cleaned += deleted;
                                info!("Periodic cleanup: removed {} bytes", deleted);
                            }
                            Ok(_) => {
                                debug!("Periodic cleanup: nothing to clean");
                            }
                            Err(e) => {
                                warn!("Periodic cleanup failed: {}", e);
                            }
                        }
                    }
                    _ = shutdown_rx.recv() => {
                        info!("Storage cleanup task shutting down");
                        break;
                    }
                }
            }
        });
    }

    /// Stop the cleanup task
    pub fn stop_cleanup_task(&mut self) {
        self.shutdown_tx.take();
    }
}

impl Drop for StorageManager {
    fn drop(&mut self) {
        self.stop_cleanup_task();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kodama_core::{Channel, FrameFlags};
    use kodama_storage::{LocalStorage, LocalStorageConfig};
    use bytes::Bytes;
    use tempfile::tempdir;

    #[tokio::test(flavor = "multi_thread")]
    async fn test_storage_manager_basic() {
        let dir = tempdir().unwrap();
        let local_config = LocalStorageConfig {
            root_path: dir.path().to_path_buf(),
            max_size_bytes: 0,
            segment_duration_us: 1_000_000,
        };

        let backend = Arc::new(LocalStorage::new(local_config).unwrap());
        let config = StorageConfig::default();
        let manager = StorageManager::new(config, backend);

        let source = SourceId::new([1, 2, 3, 4, 5, 6, 7, 8]);
        let frame = Frame {
            source,
            channel: Channel::Video,
            flags: FrameFlags::keyframe(),
            timestamp_us: 1000,
            payload: Bytes::from_static(b"test frame"),
        };

        assert!(manager.store(&frame).await.unwrap());

        let stats = manager.stats().await;
        assert_eq!(stats.frames_stored, 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_storage_manager_keyframes_only() {
        let dir = tempdir().unwrap();
        let local_config = LocalStorageConfig {
            root_path: dir.path().to_path_buf(),
            max_size_bytes: 0,
            segment_duration_us: 1_000_000,
        };

        let backend = Arc::new(LocalStorage::new(local_config).unwrap());
        let config = StorageConfig {
            keyframes_only: true,
            ..Default::default()
        };
        let manager = StorageManager::new(config, backend);

        let source = SourceId::new([1, 2, 3, 4, 5, 6, 7, 8]);

        // Keyframe should be stored
        let keyframe = Frame {
            source,
            channel: Channel::Video,
            flags: FrameFlags::keyframe(),
            timestamp_us: 1000,
            payload: Bytes::from_static(b"keyframe"),
        };
        assert!(manager.store(&keyframe).await.unwrap());

        // Non-keyframe should be skipped
        let regular = Frame {
            source,
            channel: Channel::Video,
            flags: FrameFlags::default(),
            timestamp_us: 2000,
            payload: Bytes::from_static(b"regular"),
        };
        assert!(!manager.store(&regular).await.unwrap());

        let stats = manager.stats().await;
        assert_eq!(stats.frames_stored, 1);
        assert_eq!(stats.frames_skipped, 1);
    }
}
