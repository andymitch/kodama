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
cargo build -p kodama-camera       # Specific app
cargo build -p kodama-camera --features test-source  # With synthetic test source
```

### Test
```bash
cargo test                         # All unit tests
cargo test <name_substring>        # Single test by name
./scripts/test-e2e.sh             # Full pipeline test
```

### Run
```bash
# Server (prints public key for cameras/clients)
cargo run -p kodama-server-cli

# Camera (requires server key)
KODAMA_SERVER_KEY=<base32_key> cargo run -p kodama-camera

# Camera with test source (no hardware)
KODAMA_SERVER_KEY=<key> cargo run -p kodama-camera --features test-source -- --test-source

# CLI client
KODAMA_SERVER_KEY=<key> cargo run -p kodama-client

# Desktop app (Tauri)
cd apps/kodama-desktop && npm run tauri dev

# Mobile app (Tauri)
cd apps/kodama-mobile && npm run tauri dev
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
cargo build --release --target aarch64-unknown-linux-gnu -p kodama-camera
sshpass -p "password" scp -o StrictHostKeyChecking=accept-new \
  target/aarch64-unknown-linux-gnu/release/kodama-camera \
  yurei@10.0.0.229:~/kodama/

# Run on Pi (replace <server_key> with actual key from desktop app)
sshpass -p "password" ssh yurei@10.0.0.229 \
  "cd ~/kodama && KODAMA_SERVER_KEY=<server_key> KODAMA_KEY_PATH=/home/yurei/kodama/camera.key ./kodama-camera"
```

### Notes
- Use `KODAMA_KEY_PATH=/home/yurei/kodama/camera.key` (default `/var/lib/kodama/camera.key` requires root)
- A `yurei` service may be running and holding the camera - stop it first:
  ```bash
  ssh yurei@10.0.0.229 "sudo systemctl stop yurei"
  # or
  ssh yurei@10.0.0.229 "sudo pkill -f '/usr/local/bin/yurei'"
  ```
- GPS requires `gpsd` service running with `/dev/ttyUSB1` and GPS enabled on the SIM7600 modem via ModemManager (handled by `kodama-gps.service`)

## Architecture

Kodama is a privacy-focused P2P security camera system using Iroh for transport.

### Module Layout (Cargo workspace)

**Library crates** (`crates/`):
- **kodama-core** - Frame type, Channel enum, SourceId, protocol constants (ALPN: "kodama/0")
- **kodama-capture** - Video/audio/telemetry capture, H.264 keyframe detection
- **kodama-relay** - Iroh endpoint wrapper (`transport/`) and frame serialization (`mux/`)
- **kodama-server** - Router (broadcast channel), ClientManager, StorageManager
- **kodama-storage** - StorageBackend trait with local filesystem and cloud (S3/R2) implementations

**Application crates** (`apps/`):
- **kodama-server-cli** - Headless server binary
- **kodama-camera** - Camera capture binary
- **kodama-client** - Lite CLI viewer (client-only)
- **kodama-desktop** - Tauri + SvelteKit desktop app (server + client modes)
- **kodama-mobile** - Tauri + SvelteKit mobile app (server + client modes)

### Data Flow
```
Camera (capture → Frame) → Iroh QUIC → Server (Router → broadcast) → Clients
                                              ↓
                                        StorageBackend
```

### Frame Format (18-byte header + 4-byte length prefix)
`4-byte length prefix on wire | [source_id: 8][channel: 1][flags: 1][timestamp: 8][payload: var]`

Channels: Video(0), Audio(1), Telemetry(2). Flags include KEYFRAME (0x01).

### Key Abstractions
- `Relay` - wraps Iroh endpoint, handles connections
- `RelayConnection::open_frame_stream()` - persistent QUIC stream for frames
- `Router` - broadcasts frames to clients via `tokio::sync::broadcast`
- `relay::mux::frame::{read_frame, write_frame}` - all binary frame I/O goes through here

### Peer Detection
Cameras open a frame stream immediately (they're senders). Clients wait for the server to open a stream to them.

## Environment Variables

Key variables (all prefixed `KODAMA_`):
- `KODAMA_SERVER_KEY` - Server's base32 public key (required for camera/client)
- `KODAMA_KEY_PATH` - Path to keypair file (default: ./camera.key or ./server.key)
- `KODAMA_STORAGE_PATH` - Recording location
- `KODAMA_BUFFER_SIZE` - Broadcast buffer capacity (default: 512)
- `RUST_LOG` - Tracing filter (e.g., `kodama=debug`)

## Key Patterns

- 100% async with Tokio
- Use `anyhow::Result<T>` for errors
- Work with existing abstractions (Relay, Router, StorageBackend) rather than Iroh primitives directly
- Feature flag `test-source` enables synthetic video/audio without hardware
- Code is source of truth; docs in `docs/architecture/` may lag

## Additional Documentation

See `AGENTS.md` for detailed binary responsibilities, interaction patterns, and extension guidance.
