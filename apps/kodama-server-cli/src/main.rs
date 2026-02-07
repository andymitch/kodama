//! Kodama Server Binary
//!
//! Accepts camera connections, routes frames to clients, and optionally records to storage.
//!
//! ## Usage
//!
//! ```bash
//! # Run the server
//! kodama-server
//!
//! # With custom key path
//! KODAMA_KEY_PATH=./my-server.key kodama-server
//!
//! # With recording enabled
//! KODAMA_STORAGE_PATH=/var/lib/kodama/recordings kodama-server
//!
//! # With verbose logging
//! RUST_LOG=kodama=debug kodama-server
//! ```
//!
//! The server will print its PublicKey on startup. Share this key with:
//! - Cameras: set KODAMA_SERVER_KEY to connect
//! - Clients: use to connect from the desktop app

use anyhow::Result;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::time::{interval, Duration};
use tracing::{debug, error, info, warn};
use kodama_core::Frame;
use kodama_server::{
    Relay, Router,
    StorageBackend, LocalStorage, LocalStorageConfig,
    StorageManager, StorageConfig,
};

/// Server configuration from environment
struct Config {
    /// Path to store our secret key
    key_path: PathBuf,
    /// Frame buffer capacity for broadcast
    buffer_capacity: usize,
    /// Storage path (None = recording disabled)
    storage_path: Option<PathBuf>,
    /// Maximum storage size in GB
    storage_max_gb: u64,
    /// Retention period in days (0 = unlimited)
    retention_days: u64,
    /// Only store keyframes
    keyframes_only: bool,
}

