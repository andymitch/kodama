# ADR-001: Three-Module System Architecture

**Status:** Accepted  
**Date:** 2025-02-03  
**Context:** Designing Yurei v2 as an open-source, privacy-focused security camera system with both self-hosted and cloud-hosted deployment options.

## Summary

Yurei v2 is built on three core modules with clear separation of concerns:

1. **Camera Module** - Captures video, audio, and telemetry as 3 separate channels
2. **Relay Module** - Generic muxer that combines channels from N sources, handles Iroh transport
3. **Server Module** - Authoritative core that routes streams to clients and storage

## Decision Drivers

- **Market approach**: Open-source hackable home security system, potential defense pivot
- **Privacy-first**: Self-hostable with no mandatory cloud dependency
- **Business model**: Value-add cloud tier, not artificial friction
- **Simplicity**: Single protocol (Iroh) throughout, no WebRTC for MVP

## Architecture

### Module Responsibilities

```
┌─────────────────────────────────────────────────────────────────────────┐
│                            CAMERA MODULE                                │
│                                                                         │
│  Responsibility: Capture sensor data, output as tagged channel streams  │
│                                                                         │
│  Outputs:                                                               │
│    - Channel 0: Video (H.264 frames)                                    │
│    - Channel 1: Audio (Opus packets)                                    │
│    - Channel 2: Telemetry (MessagePack-encoded metrics)                 │
│                                                                         │
│  Does NOT: Mux, transport, store, or make routing decisions             │
└─────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────────┐
│                            RELAY MODULE                                 │
│                                                                         │
│  Responsibility: Mux N inputs → 1 output, handle Iroh transport         │
│                                                                         │
│  Capabilities:                                                          │
│    - Accept inputs via: embedded bus, WiFi, cellular, Iroh              │
│    - Tag frames with source ID + channel type                           │
│    - Mux multiple sources into single stream                            │
│    - Output via: embedded bus, WiFi, cellular, Iroh                     │
│    - Can be chained (relay → relay → server)                            │
│                                                                         │
│  Deployment:                                                            │
│    - Embedded in camera (typical)                                       │
│    - Standalone gateway aggregating multiple cameras                    │
│    - Embedded in server                                                 │
│                                                                         │
│  Does NOT: Capture, decode, route to clients, or store                  │
└─────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────────┐
│                            SERVER MODULE                                │
│                                                                         │
│  Responsibility: Authoritative routing, client management, storage      │
│                                                                         │
│  Inputs:                                                                │
│    - From cameras (via embedded relay)                                  │
│    - From standalone relays (gateway pattern)                           │
│    - From other servers (server-to-server sharing)                      │
│                                                                         │
│  Outputs:                                                               │
│    - To native clients via Iroh (desktop app, mobile app)               │
│    - To other servers via Iroh (home station → mobile app server)       │
│    - To local UI (if running headed)                                    │
│    - To storage (local filesystem or cloud)                             │
│                                                                         │
│  Contains: Embedded relay for accepting connections and forwarding      │
└─────────────────────────────────────────────────────────────────────────┘
```

### Frame Format

Every frame includes source and channel identification:

```
┌──────────────┬──────────────┬──────────────┬──────────────┬─────────────┐
│   source     │   channel    │    flags     │    length    │   payload   │
│   (8 bytes)  │   (1 byte)   │   (1 byte)   │   (4 bytes)  │   (var)     │
│   camera ID  │   0=video    │              │              │             │
│              │   1=audio    │              │              │             │
│              │   2=telemetry│              │              │             │
└──────────────┴──────────────┴──────────────┴──────────────┴─────────────┘
```

Relays preserve this tagging when muxing. Servers demux by reading source + channel.

### Key Design Decisions

#### 1. Cameras Always Have Embedded Relay

Every camera ships with capture + relay. No separate "dumb camera" SKU. Simplifies the mental model and reduces configurations to test.

```
CAMERA = yurei-capture + yurei-relay (always)
```

#### 2. Muxing is Always the Relay's Job

Cameras output 3 separate channel streams. Muxing happens at the relay layer, never in capture. This keeps capture simple and relay generic.

#### 3. Single Protocol: Iroh

Everything speaks Iroh. No WebRTC for MVP (browser support can come later via Iroh-over-WebSocket through relay). Benefits:

- One protocol to implement, test, debug
- Native P2P to mobile apps (no relay needed for viewing)
- NAT traversal built-in (~90% direct connections)
- Encryption built-in (QUIC)

#### 4. Servers Can Connect to Servers

Servers have an embedded relay that can both receive AND forward streams. This enables:

- Home station (Pi) → Mobile app (phone acts as secondary server)
- Self-hosted → Cloud backup
- Multi-site deployments

#### 5. Open Core Business Model

```
OPEN SOURCE (MIT/Apache):
├── yurei-core           # Types, frame format, crypto
├── yurei-capture        # Camera capture
├── yurei-relay          # Mux/demux + Iroh transport
└── yurei-server         # Single-tenant server logic

PROPRIETARY:
└── yurei-cloud          # Multi-tenant layer
    ├── Account management
    ├── Billing integration
    ├── Multi-tenant isolation
    ├── Cloud storage backends
    └── Future premium features
```

Self-hosters get a fully functional system. Cloud tier adds operational convenience, not artificial limitations.

