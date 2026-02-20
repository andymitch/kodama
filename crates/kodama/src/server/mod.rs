//! Server module for routing frames to clients and storage
//!
//! The server is the authoritative core that:
//! - Accepts connections from cameras and clients
//! - Routes frames from cameras to subscribed clients
//! - Manages client connections and permissions
//! - Optionally stores frames to local or cloud storage

mod rate_limit;
mod router;
mod storage;

pub use rate_limit::RateLimitConfig;
pub use router::{PeerDetail, PeerRole, Router, RouterHandle, RouterStats};
pub use storage::{StorageConfig, StorageManager, StorageStats};

// Re-export transport and storage types for convenience
pub use crate::storage::{
    CloudStorage, CloudStorageConfig, LocalStorage, LocalStorageConfig, StorageBackend,
};
pub use crate::transport::{ClientCommandReceiver, ClientCommandSender, ClientCommandStream};
pub use crate::transport::{CommandReceiver, CommandSender, CommandStream};
pub use crate::transport::{FrameReceiver, FrameSender, Relay, RelayConnection};
