//! Tauri commands for the desktop app

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tauri::{AppHandle, Emitter, State};
use serde::Serialize;
use tokio::sync::RwLock;
use tokio::time::Duration;

use kodama_core::{Channel, Frame, SourceId};
use kodama_relay::Relay;
use kodama_server::{
    Router, StorageManager, StorageConfig as ServerStorageConfig,
    LocalStorage, LocalStorageConfig, StorageBackend,
};
use kodama_capture::{decode_telemetry, TelemetryData};

use crate::events::{
    AudioDataEvent, AudioLevelEvent, CameraEvent, GpsEvent, TelemetryEvent, VideoInitEvent, VideoSegmentEvent,
};
use crate::fmp4_ffmpeg::Fmp4Muxer; // Using FFmpeg-based muxer
use crate::state::{AppMode, AppState, ServerState, ClientState, CameraInfo, StorageConfig, AppSettings};

/// Server status response
#[derive(Debug, Serialize)]
pub struct ServerStatus {
    pub running: bool,
    pub public_key: Option<String>,
    pub cameras_connected: usize,
    pub clients_connected: usize,
    pub frames_received: u64,
}

/// Connection status response
#[derive(Debug, Serialize)]
pub struct ConnectionStatus {
    pub connected: bool,
    pub mode: AppMode,
    pub server_key: Option<String>,
}

/// Storage stats response
#[derive(Debug, Serialize)]
pub struct StorageStats {
    pub enabled: bool,
    pub bytes_used: u64,
    pub frames_stored: u64,
    pub bytes_cleaned: u64,
}

/// Per-source muxer state shared across frame processing
struct SourceMuxState {
    muxer: Fmp4Muxer,
    last_audio_emit: std::time::Instant,
    last_video_time: std::time::Instant,
    telemetry_state: TelemetryData,
}

