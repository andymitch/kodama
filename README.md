# Kodama

[![CI](https://github.com/andymitch/kodama/actions/workflows/ci.yml/badge.svg)](https://github.com/andymitch/kodama/actions/workflows/ci.yml)
[![Firmware](https://github.com/andymitch/kodama/actions/workflows/release-firmware.yml/badge.svg)](https://github.com/andymitch/kodama/actions/workflows/release-firmware.yml)
[![Release](https://img.shields.io/github/v/release/andymitch/kodama?include_prereleases&label=firmware)](https://github.com/andymitch/kodama/releases/tag/alpha)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](LICENSE-MIT)
[![MSRV](https://img.shields.io/badge/rust-1.75%2B-orange.svg)](https://www.rust-lang.org)
[![Platform](https://img.shields.io/badge/platform-linux%2Faarch64-green.svg)](https://www.raspberrypi.com/products/raspberry-pi-zero-2-w/)

Privacy-focused P2P security camera system built on [Iroh](https://iroh.computer). Stream video from Raspberry Pi cameras to servers and clients over end-to-end encrypted QUIC connections, with no mandatory cloud dependency.

> **Looking for the desktop app or UI?** See [kodama-app](https://github.com/andymitch/kodama-app) (BSL 1.1).

## Features

- **End-to-end Encryption**: All connections use Iroh/QUIC — traffic is encrypted between peers with no central authority
- **Adaptive Bitrate**: Camera-side ABR with 4 quality tiers (500K-4M), adjusts to network conditions with hysteresis
- **Cellular Failover**: Automatic WiFi-to-cellular handoff with reconnection and ABR ramp-up (~25s failover)
- **Multi-channel Streaming**: Video, audio, and telemetry multiplexed as separate channels over a single QUIC stream
- **Per-connection Rate Limiting**: Lock-free atomic rate limiter with abuse detection (120fps video, 100fps audio, 10fps telemetry)
- **Pluggable Storage**: Local filesystem or cloud (S3/R2) backends with retention policies and cleanup
- **GPS & Telemetry**: CPU, memory, temperature, GPS position, and motion detection streamed alongside video
- **HTTP API + WebSocket**: axum-based HTTP API with WebSocket bridge and fMP4 muxing for browser playback

## Status

The core streaming pipeline (camera -> server -> client/browser) is **production-ready** and deployed on Raspberry Pi hardware with cellular failover.

| Component | Status |
|---|---|
| Library (`kodama`) | Production - single crate with feature-gated modules |
| Firmware (`kodama-firmware`) | Production - camera streaming, ABR, reconnection, relay mode |
| Server (`kodama-server`) | Production - routing, rate limiting, storage, HTTP API |

## Quick Start

```bash
# Clone and setup
git clone https://github.com/andymitch/kodama.git
cd kodama
./scripts/setup.sh   # Pin crypto dependencies for Iroh compatibility

# Build everything
cargo build

# Run tests
cargo test --workspace
```

### Running

```bash
# 1. Start the server (HTTP API on port 3000, prints public key)
cargo run -p kodama-server

# 2. Start a camera with synthetic test source (no hardware needed)
KODAMA_SERVER_KEY=<key> cargo run -p kodama-firmware --features test-source -- --mode camera --test-source

# 3. Connect via HTTP API, WebSocket, or use kodama-app for a full UI
```

### Using with kodama-app UI

The server ships with an HTTP API but no bundled UI. To get the polished SvelteKit UI:

```bash
# Option 1: Use the desktop app (from kodama-app repo)
# Option 2: Use pre-built static files with the server
KODAMA_UI_PATH=/path/to/kodama-ui/build kodama-server
```

## Architecture

```
Camera (capture) --> Iroh QUIC --> Server (Router --> broadcast) --> WS+MSE --> Browser
                                           |
                                     StorageBackend
```

Cameras open a persistent QUIC stream and push frames. The server detects cameras vs clients by behavior (cameras open a stream within 2s; clients wait). Frames are broadcast to all subscribed clients and optionally written to storage. The web module muxes H.264 into fMP4 and delivers it over WebSocket for MSE playback in browsers.

### Workspace Layout

```
kodama/
├── crates/kodama/                 # Single library crate (feature-gated)
│                                  #   Core types, transport, capture, storage, server, web
├── apps/
│   ├── kodama-firmware/           #   Camera or relay (--mode camera|relay)
│   └── kodama-server/             #   Headless server + HTTP API
├── pi/                            # Pi system configs (gpsd, NetworkManager, systemd)
└── scripts/
    ├── setup.sh                   #   Pin crypto dependencies
    ├── pi.sh                      #   Pi management (setup, deploy, wifi-off)
    └── test-e2e.sh                #   Full pipeline test (server + firmware)
```

### Related Repos

| Repo | License | Description |
|---|---|---|
| [kodama](https://github.com/andymitch/kodama) (this repo) | MIT / Apache-2.0 | Core library, server, firmware |
| [kodama-app](https://github.com/andymitch/kodama-app) | BSL 1.1 | Desktop/mobile app + SvelteKit UI |

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

### Frame Format

```
Wire: [4-byte length prefix][22-byte header][payload]

Header:
┌──────────┬─────────┬───────┬───────────┬────────┐
│ source   │ channel │ flags │ timestamp │ length │
│ (8 bytes)│ (1 byte)│(1 byte)│ (8 bytes)│(4 bytes)│
└──────────┴─────────┴───────┴───────────┴────────┘
```

- **Channels**: Video (0), Audio (1), Telemetry (2)
- **Flags**: KEYFRAME (0x01)
- **Max payload**: 2 MB (enforced on wire)

## Pi Deployment

Kodama runs on a Raspberry Pi Zero 2W with an IMX219 camera sensor, optional GPS (SIM7600G-H), and cellular connectivity.

```bash
# First-time setup (installs deps, configures GPS/cellular, deploys binary)
./scripts/pi.sh setup

# Quick deploy after code changes (cross-compile + scp)
./scripts/pi.sh deploy

# Toggle WiFi off for cellular failover testing (auto-restores after N seconds)
./scripts/pi.sh wifi-off 60
```

The deploy script cross-compiles for `aarch64-unknown-linux-gnu` and pushes the binary over SSH. System configs in `pi/` handle GPS daemon setup, NetworkManager cellular routing, and service management.

## Testing

```bash
# All unit tests
cargo test --workspace

# E2E regression suite (real QUIC connections, no hardware)
cargo test -p kodama --test e2e

# Full pipeline test (builds release, runs server + firmware for 20s)
./scripts/test-e2e.sh
```

The E2E suite validates frame flow through the router, 2 MB frame size enforcement, and per-connection rate limiting using real Iroh QUIC endpoints.

## Environment Variables

| Variable | Description | Default |
|---|---|---|
| `KODAMA_SERVER_KEY` | Server's base32 public key (required for camera) | - |
| `KODAMA_KEY_PATH` | Path to persistent keypair file | `./server.key` or `./camera.key` |
| `KODAMA_STORAGE_PATH` | Recording storage location (enables recording) | disabled |
| `KODAMA_STORAGE_MAX_GB` | Maximum storage size in GB | `10` |
| `KODAMA_RETENTION_DAYS` | Recording retention period | `7` |
| `KODAMA_BUFFER_SIZE` | Broadcast channel capacity | `512` |
| `KODAMA_WEB_PORT` | Web server port (server only) | `3000` |
| `KODAMA_UI_PATH` | Path to static UI build directory | auto-detect |
| `KODAMA_ABR` | Set to `0` to disable adaptive bitrate | enabled |
| `KODAMA_MODE` | Firmware mode: `camera` or `relay` | `camera` |
| `KODAMA_UPSTREAM_KEY` | Upstream server key (relay mode only) | - |
| `RUST_LOG` | Tracing filter (e.g., `kodama=debug`) | `kodama=info` |

## Development

### Prerequisites

- Rust stable (1.75+)
- FFmpeg (for video muxing - server)
- For Pi deployment: `aarch64-unknown-linux-gnu` cross-compilation toolchain

### Crypto Dependency Pinning

Iroh depends on pre-release `curve25519-dalek` and `ed25519-dalek` which require pinned `digest` and `sha2` versions. After any `cargo update`, re-run:

```bash
./scripts/setup.sh
```

### Key Patterns

- 100% async with Tokio
- `anyhow::Result<T>` for errors throughout
- Work with `Relay`, `Router`, and `StorageBackend` abstractions - not Iroh primitives directly
- Feature flag `test-source` enables synthetic video/audio without camera hardware

## Roadmap

See [open issues](https://github.com/andymitch/kodama/issues) for the full list. Key priorities:

- [#6](https://github.com/andymitch/kodama/issues/6) - QR code camera registration
- [#7](https://github.com/andymitch/kodama/issues/7) - Production Pi deployment with minimal setup
- [#9](https://github.com/andymitch/kodama/issues/9) - Graceful shutdown with CancellationToken

## License

MIT OR Apache-2.0