impl Config {
    fn from_env() -> Self {
        let key_path = std::env::var("KODAMA_KEY_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("./server.key"));

        let buffer_capacity: usize = std::env::var("KODAMA_BUFFER_SIZE")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(512);

        let storage_path = std::env::var("KODAMA_STORAGE_PATH")
            .map(PathBuf::from)
            .ok();

        let storage_max_gb: u64 = std::env::var("KODAMA_STORAGE_MAX_GB")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(10);

        let retention_days: u64 = std::env::var("KODAMA_RETENTION_DAYS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(7);

        let keyframes_only = std::env::var("KODAMA_KEYFRAMES_ONLY")
            .map(|v| v == "1" || v.to_lowercase() == "true")
            .unwrap_or(false);

        Self {
            key_path,
            buffer_capacity,
            storage_path,
            storage_max_gb,
            retention_days,
            keyframes_only,
        }
    }
}

/// Storage handle for recording frames
struct StorageHandle {
    manager: StorageManager,
}

impl StorageHandle {
    fn new(path: PathBuf, max_gb: u64, retention_days: u64, keyframes_only: bool) -> Result<Self> {
        let local_config = LocalStorageConfig {
            root_path: path,
            max_size_bytes: max_gb * 1024 * 1024 * 1024,
            segment_duration_us: 60 * 1_000_000, // 1-minute segments
        };

        let backend: Arc<dyn StorageBackend> = Arc::new(LocalStorage::new(local_config)?);

        let storage_config = StorageConfig {
            max_size_bytes: max_gb * 1024 * 1024 * 1024,
            retention_secs: retention_days * 24 * 60 * 60,
            keyframes_only,
            cleanup_interval_secs: 3600, // 1 hour
        };

        let mut manager = StorageManager::new(storage_config, backend);
        manager.start_cleanup_task();

        Ok(Self { manager })
    }

    async fn store(&self, frame: &Frame) -> Result<bool> {
        self.manager.store(frame).await
    }

    async fn stats(&self) -> kodama_server::StorageStats {
        self.manager.stats().await
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("kodama=info".parse().unwrap()),
        )
        .init();

    let config = Config::from_env();

    info!("Kodama Server starting");
    info!("  Key path: {:?}", config.key_path);
    info!("  Buffer capacity: {}", config.buffer_capacity);

    // Initialize storage if configured
    let storage: Option<Arc<StorageHandle>> = if let Some(ref path) = config.storage_path {
        info!("  Storage path: {:?}", path);
        info!("  Storage max: {} GB", config.storage_max_gb);
        info!("  Retention: {} days", config.retention_days);
        info!("  Keyframes only: {}", config.keyframes_only);

        match StorageHandle::new(
            path.clone(),
            config.storage_max_gb,
            config.retention_days,
            config.keyframes_only,
        ) {
            Ok(handle) => {
                info!("Recording enabled");
                Some(Arc::new(handle))
            }
            Err(e) => {
                warn!("Failed to initialize storage: {}. Recording disabled.", e);
                None
            }
        }
    } else {
        info!("  Storage: disabled (set KODAMA_STORAGE_PATH to enable recording)");
        None
    };

    // Create parent directory for key if needed
    if let Some(parent) = config.key_path.parent() {
        if !parent.exists() && !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }

    // Initialize relay endpoint
    let relay = Relay::new(Some(&config.key_path)).await?;
    info!("Server PublicKey: {}", relay.public_key());
    info!("");
    info!("Share this key with cameras (KODAMA_SERVER_KEY)");
    info!("and clients to connect.");
    info!("");

    // Create router
    let router = Router::new(config.buffer_capacity);
    let handle = router.handle();

    // Spawn storage task that subscribes to frames and records them
    if let Some(storage) = storage.clone() {
        let storage_handle = handle.clone();
        tokio::spawn(async move {
            let mut rx = storage_handle.subscribe();
            let mut frames_stored = 0u64;
            let mut frames_skipped = 0u64;
            let mut last_log = tokio::time::Instant::now();

            loop {
                match rx.recv().await {
                    Ok(frame) => {
                        match storage.store(&frame).await {
                            Ok(true) => frames_stored += 1,
                            Ok(false) => frames_skipped += 1,
                            Err(e) => {
                                debug!("Storage error: {}", e);
                            }
                        }

                        // Log storage stats periodically
                        if last_log.elapsed().as_secs() >= 60 {
                            let stats = storage.stats().await;
                            info!(
                                "Storage: {} stored, {} skipped, {} MB used",
                                frames_stored, frames_skipped, stats.bytes_used / (1024 * 1024)
                            );
                            last_log = tokio::time::Instant::now();
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        warn!("Storage lagged, missed {} frames", n);
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        info!("Storage task: broadcast closed");
                        break;
                    }
                }
            }
        });
    }

    // Spawn stats logging task
    let stats_handle = handle.clone();
    let stats_storage = storage.clone();
    tokio::spawn(async move {
        let mut stats_interval = interval(Duration::from_secs(30));
        loop {
            stats_interval.tick().await;
            let stats = stats_handle.stats().await;

            let storage_info = if let Some(ref s) = stats_storage {
                let ss = s.stats().await;
                format!(", {} frames recorded, {} MB", ss.frames_stored, ss.bytes_used / (1024 * 1024))
            } else {
                String::new()
            };

            info!(
                "Stats: {} cameras, {} clients, {} frames received, {} broadcast{}",
                stats.cameras_connected,
                stats.clients_connected,
                stats.frames_received,
                stats.frames_broadcast,
                storage_info
            );
        }
    });

    info!("Waiting for connections...");

    // Accept connections
    loop {
        match relay.accept().await {
            Some(conn) => {
                let remote = conn.remote_public_key();
                info!("New connection from: {}", remote);

                let router = router.clone();

                // Detect role by stream direction
                tokio::spawn(async move {
                    let detect_timeout = Duration::from_secs(2);

                    tokio::select! {
                        // Try to accept a frame stream (camera behavior)
                        result = conn.accept_frame_stream() => {
                            match result {
                                Ok(receiver) => {
                                    info!(peer = %remote, "Detected as camera (opened stream)");

                                    // Accept the command bi-stream from camera
                                    let cmd_conn = conn.clone_handle();
                                    let cmd_router = router.clone();
                                    tokio::spawn(async move {
                                        match cmd_conn.accept_command_stream().await {
                                            Ok(cmd_stream) => {
                                                info!(peer = %remote, "Command channel accepted from camera");
                                                cmd_router.register_camera_commands(remote, cmd_stream);
                                            }
                                            Err(e) => {
                                                warn!(peer = %remote, error = %e, "Failed to accept command stream from camera (commands unavailable)");
                                            }
                                        }
                                    });

                                    if let Err(e) = router.handle_camera_with_receiver(remote, receiver).await {
                                        warn!(peer = %remote, error = %e, "Camera handler error");
                                    }
                                }
                                Err(e) => {
                                    warn!(peer = %remote, error = %e, "Failed to accept stream");
                                }
                            }
                        }
                        // Timeout - assume it's a client waiting for frames
                        _ = tokio::time::sleep(detect_timeout) => {
                            info!(peer = %remote, "Detected as client (no stream opened)");

                            // Spawn task to accept client command stream
                            let cmd_conn = conn.clone_handle();
                            let cmd_router = router.clone();
                            tokio::spawn(async move {
                                match cmd_conn.accept_client_command_stream().await {
                                    Ok(cmd_stream) => {
                                        info!(peer = %remote, "Client command channel accepted");
                                        cmd_router.handle_client_commands(remote, cmd_stream).await;
                                    }
                                    Err(e) => {
                                        debug!(peer = %remote, error = %e, "No client command stream (client may not support commands)");
                                    }
                                }
                            });

                            if let Err(e) = router.handle_client(conn).await {
                                warn!(peer = %remote, error = %e, "Client handler error");
                            }
                        }
                    }
                });
            }
            None => {
                error!("Relay accept returned None, shutting down");
                break;
            }
        }
    }

    Ok(())
}