/// Process a frame and emit appropriate Tauri events
fn process_frame(
    app: &AppHandle,
    frame: &Frame,
    muxers: &mut HashMap<SourceId, SourceMuxState>,
) {
    let source_id = format!("{}", frame.source);

    match frame.channel {
        Channel::Video => {
            let mux_state = muxers
                .entry(frame.source)
                .or_insert_with(|| SourceMuxState {
                    muxer: Fmp4Muxer::new(),
                    last_audio_emit: std::time::Instant::now(),
                    last_video_time: std::time::Instant::now(),
                    telemetry_state: TelemetryData::default(),
                });

            // Detect camera reconnection: if >2s gap, reset muxer so a fresh
            // init segment is emitted on the next keyframe.
            let gap = mux_state.last_video_time.elapsed();
            if gap.as_secs() >= 2 {
                tracing::info!(source = %frame.source, gap_ms = gap.as_millis(), "Camera reconnected, resetting muxer");
                mux_state.muxer.reset();
            }
            mux_state.last_video_time = std::time::Instant::now();

            let result = mux_state.muxer.mux_frame(
                &frame.payload,
                frame.flags.is_keyframe(),
                frame.timestamp_us,
            );

            if let Some(init) = result.init_segment {
                let codec = result.codec.unwrap_or_else(|| "avc1.42001e".to_string());
                let width = result.width.unwrap_or(640);
                let height = result.height.unwrap_or(480);
                tracing::info!("Emitting video-init: {}x{} codec={} init_size={}", width, height, codec, init.len());
                #[cfg(debug_assertions)]
                {
                    if let Err(e) = std::fs::write("/tmp/kodama_init_debug.mp4", &init) {
                        tracing::warn!("Failed to dump init segment: {}", e);
                    }
                }
                let _ = app.emit("video-init", VideoInitEvent {
                    source_id: source_id.clone(),
                    codec,
                    width,
                    height,
                    init_segment: base64::Engine::encode(
                        &base64::engine::general_purpose::STANDARD,
                        &init,
                    ),
                });
            }

            if let Some(ref segment) = result.media_segment {
                tracing::trace!("Emitting video-segment: source={}, size={}", source_id, segment.len());

                #[cfg(debug_assertions)]
                {
                    static DUMPED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
                    if !DUMPED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                        if let Err(e) = std::fs::write("/tmp/kodama_media_segment.m4s", segment) {
                            tracing::warn!("Failed to dump media segment: {}", e);
                        } else {
                            tracing::info!("Dumped first media segment to /tmp/kodama_media_segment.m4s");
                        }
                    }
                }

                let _ = app.emit("video-segment", VideoSegmentEvent {
                    source_id,
                    data: base64::Engine::encode(
                        &base64::engine::general_purpose::STANDARD,
                        segment,
                    ),
                });
            }
        }
        Channel::Audio => {
            let mux_state = muxers
                .entry(frame.source)
                .or_insert_with(|| SourceMuxState {
                    muxer: Fmp4Muxer::new(),
                    last_audio_emit: std::time::Instant::now(),
                    last_video_time: std::time::Instant::now(),
                    telemetry_state: TelemetryData::default(),
                });
            // Emit audio level (throttled to ~10/sec)
            if mux_state.last_audio_emit.elapsed() >= std::time::Duration::from_millis(100) {
                mux_state.last_audio_emit = std::time::Instant::now();
                let level_db = compute_audio_rms(&frame.payload);
                let _ = app.emit("audio-level", AudioLevelEvent {
                    source_id: source_id.clone(),
                    level_db,
                });
            }

            // Emit audio data for playback (every frame for smooth audio)
            let _ = app.emit("audio-data", AudioDataEvent {
                source_id,
                data: base64::Engine::encode(
                    &base64::engine::general_purpose::STANDARD,
                    &frame.payload,
                ),
                sample_rate: 48000,
                channels: 1,
            });
        }
        Channel::Telemetry => {
            if let Ok(sparse) = decode_telemetry(&frame.payload) {
                let mux_state = muxers
                    .entry(frame.source)
                    .or_insert_with(|| SourceMuxState {
                        muxer: Fmp4Muxer::new(),
                        last_audio_emit: std::time::Instant::now(),
                        last_video_time: std::time::Instant::now(),
                        telemetry_state: TelemetryData::default(),
                    });

                // Merge sparse update into running state
                if sparse.full {
                    mux_state.telemetry_state = TelemetryData::default();
                }
                sparse.merge_into(&mut mux_state.telemetry_state);

                let state = &mux_state.telemetry_state;
                let _ = app.emit("telemetry", TelemetryEvent {
                    source_id,
                    cpu_usage: state.cpu_usage,
                    cpu_temp: state.cpu_temp,
                    memory_usage: state.memory_usage,
                    disk_usage: state.disk_usage,
                    uptime_secs: state.uptime_secs,
                    load_average: state.load_average,
                    gps: state.gps.as_ref().map(|g| GpsEvent {
                        latitude: g.latitude,
                        longitude: g.longitude,
                        altitude: g.altitude,
                        speed: g.speed,
                        heading: g.heading,
                        fix_mode: g.fix_mode,
                    }),
                    motion_level: state.motion_level,
                });
            }
        }
        Channel::Unknown(_) => {
            // Silently ignore unknown channel types
        }
    }
}

/// Compute RMS level in dB from PCM s16le audio data
fn compute_audio_rms(data: &[u8]) -> f32 {
    if data.len() < 2 {
        return -60.0;
    }

    let mut sum_sq = 0.0f64;
    let sample_count = data.len() / 2;

    for chunk in data.chunks_exact(2) {
        let sample = i16::from_le_bytes([chunk[0], chunk[1]]) as f64;
        sum_sq += sample * sample;
    }

    if sample_count == 0 {
        return -60.0;
    }

    let rms = (sum_sq / sample_count as f64).sqrt();
    let db = 20.0 * (rms / 32768.0).log10();
    db.max(-60.0) as f32
}

