# Kodama

[![Rust](https://github.com/andymitch/kodama/actions/workflows/rust.yml/badge.svg)](https://github.com/andymitch/kodama/actions/workflows/rust.yml)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](LICENSE)
[![Rust Version](https://img.shields.io/badge/rust-1.75%2B-orange.svg)](https://www.rust-lang.org/)

Privacy-focused P2P security camera system built on [Iroh](https://iroh.computer).

## Features

- **Privacy-first**: Self-hostable with no mandatory cloud dependency
- **P2P Transport**: Direct peer-to-peer connections via Iroh (~90% NAT traversal success)
- **Multi-channel Streaming**: Video, audio, and telemetry as separate channels
- **Flexible Deployment**: Run on Raspberry Pi, desktop, or cloud
- **End-to-end Encryption**: Built-in via QUIC transport

## Quick Start

```bash
# Clone and setup
git clone https://github.com/andymitch/kodama.git
cd kodama

# IMPORTANT: Pin crypto dependencies for compatibility
./scripts/setup.sh

# Build
cargo build

# Run tests
cargo test
```

## Project Structure

The codebase is organized as a single Rust crate with library modules and application binaries:

```
kodama/
├── src/
│   ├── lib.rs           # Library entry point with re-exports
│   ├── core/            # Types, frame format, protocol constants
│   │   ├── frame.rs     # Frame struct, Channel enum, SourceId
│   │   ├── identity.rs  # Ed25519 key management
│   │   └── protocol.rs  # ALPN, ports, constants
│   ├── capture/         # Sensor capture
│   │   ├── video.rs     # Video capture (libcamera/v4l2)
│   │   ├── audio.rs     # Audio capture (ALSA/cpal + Opus)
│   │   ├── telemetry.rs # System metrics
│   │   └── h264.rs      # H.264 parsing utilities
│   ├── relay/           # P2P transport and muxing
│   │   ├── transport/   # Iroh endpoint wrapper
│   │   └── mux/         # Frame mux/demux logic
│   ├── server/          # Server routing and management
│   │   ├── router.rs    # Frame routing to clients
│   │   ├── clients.rs   # Client connection management
│   │   └── storage.rs   # Storage manager
│   ├── storage/         # Persistence backends
│   │   ├── local.rs     # Filesystem backend
│   │   └── cloud.rs     # S3/R2 backend
│   └── bin/             # Application binaries
│       ├── kodama-camera/   # Camera binary (Pi Zero 2W)
│       ├── kodama-desktop/  # Desktop client
│       ├── kodama-relay/    # Relay server for NAT traversal
│       └── kodama-server/   # Headless server binary
├── docs/
│   └── architecture/    # ADRs and specs
└── scripts/
    ├── setup.sh         # Dependency pinning script
    └── test-e2e.sh      # End-to-end tests
```

## Architecture

Kodama is built on three core modules:

```
┌─────────────────┐     ┌─────────────────┐     ┌─────────────────┐
│  Camera Module  │────▶│  Relay Module   │────▶│  Server Module  │
│   (capture/)    │     │    (relay/)     │     │    (server/)    │
└─────────────────┘     └─────────────────┘     └─────────────────┘
    Captures             Muxes channels,         Routes to clients
    video/audio/         handles Iroh            and storage
    telemetry            transport
```

### Frame Format

```
┌──────────┬─────────┬───────┬────────┬─────────┐
│  source  │ channel │ flags │ length │ payload │
│ (8 bytes)│ (1 byte)│(1 byte)│(4 bytes)│  (var) │
└──────────┴─────────┴───────┴────────┴─────────┘
            0=video
            1=audio
            2=telemetry
```

See [docs/architecture/adr-001-three-module-system.md](docs/architecture/adr-001-three-module-system.md) for detailed design.

## Development

### Prerequisites

- Rust stable (1.75+)
- For Pi development: cross-compilation toolchain

### Building

```bash
# Build library and all binaries
cargo build

# Build specific binary
cargo build --bin kodama-camera
cargo build --bin kodama-server
cargo build --bin kodama-desktop
cargo build --bin kodama-relay
```

### Testing

```bash
# Run all tests
cargo test

# Run with test video source (no hardware needed)
cargo run --bin kodama-camera --features test-source
```

### Known Issues

#### Crypto Dependency Pinning

Iroh uses pre-release versions of `curve25519-dalek` and `ed25519-dalek` that have compatibility issues with newer `digest` crate versions. After any `cargo update`, re-pin dependencies:

```bash
./scripts/setup.sh
```

Or manually:

```bash
cargo update -p sha2 --precise 0.11.0-rc.4
cargo update -p digest --precise 0.11.0-rc.9
```

## Usage Examples

### Camera

```rust
use kodama::{Relay, Frame, SourceId, capture::h264};
use std::path::Path;

let relay = Relay::new(Some(Path::new("./camera.key"))).await?;
let conn = relay.connect(server_key).await?;
let sender = conn.open_frame_stream().await?;

// Stream video frames
while let Some(payload) = video_rx.recv().await {
    let frame = Frame::video(source_id, payload, h264::contains_keyframe(&payload));
    sender.send(&frame).await?;
}
```

### Server

```rust
use kodama::{Relay, Router};
use std::path::Path;

let relay = Relay::new(Some(Path::new("./server.key"))).await?;
let router = Router::new(64);

while let Some(conn) = relay.accept().await {
    // Handle camera or client connection
}
```

## License

MIT OR Apache-2.0
