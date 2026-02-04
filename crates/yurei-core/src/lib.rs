//! yurei-core: Core types, frame format, and protocol constants for Yurei
//!
//! This crate provides the fundamental types shared across all Yurei components:
//! - Frame format for video, audio, and telemetry data
//! - Source identification (camera/device IDs)
//! - Protocol constants (ALPN, ports)

mod frame;
mod protocol;

pub use frame::{Channel, Frame, FrameFlags, SourceId};
pub use protocol::*;
