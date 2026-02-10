//! Web server module: axum HTTP + WebSocket for browser access.
//!
//! Serves the SvelteKit UI as static files and provides:
//! - `GET /` — SvelteKit UI (static files)
//! - `GET /api/cameras` — list connected cameras
//! - `GET /api/status` — server status
//! - `WS /ws` — multiplexed live stream (video, audio, telemetry)

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

use crate::server::RouterHandle;

/// Shared state for the web server
struct WebState {
    handle: RouterHandle,
    start_time: Instant,
    public_key: Option<String>,
}

/// Start the web server.
///
/// `ui_path` — directory containing the SvelteKit build output.
/// If None, only API/WS endpoints are served (no static files).
/// `public_key` — server's public key (included in status endpoint).
pub async fn start(
    handle: RouterHandle,
    bind: SocketAddr,
    ui_path: Option<PathBuf>,
    public_key: Option<String>,
) -> Result<()> {
    let state = Arc::new(WebState {
        handle,
        start_time: Instant::now(),
        public_key,
    });

    let mut app = Router::new()
        .route("/ws", get(ws_upgrade))
        .route("/api/cameras", get(api_cameras))
        .route("/api/status", get(api_status))
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
    ws.on_upgrade(move |socket| ws::handle_ws(socket, state.handle.clone()))
}

/// GET /api/cameras — list connected cameras
async fn api_cameras(
    State(state): State<Arc<WebState>>,
) -> Json<serde_json::Value> {
    use crate::server::PeerRole;

    let peers = state.handle.peers().await;
    let cameras: Vec<serde_json::Value> = peers
        .into_iter()
        .filter(|(_, role)| *role == PeerRole::Camera)
        .map(|(key, _)| {
            let source = crate::SourceId::from_node_id_bytes(key.as_bytes());
            serde_json::json!({
                "id": format!("{}", source),
                "name": format!("Camera {}", &format!("{}", source)[..8]),
                "connected": true,
            })
        })
        .collect();

    Json(serde_json::json!(cameras))
}

/// GET /api/status — server status
async fn api_status(
    State(state): State<Arc<WebState>>,
) -> Json<serde_json::Value> {
    let stats = state.handle.stats().await;
    let uptime = state.start_time.elapsed().as_secs();

    let mut resp = serde_json::json!({
        "cameras": stats.cameras_connected,
        "clients": stats.clients_connected,
        "uptime_secs": uptime,
        "frames_received": stats.frames_received,
        "frames_broadcast": stats.frames_broadcast,
    });
    if let Some(ref key) = state.public_key {
        resp["public_key"] = serde_json::json!(key);
    }
    Json(resp)
}
