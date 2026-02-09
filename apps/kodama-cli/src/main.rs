//! Kodama Server (TUI + Headless)
//!
//! Terminal UI server for testing and monitoring. Falls back to plain logging
//! when stdout is not a TTY (e.g. redirected to a file).
//!
//! ## Usage
//!
//! ```bash
//! # TUI mode (interactive terminal)
//! kodama-cli
//!
//! # Headless mode (stdout redirected)
//! kodama-cli > /tmp/server.log 2>&1
//!
//! # With custom key path
//! KODAMA_KEY_PATH=./my-server.key kodama-cli
//!
//! # With recording enabled
//! KODAMA_STORAGE_PATH=/var/lib/kodama/recordings kodama-cli
//! ```

mod app;
mod ui;

use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::mpsc;
use tokio::time::{interval, Duration};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use tracing::{debug, error, info, warn};

use kodama_core::Frame;
use kodama_server::{
    LocalStorage, LocalStorageConfig, Relay, Router, StorageBackend, StorageConfig,
    StorageManager, StorageStats,
};

use app::App;

/// Server configuration from environment
struct Config {
    key_path: PathBuf,
    buffer_capacity: usize,
    storage_path: Option<PathBuf>,
    storage_max_gb: u64,
    retention_days: u64,
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

struct StorageHandle {
    manager: StorageManager,
}

impl StorageHandle {
    fn new(path: PathBuf, max_gb: u64, retention_days: u64, keyframes_only: bool) -> Result<Self> {
        let local_config = LocalStorageConfig {
            root_path: path,
            max_size_bytes: max_gb * 1024 * 1024 * 1024,
            segment_duration_us: 60 * 1_000_000,
        };

        let backend: Arc<dyn StorageBackend> = Arc::new(LocalStorage::new(local_config)?);

        let storage_config = StorageConfig {
            max_size_bytes: max_gb * 1024 * 1024 * 1024,
            retention_secs: retention_days * 24 * 60 * 60,
            keyframes_only,
            cleanup_interval_secs: 3600,
        };

        let mut manager = StorageManager::new(storage_config, backend);
        manager.start_cleanup_task();

        Ok(Self { manager })
    }

    async fn store(&self, frame: &Frame) -> Result<bool> {
        self.manager.store(frame).await
    }

    async fn stats(&self) -> StorageStats {
        self.manager.stats().await
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let is_tty = std::io::stdout().is_terminal();

    // In TUI mode, capture logs via a channel; in headless mode, use standard fmt logging
    let log_tx = if is_tty {
        let (tx, rx) = mpsc::unbounded_channel::<String>();
        use tracing_subscriber::layer::SubscriberExt;
        use tracing_subscriber::util::SubscriberInitExt;
        let channel_layer = ChannelLayer { tx: tx.clone() };
        tracing_subscriber::registry()
            .with(
                tracing_subscriber::EnvFilter::from_default_env()
                    .add_directive("kodama=info".parse().unwrap()),
            )
            .with(channel_layer)
            .init();
        Some((tx, rx))
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::from_default_env()
                    .add_directive("kodama=info".parse().unwrap()),
            )
            .init();
        None
    };

    let config = Config::from_env();

    info!("Kodama Server starting");
    info!("  Key path: {:?}", config.key_path);
    info!("  Buffer capacity: {}", config.buffer_capacity);

    // Initialize storage
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
    let public_key = relay.public_key_base32();
    info!("Server PublicKey: {}", public_key);
    info!("Share this key with cameras (KODAMA_SERVER_KEY) and clients to connect.");

    // Create router
    let router = Router::new(config.buffer_capacity);
    let handle = router.handle();

