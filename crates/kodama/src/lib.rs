//! Kodama - Privacy-focused P2P security camera system
//!
//! This is the unified library crate for Kodama. Feature flags control
//! which modules are compiled:
//!
//! - **Core types** (always available): Frame, Channel, SourceId, Identity, protocol constants
//! - **`transport`**: Iroh endpoint wrapper, frame mux/demux, connection types
//! - **`capture`**: Video/audio/telemetry capture, H.264 parsing, ABR
//! - **`storage`**: Local filesystem and cloud (S3/R2) storage backends (implies `transport`)
//! - **`server`**: Router, rate limiter, storage manager (implies `transport` + `storage`)
//! - **`test-source`**: Synthetic video/audio without hardware (implies `capture`)

// Core modules (always compiled)
mod command;
mod frame;
mod identity;
mod protocol;

pub use command::*;
pub use frame::{Channel, Frame, FrameFlags, SourceId};
pub use identity::{Identity, KeyPair};
pub use protocol::*;

// Transport: Iroh endpoint, connection, frame mux/demux
#[cfg(feature = "transport")]
pub mod transport;

// Capture: video, audio, telemetry, H.264, ABR
#[cfg(feature = "capture")]
pub mod capture;

// Storage: local filesystem, cloud (S3/R2)
#[cfg(feature = "storage")]
pub mod storage;

// Server: router, client manager, rate limiter, storage manager
#[cfg(feature = "server")]
pub mod server;

// Web: axum HTTP server, WebSocket bridge, fMP4 muxer
#[cfg(feature = "web")]
pub mod web;
