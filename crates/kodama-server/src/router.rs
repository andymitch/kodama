//! Frame router for distributing frames from cameras to clients

use anyhow::Result;
use iroh::PublicKey;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};
use tracing::{debug, info, warn};

use kodama_core::Frame;
use kodama_relay::{RelayConnection, FrameReceiver};

/// Role of a connected peer
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerRole {
    /// Camera: sends frames to the server
    Camera,
    /// Client: receives frames from the server
    Client,
}

/// Statistics about router state
#[derive(Debug, Clone, Default)]
pub struct RouterStats {
    pub cameras_connected: usize,
    pub clients_connected: usize,
    pub frames_received: u64,
    pub frames_broadcast: u64,
}

/// Handle to the router for getting stats and managing peers
#[derive(Clone)]
pub struct RouterHandle {
    inner: Arc<RouterInner>,
}

struct RouterInner {
    /// Broadcast sender for distributing frames to clients
    frame_tx: broadcast::Sender<Frame>,
    /// Connected peers and their roles
    peers: RwLock<HashMap<PublicKey, PeerRole>>,
    /// Stats
    stats: RwLock<RouterStats>,
}

impl RouterHandle {
    /// Get current router statistics
    pub async fn stats(&self) -> RouterStats {
        self.inner.stats.read().await.clone()
    }

    /// Get list of connected peers
    pub async fn peers(&self) -> Vec<(PublicKey, PeerRole)> {
        self.inner
            .peers
            .read()
            .await
            .iter()
            .map(|(k, v)| (*k, *v))
            .collect()
    }

    /// Subscribe to the frame broadcast
    pub fn subscribe(&self) -> broadcast::Receiver<Frame> {
        self.inner.frame_tx.subscribe()
    }
}

/// Frame router that distributes frames from cameras to clients
pub struct Router {
    inner: Arc<RouterInner>,
}

impl Clone for Router {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl Router {
    /// Create a new router with the given broadcast buffer capacity
    ///
    /// The buffer capacity determines how many frames can be buffered
    /// before slow clients start losing frames.
    pub fn new(buffer_capacity: usize) -> Self {
        let (frame_tx, _) = broadcast::channel(buffer_capacity);

        Self {
            inner: Arc::new(RouterInner {
                frame_tx,
                peers: RwLock::new(HashMap::new()),
                stats: RwLock::new(RouterStats::default()),
            }),
        }
    }

    /// Get a handle for stats and subscriptions
    pub fn handle(&self) -> RouterHandle {
        RouterHandle {
            inner: Arc::clone(&self.inner),
        }
    }

    /// Register a peer with a specific role
    async fn register_peer(&self, key: PublicKey, role: PeerRole) {
        let mut peers = self.inner.peers.write().await;
        let mut stats = self.inner.stats.write().await;

        if peers.insert(key, role).is_none() {
            match role {
                PeerRole::Camera => stats.cameras_connected += 1,
                PeerRole::Client => stats.clients_connected += 1,
            }
        }

        info!(?role, peer = %key, "Peer registered");
    }

    /// Unregister a peer
    async fn unregister_peer(&self, key: PublicKey) {
        let mut peers = self.inner.peers.write().await;
        let mut stats = self.inner.stats.write().await;

        if let Some(role) = peers.remove(&key) {
            match role {
                PeerRole::Camera => stats.cameras_connected = stats.cameras_connected.saturating_sub(1),
                PeerRole::Client => stats.clients_connected = stats.clients_connected.saturating_sub(1),
            }
            info!(?role, peer = %key, "Peer unregistered");
        }
    }

    /// Broadcast a frame to all subscribers
    pub async fn broadcast_frame(&self, frame: Frame) {
        // Update stats
        {
            let mut stats = self.inner.stats.write().await;
            stats.frames_received += 1;
        }

        // Broadcast to all subscribers
        match self.inner.frame_tx.send(frame) {
            Ok(n) => {
                let mut stats = self.inner.stats.write().await;
                stats.frames_broadcast += 1;
                debug!(subscribers = n, "Frame broadcast");
            }
            Err(_) => {
                // No subscribers - that's okay
                debug!("No subscribers for frame");
            }
        }
    }

    /// Handle camera with a pre-accepted frame receiver
    ///
    /// Use this when you've already accepted the frame stream
    /// (e.g., for role detection via stream direction).
    pub async fn handle_camera_with_receiver(
        &self,
        remote: PublicKey,
        receiver: FrameReceiver,
    ) -> Result<()> {
        self.register_peer(remote, PeerRole::Camera).await;
        info!(camera = %remote, "Camera stream opened");

        // Receive frames and broadcast
        loop {
            match receiver.recv().await {
                Ok(Some(frame)) => {
                    debug!(
                        source = ?frame.source,
                        keyframe = frame.flags.is_keyframe(),
                        len = frame.payload.len(),
                        "Received frame from camera"
                    );
                    self.broadcast_frame(frame).await;
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

    /// Handle a camera connection - receive frames and broadcast them
    pub async fn handle_camera(&self, conn: RelayConnection) -> Result<()> {
        let remote = conn.remote_public_key();
        self.register_peer(remote, PeerRole::Camera).await;

        // Accept the frame stream from the camera
        let receiver = conn.accept_frame_stream().await?;
        info!(camera = %remote, "Camera stream opened");

        // Receive frames and broadcast
        loop {
            match receiver.recv().await {
                Ok(Some(frame)) => {
                    debug!(
                        source = ?frame.source,
                        keyframe = frame.flags.is_keyframe(),
                        len = frame.payload.len(),
                        "Received frame from camera"
                    );
                    self.broadcast_frame(frame).await;
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

    /// Handle a client connection - subscribe to frames and forward them
    pub async fn handle_client(&self, conn: RelayConnection) -> Result<()> {
        let remote = conn.remote_public_key();
        self.register_peer(remote, PeerRole::Client).await;

        // Subscribe to frames
        let mut rx = self.inner.frame_tx.subscribe();

        // Open a frame stream to the client
        let sender = conn.open_frame_stream().await?;
        info!(client = %remote, "Client stream opened");

        // Forward frames to client
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
                    // Continue - client will get the next frame
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_router_creation() {
        let router = Router::new(64);
        let _handle = router.handle();
    }
}
