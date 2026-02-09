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
use std::sync::Arc;

use kodama_core::Frame;
use mux::frame::{read_frame, write_frame};

// Re-export key types
pub use transport::{KodamaEndpoint, FrameSender, FrameReceiver, FrameStream};
pub use transport::{CommandSender, CommandReceiver, CommandStream};
pub use transport::{ClientCommandSender, ClientCommandReceiver, ClientCommandStream};
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

    /// Get this relay's public key as a base32 string
    ///
    /// This is the format used for sharing with users.
    pub fn public_key_base32(&self) -> String {
        self.endpoint.public_key().to_string()
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
///
/// Uses `Arc<Connection>` internally so it can be cloned cheaply
/// (e.g. to use frame stream and command stream concurrently).
pub struct RelayConnection {
    conn: Arc<Connection>,
}

impl RelayConnection {
    pub(crate) fn new(conn: Connection) -> Self {
        Self { conn: Arc::new(conn) }
    }

    /// Create a RelayConnection from a raw iroh Connection.
    ///
    /// Useful for integration tests that need direct addressing
    /// without going through Relay's discovery-based connect.
    pub fn from_connection(conn: Connection) -> Self {
        Self::new(conn)
    }

    /// Create a cheap clone of this connection handle.
    ///
    /// Both handles share the same underlying QUIC connection.
    /// Use this when you need to open multiple stream types concurrently
    /// (e.g. frame stream + command stream).
    pub fn clone_handle(&self) -> Self {
        Self {
            conn: Arc::clone(&self.conn),
        }
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

    // ========== Command stream API ==========

    /// Open a bidirectional command stream to a camera.
    ///
    /// The server opens this to the camera; the camera accepts it.
    pub async fn open_command_stream(&self) -> Result<CommandStream> {
        let (send, recv) = self.conn.open_bi().await?;
        Ok(CommandStream::new(send, recv))
    }

    /// Accept a bidirectional command stream.
    ///
    /// The camera accepts this from the server.
    pub async fn accept_command_stream(&self) -> Result<CommandStream> {
        let (send, recv) = self.conn.accept_bi().await?;
        Ok(CommandStream::new(send, recv))
    }

    /// Open a client command stream (for client <-> server communication).
    pub async fn open_client_command_stream(&self) -> Result<ClientCommandStream> {
        let (send, recv) = self.conn.open_bi().await?;
        Ok(ClientCommandStream::new(send, recv))
    }

    /// Accept a client command stream.
    pub async fn accept_client_command_stream(&self) -> Result<ClientCommandStream> {
        let (send, recv) = self.conn.accept_bi().await?;
        Ok(ClientCommandStream::new(send, recv))
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
    use bytes::Bytes;
    use kodama_core::{Channel, FrameFlags, SourceId, ALPN};
    use std::sync::Arc;

    #[tokio::test]
    async fn test_relay_creation() {
        let relay = Relay::new(None).await.unwrap();
        let pk = relay.public_key();
        assert_eq!(pk.as_bytes().len(), 32);
    }

    #[tokio::test]
    async fn relay_ephemeral_keys_are_unique() {
        let r1 = Relay::new(None).await.unwrap();
        let r2 = Relay::new(None).await.unwrap();
        assert_ne!(r1.public_key(), r2.public_key());
    }

    #[tokio::test]
    async fn relay_persistent_key_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("test.key");

        let r1 = Relay::new(Some(&key_path)).await.unwrap();
        let pk1 = r1.public_key();
        r1.close().await;

        // Reload from same path — should get the same key
        let r2 = Relay::new(Some(&key_path)).await.unwrap();
        let pk2 = r2.public_key();
        r2.close().await;

        assert_eq!(pk1, pk2, "Persistent key should survive reload");
    }

    #[tokio::test]
    async fn relay_public_key_base32_roundtrip() {
        let relay = Relay::new(None).await.unwrap();
        let base32 = relay.public_key_base32();
        assert!(!base32.is_empty());
        // Base32 of a 32-byte key should parse back
        let parsed: PublicKey = base32.parse().unwrap();
        assert_eq!(parsed, relay.public_key());
    }

    #[tokio::test]
    async fn relay_key_file_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("perm.key");

        let _r = Relay::new(Some(&key_path)).await.unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::metadata(&key_path).unwrap().permissions();
            assert_eq!(perms.mode() & 0o777, 0o600, "Key file should be 0600");
        }
    }

    fn test_frame() -> Frame {
        Frame {
            source: SourceId::new([1, 2, 3, 4, 5, 6, 7, 8]),
            channel: Channel::Video,
            flags: FrameFlags::keyframe(),
            timestamp_us: 12345,
            payload: Bytes::from_static(b"hello"),
        }
    }

    /// Helper: create two connected relays and return (server_relay, client_relay, server_conn, client_conn).
    /// Both relays are wrapped in Arc to keep endpoints alive for the duration of the test.
    async fn connected_pair() -> (Arc<Relay>, Arc<Relay>, RelayConnection, RelayConnection) {
        let server = Arc::new(Relay::new(None).await.unwrap());
        let client = Arc::new(Relay::new(None).await.unwrap());
        let server_addr = server.endpoint().endpoint().addr();

        let server_clone = Arc::clone(&server);
        let accept_task = tokio::spawn(async move {
            server_clone.endpoint().accept().await.unwrap()
        });

        let client_ep = client.endpoint().endpoint().clone();
        let connect_task = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            client_ep.connect(server_addr, ALPN).await.unwrap()
        });

        let server_conn = RelayConnection::from_connection(accept_task.await.unwrap());
        let client_conn = RelayConnection::from_connection(connect_task.await.unwrap());

        (server, client, server_conn, client_conn)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn relay_connect_and_send_frame() {
        let (_server, _client, server_conn, client_conn) = connected_pair().await;

        client_conn.send_frame(&test_frame()).await.unwrap();

        let frame = server_conn.recv_frame().await.unwrap();
        assert_eq!(frame.timestamp_us, 12345);
        assert_eq!(frame.payload, Bytes::from_static(b"hello"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn persistent_frame_stream_multiple_frames() {
        let (_server, _client, server_conn, client_conn) = connected_pair().await;

        // Server must accept concurrently (QUIC streams are lazy until data flows)
        let server_task = tokio::spawn(async move {
            let receiver = server_conn.accept_frame_stream().await.unwrap();
            let mut count = 0;
            while let Ok(Some(frame)) = receiver.recv().await {
                assert_eq!(frame.channel, Channel::Video);
                count += 1;
            }
            count
        });

        let sender = client_conn.open_frame_stream().await.unwrap();
        for i in 0..5u64 {
            let frame = Frame {
                source: SourceId::new([0; 8]),
                channel: Channel::Video,
                flags: FrameFlags::default(),
                timestamp_us: i,
                payload: Bytes::from(vec![0xAB; 100]),
            };
            sender.send(&frame).await.unwrap();
        }
        sender.finish().await.unwrap();

        let count = server_task.await.unwrap();
        assert_eq!(count, 5);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn bidirectional_frame_stream() {
        let (_server, _client, server_conn, client_conn) = connected_pair().await;

        // clone_handle keeps the connection alive after the server task drops its copy
        let server_handle = server_conn.clone_handle();
        let server_task = tokio::spawn(async move {
            let stream = server_handle.accept_bi_stream().await.unwrap();
            let frame = stream.recv().await.unwrap().unwrap();
            assert_eq!(frame.timestamp_us, 1);

            let pong = Frame {
                source: SourceId::new([0; 8]),
                channel: Channel::Video,
                flags: FrameFlags::default(),
                timestamp_us: 2,
                payload: Bytes::from_static(b"pong"),
            };
            stream.send(&pong).await.unwrap();
        });

        let client_stream = client_conn.open_bi_stream().await.unwrap();
        let ping = Frame {
            source: SourceId::new([0; 8]),
            channel: Channel::Video,
            flags: FrameFlags::default(),
            timestamp_us: 1,
            payload: Bytes::from_static(b"ping"),
        };
        client_stream.send(&ping).await.unwrap();

        let response = client_stream.recv().await.unwrap().unwrap();
        assert_eq!(response.timestamp_us, 2);
        assert_eq!(response.payload, Bytes::from_static(b"pong"));

        server_task.await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn clone_handle_concurrent_streams() {
        let (_server, _client, server_conn, client_conn) = connected_pair().await;
        let client_handle2 = client_conn.clone_handle();

        let server_task = tokio::spawn(async move {
            let rx1 = server_conn.accept_frame_stream().await.unwrap();
            let rx2 = server_conn.accept_frame_stream().await.unwrap();
            let r1 = rx1.recv().await.unwrap().unwrap();
            let r2 = rx2.recv().await.unwrap().unwrap();
            let mut timestamps = vec![r1.timestamp_us, r2.timestamp_us];
            timestamps.sort();
            timestamps
        });

        let tx1 = client_conn.open_frame_stream().await.unwrap();
        let tx2 = client_handle2.open_frame_stream().await.unwrap();

        let f1 = Frame {
            source: SourceId::new([0; 8]),
            channel: Channel::Video,
            flags: FrameFlags::default(),
            timestamp_us: 100,
            payload: Bytes::from_static(b"stream1"),
        };
        let f2 = Frame {
            source: SourceId::new([0; 8]),
            channel: Channel::Audio,
            flags: FrameFlags::default(),
            timestamp_us: 200,
            payload: Bytes::from_static(b"stream2"),
        };

        tx1.send(&f1).await.unwrap();
        tx2.send(&f2).await.unwrap();
        tx1.finish().await.unwrap();
        tx2.finish().await.unwrap();

        let timestamps = server_task.await.unwrap();
        assert_eq!(timestamps, vec![100, 200]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn connection_close_detected_by_peer() {
        let (_server, _client, server_conn, client_conn) = connected_pair().await;

        let server_task = tokio::spawn(async move {
            // Wait for the client to close, then try to accept
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            server_conn.accept_frame_stream().await
        });

        // Client closes the connection
        client_conn.close(42, b"done");

        let result = server_task.await.unwrap();
        assert!(result.is_err(), "Should fail after peer closes connection");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn remote_public_key_matches() {
        let (server, client, server_conn, client_conn) = connected_pair().await;

        assert_eq!(server_conn.remote_public_key(), client.public_key());
        assert_eq!(client_conn.remote_public_key(), server.public_key());
    }
}