    // Graceful shutdown infrastructure
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
                                    Err(e) => {
                                        debug!("Storage error: {}", e);
                                    }
                                }
                                if last_log.elapsed().as_secs() >= 60 {
                                    let stats = storage.stats().await;
                                    info!(
                                        "Storage: {} stored, {} skipped, {} MB used",
                                        frames_stored,
                                        frames_skipped,
                                        stats.bytes_used / (1024 * 1024)
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
                }
            }
        });
    }

    // Spawn accept loop (shared between TUI and headless)
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
                                                info!(peer = %remote, "Detected as camera (opened stream)");
                                                let cmd_conn = conn.clone_handle();
                                                let cmd_router = router.clone();
                                                tokio::spawn(async move {
                                                    match cmd_conn.accept_command_stream().await {
                                                        Ok(cmd_stream) => {
                                                            info!(peer = %remote, "Command channel accepted from camera");
                                                            cmd_router.register_camera_commands(remote, cmd_stream);
                                                        }
                                                        Err(e) => {
                                                            warn!(peer = %remote, error = %e, "Failed to accept command stream from camera");
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
                                        info!(peer = %remote, "Detected as client (no stream opened)");
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

    // Close the tracker so wait() can complete once all tasks finish
    tracker.close();

    if let Some((_tx, rx)) = log_tx {
        run_tui(public_key, handle, storage, cancel, tracker, rx).await
    } else {
        run_headless(handle, storage, cancel, tracker).await
    }
}

/// Headless mode: log stats periodically, shut down on SIGINT/SIGTERM
async fn run_headless(
    handle: kodama_server::RouterHandle,
    storage: Option<Arc<StorageHandle>>,
    cancel: CancellationToken,
    tracker: TaskTracker,
) -> Result<()> {
    info!("Waiting for connections...");
    let mut stats_interval = interval(Duration::from_secs(30));
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                info!("Received shutdown signal, draining tasks...");
                cancel.cancel();
                break;
            }
            _ = stats_interval.tick() => {
                let stats = handle.stats().await;
                let storage_info = if let Some(ref s) = storage {
                    let ss = s.stats().await;
                    format!(
                        ", {} frames recorded, {} MB",
                        ss.frames_stored,
                        ss.bytes_used / (1024 * 1024)
                    )
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
        }
    }

    // Wait for all tracked tasks to finish (with timeout)
    if tokio::time::timeout(Duration::from_secs(5), tracker.wait()).await.is_err() {
        warn!("Shutdown timed out after 5s, some tasks may not have finished");
    } else {
        info!("All tasks shut down cleanly");
    }
    Ok(())
}

/// TUI mode: interactive ratatui terminal
async fn run_tui(
    public_key: String,
    handle: kodama_server::RouterHandle,
    storage: Option<Arc<StorageHandle>>,
    cancel: CancellationToken,
    tracker: TaskTracker,
    mut log_rx: mpsc::UnboundedReceiver<String>,
) -> Result<()> {
    use crossterm::{
        event::{self, Event, KeyCode, KeyEventKind},
        terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    };
    use ratatui::backend::CrosstermBackend;
    use ratatui::Terminal;

    enable_raw_mode()?;
    crossterm::execute!(std::io::stdout(), EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(std::io::stdout());
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(public_key, handle);
    let tick_rate = Duration::from_millis(250);

    let result: Result<()> = loop {
        // Drain log messages
        while let Ok(msg) = log_rx.try_recv() {
            app.push_log(msg);
        }

        // Refresh stats
        app.refresh().await;

        // Update storage stats
        if let Some(ref s) = storage {
            app.storage_stats = Some(s.stats().await);
        }

        // Draw
        terminal.draw(|f| ui::draw(f, &app))?;

        // Handle input
        if event::poll(tick_rate)? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press && key.code == KeyCode::Char('q') {
                    break Ok(());
                }
            }
        }
    };

    // Restore terminal
    disable_raw_mode()?;
    crossterm::execute!(std::io::stdout(), LeaveAlternateScreen)?;

    // Signal all tasks to shut down and wait
    info!("Shutting down...");
    cancel.cancel();
    if tokio::time::timeout(Duration::from_secs(5), tracker.wait()).await.is_err() {
        warn!("Shutdown timed out after 5s, some tasks may not have finished");
    }

    result
}

// --- Custom tracing layer that sends formatted events to an mpsc channel ---

use tracing_subscriber::layer::Context;
use tracing_subscriber::Layer;

struct ChannelLayer {
    tx: mpsc::UnboundedSender<String>,
}

impl<S> Layer<S> for ChannelLayer
where
    S: tracing::Subscriber,
{
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        let level = meta.level();
        let target = meta.target();
        let short_target = target.rsplit("::").next().unwrap_or(target);

        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);

        let now = utc_timestamp();
        let msg = format!("{} {:>5} {}: {}", now, level, short_target, visitor.message);
        let _ = self.tx.send(msg);
    }
}

#[derive(Default)]
struct MessageVisitor {
    message: String,
}

impl tracing::field::Visit for MessageVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{:?}", value);
        } else if !self.message.is_empty() {
            self.message
                .push_str(&format!(" {}={:?}", field.name(), value));
        } else {
            self.message = format!("{}={:?}", field.name(), value);
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        } else if !self.message.is_empty() {
            self.message
                .push_str(&format!(" {}={}", field.name(), value));
        } else {
            self.message = format!("{}={}", field.name(), value);
        }
    }
}

/// Simple HH:MM:SS UTC timestamp without chrono
fn utc_timestamp() -> String {
    use std::time::SystemTime;
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let secs_of_day = now.as_secs() % 86400;
    let h = secs_of_day / 3600;
    let m = (secs_of_day % 3600) / 60;
    let s = secs_of_day % 60;
    format!("{:02}:{:02}:{:02}", h, m, s)
}
