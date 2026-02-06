//! Tauri commands for the desktop app

use std::sync::Arc;
use tauri::State;
use serde::Serialize;

use kodama_relay::Relay;
use kodama_server::{
    Router, StorageManager, StorageConfig as ServerStorageConfig,
    LocalStorage, LocalStorageConfig, StorageBackend,
};

use crate::state::{AppMode, AppState, ServerState, ClientState, CameraInfo, StorageConfig, AppSettings};

/// Server status response
#[derive(Debug, Serialize)]
pub struct ServerStatus {
    pub running: bool,
    pub public_key: Option<String>,
    pub cameras_connected: usize,
    pub clients_connected: usize,
    pub frames_received: u64,
}

/// Connection status response
#[derive(Debug, Serialize)]
pub struct ConnectionStatus {
    pub connected: bool,
    pub mode: AppMode,
    pub server_key: Option<String>,
}

/// Storage stats response
#[derive(Debug, Serialize)]
pub struct StorageStats {
    pub enabled: bool,
    pub bytes_used: u64,
    pub frames_stored: u64,
    pub bytes_cleaned: u64,
}

/// Start the server
#[tauri::command]
pub async fn start_server(
    state: State<'_, AppState>,
    storage_config: Option<StorageConfig>,
) -> Result<String, String> {
    let mut mode = state.mode.write().await;
    if *mode != AppMode::Idle {
        return Err("Already running in another mode".to_string());
    }

    let settings = state.settings.read().await;

    // Create relay endpoint
    let relay = Relay::new(None).await
        .map_err(|e| format!("Failed to create relay: {}", e))?;

    let public_key = relay.public_key().to_string();
    tracing::info!("Server started with key: {}", public_key);

    // Create router
    let router = Router::new(settings.buffer_size);
    let handle = router.handle();

    // Setup storage if configured
    let storage = if let Some(ref config) = storage_config {
        if config.enabled {
            if let Some(ref path) = config.path {
                let local_config = LocalStorageConfig {
                    root_path: path.clone(),
                    max_size_bytes: config.max_gb * 1024 * 1024 * 1024,
                    segment_duration_us: 60 * 1_000_000,
                };

                match LocalStorage::new(local_config) {
                    Ok(backend) => {
                        let backend: Arc<dyn StorageBackend> = Arc::new(backend);
                        let storage_config = ServerStorageConfig {
                            max_size_bytes: config.max_gb * 1024 * 1024 * 1024,
                            retention_secs: (config.retention_days as u64) * 24 * 60 * 60,
                            keyframes_only: config.keyframes_only,
                            cleanup_interval_secs: 3600,
                        };
                        let mut manager = StorageManager::new(storage_config, backend);
                        manager.start_cleanup_task();
                        Some(manager)
                    }
                    Err(e) => {
                        tracing::warn!("Failed to initialize storage: {}", e);
                        None
                    }
                }
            } else {
                None
            }
        } else {
            None
        }
    } else {
        None
    };

    // Store server state
    let server_state = Arc::new(ServerState {
        relay,
        router,
        handle,
        storage,
    });

    *state.server.write().await = Some(server_state);
    *mode = AppMode::Server;

    // TODO: Spawn connection accept loop

    Ok(public_key)
}

/// Stop the server
#[tauri::command]
pub async fn stop_server(state: State<'_, AppState>) -> Result<(), String> {
    let mut mode = state.mode.write().await;
    if *mode != AppMode::Server {
        return Err("Not running as server".to_string());
    }

    // Clean up server state
    let mut server = state.server.write().await;
    if let Some(server_state) = server.take() {
        server_state.relay.close().await;
    }

    *mode = AppMode::Idle;
    tracing::info!("Server stopped");

    Ok(())
}

/// Get server status
#[tauri::command]
pub async fn get_server_status(state: State<'_, AppState>) -> Result<ServerStatus, String> {
    let mode = state.mode.read().await;
    let server = state.server.read().await;

    if *mode != AppMode::Server || server.is_none() {
        return Ok(ServerStatus {
            running: false,
            public_key: None,
            cameras_connected: 0,
            clients_connected: 0,
            frames_received: 0,
        });
    }

    let server_state = server.as_ref().unwrap();
    let stats = server_state.handle.stats().await;

    Ok(ServerStatus {
        running: true,
        public_key: Some(server_state.relay.public_key().to_string()),
        cameras_connected: stats.cameras_connected,
        clients_connected: stats.clients_connected,
        frames_received: stats.frames_received,
    })
}

