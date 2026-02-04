//! Server module for routing frames to clients and storage
//!
//! The server is the authoritative core that:
//! - Accepts connections from cameras and clients
//! - Routes frames from cameras to subscribed clients
//! - Manages client connections and permissions
//! - Optionally stores frames to local or cloud storage

mod router;
mod clients;
mod storage;

pub use router::{Router, RouterHandle, RouterStats, PeerRole};
pub use clients::{ClientManager, ClientPermissions, ClientInfo};
pub use storage::{StorageManager, StorageConfig, StorageStats};
