# Kodama

[![Rust](https://github.com/andymitch/kodama/actions/workflows/rust.yml/badge.svg)](https://github.com/andymitch/kodama/actions/workflows/rust.yml)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](LICENSE)

Privacy-focused P2P security camera system built on [Iroh](https://iroh.computer).

## Features

- **Privacy-first**: Self-hostable with no mandatory cloud dependency
- **P2P Transport**: Direct peer-to-peer connections via Iroh/QUIC with end-to-end encryption
- **Adaptive Bitrate**: Camera-side ABR adjusts encoding quality based on network throughput
- **Cellular Failover**: Automatic WiFi-to-cellular failover with reconnection and ABR ramp-up
- **Multi-channel Streaming**: Video, audio, and telemetry as separate multiplexed channels
- **Flexible Deployment**: Raspberry Pi camera, desktop/mobile viewer, cloud or local server
- **Live Video**: Sub-second latency via FFmpeg fMP4 muxing and MSE playback

## Quick Start

```bash
# Clone and setup
git clone https://github.com/andymitch/kodama.git
cd kodama
./scripts/setup.sh   # Pin crypto dependencies for Iroh compatibility

# Build everything
cargo build

# Run tests
cargo test
```

### Running

```bash
# 1. Start the server (prints public key)
cargo run -p kodama-server-cli

# 2. Start a camera (pass server's public key)
KODAMA_SERVER_KEY=<key> cargo run -p kodama-camera

# 3. View in desktop app
cd apps/kodama-desktop && bun run tauri dev
```

Use `--features test-source` with `-- --test-source` for a synthetic video source (no camera hardware needed).

## Architecture

```
Camera (capture) ──▶ Iroh QUIC ──▶ Server (Router ──▶ broadcast) ──▶ Clients
                                           │
                                     StorageBackend
```

### Workspace Layout

```
kodama/
├── crates/                    # Library crates
│   ├── kodama-core/           # Frame format, Channel enum, SourceId, protocol constants
│   ├── kodama-capture/        # Video/audio/telemetry capture, H.264 parsing, ABR
│   ├── kodama-relay/          # Iroh endpoint wrapper and frame serialization
│   ├── kodama-server/         # Router (broadcast), ClientManager, StorageManager
│   └── kodama-storage/        # StorageBackend trait (local filesystem, S3/R2)
├── apps/                      # Application binaries
│   ├── kodama-camera/         # Camera binary (Raspberry Pi)
│   ├── kodama-server-cli/     # Headless server
│   ├── kodama-client/         # CLI viewer
│   ├── kodama-desktop/        # Tauri + SvelteKit desktop app
│   └── kodama-mobile/         # Tauri + SvelteKit mobile app
└── scripts/
    ├── setup.sh               # Pin crypto dependencies
    ├── pi-setup.sh            # First-time Pi provisioning
    ├── pi-deploy.sh           # Quick rebuild and deploy to Pi
    └── test-e2e.sh            # End-to-end pipeline test
```

### Frame Format (22-byte header)

```
┌──────────┬─────────┬───────┬───────────┬────────┬─────────┐
│ source   │ channel │ flags │ timestamp │ length │ payload │
│ (8 bytes)│ (1 byte)│(1 byte)│ (8 bytes)│(4 bytes)│  (var)  │
└──────────┴─────────┴───────┴───────────┴────────┴─────────┘
```

Channels: Video (0), Audio (1), Telemetry (2). Flags: KEYFRAME (0x01).

## Pi Deployment

Kodama runs on a Raspberry Pi Zero 2W with an IMX219 camera sensor.

```bash
# First-time setup (installs deps, configures GPS, deploys)
./scripts/pi-setup.sh

# Quick deploy after code changes
./scripts/pi-deploy.sh
```

See [CLAUDE.md](CLAUDE.md) for detailed Pi configuration and manual commands.

## Environment Variables

| Variable | Description |
|---|---|
| `KODAMA_SERVER_KEY` | Server's base32 public key (required for camera/client) |
| `KODAMA_KEY_PATH` | Path to keypair file |
| `KODAMA_STORAGE_PATH` | Recording storage location |
| `KODAMA_BUFFER_SIZE` | Broadcast buffer capacity (default: 512) |
| `KODAMA_ABR` | Set to `0` to disable adaptive bitrate |
| `RUST_LOG` | Tracing filter (e.g., `kodama=debug`) |

## Development

### Prerequisites

- Rust stable (1.75+)
- `bun` for frontend development
- FFmpeg (for desktop app video muxing)
- For Pi: `aarch64-unknown-linux-gnu` cross-compilation toolchain

### Crypto Dependency Pinning

Iroh uses pre-release versions of `curve25519-dalek` and `ed25519-dalek` that require pinned `digest` crate versions. After any `cargo update`, re-run:

```bash
./scripts/setup.sh
```

## License

MIT OR Apache-2.0