/// Start the server
#[tauri::command]
pub async fn start_server(
    app: AppHandle,
    state: State<'_, AppState>,
    storage_config: Option<StorageConfig>,
) -> Result<String, String> {
    let mut mode = state.mode.write().await;
    if *mode != AppMode::Idle {
        return Err("Already running in another mode".to_string());
    }

    let settings = state.settings.read().await;

    // Create relay endpoint
    let relay = Relay::new(None).await
        .map_err(|e| format!("Failed to create relay: {}", e))?;

    let public_key = relay.public_key().to_string();
    tracing::info!("Server started with key: {}", public_key);

    // Create router
    let router = Router::new(settings.buffer_size);
    let handle = router.handle();

    // Setup storage if configured
    let storage = if let Some(ref config) = storage_config {
        if config.enabled {
            if let Some(ref path) = config.path {
                let local_config = LocalStorageConfig {
                    root_path: path.clone(),
                    max_size_bytes: config.max_gb * 1024 * 1024 * 1024,
                    segment_duration_us: 60 * 1_000_000,
                };

                match LocalStorage::new(local_config) {
                    Ok(backend) => {
                        let backend: Arc<dyn StorageBackend> = Arc::new(backend);
                        let storage_config = ServerStorageConfig {
                            max_size_bytes: config.max_gb * 1024 * 1024 * 1024,
                            retention_secs: (config.retention_days as u64) * 24 * 60 * 60,
                            keyframes_only: config.keyframes_only,
                            cleanup_interval_secs: 3600,
                        };
                        let mut manager = StorageManager::new(storage_config, backend);
                        manager.start_cleanup_task();
                        Some(manager)
                    }
                    Err(e) => {
                        tracing::warn!("Failed to initialize storage: {}", e);
                        None
                    }
                }
            } else {
                None
            }
        } else {
            None
        }
    } else {
        None
    };

    // Store server state
    let server_state = Arc::new(ServerState {
        relay,
        router,
        handle,
        storage,
    });

    *state.server.write().await = Some(server_state.clone());
    *mode = AppMode::Server;

    // Spawn connection accept loop
    let cameras_state = state.cameras.clone();
    let camera_gen = state.camera_generation.clone();
    let accept_task = tokio::spawn(async move {
        // Subscribe to broadcast for local frame processing (video mux, audio, telemetry)
        let mut local_rx = server_state.handle.subscribe();
        let app_for_local = app.clone();
        tokio::spawn(async move {
            let mut muxers: HashMap<SourceId, SourceMuxState> = HashMap::new();
            loop {
                match local_rx.recv().await {
                    Ok(frame) => {
                        process_frame(&app_for_local, &frame, &mut muxers);
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!("Local frame processing lagged, missed {} frames", n);
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        tracing::info!("Broadcast closed, stopping local frame processing");
                        break;
                    }
                }
            }
        });

        // Track active peer handlers so we can abort old ones on reconnect.
        // When a peer reconnects, the old QUIC connection lingers for 30s (idle timeout),
        // and its cleanup can kill the new connection. Aborting immediately prevents this.
        let peer_handlers: Arc<tokio::sync::Mutex<HashMap<iroh::PublicKey, tokio::task::JoinHandle<()>>>> =
            Arc::new(tokio::sync::Mutex::new(HashMap::new()));

        loop {
            match server_state.relay.accept().await {
                Some(conn) => {
                    let remote = conn.remote_public_key();
                    tracing::info!("New connection from: {}", remote);

                    // Abort old handler for this peer (prevents 30s QUIC timeout from killing new connection)
                    {
                        let mut handlers = peer_handlers.lock().await;
                        if let Some(old_handle) = handlers.remove(&remote) {
                            tracing::info!(peer = %remote, "Aborting old handler (peer reconnected)");
                            old_handle.abort();
                        }
                    }

                    let router = server_state.router.clone();
                    let app_clone = app.clone();
                    let cameras: Arc<RwLock<Vec<CameraInfo>>> = cameras_state.clone();
                    let gen_counter: Arc<AtomicU64> = camera_gen.clone();
                    let handlers_clone = peer_handlers.clone();

                    let handle = tokio::spawn(async move {
                        let detect_timeout = Duration::from_secs(2);

                        tokio::select! {
                            result = conn.accept_frame_stream() => {
                                match result {
                                    Ok(receiver) => {
                                        tracing::info!(peer = %remote, "Detected as camera");

                                        let source_id = SourceId::from_node_id_bytes(
                                            remote.as_bytes()
                                        );
                                        let source_str = format!("{}", source_id);

                                        // Assign a generation to this connection so stale
                                        // disconnect handlers don't remove a newer entry.
                                        let gen = gen_counter.fetch_add(1, Ordering::Relaxed);

                                        // Add to cameras list (remove any stale entry first)
                                        {
                                            let mut cams = cameras.write().await;
                                            cams.retain(|c| c.id != source_str);
                                            cams.push(CameraInfo {
                                                id: source_str.clone(),
                                                name: format!("Camera {}", &source_str[..8]),
                                                connected: true,
                                                last_frame_time: None,
                                                generation: gen,
                                            });
                                        }

                                        match app_clone.emit("camera-event", CameraEvent {
                                            source_id: source_str.clone(),
                                            connected: true,
                                        }) {
                                            Ok(_) => tracing::info!(gen, "Emitted camera-event connected for {}", &source_str[..8]),
                                            Err(e) => tracing::error!("Failed to emit camera-event: {}", e),
                                        }

                                        // Handle camera frames via router (broadcasts to clients + local subscriber)
                                        if let Err(e) = router.handle_camera_with_receiver(remote, receiver).await {
                                            tracing::warn!(peer = %remote, error = %e, "Camera handler error");
                                        }

                                        // Camera disconnected - only remove if generation matches
                                        // (a newer connection may have already replaced this entry)
                                        {
                                            let mut cams = cameras.write().await;
                                            let had_entry = cams.len();
                                            cams.retain(|c| !(c.id == source_str && c.generation == gen));
                                            if cams.len() < had_entry {
                                                // We actually removed the entry, emit disconnect
                                                drop(cams);
                                                let _ = app_clone.emit("camera-event", CameraEvent {
                                                    source_id: source_str,
                                                    connected: false,
                                                });
                                            } else {
                                                tracing::info!(gen, "Stale disconnect for camera {}, newer connection exists", &source_str[..8]);
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        tracing::warn!(peer = %remote, error = %e, "Failed to accept stream");
                                    }
                                }
                            }
                            _ = tokio::time::sleep(detect_timeout) => {
                                tracing::info!(peer = %remote, "Detected as client");
                                if let Err(e) = router.handle_client(conn).await {
                                    tracing::warn!(peer = %remote, error = %e, "Client handler error");
                                }
                            }
                        }

                        // Clean up handler entry on exit
                        handlers_clone.lock().await.remove(&remote);
                    });

                    // Store the handle so we can abort it if the peer reconnects
                    peer_handlers.lock().await.insert(remote, handle);
                }
                None => {
                    tracing::info!("Relay accept returned None, stopping accept loop");
                    break;
                }
            }
        }
    });

    *state.accept_task.write().await = Some(accept_task);

    Ok(public_key)
}

/// Stop the server
#[tauri::command]
pub async fn stop_server(state: State<'_, AppState>) -> Result<(), String> {
    let mut mode = state.mode.write().await;
    if *mode != AppMode::Server {
        return Err("Not running as server".to_string());
    }

    // Abort accept task
    if let Some(task) = state.accept_task.write().await.take() {
        task.abort();
    }

    // Clean up cameras list
    state.cameras.write().await.clear();

    // Clean up server state
    let mut server = state.server.write().await;
    if let Some(server_state) = server.take() {
        server_state.relay.close().await;
    }

    *mode = AppMode::Idle;
    tracing::info!("Server stopped");

    Ok(())
}

/// Get server status
#[tauri::command]
pub async fn get_server_status(state: State<'_, AppState>) -> Result<ServerStatus, String> {
    let mode = state.mode.read().await;
    let server = state.server.read().await;

    if *mode != AppMode::Server || server.is_none() {
        return Ok(ServerStatus {
            running: false,
            public_key: None,
            cameras_connected: 0,
            clients_connected: 0,
            frames_received: 0,
        });
    }

    let server_state = server.as_ref().unwrap();
    let stats = server_state.handle.stats().await;

    Ok(ServerStatus {
        running: true,
        public_key: Some(server_state.relay.public_key().to_string()),
        cameras_connected: stats.cameras_connected,
        clients_connected: stats.clients_connected,
        frames_received: stats.frames_received,
    })
}

/// Connect to an external server as a client
#[tauri::command]
pub async fn connect_to_server(
    app: AppHandle,
    state: State<'_, AppState>,
    server_key: String,
) -> Result<(), String> {
    let mut mode = state.mode.write().await;
    if *mode != AppMode::Idle {
        return Err("Already running in another mode".to_string());
    }

    // Parse server key
    let public_key: iroh::PublicKey = server_key.parse()
        .map_err(|e| format!("Invalid server key: {}", e))?;

    // Create ephemeral relay
    let relay = Relay::new(None).await
        .map_err(|e| format!("Failed to create relay: {}", e))?;

    tracing::info!("Connecting to server: {}", server_key);

    // Connect to server
    let conn = relay.connect(public_key).await
        .map_err(|e| format!("Failed to connect: {}", e))?;

    tracing::info!("Connected to server!");

    // Store client state
    let client_state = Arc::new(ClientState {
        relay,
        server_key: public_key,
    });

    *state.client.write().await = Some(client_state);
    *mode = AppMode::Client;

    // Spawn frame receiving loop
    let cameras_state: Arc<RwLock<Vec<CameraInfo>>> = state.cameras.clone();
    let receive_task = tokio::spawn(async move {
        // Wait for server to detect us as a client and open a stream
        tokio::time::sleep(Duration::from_secs(3)).await;

        let receiver = match conn.accept_frame_stream().await {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("Failed to accept frame stream: {}", e);
                return;
            }
        };

        tracing::info!("Frame stream opened, receiving frames...");

        let mut muxers: HashMap<SourceId, SourceMuxState> = HashMap::new();
        let mut known_sources: std::collections::HashSet<SourceId> = std::collections::HashSet::new();

        loop {
            match receiver.recv().await {
                Ok(Some(frame)) => {
                    // Track new cameras by source ID
                    if known_sources.insert(frame.source) {
                        let source_str = format!("{}", frame.source);

                        {
                            let mut cams = cameras_state.write().await;
                            cams.push(CameraInfo {
                                id: source_str.clone(),
                                name: format!("Camera {}", &source_str[..8]),
                                connected: true,
                                last_frame_time: None,
                                generation: 0, // Client mode doesn't have reconnection race
                            });
                        }

                        let _ = app.emit("camera-event", CameraEvent {
                            source_id: source_str,
                            connected: true,
                        });
                    }

                    process_frame(&app, &frame, &mut muxers);
                }
                Ok(None) => {
                    tracing::info!("Frame stream closed by server");
                    break;
                }
                Err(e) => {
                    tracing::error!("Frame stream error: {}", e);
                    break;
                }
            }
        }
    });

    *state.receive_task.write().await = Some(receive_task);

    Ok(())
}

/// Disconnect from server
#[tauri::command]
pub async fn disconnect(state: State<'_, AppState>) -> Result<(), String> {
    let mut mode = state.mode.write().await;
    if *mode != AppMode::Client {
        return Err("Not connected as client".to_string());
    }

    // Abort receive task
    if let Some(task) = state.receive_task.write().await.take() {
        task.abort();
    }

    // Clean up cameras list
    state.cameras.write().await.clear();

    // Clean up client state
    let mut client = state.client.write().await;
    if let Some(client_state) = client.take() {
        client_state.relay.close().await;
    }

    *mode = AppMode::Idle;
    tracing::info!("Disconnected from server");

    Ok(())
}

/// Get connection status
#[tauri::command]
pub async fn get_connection_status(state: State<'_, AppState>) -> Result<ConnectionStatus, String> {
    let mode = state.mode.read().await;
    let client = state.client.read().await;

    let server_key = client.as_ref().map(|c| c.server_key.to_string());

    Ok(ConnectionStatus {
        connected: *mode == AppMode::Client,
        mode: *mode,
        server_key,
    })
}

/// List connected cameras
#[tauri::command]
pub async fn list_cameras(state: State<'_, AppState>) -> Result<Vec<CameraInfo>, String> {
    let cameras = state.cameras.read().await;
    Ok(cameras.clone())
}

/// Get camera info
#[tauri::command]
pub async fn get_camera_info(
    state: State<'_, AppState>,
    camera_id: String,
) -> Result<Option<CameraInfo>, String> {
    let cameras = state.cameras.read().await;
    Ok(cameras.iter().find(|c| c.id == camera_id).cloned())
}

/// Get storage statistics
#[tauri::command]
pub async fn get_storage_stats(state: State<'_, AppState>) -> Result<StorageStats, String> {
    let server = state.server.read().await;

    if let Some(ref server_state) = *server {
        if let Some(ref storage) = server_state.storage {
            let stats = storage.stats().await;
            return Ok(StorageStats {
                enabled: true,
                bytes_used: stats.bytes_used,
                frames_stored: stats.frames_stored,
                bytes_cleaned: stats.bytes_cleaned,
            });
        }
    }

    Ok(StorageStats {
        enabled: false,
        bytes_used: 0,
        frames_stored: 0,
        bytes_cleaned: 0,
    })
}

/// Configure storage
#[tauri::command]
pub async fn configure_storage(
    state: State<'_, AppState>,
    config: StorageConfig,
) -> Result<(), String> {
    let mut settings = state.settings.write().await;
    settings.storage = config;
    Ok(())
}

/// Get application settings
#[tauri::command]
pub async fn get_settings(state: State<'_, AppState>) -> Result<AppSettings, String> {
    let settings = state.settings.read().await;
    Ok(settings.clone())
}

/// Save application settings
#[tauri::command]
pub async fn save_settings(
    state: State<'_, AppState>,
    settings: AppSettings,
) -> Result<(), String> {
    let mut current = state.settings.write().await;
    *current = settings;
    Ok(())
}
