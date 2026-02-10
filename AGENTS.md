# AGENTS.md

This file provides guidance to AI coding agents when working with code in this repository.

## Commands

### Setup
- Pin crypto dependencies after `cargo update` or when builds fail due to `digest`/`sha2` conflicts:
  - `./scripts/setup.sh`
  - or manually:
    - `cargo update -p sha2 --precise 0.11.0-rc.4`
    - `cargo update -p digest --precise 0.11.0-rc.9`

### Build
- Build everything (lib + all binaries):
  - `cargo build`
- Build in release mode:
  - `cargo build --release`
- Build specific binaries:
  - `cargo build -p kodama-server`
  - `cargo build -p kodama-firmware`
  - `cargo build -p kodama-firmware --features test-source`
- Build frontend:
  - `cd ui && bun install && bun run build`

### Tests
- Run all tests:
  - `cargo test --workspace --exclude kodama-app`
- Run tests for the library:
  - `cargo test -p kodama`
- Run a single test by name (substring match):
  - `cargo test <test_name_substring>`
- Run E2E regression suite:
  - `cargo test -p kodama --test e2e`

### Running the system

#### Server
- Start server (web UI on port 3000, prints public key):
  - `cargo run -p kodama-server`
- With explicit key, storage, and port:
  - `KODAMA_KEY_PATH=./server.key KODAMA_STORAGE_PATH=./recordings KODAMA_WEB_PORT=8080 cargo run -p kodama-server`

#### Firmware (Camera mode)
- Environment variable required by all camera runs:
  - `KODAMA_SERVER_KEY=<base32 server PublicKey printed by kodama-server>`
- Run camera against real Pi camera (on device):
  - `KODAMA_SERVER_KEY=<...> cargo run -p kodama-firmware`
- Run camera with test video source (no hardware required, feature flag must be enabled at build time):
  - `KODAMA_SERVER_KEY=<...> cargo run -p kodama-firmware --features test-source -- --mode camera --test-source`
- Useful environment overrides:
  - `KODAMA_KEY_PATH=/var/lib/kodama/camera.key` (where the camera keypair is stored)
  - `KODAMA_WIDTH`, `KODAMA_HEIGHT`, `KODAMA_FPS`
  - `KODAMA_TELEMETRY_INTERVAL` (seconds between telemetry samples, default: 1)
  - `KODAMA_TELEMETRY_HEARTBEAT` (seconds between full telemetry heartbeats, default: 30)
  - `KODAMA_TELEMETRY_GPS_THRESHOLD` (GPS position change threshold in degrees, default: 0.0001)

#### Firmware (Relay mode)
- Run a lightweight relay (frame forwarder, no storage or rate limiting):
  - `cargo run -p kodama-firmware -- --mode relay`
- Relay that forwards to an upstream server:
  - `KODAMA_UPSTREAM_KEY=<server_public_key> cargo run -p kodama-firmware -- --mode relay`
- Mode selection: `--mode relay` CLI arg or `KODAMA_MODE=relay` env var

#### Desktop app
- Run the Tauri desktop app (embeds server + web UI):
  - `cd apps/kodama-app && bun install && bun run tauri dev`

## Architecture Overview

### Cargo workspace layout
- Single library crate (`crates/kodama`) with feature-gated modules, plus application crates in `apps/`:
  - `crates/kodama` – Feature-gated library: core types, transport, capture, storage, server, web.
  - `apps/kodama-firmware` – Camera or relay binary (`--mode camera|relay`).
  - `apps/kodama-server` – Headless server with embedded web UI.
  - `apps/kodama-app` – Tauri desktop app (embeds server, same web UI).
- Shared SvelteKit frontend in `ui/` (adapter-static, used by server and Tauri app).

### Feature flags (kodama library)
```
default = []
transport       — Iroh endpoint, frame mux/demux
capture         — Video/audio/telemetry, ABR, H.264
storage         — Local + cloud backends (implies transport)
server          — Router, ClientManager, RateLimiter (implies transport + storage)
web             — axum HTTP, WebSocket bridge, fMP4 muxer (implies server)
test-source     — Synthetic video/audio (implies capture)
```

Core types (Frame, Channel, SourceId, Identity, protocol constants) always compile.

### Three-module system

Conceptually the system follows a three-module design:

- **Camera module (capture + firmware binary):**
  - Runs in `kodama-firmware --mode camera`.
  - Uses `capture::VideoCapture`, `AudioCapture`, and `TelemetryCapture` to produce per-channel payloads.
  - Uses `Relay` to connect to a server/relay and send `Frame` objects over a persistent Iroh stream.
  - All capture channels are funneled through a single `mpsc` channel (`CaptureMessage`) for multiplexing.

