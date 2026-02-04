//! yurei-relay: Mux/demux and Iroh transport layer for Yurei
//!
//! This crate provides:
//! - Iroh endpoint management and connection handling
//! - Frame serialization/deserialization over the wire
//! - High-level `Relay` API for sending/receiving frames
//!
//! # Example - Simple per-frame API
//!
//! ```ignore
//! use yurei_relay::Relay;
//!
//! let relay = Relay::new(Some(Path::new("./my.key"))).await?;
//! let conn = relay.connect(remote_public_key).await?;
//!
//! // Simple API: one stream per frame (convenient but has overhead)
//! conn.send_frame(&frame).await?;
//! let frame = conn.recv_frame().await?;
//! ```
//!
//! # Example - Persistent stream API (recommended for video)
//!
//! ```ignore
//! use yurei_relay::Relay;
//!
//! let relay = Relay::new(None).await?;
//! let conn = relay.connect(remote_public_key).await?;
//!
//! // Open a persistent stream for high-throughput streaming
//! let sender = conn.open_frame_stream().await?;
//! for frame in frames {
//!     sender.send(&frame).await?;
//! }
//! sender.finish().await?;
//!
//! // On the receiving side:
//! let receiver = conn.accept_frame_stream().await?;
//! while let Some(frame) = receiver.recv().await? {
//!     // Process frame...
//! }
//! ```

mod mux;
mod transport;

use anyhow::Result;
use iroh::endpoint::Connection;
use iroh::PublicKey;
use std::path::Path;
use yurei_core::Frame;

pub use mux::buffer::{BackpressureSender, BufferStats, FrameBuffer};
pub use mux::stream::{FrameReceiver, FrameSender, FrameStream};
pub use mux::wire::{frame_from_bytes, frame_to_bytes, read_frame, write_frame};
pub use transport::YureiEndpoint;

/// High-level relay interface for sending/receiving frames over Iroh
pub struct Relay {
    endpoint: YureiEndpoint,
}

impl Relay {
    /// Create a new relay endpoint
    ///
    /// If `key_path` is provided, the secret key is loaded from or saved to that path.
    /// If `key_path` is None, an ephemeral key is generated.
    pub async fn new(key_path: Option<&Path>) -> Result<Self> {
        let endpoint = YureiEndpoint::new(key_path).await?;
        Ok(Self { endpoint })
    }

    /// Get this relay's public key (EndpointId)
    ///
    /// Share this with peers so they can connect to you.
    pub fn public_key(&self) -> PublicKey {
        self.endpoint.public_key()
    }

    /// Connect to a remote endpoint by its public key
    ///
    /// Uses iroh's discovery services to locate the peer.
    pub async fn connect(&self, remote: PublicKey) -> Result<RelayConnection> {
        let conn = self.endpoint.connect(remote).await?;
        Ok(RelayConnection::new(conn))
    }

    /// Accept an incoming connection
    ///
    /// Returns None if the relay is closed.
    pub async fn accept(&self) -> Option<RelayConnection> {
        let conn = self.endpoint.accept().await?;
        Some(RelayConnection::new(conn))
    }

    /// Close the relay endpoint
    pub async fn close(&self) {
        self.endpoint.close().await;
    }

    /// Get the underlying YureiEndpoint for advanced usage
    pub fn endpoint(&self) -> &YureiEndpoint {
        &self.endpoint
    }
}

/// A connection to a remote relay endpoint
pub struct RelayConnection {
    conn: Connection,
}

impl RelayConnection {
    fn new(conn: Connection) -> Self {
        Self { conn }
    }

    /// Get the remote endpoint's public key
    pub fn remote_public_key(&self) -> PublicKey {
        self.conn.remote_id()
    }

    // ========== Simple per-frame API ==========

    /// Send a frame to the remote endpoint
    ///
    /// Opens a new unidirectional stream for each frame.
    /// Simple but has overhead at high frame rates.
    /// For video streaming, prefer `open_frame_stream()`.
    pub async fn send_frame(&self, frame: &Frame) -> Result<()> {
        let mut send = self.conn.open_uni().await?;
        write_frame(&mut send, frame).await?;
        send.finish()?;
        Ok(())
    }

    /// Receive a frame from the remote endpoint
    ///
    /// Accepts a new unidirectional stream and reads one frame.
    /// For video streaming, prefer `accept_frame_stream()`.
    pub async fn recv_frame(&self) -> Result<Frame> {
        let mut recv = self.conn.accept_uni().await?;
        read_frame(&mut recv).await
    }

    // ========== Persistent stream API (recommended for video) ==========

    /// Open a persistent unidirectional stream for sending frames
    ///
    /// More efficient than `send_frame()` for high-throughput streaming
    /// as it reuses the same QUIC stream for all frames.
    pub async fn open_frame_stream(&self) -> Result<FrameSender> {
        let send = self.conn.open_uni().await?;
        Ok(FrameSender::new(send))
    }

    /// Accept a persistent unidirectional stream for receiving frames
    ///
    /// More efficient than `recv_frame()` for high-throughput streaming.
    pub async fn accept_frame_stream(&self) -> Result<FrameReceiver> {
        let recv = self.conn.accept_uni().await?;
        Ok(FrameReceiver::new(recv))
    }

    /// Open a bidirectional frame stream
    ///
    /// Useful for request/response patterns where both sides send frames.
    pub async fn open_bi_stream(&self) -> Result<FrameStream> {
        let (send, recv) = self.conn.open_bi().await?;
        Ok(FrameStream::new(send, recv))
    }

    /// Accept a bidirectional frame stream
    pub async fn accept_bi_stream(&self) -> Result<FrameStream> {
        let (send, recv) = self.conn.accept_bi().await?;
        Ok(FrameStream::new(send, recv))
    }

    // ========== Connection management ==========

    /// Get the underlying QUIC connection for advanced usage
    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    /// Close the connection
    pub fn close(&self, error_code: u32, reason: &[u8]) {
        self.conn.close(error_code.into(), reason);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_relay_creation() {
        let relay = Relay::new(None).await.unwrap();
        let pk = relay.public_key();
        assert_eq!(pk.as_bytes().len(), 32);
    }
}
