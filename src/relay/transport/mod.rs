//! Transport layer for Iroh P2P connections
//!
//! Provides endpoint management and connection handling over Iroh/QUIC.

mod endpoint;
mod connection;

pub use endpoint::YureiEndpoint;
pub use connection::{FrameSender, FrameReceiver, FrameStream};
