//! Kodama App - Tauri desktop application
//!
//! Embeds a full Kodama server (Iroh + axum web) and opens a webview to it.
//! The SvelteKit UI communicates entirely over WebSocket and REST — no Tauri IPC
//! is needed for video, audio, or telemetry.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use tauri::Manager;
use tokio::time::Duration;
use tracing_subscriber::EnvFilter;

use kodama::server::{
    LocalStorage, LocalStorageConfig, Relay, Router, StorageBackend, StorageConfig,
    StorageManager,
};

/// Initialize and run the Tauri application.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::from_default_env()
                .add_directive("kodama=info".parse().unwrap())
                .add_directive("kodama_app=debug".parse().unwrap()),
        )
        .init();

    tracing::info!("Kodama starting");

    tauri::Builder::default()
        .setup(|app| {
            // Resolve UI path from bundled resources (production only)
            let ui_path = app
                .path()
                .resource_dir()
                .ok()
                .map(|p| p.join("ui"))
                .filter(|p| p.exists());

            // Start embedded server in background
            tauri::async_runtime::spawn(async move {
                if let Err(e) = start_embedded_server(ui_path).await {
                    tracing::error!("Embedded server error: {}", e);
                }
            });

            #[cfg(debug_assertions)]
            if let Some(window) = app.get_webview_window("main") {
                window.open_devtools();
            }

            tracing::info!("App setup complete");
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running Kodama");
}

/// Start the embedded Kodama server (Iroh + axum web).
async fn start_embedded_server(ui_path: Option<PathBuf>) -> anyhow::Result<()> {
    // --- Config ---
    let key_path = std::env::var("KODAMA_KEY_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            // Default to OS data directory
            let base = dirs_next::data_dir().unwrap_or_else(|| PathBuf::from("."));
            base.join("kodama").join("server.key")
        });

    let buffer_capacity: usize = std::env::var("KODAMA_BUFFER_SIZE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(512);

    let web_port: u16 = std::env::var("KODAMA_WEB_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3000);

    let storage_path = std::env::var("KODAMA_STORAGE_PATH")
        .map(PathBuf::from)
        .ok();

    tracing::info!("  Key path: {:?}", key_path);
    tracing::info!("  Buffer capacity: {}", buffer_capacity);
    tracing::info!("  Web port: {}", web_port);

    // --- Key directory ---
    if let Some(parent) = key_path.parent() {
        if !parent.exists() && !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }

    // --- Iroh endpoint ---
    let relay = Relay::new(Some(&key_path)).await?;
    let public_key = relay.public_key_base32();
    tracing::info!("Server PublicKey: {}", public_key);

    // --- Router ---
    let router = Router::new(buffer_capacity);
    let handle = router.handle();

    // --- Storage (optional) ---
    if let Some(ref path) = storage_path {
        let storage_max_gb: u64 = std::env::var("KODAMA_STORAGE_MAX_GB")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(10);
        let retention_days: u64 = std::env::var("KODAMA_RETENTION_DAYS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(7);

        let local_config = LocalStorageConfig {
            root_path: path.clone(),
            max_size_bytes: storage_max_gb * 1024 * 1024 * 1024,
            segment_duration_us: 60 * 1_000_000,
        };

        match LocalStorage::new(local_config) {
            Ok(backend) => {
                let backend: Arc<dyn StorageBackend> = Arc::new(backend);
                let storage_config = StorageConfig {
                    max_size_bytes: storage_max_gb * 1024 * 1024 * 1024,
                    retention_secs: retention_days * 24 * 60 * 60,
                    keyframes_only: false,
                    cleanup_interval_secs: 3600,
                };
                let mut manager = StorageManager::new(storage_config, backend);
                manager.start_cleanup_task();

                // Spawn storage recording task
                let storage_handle = handle.clone();
                tokio::spawn(async move {
                    let mut rx = storage_handle.subscribe();
                    loop {
                        match rx.recv().await {
                            Ok(frame) => {
                                let _ = manager.store(&frame).await;
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                                tracing::warn!("Storage lagged, missed {} frames", n);
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                        }
                    }
                });

                tracing::info!("Recording enabled: {:?}", path);
            }
            Err(e) => {
                tracing::warn!("Failed to initialize storage: {}. Recording disabled.", e);
            }
        }
    }

    // --- Web server ---
    let web_handle = handle.clone();
    let bind = SocketAddr::from(([127, 0, 0, 1], web_port));
    let web_public_key = Some(public_key);
    tokio::spawn(async move {
        if let Err(e) = kodama::web::start(web_handle, bind, ui_path, web_public_key).await {
            tracing::error!("Web server error: {}", e);
        }
    });

    // --- Accept loop ---
    loop {
        match relay.accept().await {
            Some(conn) => {
                let remote = conn.remote_public_key();
                tracing::info!("New connection from: {}", remote);
                let r = router.clone();
                tokio::spawn(async move {
                    let detect_timeout = Duration::from_secs(2);
                    tokio::select! {
                        result = conn.accept_frame_stream() => {
                            match result {
                                Ok(receiver) => {
                                    tracing::info!(peer = %remote, "Detected as camera");
                                    // Accept command stream for OTA updates etc.
                                    let cmd_conn = conn.clone_handle();
                                    let cmd_router = r.clone();
                                    tokio::spawn(async move {
                                        match cmd_conn.accept_command_stream().await {
                                            Ok(cmd_stream) => {
                                                tracing::info!(peer = %remote, "Command channel accepted");
                                                cmd_router.register_camera_commands(remote, cmd_stream);
                                            }
                                            Err(e) => {
                                                tracing::warn!(peer = %remote, error = %e, "Failed to accept command stream");
                                            }
                                        }
                                    });
                                    if let Err(e) = r.handle_camera_with_receiver(remote, receiver).await {
                                        tracing::warn!(peer = %remote, error = %e, "Camera handler error");
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!(peer = %remote, error = %e, "Failed to accept stream");
                                }
                            }
                        }
                        _ = tokio::time::sleep(detect_timeout) => {
                            tracing::info!(peer = %remote, "Detected as client");
                            let cmd_conn = conn.clone_handle();
                            let cmd_router = r.clone();
                            tokio::spawn(async move {
                                match cmd_conn.accept_client_command_stream().await {
                                    Ok(cmd_stream) => {
                                        tracing::info!(peer = %remote, "Client command channel accepted");
                                        cmd_router.handle_client_commands(remote, cmd_stream).await;
                                    }
                                    Err(e) => {
                                        tracing::debug!(peer = %remote, error = %e, "No client command stream");
                                    }
                                }
                            });
                            if let Err(e) = r.handle_client(conn).await {
                                tracing::warn!(peer = %remote, error = %e, "Client handler error");
                            }
                        }
                    }
                });
            }
            None => {
                tracing::error!("Relay accept returned None, server shutting down");
                break;
            }
        }
    }

    Ok(())
}

/// Get the default data directory for Kodama.
mod dirs_next {
    use std::path::PathBuf;

    pub fn data_dir() -> Option<PathBuf> {
        #[cfg(target_os = "macos")]
        {
            dirs_inner::home_dir().map(|h| h.join("Library/Application Support"))
        }
        #[cfg(target_os = "linux")]
        {
            std::env::var("XDG_DATA_HOME")
                .ok()
                .map(PathBuf::from)
                .or_else(|| dirs_inner::home_dir().map(|h| h.join(".local/share")))
        }
        #[cfg(target_os = "windows")]
        {
            std::env::var("APPDATA").ok().map(PathBuf::from)
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
        {
            None
        }
    }

    mod dirs_inner {
        use std::path::PathBuf;

        pub fn home_dir() -> Option<PathBuf> {
            std::env::var("HOME")
                .ok()
                .map(PathBuf::from)
        }
    }
}
