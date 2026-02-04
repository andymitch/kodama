//! Yurei - Privacy-focused P2P security camera system
//!
//! This crate provides everything needed to build a Yurei camera system:
//! - Frame types and protocol definitions
//! - Video capture (with optional test source)
//! - Iroh P2P transport and frame streaming
//! - Server routing for multi-client distribution
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

// Core types
mod frame;
mod protocol;

// Video capture
pub mod capture;
pub mod h264;

// P2P transport
mod transport;
mod wire;
mod stream;
mod buffer;
mod relay;

// Server
mod router;

// Re-exports
pub use frame::{Channel, Frame, FrameFlags, SourceId};
pub use protocol::*;

pub use capture::{VideoCapture, VideoCaptureConfig};
#[cfg(feature = "test-source")]
pub use capture::{start_test_source, TestSourceConfig};

pub use relay::{Relay, RelayConnection};
pub use stream::{FrameReceiver, FrameSender, FrameStream};
pub use buffer::{BackpressureSender, BufferStats, FrameBuffer};
pub use wire::{frame_from_bytes, frame_to_bytes, read_frame, write_frame};
pub use transport::YureiEndpoint;

pub use router::{PeerRole, Router, RouterHandle, RouterStats};
