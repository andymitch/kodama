//! Kodama Server — Headless server with web UI
//!
//! Accepts camera connections over Iroh QUIC, routes frames to clients,
//! and serves a web UI + WebSocket endpoint for browser access.
//!
//! ## Usage
//!
//! ```bash
//! # Start server (web UI on port 3000)
//! kodama-server
//!
//! # Custom web port
//! KODAMA_WEB_PORT=8080 kodama-server
//!
//! # With recording enabled
//! KODAMA_STORAGE_PATH=/var/lib/kodama/recordings kodama-server
//! ```

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use tokio::time::{interval, Duration};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use tracing::{debug, error, info, warn};

use kodama::Frame;
use kodama::server::{
    LocalStorage, LocalStorageConfig, Relay, Router, StorageBackend, StorageConfig,
    StorageManager, StorageStats,
};

/// Server configuration from environment
struct Config {
    key_path: PathBuf,
    buffer_capacity: usize,
    storage_path: Option<PathBuf>,
    storage_max_gb: u64,
    retention_days: u64,
    keyframes_only: bool,
    web_port: u16,
    ui_path: Option<PathBuf>,
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

        let web_port: u16 = std::env::var("KODAMA_WEB_PORT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(3000);

        let ui_path = std::env::var("KODAMA_UI_PATH")
            .map(PathBuf::from)
            .ok()
            .or_else(|| {
                // Auto-detect: check common locations relative to binary
                let candidates = ["./ui/build", "../ui/build", "./build"];
                candidates.iter()
                    .map(PathBuf::from)
                    .find(|p| p.exists())
            });

        Self {
            key_path,
            buffer_capacity,
            storage_path,
            storage_max_gb,
            retention_days,
            keyframes_only,
            web_port,
            ui_path,
        }
    }
}

struct StorageHandle {
    manager: StorageManager,
    max_bytes: u64,
}

impl StorageHandle {
    fn new(path: PathBuf, max_gb: u64, retention_days: u64, keyframes_only: bool) -> Result<Self> {
        let max_bytes = max_gb * 1024 * 1024 * 1024;
        let local_config = LocalStorageConfig {
            root_path: path,
            max_size_bytes: max_bytes,
            segment_duration_us: 60 * 1_000_000,
        };

        let backend: Arc<dyn StorageBackend> = Arc::new(LocalStorage::new(local_config)?);

        let storage_config = StorageConfig {
            max_size_bytes: max_bytes,
            retention_secs: retention_days * 24 * 60 * 60,
            keyframes_only,
            cleanup_interval_secs: 3600,
        };

        let mut manager = StorageManager::new(storage_config, backend);
        manager.start_cleanup_task();

        Ok(Self { manager, max_bytes })
    }

    async fn store(&self, frame: &Frame) -> Result<bool> {
        self.manager.store(frame).await
    }

    async fn stats(&self) -> StorageStats {
        self.manager.stats().await
    }
}