/// Connect to an external server as a client
#[tauri::command]
pub async fn connect_to_server(
    state: State<'_, AppState>,
    server_key: String,
) -> Result<(), String> {
    let mut mode = state.mode.write().await;
    if *mode != AppMode::Idle {
        return Err("Already running in another mode".to_string());
    }

    // Parse server key
    let public_key: iroh::PublicKey = server_key.parse()
        .map_err(|e| format!("Invalid server key: {}", e))?;

    // Create ephemeral relay
    let relay = Relay::new(None).await
        .map_err(|e| format!("Failed to create relay: {}", e))?;

    tracing::info!("Connecting to server: {}", server_key);

    // Connect to server
    let _conn = relay.connect(public_key).await
        .map_err(|e| format!("Failed to connect: {}", e))?;

    tracing::info!("Connected to server!");

    // Store client state
    let client_state = Arc::new(ClientState {
        relay,
        server_key: public_key,
    });

    *state.client.write().await = Some(client_state);
    *mode = AppMode::Client;

    // TODO: Spawn frame receiving loop

    Ok(())
}

/// Disconnect from server
#[tauri::command]
pub async fn disconnect(state: State<'_, AppState>) -> Result<(), String> {
    let mut mode = state.mode.write().await;
    if *mode != AppMode::Client {
        return Err("Not connected as client".to_string());
    }

    // Clean up client state
    let mut client = state.client.write().await;
    if let Some(client_state) = client.take() {
        client_state.relay.close().await;
    }

    *mode = AppMode::Idle;
    tracing::info!("Disconnected from server");

    Ok(())
}

/// Get connection status
#[tauri::command]
pub async fn get_connection_status(state: State<'_, AppState>) -> Result<ConnectionStatus, String> {
    let mode = state.mode.read().await;
    let client = state.client.read().await;

    let server_key = client.as_ref().map(|c| c.server_key.to_string());

    Ok(ConnectionStatus {
        connected: *mode == AppMode::Client,
        mode: *mode,
        server_key,
    })
}

/// List connected cameras
#[tauri::command]
pub async fn list_cameras(state: State<'_, AppState>) -> Result<Vec<CameraInfo>, String> {
    let cameras = state.cameras.read().await;
    Ok(cameras.clone())
}

/// Get camera info
#[tauri::command]
pub async fn get_camera_info(
    state: State<'_, AppState>,
    camera_id: String,
) -> Result<Option<CameraInfo>, String> {
    let cameras = state.cameras.read().await;
    Ok(cameras.iter().find(|c| c.id == camera_id).cloned())
}

/// Get storage statistics
#[tauri::command]
pub async fn get_storage_stats(state: State<'_, AppState>) -> Result<StorageStats, String> {
    let server = state.server.read().await;

    if let Some(ref server_state) = *server {
        if let Some(ref storage) = server_state.storage {
            let stats = storage.stats().await;
            return Ok(StorageStats {
                enabled: true,
                bytes_used: stats.bytes_used,
                frames_stored: stats.frames_stored,
                bytes_cleaned: stats.bytes_cleaned,
            });
        }
    }

    Ok(StorageStats {
        enabled: false,
        bytes_used: 0,
        frames_stored: 0,
        bytes_cleaned: 0,
    })
}

/// Configure storage
#[tauri::command]
pub async fn configure_storage(
    state: State<'_, AppState>,
    config: StorageConfig,
) -> Result<(), String> {
    let mut settings = state.settings.write().await;
    settings.storage = config;
    // TODO: Persist settings
    Ok(())
}

/// Get application settings
#[tauri::command]
pub async fn get_settings(state: State<'_, AppState>) -> Result<AppSettings, String> {
    let settings = state.settings.read().await;
    Ok(settings.clone())
}

/// Save application settings
#[tauri::command]
pub async fn save_settings(
    state: State<'_, AppState>,
    settings: AppSettings,
) -> Result<(), String> {
    let mut current = state.settings.write().await;
    *current = settings;
    // TODO: Persist to disk
    Ok(())
}
