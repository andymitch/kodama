# AGENTS.md

This file provides guidance to WARP (warp.dev) when working with code in this repository.

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
  - `cargo build --bin kodama-camera`
  - `cargo build --bin kodama-server`
  - `cargo build --bin kodama-desktop`
  - `cargo build --bin kodama-relay`

### Tests
- Run all tests:
  - `cargo test`
- Run tests for a specific module (example: relay):
  - `cargo test -p kodama relay` *(filters by test name/module)*
- Run a single test by name (substring match):
  - `cargo test <test_name_substring>`

### Running the system

#### Server
- Start server with default settings and ephemeral data-only storage:
  - `cargo run --bin kodama-server`
- Start server with explicit key and recording location:
  - `KODAMA_KEY_PATH=./server.key KODAMA_STORAGE_PATH=/var/lib/kodama/recordings cargo run --bin kodama-server`

#### Camera
- Environment variable required by all camera runs:
  - `KODAMA_SERVER_KEY=<base32 server PublicKey printed by kodama-server>`
- Run camera against real Pi camera (on device):
  - `KODAMA_SERVER_KEY=<...> cargo run --bin kodama-camera`
- Run camera with test video source (no hardware required, feature flag must be enabled at build time):
  - `cargo run --features test-source --bin kodama-camera -- --test-source`
- Useful environment overrides:
  - `KODAMA_KEY_PATH=/var/lib/kodama/camera.key` (where the camera keypair is stored)
  - `KODAMA_WIDTH`, `KODAMA_HEIGHT`, `KODAMA_FPS`
  - `KODAMA_TELEMETRY_INTERVAL` (seconds between telemetry updates)

#### Desktop client
- Connect a CLI desktop client to a running server:
  - `KODAMA_SERVER_KEY=<server_public_key> cargo run --bin kodama-desktop`
- Save all received frames to disk for debugging:
  - `KODAMA_SERVER_KEY=<server_public_key> cargo run --bin kodama-desktop -- --save-frames /tmp/kodama-frames`
- Disable telemetry logging in the client:
  - `KODAMA_SERVER_KEY=<server_public_key> cargo run --bin kodama-desktop -- --no-telemetry`

#### Relay
- Run a standalone relay (acts as a lightweight mini-server):
  - `cargo run --bin kodama-relay`
- Run relay with explicit key path:
  - `KODAMA_KEY_PATH=./relay.key cargo run --bin kodama-relay`
- Relay that forwards everything to an upstream kodama-server while also serving local clients:
  - `KODAMA_UPSTREAM_KEY=<server_public_key> cargo run --bin kodama-relay`

## Architecture Overview

### Top-level crate layout
- Single Rust crate `kodama` (no workspace) with modules:
  - `core/` – frame and protocol types, identity and keys.
  - `capture/` – video, audio, and telemetry capture plus H.264 helpers.
  - `relay/` – Iroh endpoint wrapper and frame mux/demux utilities.
  - `server/` – router, client management, and storage coordination.
  - `storage/` – local filesystem and cloud (S3/R2-style) backends.
  - `bin/` – application binaries: `kodama-camera`, `kodama-server`, `kodama-desktop`, `kodama-relay`.

`src/lib.rs` re-exports the main types and modules, so consumers can mostly work via the crate root (`kodama::...`) rather than deep paths.

### Three-module system

Conceptually the system is still the three-module design described in `docs/architecture/adr-001-three-module-system.md`, but collapsed into a single crate:
- **Camera module (capture/ + embedded relay):**
  - Runs in `kodama-camera`.
  - Uses `capture::VideoCapture`, `AudioCapture`, and `TelemetryCapture` to produce per-channel payloads.
  - Uses `Relay` to connect to a server/relay and send `Frame` objects over a persistent Iroh stream.
  - All capture channels are funneled through a single `mpsc` channel (`CaptureMessage`) for multiplexing.

- **Relay module (relay/ and kodama-relay binary):**
  - `relay::transport` wraps the Iroh `Endpoint` (as `KodamaEndpoint`) and provides connection primitives.
  - `relay::mux` handles serialization and de/serialization of `Frame` to raw bytes and back.
  - `kodama-relay` embeds a lightweight router built on a `broadcast::Sender<Frame>`:
    - Cameras are modeled as producers that push frames into the broadcast channel.
    - Clients subscribe and receive frames via `FrameReceiver` over a persistent stream.
    - Optional upstream connection pushes the same frames to a full `kodama-server` instance.

- **Server module (server/ + kodama-server binary):**
  - `server::Router` owns a broadcast channel of frames and per-peer connection management.
  - Cameras are detected by behavior (they open a frame stream first) and handled via `Router::handle_camera_with_receiver`.
  - Clients are detected as connections that do not open a stream within a timeout; the server opens a stream out to them via `Router::handle_client`.
  - `StorageManager` subscribes to the router’s broadcast and writes frames to a pluggable `StorageBackend` implementation.

