//! Kodama Relay Binary
//!
//! Lightweight relay server for frame forwarding and NAT traversal assistance.
//! Unlike the full server, the relay doesn't store frames - it just forwards them.
//!
//! Use cases:
//! - Edge deployment where a lightweight forwarder is needed
//! - NAT traversal assistance for cameras behind restrictive firewalls
//! - Multi-site deployments where cameras connect to a local relay
//!
//! ## Usage
//!
//! ```bash
//! # Run the relay
//! kodama-relay
//!
//! # With custom key path
//! KODAMA_KEY_PATH=./relay.key kodama-relay
//!
//! # Forward to an upstream server
//! KODAMA_UPSTREAM_KEY=<server-public-key> kodama-relay
//!
//! # With verbose logging
//! RUST_LOG=kodama=debug kodama-relay
//! ```
//!
//! ## Modes
//!
//! 1. **Standalone mode** (default): Acts as a mini-server, routing frames to connected clients
//! 2. **Upstream mode**: Forwards frames to a configured upstream server while also serving local clients

use anyhow::Result;
use iroh::PublicKey;
use std::collections::HashMap;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};
use tokio::time::{interval, Duration, Instant};
use tracing::{debug, error, info, warn};
use kodama::{Frame, Relay, RelayConnection, FrameReceiver};

/// Relay configuration from environment
struct Config {
    /// Path to store our secret key
    key_path: PathBuf,
    /// Frame buffer capacity for broadcast
    buffer_capacity: usize,
    /// Optional upstream server to forward frames to
    upstream_key: Option<PublicKey>,
}

impl Config {
    fn from_env() -> Result<Self> {
        let key_path = std::env::var("KODAMA_KEY_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("./relay.key"));

        let buffer_capacity: usize = std::env::var("KODAMA_BUFFER_SIZE")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(64);

        let upstream_key = std::env::var("KODAMA_UPSTREAM_KEY")
            .ok()
            .and_then(|s| PublicKey::from_str(&s).ok());

        Ok(Self {
            key_path,
            buffer_capacity,
            upstream_key,
        })
    }
}

/// Lightweight frame router for the relay
struct FrameRouter {
    /// Broadcast channel for distributing frames
    tx: broadcast::Sender<Frame>,
    /// Connected peers
    peers: RwLock<HashMap<PublicKey, PeerInfo>>,
    /// Statistics
    stats: RwLock<RelayStats>,
}

#[derive(Debug, Clone, Copy)]
enum PeerRole {
    Camera,
    Client,
}

#[derive(Debug)]
struct PeerInfo {
    role: PeerRole,
    connected_at: Instant,
}

#[derive(Debug, Default)]
struct RelayStats {
    cameras_connected: usize,
    clients_connected: usize,
    frames_forwarded: u64,
    bytes_forwarded: u64,
}

impl FrameRouter {
    fn new(buffer_capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(buffer_capacity);
        Self {
            tx,
            peers: RwLock::new(HashMap::new()),
            stats: RwLock::new(RelayStats::default()),
        }
    }

    async fn register_peer(&self, key: PublicKey, role: PeerRole) {
        let mut peers = self.peers.write().await;
        let mut stats = self.stats.write().await;

        peers.insert(key, PeerInfo {
            role,
            connected_at: Instant::now(),
        });

        match role {
            PeerRole::Camera => stats.cameras_connected += 1,
            PeerRole::Client => stats.clients_connected += 1,
        }

        info!(?role, peer = %key, "Peer connected");
    }

    async fn unregister_peer(&self, key: PublicKey) {
        let mut peers = self.peers.write().await;
        let mut stats = self.stats.write().await;

        if let Some(info) = peers.remove(&key) {
            match info.role {
                PeerRole::Camera => stats.cameras_connected = stats.cameras_connected.saturating_sub(1),
                PeerRole::Client => stats.clients_connected = stats.clients_connected.saturating_sub(1),
            }
            let duration = info.connected_at.elapsed();
            info!(?info.role, peer = %key, duration_secs = duration.as_secs(), "Peer disconnected");
        }
    }

    async fn forward_frame(&self, frame: Frame) {
        let payload_len = frame.payload.len();

        match self.tx.send(frame) {
            Ok(n) => {
                let mut stats = self.stats.write().await;
                stats.frames_forwarded += 1;
                stats.bytes_forwarded += payload_len as u64;
                debug!(subscribers = n, "Frame forwarded");
            }
            Err(_) => {
                debug!("No subscribers for frame");
            }
        }
    }

    fn subscribe(&self) -> broadcast::Receiver<Frame> {
        self.tx.subscribe()
    }

    async fn stats(&self) -> RelayStats {
        let stats = self.stats.read().await;
        RelayStats {
            cameras_connected: stats.cameras_connected,
            clients_connected: stats.clients_connected,
            frames_forwarded: stats.frames_forwarded,
            bytes_forwarded: stats.bytes_forwarded,
        }
    }

    /// Handle a camera connection - receive frames and forward them
    async fn handle_camera(&self, remote: PublicKey, receiver: FrameReceiver) -> Result<()> {
        self.register_peer(remote, PeerRole::Camera).await;

        loop {
            match receiver.recv().await {
                Ok(Some(frame)) => {
                    self.forward_frame(frame).await;
                }
                Ok(None) => {
                    info!(camera = %remote, "Camera stream closed");
                    break;
                }
                Err(e) => {
                    warn!(camera = %remote, error = %e, "Camera stream error");
                    break;
                }
            }
        }

        self.unregister_peer(remote).await;
        Ok(())
    }

