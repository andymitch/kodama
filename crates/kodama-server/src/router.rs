//! Frame router for distributing frames from cameras to clients

use anyhow::Result;
use iroh::PublicKey;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use tokio::sync::{broadcast, oneshot, RwLock};
use tokio::time::Duration;
use tracing::{debug, info, warn};

use kodama_core::{
    Frame, Command, CommandMessage, CommandRequest, CommandResponse, CommandResult,
    ClientCommandMessage,
};
use kodama_relay::{
    RelayConnection, FrameReceiver,
    CommandSender, CommandStream, ClientCommandStream,
};

/// Role of a connected peer
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerRole {
    /// Camera: sends frames to the server
    Camera,
    /// Client: receives frames from the server
    Client,
}

/// Statistics about router state (returned as a snapshot from atomic counters)
#[derive(Debug, Clone, Default)]
pub struct RouterStats {
    pub cameras_connected: usize,
    pub clients_connected: usize,
    pub frames_received: u64,
    pub frames_broadcast: u64,
}

/// Internal atomic counters for lock-free stats tracking
struct AtomicRouterStats {
    frames_received: AtomicU64,
    frames_broadcast: AtomicU64,
    cameras_connected: AtomicUsize,
    clients_connected: AtomicUsize,
}

impl AtomicRouterStats {
    fn new() -> Self {
        Self {
            frames_received: AtomicU64::new(0),
            frames_broadcast: AtomicU64::new(0),
            cameras_connected: AtomicUsize::new(0),
            clients_connected: AtomicUsize::new(0),
        }
    }

    /// Read all atomics and return a plain RouterStats snapshot
    fn snapshot(&self) -> RouterStats {
        RouterStats {
            frames_received: self.frames_received.load(Ordering::Relaxed),
            frames_broadcast: self.frames_broadcast.load(Ordering::Relaxed),
            cameras_connected: self.cameras_connected.load(Ordering::Relaxed),
            clients_connected: self.clients_connected.load(Ordering::Relaxed),
        }
    }
}

/// Info about a connected peer, including a generation counter for race-condition protection
#[derive(Debug, Clone)]
struct PeerInfo {
    role: PeerRole,
    generation: u64,
}

/// Handle to the router for getting stats and managing peers
#[derive(Clone)]
pub struct RouterHandle {
    inner: Arc<RouterInner>,
}

struct RouterInner {
    /// Broadcast sender for distributing frames to clients
    frame_tx: broadcast::Sender<Frame>,
    /// Connected peers and their roles (protected by RwLock since it's a HashMap)
    peers: RwLock<HashMap<PublicKey, PeerInfo>>,
    /// Lock-free atomic stats
    stats: AtomicRouterStats,
    /// Monotonically increasing generation counter for peer registration
    connection_generation: AtomicU64,
    /// Command channels to cameras (keyed by camera PublicKey)
    camera_commands: RwLock<HashMap<PublicKey, Arc<CameraCommandState>>>,
}

/// State for sending commands to a specific camera and receiving responses
struct CameraCommandState {
    sender: Arc<CommandSender>,
    pending: RwLock<HashMap<u32, oneshot::Sender<CommandResponse>>>,
    next_id: AtomicU32,
}

impl RouterHandle {
    /// Get current router statistics
    pub async fn stats(&self) -> RouterStats {
        self.inner.stats.snapshot()
    }