### Frame and channel model

At the heart of the system is `core::Frame`:
- `SourceId` – stable identifier per camera/source (derived from the relay public key).
- `Channel` – enum of `{Video, Audio, Telemetry}`.
- `FrameFlags` – includes KEYFRAME (used for video keyframe detection and storage decisions).
- `timestamp_us` – microsecond timestamp filled in by producers (camera/telemetry code).
- `payload` – raw bytes, interpreted by the consumer based on `channel`.

The serialized frame format (see ADR-001 and `core::protocol`) includes:
- Fixed-size header (source id, channel, flags, timestamp, length) followed by payload bytes.
- `relay::mux::frame` is the single place that knows how to convert between `Frame` and bytes; all binaries should route binary I/O through this layer rather than hand-rolling parsing.

### Binary responsibilities and interaction patterns

**`kodama-camera`** (`src/bin/kodama-camera/main.rs`)
- Builds a `Config` from env + CLI flags, then:
  - Creates a `Relay` with a long-lived keypair at `KODAMA_KEY_PATH` (or default `/var/lib/kodama/camera.key`).
  - Derives `SourceId` from the relay’s public key.
  - Connects to a server/relay using `KODAMA_SERVER_KEY`.
  - Opens a persistent frame stream via `RelayConnection::open_frame_stream`.
- Spawns capture tasks:
  - Video: real camera via `VideoCapture::start` or synthetic tests via `start_test_source` (feature gated).
  - Audio: `AudioCapture` or test audio when enabled.
  - Telemetry: `TelemetryCapture` at configurable intervals.
- Each capture task pushes `CaptureMessage::{Video, Audio, Telemetry}` onto a shared `mpsc` channel.
- Main loop converts messages into typed `Frame`s, sets timestamps, applies H.264 keyframe detection, and sends over the persistent stream; it periodically logs throughput statistics.

**`kodama-server`** (`src/bin/kodama-server/main.rs`)
- Initializes a `Relay` (persistent key at `KODAMA_KEY_PATH`) and a `Router` with a configurable buffer size.
- Optionally wires up recording by creating:
  - `LocalStorage` with capacity and segment configuration.
  - `StorageManager` with retention and cleanup policies.
- Accepts incoming connections and infers peer role:
  - Cameras: connections that open a frame stream quickly; handled via `Router::handle_camera_with_receiver`, which consumes frames and broadcasts them.
  - Clients: connections that don’t open a stream; handled via `Router::handle_client`, which subscribes to broadcast and writes frames to the client over a persistent stream.
- Background tasks:
  - Storage task subscribes to router, calls `StorageManager::store` for each frame, and logs periodic stats.
  - Stats task periodically logs router metrics (cameras/clients connected, frames received/broadcast) plus storage usage.

**`kodama-desktop`** (`src/bin/kodama-desktop/main.rs`)
- CLI client that:
  - Creates an ephemeral `Relay`.
  - Connects to the server via `KODAMA_SERVER_KEY`.
  - Waits for server-initiated frame stream (client role) and receives frames via `FrameReceiver`.
- Maintains per-`SourceId` statistics for video/audio/telemetry and logs:
  - Per-source FPS, bitrate, keyframe counts.
  - Telemetry decoded via `capture::decode_telemetry` (CPU, memory, temperature, uptime, etc.).
- Optionally saves raw frame payloads to disk for debugging when `--save-frames` is passed.

**`kodama-relay`** (`src/bin/kodama-relay/main.rs`)
- Behavior is similar to a very small server:
  - Holds a `FrameRouter` with an internal broadcast channel and per-peer bookkeeping.
  - Cameras connect and are detected by opening a frame stream; their frames are forwarded into the router.
  - Clients connect and are served frames from the router via a persistent stream.
- Optional upstream mode:
  - When `KODAMA_UPSTREAM_KEY` is set, a separate background `Relay` connects to a full server and forwards frames from the local broadcast to that upstream.
- Periodic stats logging summarizes connected cameras/clients and forwarded volume.

### How to extend or modify behavior

When changing behavior, prefer to work with the existing abstraction layers:
- **Transport and framing:** Use `Relay`, `RelayConnection`, and `relay::mux` helpers rather than touching Iroh primitives directly.
- **Routing/recording:** Use `Router` + `StorageManager` APIs instead of re-implementing your own broadcast or storage loops.
- **Capture/telemetry:** Extend the `capture` module (and `CaptureMessage` in the camera binary) when adding new sensor channels, then map them to new `Channel` variants if needed.

The architecture docs under `docs/architecture/` are occasionally ahead of or behind the implementation. Prefer the **code** as the source of truth for current behavior, and use the ADR/POC docs for high-level intent when designing new features.