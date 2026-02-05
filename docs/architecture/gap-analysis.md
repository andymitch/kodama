# Kodama Gap Analysis

**Date:** 2026-02-05
**Purpose:** Analysis of documented features vs actual implementation status

## Executive Summary

Kodama has a solid foundation with core P2P streaming working end-to-end. However, several documented features are either partially implemented or not yet integrated. The POC 1 goals are **mostly achieved** for basic video streaming, but audio/telemetry channels, storage, and the desktop GUI remain incomplete.

---

## POC 1 Success Criteria Status

From `docs/architecture/poc-1-spec.md`:

| Criteria | Status | Notes |
|----------|--------|-------|
| `cargo build` succeeds | ✅ Complete | All crates build successfully |
| Camera prints PublicKey and starts | ✅ Complete | Works with real camera and test source |
| Server accepts connections | ✅ Complete | Proper role detection for cameras/clients |
| Camera streams to server | ✅ Complete | Video frames stream correctly |
| Desktop connects to server | ✅ Complete | CLI client connects and receives frames |
| Desktop receives frames | ⚠️ Partial | CLI stats only - no video display |
| Stable for 10+ minutes | ⚠️ Untested | No automated stability tests |

---

## Module-by-Module Analysis

### Core Module (`src/core/`) - ✅ Complete

| Component | Status | Location |
|-----------|--------|----------|
| Frame struct | ✅ | `frame.rs:39-55` |
| SourceId | ✅ | `frame.rs:12-28` |
| Channel enum | ✅ | `frame.rs:31-36` |
| FrameFlags | ✅ | `frame.rs:61-86` |
| Identity/KeyPair | ✅ | `identity.rs` |
| Protocol constants | ✅ | `protocol.rs` |

---

### Capture Module (`src/capture/`) - ⚠️ Implemented but Partially Used

| Component | Implemented | Integrated | Location |
|-----------|-------------|------------|----------|
| VideoCapture (libcamera) | ✅ | ✅ Used in camera | `video.rs` |
| Test video source | ✅ | ✅ Used with --test-source | `video.rs:195-234` |
| H.264 keyframe detection | ✅ | ✅ Used in camera | `h264.rs` |
| AudioCapture (arecord) | ✅ | ❌ **Not used** | `audio.rs` |
| Test audio source | ✅ | ❌ **Not used** | `audio.rs:235-278` |
| TelemetryCapture | ✅ | ❌ **Not used** | `telemetry.rs` |

**Gap:** Audio and telemetry capture code is fully implemented but the camera binary only streams video. The ADR specifies 3 channels (video, audio, telemetry) but only video is active.

---

### Relay Module (`src/relay/`) - ✅ Mostly Complete

| Component | Status | Location |
|-----------|--------|----------|
| KodamaEndpoint (Iroh wrapper) | ✅ | `transport/endpoint.rs` |
| FrameSender/FrameReceiver | ✅ | `transport/connection.rs` |
| Frame serialization | ✅ | `mux/frame.rs` |
| Muxer (combine streams) | ✅ | `mux/muxer.rs` |
| Demuxer (split streams) | ✅ | `mux/demuxer.rs` |
| FrameBuffer (backpressure) | ✅ | `mux/muxer.rs:69-143` |

**Note:** Muxer/demuxer are implemented but currently unused since camera only sends a single video channel.

---

### Server Module (`src/server/`) - ⚠️ Partially Integrated

| Component | Implemented | Integrated | Location |
|-----------|-------------|------------|----------|
| Router (frame broadcast) | ✅ | ✅ | `router.rs` |
| PeerRole detection | ✅ | ✅ | `router.rs:14-20` |
| RouterStats | ✅ | ✅ | `router.rs:23-29` |
| ClientManager | ✅ | ⚠️ Basic use | `clients.rs` |
| StorageManager | ✅ | ❌ **Not integrated** | `storage.rs` |

**Gap:** StorageManager exists but the server binary doesn't call `store_frame()` for recording. Frames are only broadcast, never persisted.

---

### Storage Module (`src/storage/`) - ⚠️ Implemented but Not Used

| Component | Implemented | Integrated | Location |
|-----------|-------------|------------|----------|
| StorageBackend trait | ✅ | ❌ | `mod.rs:16-34` |
| LocalStorage (filesystem) | ✅ | ❌ | `local.rs` |
| CloudStorage (S3/R2) | ✅ | ❌ | `cloud.rs` |
| Segment management | ✅ | ❌ | Both backends |
| Cleanup/retention | ✅ | ❌ | Both backends |

**Gap:** Both storage backends are fully implemented with tests, but they're never instantiated or used by the server. Recording functionality is documented but not wired up.

---

### Application Binaries