- **Transport module (transport + relay mode):**
  - `transport::transport` wraps the Iroh `Endpoint` (as `KodamaEndpoint`) and provides connection primitives.
  - `transport::mux` handles serialization and de/serialization of `Frame` to raw bytes and back.
  - `kodama-firmware --mode relay` embeds a lightweight router built on `broadcast::Sender<Frame>`:
    - Cameras push frames into the broadcast channel.
    - Clients subscribe and receive frames over a persistent stream.
    - Optional upstream connection pushes frames to a full server instance.

- **Server module (server + web + kodama-server binary):**
  - `server::Router` owns a broadcast channel of frames and per-peer connection management.
  - Cameras are detected by behavior (they open a frame stream first) and handled via `Router::handle_camera_with_receiver`.
  - Clients are detected as connections that do not open a stream within a timeout; the server opens a stream out to them via `Router::handle_client`.
  - `StorageManager` subscribes to the router's broadcast and writes frames to a pluggable `StorageBackend`.
  - `web` module provides axum HTTP server with WebSocket bridge for browser clients.
  - WebSocket protocol: binary messages with 1-byte type prefix (camera list, fMP4 init/media segments, telemetry, audio levels).
  - fMP4 muxer converts H.264 Annex B to fragmented MP4 for MSE playback in browsers.

### Frame and channel model

At the heart of the system is `Frame`:
- `SourceId` – stable identifier per camera/source (derived from the relay public key).
- `Channel` – enum of `{Video, Audio, Telemetry, Unknown(u8)}`.
- `FrameFlags` – includes KEYFRAME (used for video keyframe detection and storage decisions).
- `timestamp_us` – microsecond timestamp filled in by producers.
- `payload` – raw bytes, interpreted by the consumer based on `channel`.

The serialized frame format (see `protocol`) includes:
- Fixed-size header (source id, channel, flags, timestamp, length) followed by payload bytes.
- `transport::mux::frame` is the single place that converts between `Frame` and bytes; all binaries route binary I/O through this layer.

### Binary responsibilities and interaction patterns

**`kodama-firmware`** (`apps/kodama-firmware/src/main.rs`)
- Single binary, runtime mode selection via `--mode camera|relay` or `KODAMA_MODE` env var.
- **Camera mode** (default):
  - Creates a `Relay` with a long-lived keypair at `KODAMA_KEY_PATH`.
  - Derives `SourceId` from the relay's public key.
  - Connects to a server/relay using `KODAMA_SERVER_KEY`.
  - Opens a persistent frame stream via `RelayConnection::open_frame_stream`.
  - Spawns capture tasks (video, audio, telemetry) that push `CaptureMessage` onto a shared `mpsc` channel.
  - Main loop converts messages into `Frame`s and sends over the persistent stream.
  - Reconnection with exponential backoff (1s→16s); ABR adjusts bitrate per network throughput.
  - OTA firmware updates via command stream.
- **Relay mode**:
  - Lightweight relay: no storage, no command routing, no rate limiting.
  - Accepts cameras and clients, forwards frames via `broadcast::Sender<Frame>`.
  - Optional upstream forwarding when `KODAMA_UPSTREAM_KEY` is set.

**`kodama-server`** (`apps/kodama-server/src/main.rs`)
- Full server with Router, storage, command routing, per-connection rate limiting, and web UI.
- Starts Iroh endpoint for camera/client connections + axum web server for browser clients.
- Serves SvelteKit UI as static files + WebSocket endpoint for live streaming.
- Accepts connections and infers peer role (camera opens stream quickly; client waits).
- Storage task subscribes to router and records frames; stats task logs periodically.

**`kodama-app`** (`apps/kodama-app/src-tauri/`)
- Tauri desktop app that embeds the full server (Iroh + axum on localhost).
- Webview connects to embedded server via WebSocket — same path as a browser.
- No Tauri IPC needed for video/audio/telemetry; Tauri commands only for lifecycle management.

### How to extend or modify behavior

When changing behavior, prefer to work with the existing abstraction layers:
- **Transport and framing:** Use `Relay`, `RelayConnection`, and `transport::mux` helpers rather than touching Iroh primitives directly.
- **Routing/recording:** Use `Router` + `StorageManager` APIs instead of re-implementing broadcast or storage loops.
- **Capture/telemetry:** Extend the `capture` module (and `CaptureMessage` in the firmware binary) when adding new sensor channels, then map them to new `Channel` variants if needed.
- **Web/browser:** Extend the `web` module for new REST endpoints or WebSocket message types.

Prefer the **code** as the source of truth for current behavior.
