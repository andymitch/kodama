# Yurei

Privacy-focused P2P security camera system built on [Iroh](https://iroh.computer).

## Quick Start

```bash
# Clone and setup
git clone https://github.com/anthropics/yurei.git
cd yurei

# IMPORTANT: Pin crypto dependencies for compatibility
./scripts/setup.sh

# Build
cargo build

# Run tests
cargo test
```

## Project Structure

```
yurei/
├── crates/
│   ├── yurei-core/      # Types, frame format, protocol constants
│   ├── yurei-capture/   # Video/audio capture (Pi camera)
│   ├── yurei-relay/     # Iroh transport, frame streaming
│   └── yurei-server/    # Server logic, routing
├── apps/
│   ├── yurei-camera/    # Camera binary (Pi Zero 2W)
│   ├── yurei-server-bin/# Server binary
│   └── yurei-desktop/   # Desktop client (Tauri)
└── docs/
    └── architecture/    # ADRs and specs
```

## Development

### Prerequisites

- Rust stable (1.75+)
- For Pi development: cross-compilation toolchain

### Known Issues

#### Crypto Dependency Pinning

Iroh 0.96 uses pre-release versions of `curve25519-dalek` and `ed25519-dalek`
that have compatibility issues with newer `digest` crate versions. After any
`cargo update`, you must re-pin these dependencies:

```bash
cargo update -p sha2 --precise 0.11.0-rc.4
cargo update -p digest --precise 0.11.0-rc.9
```

Or simply run:

```bash
./scripts/setup.sh
```

### Testing Without Hardware

Enable the `test-source` feature for fake video:

```bash
cargo run -p yurei-camera --features test-source
```

## Architecture

See [docs/architecture/adr-001-three-module-system.md](docs/architecture/adr-001-three-module-system.md)
for the system design.

## License

MIT OR Apache-2.0