#### kodama-camera (`src/bin/kodama-camera/`) - ✅ Working

**Implemented:**
- Connects to server via PublicKey
- Streams video frames with keyframe detection
- Test source mode for development
- Statistics logging

**Missing:**
- Audio streaming (capture code exists but not used)
- Telemetry streaming (capture code exists but not used)
- Multi-channel muxing

#### kodama-server (`src/bin/kodama-server/`) - ✅ Working

**Implemented:**
- Accepts camera and client connections
- Role detection via stream direction
- Frame broadcast to all clients
- Statistics logging

**Missing:**
- Storage/recording integration
- Camera-specific routing (all clients get all cameras)
- Authentication beyond NodeId

#### kodama-desktop (`src/bin/kodama-desktop/`) - ⚠️ CLI Only

**Implemented:**
- Connects to server
- Receives frames
- Logs statistics (fps, bitrate, keyframe count)

**Missing per POC-1-spec:**
- Tauri GUI framework (spec mentions `apps/yurei-desktop/tauri.conf.json`)
- Svelte frontend (`ui/src/App.svelte`)
- H.264 decoding
- Video canvas rendering
- Server EndpointId input UI

**Current state:** CLI-only frame counter, not the desktop app described in the spec.

#### kodama-relay (`src/bin/kodama-relay/`) - ❌ Not Implemented

```rust
// Current implementation (main.rs:23-24):
info!("Relay functionality not yet implemented");
// TODO: Implement relay server for NAT traversal assistance
```

**Missing:**
- Standalone relay functionality
- NAT traversal assistance for peers that can't connect directly
- The ~10% of connections mentioned in ADR that need relay fallback

---

## Documentation vs Reality Discrepancies

### 1. Project Structure

**ADR-001 describes workspace structure:**
```
yurei/
├── crates/
│   ├── yurei-core/
│   ├── yurei-capture/
│   ├── yurei-relay/
│   └── yurei-server/
├── apps/
│   ├── yurei-camera/
│   ├── yurei-server-bin/
│   └── yurei-desktop/
```

**Reality:** Single crate with `src/` organization (refactored per commit `6aad931`). Docs still reference old workspace structure.

### 2. Desktop App

**POC-1-spec describes Tauri + Svelte app with:**
- `tauri.conf.json`
- `ui/src/App.svelte`
- `connect_to_server` and `get_next_frame` Tauri commands
- Video canvas for H.264 rendering

**Reality:** Simple CLI binary that logs frame statistics.

### 3. Multi-Channel Streaming

**ADR-001 specifies:**
- Channel 0: Video (H.264)
- Channel 1: Audio (Opus)
- Channel 2: Telemetry (MessagePack/JSON)

**Reality:** Only video channel is streamed. Audio/telemetry capture code exists but isn't used.

### 4. Frame Format

**ADR-001 specifies header:**
```
source (8 bytes) + channel (1 byte) + flags (1 byte) + length (4 bytes) + payload
```

**Reality:** Frame format is implemented but includes timestamp (8 bytes) - total 22-byte header per `protocol.rs:11`:
```rust
pub const FRAME_HEADER_SIZE: usize = 22;
```

---

## Priority Gaps to Address

### High Priority (Core POC 1 Completion)

1. **kodama-relay binary** - Critical for ~10% of connections that can't establish direct P2P
2. **Desktop GUI** - POC 1 spec goal is "Live video displays in desktop app"
3. **Storage integration** - Connect existing storage backends to server for recording

### Medium Priority (Multi-Channel)

4. **Audio channel streaming** - Capture exists, need to integrate with camera
5. **Telemetry channel streaming** - Capture exists, need to integrate with camera
6. **Frame muxing** - Use existing muxer for multi-channel streams

### Low Priority (POC 2+)

7. Mobile app (Tauri mobile)
8. Multiple camera support
9. Cloud deployment
10. Pairing UX (QR codes)
11. Advanced authentication

---

## Technical Debt

1. **Outdated documentation** - ADR and POC spec reference old workspace structure and "yurei" naming
2. **Unused code** - Audio, telemetry, and storage modules are implemented but dead code
3. **Missing tests** - No integration tests for full camera→server→client pipeline
4. **No E2E test automation** - `scripts/test-e2e.sh` exists but is manual

---

## Recommendations

1. **Update documentation** to reflect current single-crate structure
2. **Implement kodama-relay** for production NAT traversal support
3. **Add storage to server** - Simple change to call `storage.store_frame()` in broadcast loop
4. **Decide on desktop approach** - Either implement Tauri GUI or update POC scope
5. **Create integration tests** that verify full streaming pipeline
6. **Wire up audio/telemetry** in camera binary to achieve multi-channel vision
