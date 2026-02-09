//! Tauri commands for Kodama Mobile
//!
//! Mobile version does not include storage capabilities to reduce complexity
//! and resource usage on mobile devices.

use crate::state::{AppMode, AppSettings, AppState, CameraInfo, ClientState, ServerState};
use kodama_transport::Relay;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::State;

/// Server status response
#[derive(Debug, Serialize, Deserialize)]
pub struct ServerStatus {
    pub running: bool,
    pub public_key: Option<String>,
    pub connected_cameras: usize,
}

/// Connection status response
#[derive(Debug, Serialize, Deserialize)]
pub struct ConnectionStatus {
    pub connected: bool,
    pub server_key: Option<String>,
}

/// Start server in server mode (no storage on mobile)
#[tauri::command]
pub async fn start_server(state: State<'_, Arc<AppState>>) -> Result<String, String> {
    let current_mode = *state.mode.read().await;
    if current_mode != AppMode::Idle {
        return Err("Cannot start server: app is not idle".to_string());
    }

    // Create relay endpoint
    let relay = Relay::new(None)
        .await
        .map_err(|e| format!("Failed to create relay: {}", e))?;

    let public_key = relay.public_key_base32();

    // Store server state
    let server_state = Arc::new(ServerState {
        relay,
        public_key: public_key.clone(),
    });

    *state.server.write().await = Some(server_state);
    *state.mode.write().await = AppMode::Server;

    // Note: On mobile, we don't spawn the full server router to conserve resources
    // The server mode primarily allows cameras to connect and stream

    Ok(public_key)
}

/// Stop the server
#[tauri::command]
pub async fn stop_server(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let current_mode = *state.mode.read().await;
    if current_mode != AppMode::Server {
        return Err("Not running as server".to_string());
    }

    *state.server.write().await = None;
    *state.mode.write().await = AppMode::Idle;
    state.cameras.write().await.clear();

    Ok(())
}

/// Get server status
#[tauri::command]
pub async fn get_server_status(state: State<'_, Arc<AppState>>) -> Result<ServerStatus, String> {
    let server = state.server.read().await;
    let cameras = state.cameras.read().await;

    Ok(ServerStatus {
        running: server.is_some(),
        public_key: server.as_ref().map(|s| s.public_key.clone()),
        connected_cameras: cameras.len(),
    })
}

/// Connect to a remote server as a client
#[tauri::command]
pub async fn connect_to_server(
    server_key: String,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    let current_mode = *state.mode.read().await;
    if current_mode != AppMode::Idle {
        return Err("Cannot connect: app is not idle".to_string());
    }

    // Create relay endpoint
    let relay = Relay::new(None)
        .await
        .map_err(|e| format!("Failed to create relay: {}", e))?;

    // Store client state
    let client_state = Arc::new(ClientState {
        relay,
        server_key: server_key.clone(),
    });

    *state.client.write().await = Some(client_state);
    *state.mode.write().await = AppMode::Client;

    Ok(())
}

/// Disconnect from server
#[tauri::command]
pub async fn disconnect(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let current_mode = *state.mode.read().await;
    if current_mode != AppMode::Client {
        return Err("Not connected as client".to_string());
    }

    *state.client.write().await = None;
    *state.mode.write().await = AppMode::Idle;
    state.cameras.write().await.clear();

    Ok(())
}

/// Get connection status
#[tauri::command]
pub async fn get_connection_status(
    state: State<'_, Arc<AppState>>,
) -> Result<ConnectionStatus, String> {
    let client = state.client.read().await;

    Ok(ConnectionStatus {
        connected: client.is_some(),
        server_key: client.as_ref().map(|c| c.server_key.clone()),
    })
}

/// Get list of available cameras
#[tauri::command]
pub async fn get_cameras(state: State<'_, Arc<AppState>>) -> Result<Vec<CameraInfo>, String> {
    let cameras = state.cameras.read().await;
    Ok(cameras.clone())
}

/// Get current app mode
#[tauri::command]
pub async fn get_app_mode(state: State<'_, Arc<AppState>>) -> Result<AppMode, String> {
    let mode = state.mode.read().await;
    Ok(*mode)
}

/// Get application settings
#[tauri::command]
pub async fn get_settings(state: State<'_, Arc<AppState>>) -> Result<AppSettings, String> {
    let settings = state.settings.read().await;
    Ok(settings.clone())
}

/// Update application settings
#[tauri::command]
pub async fn update_settings(
    buffer_size: Option<usize>,
    auto_reconnect: Option<bool>,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    let mut settings = state.settings.write().await;

    if let Some(size) = buffer_size {
        settings.buffer_size = size;
    }
    if let Some(reconnect) = auto_reconnect {
        settings.auto_reconnect = reconnect;
    }

    Ok(())
}
