# Architecture

Kodama is a privacy-focused P2P security camera system built on [Iroh](https://iroh.computer) for encrypted QUIC transport.

## Three-Module System

### Camera module (capture + firmware binary)

Runs in `kodama-firmware --mode camera`.

- Uses `capture::VideoCapture`, `AudioCapture`, and `TelemetryCapture` to produce per-channel payloads.
- Uses `Relay` to connect to a server/relay and send `Frame` objects over a persistent Iroh stream.
- All capture channels are funneled through a single `mpsc` channel (`CaptureMessage`) for multiplexing.

### Transport module (transport + relay mode)

- `transport::transport` wraps the Iroh `Endpoint` (as `KodamaEndpoint`) and provides connection primitives.
- `transport::mux` handles serialization and deserialization of `Frame` to raw bytes and back.
- `kodama-firmware --mode relay` embeds a lightweight router built on `broadcast::Sender<Frame>`:
  - Cameras push frames into the broadcast channel.
  - Clients subscribe and receive frames over a persistent stream.
  - Optional upstream connection pushes frames to a full server instance.

### Server module (server + web + kodama-server binary)

- `server::Router` owns a broadcast channel of frames and per-peer connection management.
- Cameras are detected by behavior (they open a frame stream first) and handled via `Router::handle_camera_with_receiver`.
- Clients are detected as connections that do not open a stream within a timeout; the server opens a stream out to them via `Router::handle_client`.
- `StorageManager` subscribes to the router's broadcast and writes frames to a pluggable `StorageBackend`.
- `web` module provides axum HTTP server with WebSocket bridge for browser clients.
- WebSocket protocol: binary messages with 1-byte type prefix (camera list, fMP4 init/media segments, telemetry, audio levels).
- fMP4 muxer converts H.264 Annex B to fragmented MP4 for MSE playback in browsers.

## Frame and Channel Model

At the heart of the system is `Frame`:

- `SourceId` -- stable identifier per camera/source (derived from the relay public key).
- `Channel` -- enum of `{Video, Audio, Telemetry, Unknown(u8)}`.
- `FrameFlags` -- includes KEYFRAME (used for video keyframe detection and storage decisions).
- `timestamp_us` -- microsecond timestamp filled in by producers.
- `payload` -- raw bytes, interpreted by the consumer based on `channel`.

The serialized frame format (see `protocol`) includes:
- Fixed-size header (source id, channel, flags, timestamp, length) followed by payload bytes.
- `transport::mux::frame` is the single place that converts between `Frame` and bytes; all binaries route binary I/O through this layer.

## Binary Responsibilities

### `kodama-firmware` (`apps/kodama-firmware/`)

Single binary, runtime mode selection via `--mode camera|relay` or `KODAMA_MODE` env var.

**Camera mode** (default):
- Creates a `Relay` with a long-lived keypair at `KODAMA_KEY_PATH`.
- Derives `SourceId` from the relay's public key.
- Connects to a server/relay using `KODAMA_SERVER_KEY`.
- Opens a persistent frame stream via `RelayConnection::open_frame_stream`.
- Spawns capture tasks (video, audio, telemetry) that push `CaptureMessage` onto a shared `mpsc` channel.
- Main loop converts messages into `Frame`s and sends over the persistent stream.
- Reconnection with exponential backoff (1s to 16s); ABR adjusts bitrate per network throughput.
- OTA firmware updates via command stream.

**Relay mode**:
- Lightweight relay: no storage, no command routing, no rate limiting.
- Accepts cameras and clients, forwards frames via `broadcast::Sender<Frame>`.
- Optional upstream forwarding when `KODAMA_UPSTREAM_KEY` is set.

### `kodama-server` (`apps/kodama-server/`)

- Full server with Router, storage, command routing, per-connection rate limiting, and HTTP API.
- Starts Iroh endpoint for camera/client connections + axum web server for browser clients.
- Serves static UI files (if `KODAMA_UI_PATH` is set) + WebSocket endpoint for live streaming.
- Accepts connections and infers peer role (camera opens stream quickly; client waits).
- Storage task subscribes to router and records frames; stats task logs periodically.

## Extending the System

When changing behavior, prefer to work with the existing abstraction layers:

- **Transport and framing:** Use `Relay`, `RelayConnection`, and `transport::mux` helpers rather than touching Iroh primitives directly.
- **Routing/recording:** Use `Router` + `StorageManager` APIs instead of re-implementing broadcast or storage loops.
- **Capture/telemetry:** Extend the `capture` module (and `CaptureMessage` in the firmware binary) when adding new sensor channels, then map them to new `Channel` variants if needed.
- **Web/browser:** Extend the `web` module for new REST endpoints or WebSocket message types.
