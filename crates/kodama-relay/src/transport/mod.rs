//! Transport layer for Iroh P2P connections
//!
//! Provides endpoint management and connection handling over Iroh/QUIC.

mod endpoint;
mod connection;

pub use endpoint::KodamaEndpoint;
pub use connection::{FrameSender, FrameReceiver, FrameStream};
pub use connection::{CommandSender, CommandReceiver, CommandStream};
pub use connection::{ClientCommandSender, ClientCommandReceiver, ClientCommandStream};
