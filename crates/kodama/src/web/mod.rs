//! Web server module: axum HTTP + WebSocket for browser access.
//!
//! Serves the SvelteKit UI as static files and provides:
//! - `GET /` — SvelteKit UI (static files)
//! - `GET /api/cameras` — list connected cameras
//! - `GET /api/peers` — list all connected peers
//! - `GET /api/status` — server status
//! - `GET /api/storage` — storage statistics
//! - `WS /ws` — multiplexed live stream (video, audio, telemetry, events)

pub mod fmp4;
pub mod ws;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use axum::extract::{State, WebSocketUpgrade};
use axum::response::{IntoResponse, Json};
use axum::routing::get;
use axum::Router;
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;
use tracing::info;

use crate::server::{RouterHandle, StorageStats};

/// Trait for providing storage statistics to the web module.
///
/// Implemented by the server binary's `StorageHandle` to expose storage
/// stats without the web module depending on the full storage stack.
#[async_trait::async_trait]
pub trait StorageStatsProvider: Send + Sync {
    async fn stats(&self) -> StorageStats;
}

/// Shared state for the web server
struct WebState {
    handle: RouterHandle,
    start_time: Instant,
    public_key: Option<String>,
    storage: Option<Arc<dyn StorageStatsProvider>>,
    storage_max_bytes: u64,
}

/// Start the web server.
///
/// `ui_path` — directory containing the SvelteKit build output.
/// If None, only API/WS endpoints are served (no static files).
/// `public_key` — server's public key (included in status endpoint).
/// `storage` — optional storage stats provider.
/// `storage_max_bytes` — configured max storage in bytes (0 if no storage).
pub async fn start(
    handle: RouterHandle,
    bind: SocketAddr,
    ui_path: Option<PathBuf>,
    public_key: Option<String>,
    storage: Option<Arc<dyn StorageStatsProvider>>,
    storage_max_bytes: u64,
) -> Result<()> {
    let state = Arc::new(WebState {
        handle,
        start_time: Instant::now(),
        public_key,
        storage,
        storage_max_bytes,
    });

    let mut app = Router::new()
        .route("/ws", get(ws_upgrade))
        .route("/api/cameras", get(api_cameras))
        .route("/api/peers", get(api_peers))
        .route("/api/status", get(api_status))
        .route("/api/storage", get(api_storage))
        .layer(CorsLayer::permissive())
        .with_state(state);

    // Serve static UI files if path is provided
    if let Some(ref path) = ui_path {
        if path.exists() {
            info!("Serving UI from {:?}", path);
            app = app.fallback_service(
                ServeDir::new(path).fallback(
                    ServeDir::new(path).append_index_html_on_directories(true),
                ),
            );
        } else {
            tracing::warn!("UI path {:?} does not exist, skipping static file serving", path);
        }
    }

    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .context(format!("Failed to bind to {}", bind))?;

    info!("Web server listening on http://{}", bind);

    axum::serve(listener, app)
        .await
        .context("Web server error")?;

    Ok(())
}

/// WebSocket upgrade handler
async fn ws_upgrade(
    ws: WebSocketUpgrade,
    State(state): State<Arc<WebState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| ws::handle_ws(socket, state.handle.clone(), state.start_time))
}

/// GET /api/cameras — list connected cameras
async fn api_cameras(
    State(state): State<Arc<WebState>>,
) -> Json<serde_json::Value> {
    use crate::server::PeerRole;

    let peers = state.handle.peers().await;
    let cameras: Vec<serde_json::Value> = peers
        .into_iter()
        .filter(|p| p.role == PeerRole::Camera)
        .map(|p| {
            let source = crate::SourceId::from_node_id_bytes(p.key.as_bytes());
            serde_json::json!({
                "id": format!("{}", source),
                "name": format!("Camera {}", &format!("{}", source)[..8]),
                "connected": true,
                "uptime_secs": p.connected_at.elapsed().as_secs(),
            })
        })
        .collect();

    Json(serde_json::json!(cameras))
}

/// GET /api/peers — list all connected peers
async fn api_peers(
    State(state): State<Arc<WebState>>,
) -> Json<serde_json::Value> {
    use crate::server::PeerRole;

    let peers = state.handle.peers().await;
    let list: Vec<serde_json::Value> = peers
        .into_iter()
        .map(|p| {
            let mut obj = serde_json::json!({
                "key": p.key.to_string(),
                "role": match p.role {
                    PeerRole::Camera => "camera",
                    PeerRole::Client => "client",
                },
                "uptime_secs": p.connected_at.elapsed().as_secs(),
            });
            if p.role == PeerRole::Camera {
                let source = crate::SourceId::from_node_id_bytes(p.key.as_bytes());
                obj["source_id"] = serde_json::json!(format!("{}", source));
            }
            obj
        })
        .collect();

    Json(serde_json::json!(list))
}

/// GET /api/status — server status
async fn api_status(
    State(state): State<Arc<WebState>>,
) -> Json<serde_json::Value> {
    let stats = state.handle.stats().await;
    let uptime = state.start_time.elapsed().as_secs();

    let storage_enabled = state.storage.is_some();
    let storage_bytes_used = if let Some(ref s) = state.storage {
        s.stats().await.bytes_used
    } else {
        0
    };

    let mut resp = serde_json::json!({
        "cameras": stats.cameras_connected,
        "clients": stats.clients_connected,
        "uptime_secs": uptime,
        "frames_received": stats.frames_received,
        "frames_broadcast": stats.frames_broadcast,
        "storage_enabled": storage_enabled,
        "storage_bytes_used": storage_bytes_used,
    });
    if let Some(ref key) = state.public_key {
        resp["public_key"] = serde_json::json!(key);
    }
    Json(resp)
}

/// GET /api/storage — storage statistics
async fn api_storage(
    State(state): State<Arc<WebState>>,
) -> Json<serde_json::Value> {
    if let Some(ref s) = state.storage {
        let stats = s.stats().await;
        Json(serde_json::json!({
            "enabled": true,
            "bytes_used": stats.bytes_used,
            "max_bytes": state.storage_max_bytes,
            "frames_stored": stats.frames_stored,
            "frames_skipped": stats.frames_skipped,
            "bytes_cleaned": stats.bytes_cleaned,
        }))
    } else {
        Json(serde_json::json!({
            "enabled": false,
        }))
    }
}
