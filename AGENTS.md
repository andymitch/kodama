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
  - `cargo build -p kodama-cli`
  - `cargo build -p kodama-relay`
  - `cargo build -p kodama-camera`

### Tests
- Run all tests:
  - `cargo test`
- Run tests for a specific module (example: transport):
  - `cargo test -p kodama-transport` *(filters by package)*
- Run a single test by name (substring match):
  - `cargo test <test_name_substring>`
- Run E2E regression suite:
  - `cargo test -p kodama-server --test e2e`

### Running the system

#### Server (TUI)
- Start server with TUI dashboard (interactive terminal):
  - `cargo run -p kodama-cli`
- Headless mode (stdout redirected, behaves like plain logging):
  - `cargo run -p kodama-cli > /tmp/server.log 2>&1`
- With explicit key and recording location:
  - `KODAMA_KEY_PATH=./server.key KODAMA_STORAGE_PATH=/var/lib/kodama/recordings cargo run -p kodama-cli`

#### Standalone Relay
- Run a lightweight relay (frame forwarder, no storage or rate limiting):
  - `cargo run -p kodama-relay`
- Run relay with explicit key path:
  - `KODAMA_KEY_PATH=./relay.key cargo run -p kodama-relay`
- Relay that forwards everything to an upstream server while also serving local clients:
  - `KODAMA_UPSTREAM_KEY=<server_public_key> cargo run -p kodama-relay`

#### Camera
- Environment variable required by all camera runs:
  - `KODAMA_SERVER_KEY=<base32 server PublicKey printed by kodama-cli>`
- Run camera against real Pi camera (on device):
  - `KODAMA_SERVER_KEY=<...> cargo run -p kodama-camera`
- Run camera with test video source (no hardware required, feature flag must be enabled at build time):
  - `KODAMA_SERVER_KEY=<...> cargo run -p kodama-camera --features test-source -- --test-source`
- Useful environment overrides:
  - `KODAMA_KEY_PATH=/var/lib/kodama/camera.key` (where the camera keypair is stored)
  - `KODAMA_WIDTH`, `KODAMA_HEIGHT`, `KODAMA_FPS`
  - `KODAMA_TELEMETRY_INTERVAL` (seconds between telemetry samples, default: 1)
  - `KODAMA_TELEMETRY_HEARTBEAT` (seconds between full telemetry heartbeats, default: 30)
  - `KODAMA_TELEMETRY_GPS_THRESHOLD` (GPS position change threshold in degrees, default: 0.0001)

#### Desktop app
- Run the Tauri desktop app (embeds server + client):
  - `cd apps/kodama-desktop && bun run tauri dev`

## Architecture Overview

### Cargo workspace layout
- Cargo workspace with library crates in `crates/` and application crates in `apps/`:
  - `crates/kodama-core` – Frame type, Channel enum, SourceId, protocol constants (ALPN: "kodama/0").
  - `crates/kodama-capture` – Video/audio/telemetry capture, H.264 keyframe detection, ABR.
  - `crates/kodama-transport` – Iroh endpoint wrapper (`transport/`) and frame mux/demux (`mux/`).
  - `crates/kodama-server` – Router (broadcast), ClientManager, StorageManager, rate limiting.
  - `crates/kodama-storage` – StorageBackend trait with local filesystem and cloud (S3/R2) implementations.
  - `apps/kodama-cli` – TUI server binary (interactive dashboard or headless logging).
  - `apps/kodama-relay` – Standalone relay binary (lightweight frame forwarder).
  - `apps/kodama-camera` – Camera capture binary.
  - `apps/kodama-desktop` – Tauri + SvelteKit desktop app (server + client).
  - `apps/kodama-mobile` – Tauri + SvelteKit mobile app (stub).

### Three-module system

Conceptually the system follows a three-module design:

- **Camera module (capture/ + camera binary):**
  - Runs in `kodama-camera`.
  - Uses `capture::VideoCapture`, `AudioCapture`, and `TelemetryCapture` to produce per-channel payloads.
  - Uses `Relay` to connect to a server/relay and send `Frame` objects over a persistent Iroh stream.
  - All capture channels are funneled through a single `mpsc` channel (`CaptureMessage`) for multiplexing.