    /// Handle a client connection - subscribe and send frames
    async fn handle_client(&self, conn: RelayConnection) -> Result<()> {
        let remote = conn.remote_public_key();
        self.register_peer(remote, PeerRole::Client).await;

        let mut rx = self.subscribe();
        let sender = conn.open_frame_stream().await?;

        loop {
            match rx.recv().await {
                Ok(frame) => {
                    if let Err(e) = sender.send(&frame).await {
                        warn!(client = %remote, error = %e, "Failed to send frame to client");
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!(client = %remote, missed = n, "Client lagged, missed frames");
                }
                Err(broadcast::error::RecvError::Closed) => {
                    info!(client = %remote, "Frame broadcast closed");
                    break;
                }
            }
        }

        self.unregister_peer(remote).await;
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("kodama=info".parse().unwrap()),
        )
        .init();

    let config = Config::from_env()?;

    info!("Kodama Relay starting");
    info!("  Key path: {:?}", config.key_path);
    info!("  Buffer capacity: {}", config.buffer_capacity);

    if let Some(ref upstream) = config.upstream_key {
        info!("  Upstream server: {}", upstream);
    } else {
        info!("  Mode: standalone (no upstream)");
    }

    // Create parent directory for key if needed
    if let Some(parent) = config.key_path.parent() {
        if !parent.exists() && !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }

    // Initialize relay endpoint
    let relay = Relay::new(Some(&config.key_path)).await?;
    info!("Relay PublicKey: {}", relay.public_key());
    info!("");
    info!("Share this key with cameras (KODAMA_SERVER_KEY)");
    info!("and clients to connect.");
    info!("");

    // Create frame router
    let router = Arc::new(FrameRouter::new(config.buffer_capacity));

    // If upstream server is configured, connect and forward frames
    if let Some(upstream_key) = config.upstream_key {
        let router_clone = Arc::clone(&router);

        tokio::spawn(async move {
            loop {
                info!("Connecting to upstream server: {}", upstream_key);

                // Create a separate relay for upstream connection
                let upstream_relay = match Relay::new(None).await {
                    Ok(r) => r,
                    Err(e) => {
                        error!("Failed to create upstream relay: {}", e);
                        tokio::time::sleep(Duration::from_secs(5)).await;
                        continue;
                    }
                };

                match upstream_relay.connect(upstream_key).await {
                    Ok(conn) => {
                        info!("Connected to upstream server");

                        // Forward frames to upstream
                        let mut rx = router_clone.subscribe();
                        let sender = match conn.open_frame_stream().await {
                            Ok(s) => s,
                            Err(e) => {
                                error!("Failed to open upstream stream: {}", e);
                                tokio::time::sleep(Duration::from_secs(5)).await;
                                continue;
                            }
                        };

                        loop {
                            match rx.recv().await {
                                Ok(frame) => {
                                    if let Err(e) = sender.send(&frame).await {
                                        error!("Upstream send error: {}", e);
                                        break;
                                    }
                                }
                                Err(broadcast::error::RecvError::Lagged(n)) => {
                                    warn!("Upstream lagged, missed {} frames", n);
                                }
                                Err(broadcast::error::RecvError::Closed) => {
                                    info!("Upstream broadcast closed");
                                    break;
                                }
                            }
                        }
                    }
                    Err(e) => {
                        error!("Failed to connect to upstream: {}", e);
                    }
                }

                // Reconnect after delay
                warn!("Upstream disconnected, reconnecting in 5 seconds...");
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        });
    }

    // Spawn stats logging task
    let stats_router = Arc::clone(&router);
    tokio::spawn(async move {
        let mut stats_interval = interval(Duration::from_secs(30));
        loop {
            stats_interval.tick().await;
            let stats = stats_router.stats().await;
            info!(
                "Stats: {} cameras, {} clients, {} frames ({} MB) forwarded",
                stats.cameras_connected,
                stats.clients_connected,
                stats.frames_forwarded,
                stats.bytes_forwarded / (1024 * 1024)
            );
        }
    });

    info!("Waiting for connections...");

    // Accept connections
    loop {
        match relay.accept().await {
            Some(conn) => {
                let remote = conn.remote_public_key();
                info!("New connection from: {}", remote);

                let router = Arc::clone(&router);

                tokio::spawn(async move {
                    // Detect role by stream direction (same as server)
                    let detect_timeout = Duration::from_secs(2);

                    tokio::select! {
                        result = conn.accept_frame_stream() => {
                            match result {
                                Ok(receiver) => {
                                    info!(peer = %remote, "Detected as camera");
                                    if let Err(e) = router.handle_camera(remote, receiver).await {
                                        warn!(peer = %remote, error = %e, "Camera handler error");
                                    }
                                }
                                Err(e) => {
                                    warn!(peer = %remote, error = %e, "Failed to accept stream");
                                }
                            }
                        }
                        _ = tokio::time::sleep(detect_timeout) => {
                            info!(peer = %remote, "Detected as client");
                            if let Err(e) = router.handle_client(conn).await {
                                warn!(peer = %remote, error = %e, "Client handler error");
                            }
                        }
                    }
                });
            }
            None => {
                error!("Relay accept returned None, shutting down");
                break;
            }
        }
    }

    Ok(())
}
