//! Core types and protocol definitions for Kodama
//!
//! This module contains foundational types used throughout the system:
//! - Frame types for video, audio, and telemetry
//! - Identity management (keys, NodeIds)
//! - Protocol constants

mod frame;
mod identity;
mod protocol;

pub use frame::{Channel, Frame, FrameFlags, SourceId};
pub use identity::{Identity, KeyPair};
pub use protocol::*;
