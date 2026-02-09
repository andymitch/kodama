//! Application state management for Kodama Mobile

use kodama_transport::Relay;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;

/// Application operating mode
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AppMode {
    /// Not connected to anything
    Idle,
    /// Running as a server (hosting cameras)
    Server,
    /// Connected as a client (viewing only)
    Client,
}

impl Default for AppMode {
    fn default() -> Self {
        Self::Idle
    }
}

/// Server state when running in server mode
pub struct ServerState {
    pub relay: Relay,
    pub public_key: String,
}

/// Client state when connected to a remote server
pub struct ClientState {
    pub relay: Relay,
    pub server_key: String,
}

/// Camera information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CameraInfo {
    pub id: String,
    pub name: String,
    pub connected: bool,
}

/// Application settings
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    /// Buffer size for broadcast channel
    pub buffer_size: usize,
    /// Auto-reconnect on disconnect
    pub auto_reconnect: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            buffer_size: 64,
            auto_reconnect: true,
        }
    }
}

/// Main application state
pub struct AppState {
    pub mode: RwLock<AppMode>,
    pub server: RwLock<Option<Arc<ServerState>>>,
    pub client: RwLock<Option<Arc<ClientState>>>,
    pub settings: RwLock<AppSettings>,
    pub cameras: RwLock<Vec<CameraInfo>>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            mode: RwLock::new(AppMode::Idle),
            server: RwLock::new(None),
            client: RwLock::new(None),
            settings: RwLock::new(AppSettings::default()),
            cameras: RwLock::new(Vec::new()),
        }
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}
