//! Yurei - Privacy-focused P2P security camera system
//!
//! This crate provides everything needed to build a Yurei camera system:
//! - Core types: frames, channels, identity
//! - Capture: video, audio, telemetry from cameras
//! - Relay: Iroh P2P transport and frame muxing
//! - Server: routing frames to clients and storage
//! - Storage: local and cloud persistence
//!
//! # Architecture
//!
//! Yurei is built on three core modules:
//!
//! 1. **Camera Module** - Captures video, audio, and telemetry
//! 2. **Relay Module** - Muxes channels, handles Iroh transport
//! 3. **Server Module** - Routes streams to clients and storage
//!
//! # Example - Camera
//!
//! ```ignore
//! use yurei::{Relay, Frame, SourceId, h264};
//!
//! let relay = Relay::new(Some(Path::new("./camera.key"))).await?;
//! let conn = relay.connect(server_key).await?;
//! let sender = conn.open_frame_stream().await?;
//!
//! while let Some(payload) = video_rx.recv().await {
//!     let frame = Frame::video(source_id, payload, h264::contains_keyframe(&payload));
//!     sender.send(&frame).await?;
//! }
//! ```
//!
//! # Example - Server
//!
//! ```ignore
//! use yurei::{Relay, Router};
//!
//! let relay = Relay::new(Some(Path::new("./server.key"))).await?;
//! let router = Router::new(64);
//!
//! while let Some(conn) = relay.accept().await {
//!     // Handle camera or client connection
//! }
//! ```

// Core types and protocol
pub mod core;

// Sensor capture (includes H.264 parsing)
pub mod capture;

// P2P transport and muxing
pub mod relay;

// Server routing and management
pub mod server;

// Storage backends
pub mod storage;

// ============================================================================
// Re-exports for convenience
// ============================================================================

// Core types
pub use core::{Channel, Frame, FrameFlags, SourceId};
pub use core::{Identity, KeyPair};
pub use core::{ALPN, DEFAULT_CAMERA_PORT, DEFAULT_SERVER_PORT, FRAME_HEADER_SIZE, MAX_FRAME_PAYLOAD_SIZE};

// Capture
pub use capture::{VideoCapture, VideoCaptureConfig};
#[cfg(feature = "test-source")]
pub use capture::{start_test_source, TestSourceConfig};

// Relay
pub use relay::{Relay, RelayConnection};
pub use relay::{YureiEndpoint, FrameSender, FrameReceiver, FrameStream};
pub use relay::{FrameBuffer, BackpressureSender, BufferStats};

// Server
pub use server::{Router, RouterHandle, RouterStats, PeerRole};
pub use server::{ClientManager, StorageManager};

// Storage
pub use storage::{StorageBackend, LocalStorage, CloudStorage};