#[async_trait::async_trait]
impl kodama::web::StorageStatsProvider for StorageHandle {
    async fn stats(&self) -> StorageStats {
        self.manager.stats().await
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    init_logging();

    let config = Config::from_env();

    info!("Kodama Server starting");
    info!("  Key path: {:?}", config.key_path);
    info!("  Buffer capacity: {}", config.buffer_capacity);
    info!("  Web port: {}", config.web_port);
    if let Some(ref ui_path) = config.ui_path {
        info!("  UI path: {:?}", ui_path);
    }

    // Initialize storage
    let storage: Option<Arc<StorageHandle>> = if let Some(ref path) = config.storage_path {
        info!("  Storage path: {:?}", path);
        info!("  Storage max: {} GB", config.storage_max_gb);
        info!("  Retention: {} days", config.retention_days);
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
        info!("  Storage: disabled (set KODAMA_STORAGE_PATH to enable)");
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
    let public_key = relay.public_key_base32();
    info!("Server PublicKey: {}", public_key);

    // Create router
    let router = Router::new(config.buffer_capacity);
    let handle = router.handle();

    // Graceful shutdown
    let cancel = CancellationToken::new();
    let tracker = TaskTracker::new();

    // Spawn storage recording task
    if let Some(storage) = storage.clone() {
        let storage_handle = handle.clone();
        let cancel = cancel.clone();
        tracker.spawn(async move {
            let mut rx = storage_handle.subscribe();
            let mut frames_stored = 0u64;
            let mut frames_skipped = 0u64;
            let mut last_log = tokio::time::Instant::now();
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => {
                        info!("Storage task: shutting down");
                        break;
                    }
                    result = rx.recv() => {
                        match result {
                            Ok(frame) => {
                                match storage.store(&frame).await {
                                    Ok(true) => frames_stored += 1,
                                    Ok(false) => frames_skipped += 1,
                                    Err(e) => debug!("Storage error: {}", e),
                                }
                                if last_log.elapsed().as_secs() >= 60 {
                                    let stats = storage.stats().await;
                                    info!(
                                        "Storage: {} stored, {} skipped, {} MB used",
                                        frames_stored, frames_skipped,
                                        stats.bytes_used / (1024 * 1024)
                                    );
                                    last_log = tokio::time::Instant::now();
                                }
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                                warn!("Storage lagged, missed {} frames", n);
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                        }
                    }
                }
            }
        });
    }

    // Spawn web server
    let web_handle = handle.clone();
    let web_bind = SocketAddr::from(([0, 0, 0, 0], config.web_port));
    let web_ui_path = config.ui_path.clone();
    let web_cancel = cancel.clone();
    let web_public_key = Some(public_key.clone());
    let web_storage: Option<Arc<dyn kodama::web::StorageStatsProvider>> =
        storage.clone().map(|s| s as Arc<dyn kodama::web::StorageStatsProvider>);
    let web_storage_max = storage.as_ref().map(|s| s.max_bytes).unwrap_or(0);
    tracker.spawn(async move {
        tokio::select! {
            result = kodama::web::start(web_handle, web_bind, web_ui_path, web_public_key, web_storage, web_storage_max) => {
                if let Err(e) = result {
                    error!("Web server error: {}", e);
                }
            }
            _ = web_cancel.cancelled() => {
                info!("Web server: shutting down");
            }
        }
    });

    // Spawn Iroh accept loop
    let accept_relay = relay;
    let accept_router = router;
    let accept_cancel = cancel.clone();
    let accept_tracker = tracker.clone();
    tracker.spawn(async move {
        loop {
            tokio::select! {
                _ = accept_cancel.cancelled() => {
                    info!("Accept loop: shutting down");
                    break;
                }
                conn = accept_relay.accept() => {
                    match conn {
                        Some(conn) => {
                            let remote = conn.remote_public_key();
                            info!("New connection from: {}", remote);
                            let router = accept_router.clone();
                            let cancel = accept_cancel.clone();
                            accept_tracker.spawn(async move {
                                let detect_timeout = Duration::from_secs(2);
                                tokio::select! {
                                    _ = cancel.cancelled() => {}
                                    result = conn.accept_frame_stream() => {
                                        match result {
                                            Ok(receiver) => {
                                                info!(peer = %remote, "Detected as camera");
                                                let cmd_conn = conn.clone_handle();
                                                let cmd_router = router.clone();
                                                tokio::spawn(async move {
                                                    match cmd_conn.accept_command_stream().await {
                                                        Ok(cmd_stream) => {
                                                            info!(peer = %remote, "Command channel accepted");
                                                            cmd_router.register_camera_commands(remote, cmd_stream);
                                                        }
                                                        Err(e) => {
                                                            warn!(peer = %remote, error = %e, "Failed to accept command stream");
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
                                    _ = tokio::time::sleep(detect_timeout) => {
                                        info!(peer = %remote, "Detected as client");
                                        let cmd_conn = conn.clone_handle();
                                        let cmd_router = router.clone();
                                        tokio::spawn(async move {
                                            match cmd_conn.accept_client_command_stream().await {
                                                Ok(cmd_stream) => {
                                                    info!(peer = %remote, "Client command channel accepted");
                                                    cmd_router.handle_client_commands(remote, cmd_stream).await;
                                                }
                                                Err(e) => {
                                                    debug!(peer = %remote, error = %e, "No client command stream");
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
            }
        }
    });

    tracker.close();

    run_headless(handle, storage, cancel, tracker).await
}

fn init_logging() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("kodama=info".parse().unwrap()),
        )
        .init();
}

/// Headless mode: log stats periodically, shut down on SIGINT/SIGTERM
async fn run_headless(
    handle: kodama::server::RouterHandle,
    storage: Option<Arc<StorageHandle>>,
    cancel: CancellationToken,
    tracker: TaskTracker,
) -> Result<()> {
    info!("Waiting for connections...");
    let mut stats_interval = interval(Duration::from_secs(30));
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                info!("Received shutdown signal");
                cancel.cancel();
                break;
            }
            _ = stats_interval.tick() => {
                let stats = handle.stats().await;
                let storage_info = if let Some(ref s) = storage {
                    let ss = s.stats().await;
                    format!(", {} frames recorded, {} MB", ss.frames_stored, ss.bytes_used / (1024 * 1024))
                } else {
                    String::new()
                };
                info!(
                    "Stats: {} cameras, {} clients, {} rx, {} tx{}",
                    stats.cameras_connected, stats.clients_connected,
                    stats.frames_received, stats.frames_broadcast, storage_info
                );
            }
        }
    }

    if tokio::time::timeout(Duration::from_secs(5), tracker.wait()).await.is_err() {
        warn!("Shutdown timed out after 5s");
    }
    Ok(())
}
