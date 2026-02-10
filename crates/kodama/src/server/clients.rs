//! Client connection management (stub)
//!
//! Future: Manages authenticated client connections and permissions.

use anyhow::Result;
use iroh::PublicKey;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Client permissions
#[derive(Debug, Clone, Default)]
pub struct ClientPermissions {
    /// Can view live streams
    pub can_view_live: bool,
    /// Can view recordings
    pub can_view_recordings: bool,
    /// Can control PTZ (if supported)
    pub can_control_ptz: bool,
    /// Can manage cameras
    pub can_manage_cameras: bool,
}

impl ClientPermissions {
    /// Full access permissions
    pub fn full() -> Self {
        Self {
            can_view_live: true,
            can_view_recordings: true,
            can_control_ptz: true,
            can_manage_cameras: true,
        }
    }

    /// View-only permissions
    pub fn view_only() -> Self {
        Self {
            can_view_live: true,
            can_view_recordings: true,
            can_control_ptz: false,
            can_manage_cameras: false,
        }
    }
}

/// Information about a connected client
#[derive(Debug, Clone)]
pub struct ClientInfo {
    /// Client's public key
    pub public_key: PublicKey,
    /// Client's permissions
    pub permissions: ClientPermissions,
    /// When the client connected
    pub connected_at: std::time::Instant,
}

/// Manages client connections and permissions
pub struct ClientManager {
    /// Connected clients
    clients: Arc<RwLock<HashMap<PublicKey, ClientInfo>>>,
    /// Default permissions for new clients
    default_permissions: ClientPermissions,
}

impl ClientManager {
    /// Create a new client manager
    pub fn new() -> Self {
        Self {
            clients: Arc::new(RwLock::new(HashMap::new())),
            default_permissions: ClientPermissions::view_only(),
        }
    }

    /// Create with custom default permissions
    pub fn with_default_permissions(permissions: ClientPermissions) -> Self {
        Self {
            clients: Arc::new(RwLock::new(HashMap::new())),
            default_permissions: permissions,
        }
    }

    /// Register a new client
    pub async fn register(&self, public_key: PublicKey) -> ClientInfo {
        let info = ClientInfo {
            public_key,
            permissions: self.default_permissions.clone(),
            connected_at: std::time::Instant::now(),
        };

        self.clients.write().await.insert(public_key, info.clone());
        info
    }

    /// Unregister a client
    pub async fn unregister(&self, public_key: &PublicKey) {
        self.clients.write().await.remove(public_key);
    }

    /// Get client info
    pub async fn get(&self, public_key: &PublicKey) -> Option<ClientInfo> {
        self.clients.read().await.get(public_key).cloned()
    }

    /// Check if client has a specific permission
    pub async fn check_permission(
        &self,
        public_key: &PublicKey,
        check: impl Fn(&ClientPermissions) -> bool,
    ) -> bool {
        self.clients
            .read()
            .await
            .get(public_key)
            .map(|c| check(&c.permissions))
            .unwrap_or(false)
    }

    /// Update client permissions
    pub async fn update_permissions(
        &self,
        public_key: &PublicKey,
        permissions: ClientPermissions,
    ) -> Result<()> {
        let mut clients = self.clients.write().await;
        if let Some(client) = clients.get_mut(public_key) {
            client.permissions = permissions;
            Ok(())
        } else {
            anyhow::bail!("Client not found")
        }
    }

    /// Get number of connected clients
    pub async fn count(&self) -> usize {
        self.clients.read().await.len()
    }

    /// List all connected clients
    pub async fn list(&self) -> Vec<ClientInfo> {
        self.clients.read().await.values().cloned().collect()
    }
}

impl Default for ClientManager {
    fn default() -> Self {
        Self::new()
    }
}
