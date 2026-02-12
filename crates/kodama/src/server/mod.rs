//! Server module for routing frames to clients and storage
//!
//! The server is the authoritative core that:
//! - Accepts connections from cameras and clients
//! - Routes frames from cameras to subscribed clients
//! - Manages client connections and permissions
//! - Optionally stores frames to local or cloud storage

mod router;
mod clients;
mod rate_limit;
mod storage;

pub use router::{Router, RouterHandle, RouterStats, PeerRole, PeerDetail};
pub use rate_limit::RateLimitConfig;
pub use clients::{ClientManager, ClientPermissions, ClientInfo};
pub use storage::{StorageManager, StorageConfig, StorageStats};

// Re-export transport and storage types for convenience
pub use crate::transport::{Relay, RelayConnection, FrameReceiver, FrameSender};
pub use crate::transport::{CommandSender, CommandReceiver, CommandStream};
pub use crate::transport::{ClientCommandSender, ClientCommandReceiver, ClientCommandStream};
pub use crate::storage::{StorageBackend, LocalStorage, LocalStorageConfig, CloudStorage, CloudStorageConfig};
