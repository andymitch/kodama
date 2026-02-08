//! Application state management

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use serde::{Deserialize, Serialize};
use iroh::PublicKey;

use kodama_relay::Relay;
use kodama_server::{Router, RouterHandle, StorageManager};

/// Application mode
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AppMode {
    /// Not connected to anything
    Idle,
    /// Running as a server (self-hosting)
    Server,
    /// Connected to an external server as client
    Client,
}

/// Server state when running in server mode
pub struct ServerState {
    pub relay: Relay,
    pub router: Router,
    pub handle: RouterHandle,
    pub storage: Option<StorageManager>,
}

/// Client state when connected to external server
pub struct ClientState {
    pub relay: Relay,
    pub server_key: PublicKey,
}

/// Camera information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CameraInfo {
    pub id: String,
    pub name: String,
    pub connected: bool,
    pub last_frame_time: Option<u64>,
    /// Generation counter to prevent stale disconnection handlers from
    /// removing a newer connection's entry (same race as router peer map).
    #[serde(skip)]
    pub generation: u64,
}

/// Storage configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageConfig {
    pub enabled: bool,
    pub path: Option<PathBuf>,
    pub max_gb: u64,
    pub retention_days: u32,
    pub keyframes_only: bool,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            path: None,
            max_gb: 10,
            retention_days: 7,
            keyframes_only: false,
        }
    }
}

/// Application settings
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    /// Default mode on startup
    pub default_mode: AppMode,
    /// Server key to connect to in client mode
    pub server_key: Option<String>,
    /// Storage configuration
    pub storage: StorageConfig,
    /// Buffer size for broadcast
    pub buffer_size: usize,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            default_mode: AppMode::Idle,
            server_key: None,
            storage: StorageConfig::default(),
            buffer_size: 512,
        }
    }
}

/// Main application state
pub struct AppState {
    pub mode: RwLock<AppMode>,
    pub server: RwLock<Option<Arc<ServerState>>>,
    pub client: RwLock<Option<Arc<ClientState>>>,
    pub settings: RwLock<AppSettings>,
    pub cameras: Arc<RwLock<Vec<CameraInfo>>>,
    pub camera_generation: Arc<AtomicU64>,
    pub accept_task: RwLock<Option<JoinHandle<()>>>,
    pub receive_task: RwLock<Option<JoinHandle<()>>>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            mode: RwLock::new(AppMode::Idle),
            server: RwLock::new(None),
            client: RwLock::new(None),
            settings: RwLock::new(AppSettings::default()),
            cameras: Arc::new(RwLock::new(Vec::new())),
            camera_generation: Arc::new(AtomicU64::new(0)),
            accept_task: RwLock::new(None),
            receive_task: RwLock::new(None),
        }
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}
