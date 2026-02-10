//! Relay mode: lightweight frame forwarder with no storage/routing/rate-limiting.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::broadcast;
use tokio::time::{interval, Duration};
use tracing::{debug, error, info, warn};

use kodama::Frame;
use kodama::transport::{FrameReceiver, Relay};

struct RelayStats {
    cameras: AtomicUsize,
    clients: AtomicUsize,
    frames_forwarded: AtomicU64,
}

pub fn run() -> Result<()> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run_async())
}

async fn run_async() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("kodama=info".parse().unwrap()),
        )
        .init();

    let key_path = std::env::var("KODAMA_KEY_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("./relay.key"));

    let buffer_capacity: usize = std::env::var("KODAMA_BUFFER_SIZE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(512);

    let upstream_key = std::env::var("KODAMA_UPSTREAM_KEY").ok();

    // Create parent directory for key if needed
    if let Some(parent) = key_path.parent() {
        if !parent.exists() && !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }

    let relay = Relay::new(Some(&key_path)).await?;
    info!("Relay PublicKey: {}", relay.public_key());
    info!("Share this key with cameras (KODAMA_SERVER_KEY) and clients to connect.");

    let (tx, _) = broadcast::channel::<Frame>(buffer_capacity);
    let stats = Arc::new(RelayStats {
        cameras: AtomicUsize::new(0),
        clients: AtomicUsize::new(0),
        frames_forwarded: AtomicU64::new(0),
    });

    // Spawn upstream forwarder if configured
    if let Some(ref upstream) = upstream_key {
        let upstream_key: iroh::PublicKey = upstream.parse()
            .map_err(|_| anyhow::anyhow!("Invalid KODAMA_UPSTREAM_KEY"))?;
        let upstream_relay = Relay::new(None).await?;
        let mut upstream_rx = tx.subscribe();
        tokio::spawn(async move {
            info!("Connecting to upstream server: {}", upstream_key);
            match upstream_relay.connect(upstream_key).await {
                Ok(conn) => {
                    match conn.open_frame_stream().await {
                        Ok(sender) => {
                            info!("Upstream connection established");
                            loop {
                                match upstream_rx.recv().await {
                                    Ok(frame) => {
                                        if let Err(e) = sender.send(&frame).await {
                                            warn!("Upstream send error: {}", e);
                                            break;
                                        }
                                    }
                                    Err(broadcast::error::RecvError::Lagged(n)) => {
                                        warn!("Upstream lagged, missed {} frames", n);
                                    }
                                    Err(broadcast::error::RecvError::Closed) => break,
                                }
                            }
                        }
                        Err(e) => error!("Failed to open upstream stream: {}", e),
                    }
                }
                Err(e) => error!("Failed to connect to upstream: {}", e),
            }
        });
    }

    // Spawn stats logging
    let stats_ref = stats.clone();
    tokio::spawn(async move {
        let mut stats_interval = interval(Duration::from_secs(30));
        loop {
            stats_interval.tick().await;
            info!(
                "Stats: {} cameras, {} clients, {} frames forwarded",
                stats_ref.cameras.load(Ordering::Relaxed),
                stats_ref.clients.load(Ordering::Relaxed),
                stats_ref.frames_forwarded.load(Ordering::Relaxed),
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
                let tx = tx.clone();
                let stats = stats.clone();

                tokio::spawn(async move {
                    let detect_timeout = Duration::from_secs(2);
                    tokio::select! {
                        // Camera: opens a frame stream
                        result = conn.accept_frame_stream() => {
                            match result {
                                Ok(receiver) => {
                                    info!(peer = %remote, "Detected as camera");
                                    handle_camera(remote, receiver, tx, stats).await;
                                }
                                Err(e) => {
                                    warn!(peer = %remote, error = %e, "Failed to accept stream");
                                }
                            }
                        }
                        // Client: no stream opened within timeout
                        _ = tokio::time::sleep(detect_timeout) => {
                            info!(peer = %remote, "Detected as client");
                            handle_client(conn, tx, stats).await;
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

async fn handle_camera(
    remote: iroh::PublicKey,
    receiver: FrameReceiver,
    tx: broadcast::Sender<Frame>,
    stats: Arc<RelayStats>,
) {
    stats.cameras.fetch_add(1, Ordering::Relaxed);
    loop {
        match receiver.recv().await {
            Ok(Some(frame)) => {
                stats.frames_forwarded.fetch_add(1, Ordering::Relaxed);
                let _ = tx.send(frame);
            }
            Ok(None) => {
                debug!(peer = %remote, "Camera stream finished");
                break;
            }
            Err(e) => {
                debug!(peer = %remote, error = %e, "Camera stream error");
                break;
            }
        }
    }
    stats.cameras.fetch_sub(1, Ordering::Relaxed);
    info!(peer = %remote, "Camera disconnected");
}

async fn handle_client(
    conn: kodama::transport::RelayConnection,
    tx: broadcast::Sender<Frame>,
    stats: Arc<RelayStats>,
) {
    let remote = conn.remote_public_key();
    stats.clients.fetch_add(1, Ordering::Relaxed);

    match conn.open_frame_stream().await {
        Ok(sender) => {
            let mut rx = tx.subscribe();
            loop {
                match rx.recv().await {
                    Ok(frame) => {
                        if let Err(e) = sender.send(&frame).await {
                            debug!(peer = %remote, error = %e, "Client send error");
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!(peer = %remote, "Client lagged, missed {} frames", n);
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
        Err(e) => {
            warn!(peer = %remote, error = %e, "Failed to open client stream");
        }
    }

    stats.clients.fetch_sub(1, Ordering::Relaxed);
    info!(peer = %remote, "Client disconnected");
}