## Target Configurations

### Configuration 1: Tech-Savvy Self-Hoster

```
┌─────────────────────────────────────────────────────────────────┐
│                        HOME NETWORK                             │
│                                                                 │
│   ┌──────────┐  ┌──────────┐  ┌──────────┐                     │
│   │ camera 1 │  │ camera 2 │  │ camera 3 │                     │
│   │ +relay   │  │ +relay   │  │ +relay   │                     │
│   └────┬─────┘  └────┬─────┘  └────┬─────┘                     │
│        │             │             │                            │
│        └─────────────┼─────────────┘                            │
│                      │ Iroh (local)                             │
│                      ▼                                          │
│            ┌──────────────────┐                                 │
│            │  Raspberry Pi    │                                 │
│            │  yurei-server    │                                 │
│            │  + local storage │                                 │
│            └────────┬─────────┘                                 │
│                     │                                           │
└─────────────────────┼───────────────────────────────────────────┘
                      │ Iroh (P2P via internet)
                      ▼
              ┌───────────────┐
              │ Desktop/Mobile│
              │     App       │
              └───────────────┘
```

**What they run:** Yurei cameras + Pi with yurei-server  
**What they pay:** Nothing (free Iroh relay tier)  
**Data location:** All local

### Configuration 2: Cloud-Hosted (Grandma)

```
┌─────────────────────────────────────────────────────────────────┐
│                      GRANDMA'S HOUSE                            │
│                                                                 │
│   ┌──────────┐  ┌──────────┐                                   │
│   │ camera 1 │  │ camera 2 │                                   │
│   │ +relay   │  │ +relay   │                                   │
│   └────┬─────┘  └────┬─────┘                                   │
│        │             │                                          │
│        └──────┬──────┘                                          │
│               │ Iroh (direct to cloud)                          │
└───────────────┼─────────────────────────────────────────────────┘
                │
                ▼
┌───────────────────────────────────────┐
│           YUREI CLOUD                 │
│  ┌─────────────────────────────────┐ │
│  │  Hosted yurei-server instance   │ │
│  │  + cloud storage (R2/S3)        │ │
│  └───────────────┬─────────────────┘ │
└──────────────────┼───────────────────┘
                   │ Iroh
                   ▼
           ┌───────────────┐
           │ Grandma's     │
           │ phone app     │
           └───────────────┘
```

**What grandson does:** Install cameras, download app, create account  
**What they pay:** $5/mo subscription  
**Data location:** Yurei cloud

## Crate Structure

```
yurei/
├── Cargo.toml              # Workspace
├── crates/
│   ├── yurei-core/         # Types, frame format, crypto, NodeId
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── frame.rs    # Frame struct, Channel enum
│   │       ├── identity.rs # Ed25519 key management
│   │       └── protocol.rs # Constants, ALPN, etc.
│   │
│   ├── yurei-capture/      # Camera capture
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── video.rs    # libcamera/v4l2
│   │       ├── audio.rs    # ALSA/cpal + Opus
│   │       └── telemetry.rs# System metrics
│   │
│   ├── yurei-relay/        # Mux/demux + Iroh transport
│   │   └── src/
│   │       ├── lib.rs      # Public API: Relay struct
│   │       ├── transport/  # Internal: Iroh wrapper
│   │       │   ├── mod.rs
│   │       │   ├── endpoint.rs
│   │       │   └── connection.rs
│   │       └── mux/        # Internal: mux/demux logic
│   │           ├── mod.rs
│   │           ├── frame.rs
│   │           ├── muxer.rs
│   │           └── demuxer.rs
│   │
│   ├── yurei-server/       # Server logic (open source, single-tenant)
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── router.rs   # Route frames to clients
│   │       ├── clients.rs  # Client connection management
│   │       └── storage.rs  # Storage trait + local impl
│   │
│   └── yurei-storage/      # Storage abstraction
│       └── src/
│           ├── lib.rs
│           ├── local.rs    # Filesystem backend
│           └── cloud.rs    # S3/R2 backend (may move to proprietary)
│
├── apps/
│   ├── yurei-camera/       # Binary for Pi Zero 2W
│   ├── yurei-server-bin/   # Headless server binary
│   ├── yurei-desktop/      # Tauri desktop app
│   └── yurei-mobile/       # Tauri mobile app
│
└── docs/
    └── architecture/
        ├── adr-001-three-module-system.md  # This document
        └── poc-1-spec.md                    # Implementation spec
```

## Consequences

### Positive

- Clear separation of concerns makes each module testable in isolation
- Same crates power self-hosted and cloud deployments
- Open core builds community trust and contributions
- Single protocol (Iroh) reduces complexity
- Server-to-server sharing enables flexible topologies

### Negative

- No browser support in MVP (native apps only)
- Iroh is pre-1.0 (potential breaking changes)
- Self-hosters need some technical skill (Pi setup)

### Risks

- Iroh relay infrastructure must be reliable (fallback for ~10% of connections)
- Mobile app as server has battery/backgrounding challenges
- Multi-tenant cloud layer is significant additional work

## References

- [Iroh documentation](https://iroh.computer/docs)
- [Phase 1 P2P spec](../phase-1-p2p-iroh.md)
- [Yurei v2 Architecture Summary](../yurei-v2-architecture-summary.md)