    /// Get list of connected peers
    pub async fn peers(&self) -> Vec<(PublicKey, PeerRole)> {
        self.inner
            .peers
            .read()
            .await
            .iter()
            .map(|(k, v)| (*k, v.role))
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
                stats: AtomicRouterStats::new(),
                connection_generation: AtomicU64::new(0),
                camera_commands: RwLock::new(HashMap::new()),
            }),
        }
    }

    /// Get a handle for stats and subscriptions
    pub fn handle(&self) -> RouterHandle {
        RouterHandle {
            inner: Arc::clone(&self.inner),
        }
    }

    /// Register a peer with a specific role.
    ///
    /// Returns a generation counter that must be passed to `unregister_peer`
    /// to prevent stale connection handlers from removing newer registrations.
    async fn register_peer(&self, key: PublicKey, role: PeerRole) -> u64 {
        let gen = self.inner.connection_generation.fetch_add(1, Ordering::Relaxed);
        let mut peers = self.inner.peers.write().await;

        let old = peers.insert(key, PeerInfo { role, generation: gen });
        if old.is_none() {
            match role {
                PeerRole::Camera => { self.inner.stats.cameras_connected.fetch_add(1, Ordering::Relaxed); }
                PeerRole::Client => { self.inner.stats.clients_connected.fetch_add(1, Ordering::Relaxed); }
            }
        }

        info!(?role, peer = %key, generation = gen, "Peer registered");
        gen
    }

    /// Unregister a peer, but only if the generation matches.
    ///
    /// This prevents a stale connection handler (from a previous connection)
    /// from removing a newer registration when a peer reconnects quickly.
    async fn unregister_peer(&self, key: PublicKey, generation: u64) {
        let mut peers = self.inner.peers.write().await;

        if let Some(info) = peers.get(&key) {
            if info.generation == generation {
                let role = info.role;
                peers.remove(&key);
                match role {
                    PeerRole::Camera => { self.inner.stats.cameras_connected.fetch_sub(1, Ordering::Relaxed); }
                    PeerRole::Client => { self.inner.stats.clients_connected.fetch_sub(1, Ordering::Relaxed); }
                }
                info!(?role, peer = %key, generation, "Peer unregistered");
            } else {
                debug!(
                    peer = %key,
                    registered_generation = info.generation,
                    stale_generation = generation,
                    "Skipping unregister: generation mismatch (newer connection exists)"
                );
            }
        }
    }

    /// Broadcast a frame to all subscribers
    pub async fn broadcast_frame(&self, frame: Frame) {
        self.inner.stats.frames_received.fetch_add(1, Ordering::Relaxed);

        // Broadcast to all subscribers
        match self.inner.frame_tx.send(frame) {
            Ok(n) => {
                self.inner.stats.frames_broadcast.fetch_add(1, Ordering::Relaxed);
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
        let generation = self.register_peer(remote, PeerRole::Camera).await;
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

        self.unregister_peer(remote, generation).await;
        Ok(())
    }

    /// Handle a camera connection - receive frames and broadcast them
    pub async fn handle_camera(&self, conn: RelayConnection) -> Result<()> {
        let remote = conn.remote_public_key();
        let generation = self.register_peer(remote, PeerRole::Camera).await;

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

        self.unregister_peer(remote, generation).await;
        Ok(())
    }

    /// Handle a client connection - subscribe to frames and forward them
    pub async fn handle_client(&self, conn: RelayConnection) -> Result<()> {
        let remote = conn.remote_public_key();
        let generation = self.register_peer(remote, PeerRole::Client).await;

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

        self.unregister_peer(remote, generation).await;
        Ok(())
    }

    // ========== Command routing ==========

    /// Register a camera's command stream.
    ///
    /// Stores the sender side and spawns a task to read responses
    /// and dispatch them to pending oneshot channels.
    pub fn register_camera_commands(&self, key: PublicKey, stream: CommandStream) {
        let state = Arc::new(CameraCommandState {
            sender: Arc::new(stream.sender),
            pending: RwLock::new(HashMap::new()),
            next_id: AtomicU32::new(1),
        });

        let inner = Arc::clone(&self.inner);
        let state_clone = Arc::clone(&state);

        // Store state
        let inner2 = Arc::clone(&self.inner);
        tokio::spawn(async move {
            inner2.camera_commands.write().await.insert(key, state_clone.clone());

            // Read responses from camera and dispatch to pending oneshot channels
            let receiver = stream.receiver;
            loop {
                match receiver.recv().await {
                    Ok(Some(CommandMessage::Response(resp))) => {
                        if resp.id == 0 {
                            // Ready signal from camera — ignore
                            debug!(camera = %key, "Camera command channel ready");
                            continue;
                        }
                        let mut pending = state_clone.pending.write().await;
                        if let Some(tx) = pending.remove(&resp.id) {
                            let _ = tx.send(resp);
                        } else {
                            warn!(camera = %key, id = resp.id, "Received response for unknown request");
                        }
                    }
                    Ok(Some(CommandMessage::Request(_))) => {
                        warn!(camera = %key, "Received unexpected request from camera");
                    }
                    Ok(None) => {
                        info!(camera = %key, "Camera command stream closed");
                        break;
                    }
                    Err(e) => {
                        warn!(camera = %key, error = %e, "Camera command stream error");
                        break;
                    }
                }
            }

            // Cleanup
            inner.camera_commands.write().await.remove(&key);
            info!(camera = %key, "Camera command channel unregistered");
        });
    }

    /// Send a command to a camera and wait for the response.
    pub async fn send_command(
        &self,
        camera_key: PublicKey,
        command: Command,
        timeout: Duration,
    ) -> Result<CommandResponse> {
        let state = {
            let commands = self.inner.camera_commands.read().await;
            commands.get(&camera_key).cloned()
                .ok_or_else(|| anyhow::anyhow!("Camera {} has no command channel", camera_key))?
        };

        let id = state.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();

        // Register pending response
        state.pending.write().await.insert(id, tx);

        // Send request
        let msg = CommandMessage::Request(CommandRequest { id, command });
        if let Err(e) = state.sender.send(&msg).await {
            state.pending.write().await.remove(&id);
            return Err(e);
        }

        // Wait for response with timeout
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(resp)) => Ok(resp),
            Ok(Err(_)) => {
                // oneshot sender was dropped (camera disconnected)
                anyhow::bail!("Camera disconnected while waiting for response")
            }
            Err(_) => {
                state.pending.write().await.remove(&id);
                anyhow::bail!("Command timed out after {:?}", timeout)
            }
        }
    }

    /// Handle commands from a client, routing them to the targeted camera.
    ///
    /// Reads TargetedCommandRequests from the client, forwards each to the
    /// appropriate camera via send_command, and sends the response back.
    pub async fn handle_client_commands(&self, client_key: PublicKey, stream: ClientCommandStream) {
        info!(client = %client_key, "Client command channel opened");

        loop {
            match stream.receiver.recv().await {
                Ok(Some(ClientCommandMessage::Request(targeted))) => {
                    debug!(
                        client = %client_key,
                        target = %targeted.target_camera,
                        command = ?targeted.command,
                        "Routing client command to camera"
                    );

                    // Parse camera key
                    let camera_key = match targeted.target_camera.parse::<PublicKey>() {
                        Ok(k) => k,
                        Err(_) => {
                            let resp = ClientCommandMessage::Response(CommandResponse {
                                id: targeted.id,
                                result: CommandResult::Error("Invalid camera key".into()),
                            });
                            if stream.sender.send(&resp).await.is_err() {
                                break;
                            }
                            continue;
                        }
                    };

                    // Forward to camera with 30-second timeout
                    let result = match self.send_command(
                        camera_key,
                        targeted.command,
                        Duration::from_secs(30),
                    ).await {
                        Ok(camera_resp) => CommandResponse {
                            id: targeted.id,
                            result: camera_resp.result,
                        },
                        Err(e) => CommandResponse {
                            id: targeted.id,
                            result: CommandResult::Error(e.to_string()),
                        },
                    };

                    let msg = ClientCommandMessage::Response(result);
                    if stream.sender.send(&msg).await.is_err() {
                        break;
                    }
                }
                Ok(Some(ClientCommandMessage::Response(_))) => {
                    warn!(client = %client_key, "Received unexpected response from client");
                }
                Ok(None) => {
                    info!(client = %client_key, "Client command stream closed");
                    break;
                }
                Err(e) => {
                    warn!(client = %client_key, error = %e, "Client command stream error");
                    break;
                }
            }
        }

        info!(client = %client_key, "Client command channel closed");
    }

    /// Get list of cameras that have command channels
    pub async fn cameras_with_commands(&self) -> Vec<PublicKey> {
        self.inner.camera_commands.read().await.keys().copied().collect()
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
