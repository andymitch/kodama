# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## General

- **Always use `bun` instead of `npm`/`node`** for JavaScript/TypeScript tasks (install, run, build, etc.)

## Commands

### Setup (Required after `cargo update`)
```bash
./scripts/setup.sh
```
This pins crypto dependencies (`sha2`, `digest`) to specific versions needed for Iroh compatibility.

### Build
```bash
cargo build                        # All workspace crates
cargo build -p kodama-server       # Headless server + web UI
cargo build -p kodama-firmware     # Camera/relay firmware
cargo build -p kodama-firmware --features test-source  # With synthetic test source
cd ui && bun install && bun run build  # Build SvelteKit frontend
```

### Test
```bash
cargo test --workspace --exclude kodama-app  # All unit + E2E tests
cargo test <name_substring>        # Single test by name
cargo test -p kodama --test e2e    # E2E regression suite (real QUIC, no hardware)
./scripts/test-e2e.sh             # Full pipeline test (builds release, runs server + firmware)
```

### Run
```bash
# Server (web UI on port 3000, prints public key)
cargo run -p kodama-server

# Camera firmware (requires server key)
KODAMA_SERVER_KEY=<base32_key> cargo run -p kodama-firmware

# Camera with test source (no hardware)
KODAMA_SERVER_KEY=<key> cargo run -p kodama-firmware --features test-source -- --mode camera --test-source

# Relay firmware (lightweight frame forwarder)
cargo run -p kodama-firmware -- --mode relay

# Desktop app (Tauri)
cd apps/kodama-app && bun install && bun run tauri dev
```

## Pi Deployment

### Pi Camera Setup
- **Device**: Pi Zero 2W
- **IP Address**: `10.0.0.229`
- **User**: `yurei`
- **Password**: `password`
- **Camera**: IMX219 sensor
- **OS**: Debian 13 (trixie) with `rpicam-vid` (NOT `libcamera-vid`)
- **GPS**: SimTech SIM7600G-H USB modem, NMEA on `/dev/ttyUSB1`, managed by `gpsd`

### Pi Management Script
```bash
# Full provisioning (fresh Pi): installs deps, configures GPS/cellular, deploys
./scripts/pi.sh setup [PI_HOST] [PI_USER] [PI_PASSWORD]

# Quick deploy after code changes
./scripts/pi.sh deploy

# Toggle WiFi off for cellular failover testing
./scripts/pi.sh wifi-off 60
```

System configs deployed to the Pi are in `pi/` (gpsd, NetworkManager, systemd).

### Manual Commands
```bash
# SSH into Pi
sshpass -p "password" ssh yurei@10.0.0.229

# Cross-compile and deploy manually
cargo build --release --target aarch64-unknown-linux-gnu -p kodama-firmware
sshpass -p "password" scp -o StrictHostKeyChecking=accept-new \
  target/aarch64-unknown-linux-gnu/release/kodama-firmware \
  yurei@10.0.0.229:~/kodama/

# Run on Pi (replace <server_key> with actual key from server)
sshpass -p "password" ssh yurei@10.0.0.229 \
  "cd ~/kodama && KODAMA_SERVER_KEY=<server_key> KODAMA_KEY_PATH=/home/yurei/kodama/camera.key ./kodama-firmware"
```

### Notes
- Use `KODAMA_KEY_PATH=/home/yurei/kodama/camera.key` (default `/var/lib/kodama/camera.key` requires root)
- GPS requires `gpsd` service running with `/dev/ttyUSB1` and GPS enabled on the SIM7600 modem via ModemManager (handled by `kodama-gps.service`)

## Architecture

Kodama is a privacy-focused P2P security camera system using Iroh for transport.

### Workspace Layout
```
kodama/
├── crates/kodama/                 # Single library crate (feature-gated)
│                                  #   Core types, transport, capture, storage, server, web
├── apps/
│   ├── kodama-firmware/           #   Camera or relay (--mode camera|relay)
│   ├── kodama-server/             #   Headless server + web UI (+ optional TUI)
│   └── kodama-app/                #   Tauri desktop app (embeds server)
│       └── src-tauri/
├── ui/                            # Shared SvelteKit frontend (adapter-static)
├── pi/                            # Pi system configs (gpsd, NetworkManager, systemd)
└── scripts/
    ├── setup.sh                   #   Pin crypto dependencies
    ├── pi.sh                      #   Pi management (setup, deploy, wifi-off)
    └── test-e2e.sh                #   Full pipeline test (server + firmware)
```

### Feature Flags (kodama library)
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

### Data Flow
```
Camera (capture → Frame) → Iroh QUIC → Server (Router → broadcast) → WS+MSE → Browser
                                              ↓
                                        StorageBackend
```

### Frame Format (22-byte header + 4-byte length prefix)
```
Wire: [4-byte length prefix][22-byte header][payload]

Header: [source_id: 8][channel: 1][flags: 1][timestamp: 8][length: 4]
```

Channels: Video(0), Audio(1), Telemetry(2). Flags include KEYFRAME (0x01). Max payload: 2 MB.

### Key Abstractions
- `Relay` - wraps Iroh endpoint, handles connections
- `RelayConnection::open_frame_stream()` - persistent QUIC stream for frames
- `Router` - broadcasts frames to clients via `tokio::sync::broadcast`
- `transport::mux::frame::{read_frame, write_frame}` - all binary frame I/O goes through here
- `web::start()` - axum server with static file serving + WebSocket for live streaming

### Peer Detection
Cameras open a frame stream immediately (they're senders). Clients wait for the server to open a stream to them.

## Environment Variables

Key variables (all prefixed `KODAMA_`):
- `KODAMA_SERVER_KEY` - Server's base32 public key (required for camera)
- `KODAMA_KEY_PATH` - Path to keypair file (default: ./camera.key or ./server.key)
- `KODAMA_STORAGE_PATH` - Recording location (enables recording)
- `KODAMA_STORAGE_MAX_GB` - Maximum storage size in GB (default: 10)
- `KODAMA_RETENTION_DAYS` - Recording retention period (default: 7)
- `KODAMA_BUFFER_SIZE` - Broadcast buffer capacity (default: 512)
- `KODAMA_WEB_PORT` - Web server port (default: 3000)
- `KODAMA_UI_PATH` - Path to SvelteKit build directory (default: embedded or ./ui/build)
- `KODAMA_ABR` - Set to `0` to disable adaptive bitrate
- `KODAMA_MODE` - Firmware mode: `camera` or `relay` (default: camera)
- `KODAMA_UPSTREAM_KEY` - Upstream server key (relay mode only)
- `KODAMA_TELEMETRY_INTERVAL` - Seconds between telemetry samples (default: 1)
- `KODAMA_TELEMETRY_HEARTBEAT` - Seconds between full telemetry heartbeats (default: 30)
- `KODAMA_TELEMETRY_GPS_THRESHOLD` - GPS position change threshold in degrees (default: 0.0001, ~11m)
- `RUST_LOG` - Tracing filter (e.g., `kodama=debug`)

## Key Patterns

- 100% async with Tokio
- Use `anyhow::Result<T>` for errors
- Work with existing abstractions (Relay, Router, StorageBackend) rather than Iroh primitives directly
- Feature flag `test-source` enables synthetic video/audio without hardware
- Code is source of truth

## Additional Documentation

See `AGENTS.md` for detailed binary responsibilities, interaction patterns, and extension guidance.