- **Transport module (transport/ and kodama-relay binary):**
  - `transport::transport` wraps the Iroh `Endpoint` (as `KodamaEndpoint`) and provides connection primitives.
  - `transport::mux` handles serialization and de/serialization of `Frame` to raw bytes and back.
  - `kodama-relay` embeds a lightweight router built on `broadcast::Sender<Frame>`:
    - Cameras push frames into the broadcast channel.
    - Clients subscribe and receive frames over a persistent stream.
    - Optional upstream connection pushes frames to a full server instance.

- **Server module (server/ + kodama-cli binary):**
  - `server::Router` owns a broadcast channel of frames and per-peer connection management.
  - Cameras are detected by behavior (they open a frame stream first) and handled via `Router::handle_camera_with_receiver`.
  - Clients are detected as connections that do not open a stream within a timeout; the server opens a stream out to them via `Router::handle_client`.
  - `StorageManager` subscribes to the router's broadcast and writes frames to a pluggable `StorageBackend`.
  - `kodama-cli` provides a TUI dashboard when run interactively, or falls back to plain logging in headless mode.

### Frame and channel model

At the heart of the system is `core::Frame`:
- `SourceId` – stable identifier per camera/source (derived from the relay public key).
- `Channel` – enum of `{Video, Audio, Telemetry, Unknown(u8)}`.
- `FrameFlags` – includes KEYFRAME (used for video keyframe detection and storage decisions).
- `timestamp_us` – microsecond timestamp filled in by producers.
- `payload` – raw bytes, interpreted by the consumer based on `channel`.

The serialized frame format (see `core::protocol`) includes:
- Fixed-size header (source id, channel, flags, timestamp, length) followed by payload bytes.
- `transport::mux::frame` is the single place that converts between `Frame` and bytes; all binaries route binary I/O through this layer.

### Binary responsibilities and interaction patterns

**`kodama-camera`** (`apps/kodama-camera/src/main.rs`)
- Builds a `Config` from env + CLI flags, then:
  - Creates a `Relay` with a long-lived keypair at `KODAMA_KEY_PATH`.
  - Derives `SourceId` from the relay's public key.
  - Connects to a server/relay using `KODAMA_SERVER_KEY`.
  - Opens a persistent frame stream via `RelayConnection::open_frame_stream`.
- Spawns capture tasks (video, audio, telemetry) that push `CaptureMessage` onto a shared `mpsc` channel.
- Main loop converts messages into `Frame`s and sends over the persistent stream.
- Reconnection with exponential backoff (1s→16s); ABR adjusts bitrate per network throughput.

**`kodama-cli`** (`apps/kodama-cli/src/main.rs`)
- Full server with Router, storage, command routing, and per-connection rate limiting.
- TUI mode (stdout is TTY): ratatui dashboard showing key, uptime, stats, peers, and log.
- Headless mode (stdout redirected): plain `tracing_subscriber::fmt()` logging, same as before.
- Accepts connections and infers peer role (camera opens stream quickly; client waits).
- Storage task subscribes to router and records frames; stats task logs periodically.

**`kodama-relay`** (`apps/kodama-relay/src/main.rs`, package: `kodama-relay-bin`)
- Lightweight relay: no storage, no command routing, no rate limiting.
- Accepts cameras and clients, forwards frames via `broadcast::Sender<Frame>`.
- Optional upstream forwarding when `KODAMA_UPSTREAM_KEY` is set.
- Periodic stats logging.

**`kodama-desktop`** (`apps/kodama-desktop/src-tauri/`)
- Tauri + SvelteKit desktop app that embeds server and client functionality.
- MSE video playback via FFmpeg fMP4 muxer and SourceBuffer.

### How to extend or modify behavior

When changing behavior, prefer to work with the existing abstraction layers:
- **Transport and framing:** Use `Relay`, `RelayConnection`, and `transport::mux` helpers rather than touching Iroh primitives directly.
- **Routing/recording:** Use `Router` + `StorageManager` APIs instead of re-implementing broadcast or storage loops.
- **Capture/telemetry:** Extend the `capture` module (and `CaptureMessage` in the camera binary) when adding new sensor channels, then map them to new `Channel` variants if needed.

Prefer the **code** as the source of truth for current behavior.
