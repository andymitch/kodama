# Kodama

[![Rust](https://github.com/andymitch/kodama/actions/workflows/rust.yml/badge.svg)](https://github.com/andymitch/kodama/actions/workflows/rust.yml)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](LICENSE)

Privacy-focused P2P security camera system built on [Iroh](https://iroh.computer). Stream video from Raspberry Pi cameras to servers and clients over end-to-end encrypted QUIC connections, with no mandatory cloud dependency.

## Features

- **End-to-end Encryption**: All connections use Iroh/QUIC — traffic is encrypted between peers with no central authority
- **Adaptive Bitrate**: Camera-side ABR with 4 quality tiers (500K–4M), adjusts to network conditions with hysteresis
- **Cellular Failover**: Automatic WiFi-to-cellular handoff with reconnection and ABR ramp-up (~25s failover)
- **Multi-channel Streaming**: Video, audio, and telemetry multiplexed as separate channels over a single QUIC stream
- **Per-connection Rate Limiting**: Lock-free atomic rate limiter with abuse detection (120fps video, 100fps audio, 10fps telemetry)
- **Pluggable Storage**: Local filesystem or cloud (S3/R2) backends with retention policies and cleanup
- **GPS & Telemetry**: CPU, memory, temperature, GPS position, and motion detection streamed alongside video

## Status

The core streaming pipeline (camera → server → client) is **production-ready** and deployed on Raspberry Pi hardware with cellular failover. Desktop and mobile viewer UIs are under active development.

| Component | Status |
|---|---|
| Camera binary (`kodama-camera`) | Production — streaming, ABR, reconnection, cellular failover |
| Server library (`kodama-server`) | Production — routing, rate limiting, storage, command forwarding |
| TUI server (`kodama-cli`) | Functional — headless + basic TUI dashboard ([#19](https://github.com/andymitch/kodama/issues/19)) |
| Standalone relay (`kodama-relay-cli`) | Functional — lightweight frame forwarder |
| Desktop app (`kodama-desktop`) | In progress — MSE video backend done, UI under development ([#20](https://github.com/andymitch/kodama/issues/20)) |
| Mobile app (`kodama-mobile`) | Scaffold — Tauri structure in place, needs implementation ([#21](https://github.com/andymitch/kodama/issues/21)) |
| CLI viewer (`kodama-client`) | Functional — receives frames, displays telemetry stats |

## Quick Start

```bash
# Clone and setup
git clone https://github.com/andymitch/kodama.git
cd kodama
./scripts/setup.sh   # Pin crypto dependencies for Iroh compatibility

# Build everything
cargo build

# Run tests (77 unit + E2E tests)
cargo test --workspace --exclude kodama-mobile
```

### Running

```bash
# 1. Start the server (prints public key)
cargo run -p kodama-cli

# 2. Start a camera with synthetic test source (no hardware needed)
KODAMA_SERVER_KEY=<key> cargo run -p kodama-camera --features test-source -- --test-source

# 3. Start a client to verify frames are flowing
KODAMA_SERVER_KEY=<key> cargo run -p kodama-client
```

The server auto-detects whether stdout is a TTY: interactive terminal gets a ratatui dashboard, redirected output falls back to plain logging.

## Architecture

```
Camera (capture) ──> Iroh QUIC ──> Server (Router ──> broadcast) ──> Clients
                                           |
                                     StorageBackend
```

Cameras open a persistent QUIC stream and push frames. The server detects cameras vs clients by behavior (cameras open a stream within 2s; clients wait). Frames are broadcast to all subscribed clients and optionally written to storage.

### Workspace Layout

```
kodama/
├── crates/                    # Library crates
│   ├── kodama-core/           #   Frame format, Channel enum, SourceId, ALPN protocol
│   ├── kodama-capture/        #   Video/audio/telemetry capture, H.264 parsing, ABR
│   ├── kodama-relay/          #   Iroh endpoint wrapper, frame serialization (mux/)
│   ├── kodama-server/         #   Router, rate limiting, ClientManager, StorageManager
│   └── kodama-storage/        #   StorageBackend trait (local filesystem, S3/R2)
├── apps/                      # Application binaries
│   ├── kodama-cli/            #   TUI server (ratatui dashboard / headless)
│   ├── kodama-relay-cli/      #   Standalone relay (lightweight frame forwarder)
│   ├── kodama-camera/         #   Camera capture (Raspberry Pi + test source)
│   ├── kodama-client/         #   CLI viewer (telemetry stats)
│   ├── kodama-desktop/        #   Tauri + SvelteKit desktop app
│   └── kodama-mobile/         #   Tauri + SvelteKit mobile app
├── pi/                        # Pi system configs (gpsd, NetworkManager, systemd)
├── docs/architecture/         # ADRs and design specs
└── scripts/
    ├── setup.sh               #   Pin crypto dependencies
    ├── pi.sh                  #   Pi management (setup, deploy, wifi-off)
    └── test-e2e.sh            #   Full pipeline test (server + camera + client)
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
# All unit tests (73 tests across 5 crates)
cargo test --workspace --exclude kodama-mobile

# E2E regression suite (real QUIC connections, no hardware)
cargo test -p kodama-server --test e2e

# Full pipeline test (builds release, runs server + camera + client for 30s)
./scripts/test-e2e.sh
```

The E2E suite validates frame flow through the router, 2 MB frame size enforcement, and per-connection rate limiting using real Iroh QUIC endpoints.

## Environment Variables

| Variable | Description | Default |
|---|---|---|
| `KODAMA_SERVER_KEY` | Server's base32 public key (required for camera/client) | — |
| `KODAMA_KEY_PATH` | Path to persistent keypair file | `./server.key` or `./camera.key` |
| `KODAMA_STORAGE_PATH` | Recording storage location (enables recording) | disabled |
| `KODAMA_STORAGE_MAX_GB` | Maximum storage size in GB | `10` |
| `KODAMA_RETENTION_DAYS` | Recording retention period | `7` |
| `KODAMA_BUFFER_SIZE` | Broadcast channel capacity | `512` |
| `KODAMA_ABR` | Set to `0` to disable adaptive bitrate | enabled |
| `KODAMA_UPSTREAM_KEY` | Upstream server key (relay-cli only) | — |
| `RUST_LOG` | Tracing filter (e.g., `kodama=debug`) | `kodama=info` |

## Development

### Prerequisites

- Rust stable (1.75+)
- `bun` for frontend development (desktop/mobile apps)
- FFmpeg (for desktop app video muxing)
- For Pi deployment: `aarch64-unknown-linux-gnu` cross-compilation toolchain

### Crypto Dependency Pinning

Iroh depends on pre-release `curve25519-dalek` and `ed25519-dalek` which require pinned `digest` and `sha2` versions. After any `cargo update`, re-run:

```bash
./scripts/setup.sh
```

### Key Patterns

- 100% async with Tokio
- `anyhow::Result<T>` for errors throughout
- Work with `Relay`, `Router`, and `StorageBackend` abstractions — not Iroh primitives directly
- Feature flag `test-source` enables synthetic video/audio without camera hardware

## Roadmap

See [open issues](https://github.com/andymitch/kodama/issues) for the full list. Key priorities:

- [#19](https://github.com/andymitch/kodama/issues/19) — Full TUI server dashboard (tabs, bandwidth graphs, peer management)
- [#20](https://github.com/andymitch/kodama/issues/20) — Desktop app UI (live camera view, playback, alerts)
- [#21](https://github.com/andymitch/kodama/issues/21) — Mobile app UI (connection, streaming, notifications)
- [#6](https://github.com/andymitch/kodama/issues/6) — QR code camera registration
- [#7](https://github.com/andymitch/kodama/issues/7) — Production Pi deployment with minimal setup
- [#9](https://github.com/andymitch/kodama/issues/9) — Graceful shutdown with CancellationToken

## License

MIT OR Apache-2.0
