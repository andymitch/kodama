//! Core types and protocol definitions for Kodama
//!
//! This crate contains foundational types used throughout the system:
//! - Frame types for video, audio, and telemetry
//! - Identity management (keys, NodeIds)
//! - Protocol constants

mod command;
mod frame;
mod identity;
mod protocol;

pub use command::*;
pub use frame::{Channel, Frame, FrameFlags, SourceId};
pub use identity::{Identity, KeyPair};
pub use protocol::*;
