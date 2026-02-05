//! Relay module for Iroh transport and frame muxing/demuxing
//!
//! The relay module handles:
//! - Iroh P2P transport (endpoints, connections)
//! - Frame multiplexing from multiple sources
//! - Frame demultiplexing for routing
//!
//! A relay can be embedded in a camera (typical), run as a standalone
//! gateway, or embedded in a server.

pub mod transport;
pub mod mux;

use anyhow::Result;
use iroh::endpoint::Connection;
use iroh::PublicKey;
use std::path::Path;

use kodama_core::Frame;
use mux::frame::{read_frame, write_frame};

// Re-export key types
pub use transport::{KodamaEndpoint, FrameSender, FrameReceiver, FrameStream};
pub use mux::{FrameBuffer, BackpressureSender, BufferStats, Demuxer};

/// High-level relay interface for sending/receiving frames over Iroh
pub struct Relay {
    endpoint: KodamaEndpoint,
}

impl Relay {
    /// Create a new relay endpoint
    ///
    /// If `key_path` is provided, the secret key is loaded from or saved to that path.
    /// If `key_path` is None, an ephemeral key is generated.
    pub async fn new(key_path: Option<&Path>) -> Result<Self> {
        let endpoint = KodamaEndpoint::new(key_path).await?;
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

    /// Get the underlying KodamaEndpoint for advanced usage
    pub fn endpoint(&self) -> &KodamaEndpoint {
        &self.endpoint
    }
}

/// A connection to a remote relay endpoint
pub struct RelayConnection {
    conn: Connection,
}

impl RelayConnection {
    pub(crate) fn new(conn: Connection) -> Self {
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
