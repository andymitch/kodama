//! yurei-server: Server logic for Yurei
//!
//! This crate provides:
//! - Frame routing from cameras to clients
//! - Client connection management
//! - Camera registration and tracking
//!
//! # Architecture
//!
//! The server uses a broadcast channel for frame distribution:
//! - Cameras connect and send frames via unidirectional streams
//! - Clients subscribe to the frame broadcast
//! - Frames are routed from cameras to all subscribed clients
//!
//! # Example
//!
//! ```ignore
//! use yurei_server::Router;
//! use yurei_relay::Relay;
//!
//! let relay = Relay::new(Some(Path::new("./server.key"))).await?;
//! let router = Router::new(64); // Buffer 64 frames
//!
//! // Accept connections
//! while let Some(conn) = relay.accept().await {
//!     let router = router.clone();
//!     tokio::spawn(async move {
//!         // The router determines role based on first message/stream direction
//!         router.handle_connection(conn).await;
//!     });
//! }
//! ```

mod router;

pub use router::{Router, RouterHandle, PeerRole};
