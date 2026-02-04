# POC 1: Camera → Server → Desktop Client

**Goal:** Validate core P2P streaming works end-to-end with Iroh  
**Timeline:** 4 weeks  
**Success Criteria:** Live video from Pi camera displays in desktop app over Iroh P2P

## Scope

### In Scope

- Single camera streaming video to single server
- Server forwarding to single desktop client
- Iroh transport with hardcoded NodeIds
- Basic frame format with source/channel tagging
- H.264 video (hardware encoded on Pi)

### Out of Scope (POC 2+)

- Audio and telemetry channels
- Multiple cameras
- Mobile app
- Cloud deployment
- Storage/recording
- Pairing UX (QR codes)
- Authentication beyond NodeId allowlist

## Workspace Setup

### Directory Structure

```
yurei/
├── Cargo.toml                    # Workspace manifest
├── rust-toolchain.toml           # Pin Rust version
├── .cargo/
│   └── config.toml               # Cross-compilation settings
│
├── crates/
│   ├── yurei-core/
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── frame.rs          # Frame, Channel, SourceId types
│   │       └── protocol.rs       # ALPN, constants
│   │
│   ├── yurei-capture/
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       └── video.rs          # V4L2/libcamera capture
│   │
│   ├── yurei-relay/
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs            # Re-exports, Relay struct
│   │       ├── transport/
│   │       │   ├── mod.rs
│   │       │   ├── endpoint.rs   # Iroh endpoint wrapper
│   │       │   └── connection.rs # Connection handling
│   │       └── mux/
│   │           ├── mod.rs
│   │           ├── frame.rs      # Serialization
│   │           ├── muxer.rs      # Combine streams
│   │           └── demuxer.rs    # Split streams
│   │
│   └── yurei-server/
│       ├── Cargo.toml
│       └── src/
│           ├── lib.rs
│           └── router.rs         # Route frames to clients
│
├── apps/
│   ├── yurei-camera/
│   │   ├── Cargo.toml
│   │   └── src/
│   │       └── main.rs           # Camera binary
│   │
│   ├── yurei-server-bin/
│   │   ├── Cargo.toml
│   │   └── src/
│   │       └── main.rs           # Server binary
│   │
│   └── yurei-desktop/
│       ├── Cargo.toml
│       ├── tauri.conf.json
│       ├── src/
│       │   └── main.rs           # Tauri backend
│       └── ui/                   # Svelte frontend
│           ├── package.json
│           └── src/
│               └── App.svelte
│
└── docs/
    └── architecture/
        ├── adr-001-three-module-system.md
        └── poc-1-spec.md         # This file
```

### Cargo Workspace Manifest

```toml
# yurei/Cargo.toml
[workspace]
resolver = "2"
members = [
    "crates/yurei-core",
    "crates/yurei-capture",
    "crates/yurei-relay",
    "crates/yurei-server",
    "apps/yurei-camera",
    "apps/yurei-server-bin",
    "apps/yurei-desktop",
]

[workspace.package]
version = "0.1.0"
edition = "2021"
license = "MIT OR Apache-2.0"
repository = "https://github.com/anthropics/yurei"

[workspace.dependencies]
# Async runtime
tokio = { version = "1", features = ["full"] }

# Iroh P2P - using git for latest compatible dependencies
# Note: iroh 0.96 has pre-release crypto deps that require pinning
iroh = { git = "https://github.com/n0-computer/iroh", branch = "main" }

# Serialization
serde = { version = "1", features = ["derive"] }
bytes = { version = "1", features = ["serde"] }

# Error handling
anyhow = "1"
thiserror = "2"

# Logging
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }

# Random
rand = "0.9"

# Internal crates
yurei-core = { path = "crates/yurei-core" }
yurei-capture = { path = "crates/yurei-capture" }
yurei-relay = { path = "crates/yurei-relay" }
yurei-server = { path = "crates/yurei-server" }
```

> **Note:** After initial `cargo build`, pin crypto deps for compatibility:
> ```bash
> cargo update -p sha2 --precise 0.11.0-rc.4
> cargo update -p digest --precise 0.11.0-rc.9
> ```

## Implementation Tasks

### Phase 1.1: Core Types (Day 1-2)

#### Task: Create yurei-core crate

**File: `crates/yurei-core/src/frame.rs`**

```rust
use bytes::Bytes;
use serde::{Deserialize, Serialize};

/// Identifies a camera/source in the system
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SourceId(pub [u8; 8]);

impl SourceId {
    pub fn new(id: [u8; 8]) -> Self {
        Self(id)
    }
    
    /// Generate from Iroh NodeId (truncate to 8 bytes)
    pub fn from_node_id_bytes(bytes: &[u8]) -> Self {
        let mut id = [0u8; 8];
        id.copy_from_slice(&bytes[..8]);
        Self(id)
    }
}

/// Channel types within a source
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum Channel {
    Video = 0,
    Audio = 1,
    Telemetry = 2,
}

/// Frame flags
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct FrameFlags(pub u8);

impl FrameFlags {
    pub const KEYFRAME: u8 = 0b0000_0001;
    
    pub fn is_keyframe(&self) -> bool {
        self.0 & Self::KEYFRAME != 0
    }
    
    pub fn set_keyframe(&mut self) {
        self.0 |= Self::KEYFRAME;
    }
}

/// A single frame of data (video, audio, or telemetry)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Frame {
    pub source: SourceId,
    pub channel: Channel,
    pub flags: FrameFlags,
    pub timestamp_us: u64,
    pub payload: Bytes,
}

impl Frame {
    pub fn video(source: SourceId, payload: Bytes, keyframe: bool) -> Self {
        let mut flags = FrameFlags::default();
        if keyframe {
            flags.set_keyframe();
        }
        Self {
            source,
            channel: Channel::Video,
            flags,
            timestamp_us: 0, // Caller should set
            payload,
        }
    }
    
    pub fn audio(source: SourceId, payload: Bytes) -> Self {
        Self {
            source,
            channel: Channel::Audio,
            flags: FrameFlags::default(),
            timestamp_us: 0,
            payload,
        }
    }
    
    pub fn telemetry(source: SourceId, payload: Bytes) -> Self {
        Self {
            source,
            channel: Channel::Telemetry,
            flags: FrameFlags::default(),
            timestamp_us: 0,
            payload,
        }
    }
}
```

**File: `crates/yurei-core/src/protocol.rs`**

```rust
/// ALPN protocol identifier for Yurei connections
pub const ALPN: &[u8] = b"yurei/0";

/// Default ports
pub const DEFAULT_CAMERA_PORT: u16 = 7878;
pub const DEFAULT_SERVER_PORT: u16 = 7879;
```

**File: `crates/yurei-core/src/lib.rs`**

```rust
mod frame;
mod protocol;

pub use frame::{Channel, Frame, FrameFlags, SourceId};
pub use protocol::*;
```

**Cargo.toml:**

```toml
[package]
name = "yurei-core"
version.workspace = true
edition.workspace = true

[dependencies]
bytes.workspace = true
serde.workspace = true
```

### Phase 1.2: Relay Crate (Day 3-7)

#### Task: Implement Iroh transport wrapper

> **API Note (iroh 0.96):** The iroh API uses `EndpointId` (was `NodeId`) and `endpoint.id()` (was `node_id()`).
> `EndpointId` implicitly converts to `EndpointAddr` for `connect()`.

**File: `crates/yurei-relay/src/transport/endpoint.rs`**

```rust
use anyhow::Result;
use iroh::endpoint::{Endpoint, RelayMode};
use iroh::{EndpointId, SecretKey};
use std::path::Path;

pub struct YureiEndpoint {
    endpoint: Endpoint,
}

impl YureiEndpoint {
    /// Create a new endpoint, loading or generating a secret key
    pub async fn new(key_path: Option<&Path>) -> Result<Self> {
        let secret_key = match key_path {
            Some(path) if path.exists() => {
                let bytes = std::fs::read(path)?;
                SecretKey::try_from_bytes(&bytes.try_into().map_err(|_| {
                    anyhow::anyhow!("Invalid key file length")
                })?)?
            }
            Some(path) => {
                let key = SecretKey::generate(&mut rand::rngs::OsRng);
                std::fs::write(path, key.to_bytes())?;
                key
            }
            None => SecretKey::generate(&mut rand::rngs::OsRng),
        };

        let endpoint = Endpoint::builder()
            .secret_key(secret_key)
            .relay_mode(RelayMode::Default)
            .alpns(vec![yurei_core::ALPN.to_vec()])
            .bind()
            .await?;

        Ok(Self { endpoint })
    }

    pub fn endpoint_id(&self) -> EndpointId {
        self.endpoint.id()
    }

    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }
}
```

**File: `crates/yurei-relay/src/transport/connection.rs`**

```rust
use anyhow::Result;
use iroh::endpoint::Connection;
use iroh::EndpointId;
use yurei_core::ALPN;

use super::endpoint::YureiEndpoint;

impl YureiEndpoint {
    /// Connect to a remote endpoint
    pub async fn connect(&self, endpoint_id: EndpointId) -> Result<Connection> {
        // EndpointId converts to EndpointAddr; discovery fills in relay/direct addrs
        let conn = self.endpoint.connect(endpoint_id, ALPN).await?;
        Ok(conn)
    }

    /// Accept incoming connections
    pub async fn accept(&self) -> Option<Connection> {
        let incoming = self.endpoint.accept().await?;
        incoming.await.ok()
    }
}
```

#### Task: Implement frame serialization

> **Implementation Note:** Frame serialization is in `wire.rs` as standalone functions,
> keeping `yurei-core` serialization-agnostic.

**File: `crates/yurei-relay/src/mux/wire.rs`**

```rust
use anyhow::Result;
use bytes::{Buf, BufMut, Bytes, BytesMut};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use yurei_core::{Channel, Frame, FrameFlags, SourceId, FRAME_HEADER_SIZE};

/// Serialize a frame to bytes (header + payload)
pub fn frame_to_bytes(frame: &Frame) -> Bytes {
    let mut buf = BytesMut::with_capacity(FRAME_HEADER_SIZE + frame.payload.len());
    buf.put_slice(frame.source.as_bytes());
    buf.put_u8(frame.channel as u8);
    buf.put_u8(frame.flags.0);
    buf.put_u64(frame.timestamp_us);
    buf.put_slice(&frame.payload);
    buf.freeze()
}

/// Deserialize a frame from bytes
pub fn frame_from_bytes(mut buf: Bytes) -> Result<Frame> {
    // ... parsing logic
}

/// Write a frame to an async writer (with length prefix)
pub async fn write_frame<W: AsyncWrite + Unpin>(writer: &mut W, frame: &Frame) -> Result<()> {
    let bytes = frame_to_bytes(frame);
    writer.write_all(&(bytes.len() as u32).to_be_bytes()).await?;
    writer.write_all(&bytes).await?;
    Ok(())
}

/// Read a frame from an async reader (expects length prefix)
pub async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Frame> {
    let mut len_bytes = [0u8; 4];
    reader.read_exact(&mut len_bytes).await?;
    let len = u32::from_be_bytes(len_bytes) as usize;
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf).await?;
    frame_from_bytes(Bytes::from(buf))
}
```

#### Task: Implement Relay struct (high-level API)

> **Implementation Note:** The relay provides both a simple per-frame API and an
> efficient persistent stream API for video streaming.

**File: `crates/yurei-relay/src/lib.rs`**

```rust
mod transport;
mod mux;

use anyhow::Result;
use iroh::endpoint::Connection;
use iroh::PublicKey;
use std::path::Path;
use yurei_core::Frame;

// Re-exports
pub use mux::buffer::{BackpressureSender, BufferStats, FrameBuffer};
pub use mux::stream::{FrameReceiver, FrameSender, FrameStream};
pub use mux::wire::{frame_from_bytes, frame_to_bytes, read_frame, write_frame};
pub use transport::YureiEndpoint;

/// High-level relay interface for sending/receiving frames
pub struct Relay {
    endpoint: YureiEndpoint,
}

impl Relay {
    pub async fn new(key_path: Option<&Path>) -> Result<Self> { /* ... */ }
    pub fn public_key(&self) -> PublicKey { /* ... */ }
    pub async fn connect(&self, remote: PublicKey) -> Result<RelayConnection> { /* ... */ }
    pub async fn accept(&self) -> Option<RelayConnection> { /* ... */ }
}

pub struct RelayConnection {
    conn: Connection,
}

impl RelayConnection {
    pub fn remote_public_key(&self) -> PublicKey { /* ... */ }

    // Simple API (one stream per frame - convenient but overhead at high fps)
    pub async fn send_frame(&self, frame: &Frame) -> Result<()> { /* ... */ }
    pub async fn recv_frame(&self) -> Result<Frame> { /* ... */ }

    // Persistent stream API (recommended for video streaming)
    pub async fn open_frame_stream(&self) -> Result<FrameSender> { /* ... */ }
    pub async fn accept_frame_stream(&self) -> Result<FrameReceiver> { /* ... */ }
}
```

**Persistent streaming example:**

```rust
// Sender side (camera)
let sender = conn.open_frame_stream().await?;
loop {
    let frame = capture_frame().await?;
    sender.send(&frame).await?;
}

// Receiver side (server/client)
let receiver = conn.accept_frame_stream().await?;
while let Some(frame) = receiver.recv().await? {
    process_frame(frame);
}
```

### Phase 1.3: Video Capture (Day 8-12)

#### Task: Implement basic V4L2 capture

**File: `crates/yurei-capture/src/video.rs`**

```rust
use anyhow::Result;
use bytes::Bytes;
use std::process::{Child, Command, Stdio};
use std::io::Read;
use tokio::sync::mpsc;

/// Video capture using libcamera-vid (Pi-specific, can be swapped)
pub struct VideoCapture {
    process: Child,
}

impl VideoCapture {
    /// Start video capture
    /// Returns a receiver for H.264 NAL units
    pub fn start(width: u32, height: u32, fps: u32) -> Result<mpsc::Receiver<Bytes>> {
        let (tx, rx) = mpsc::channel(30); // Buffer ~1 second at 30fps

        // Use libcamera-vid for Pi camera
        // Outputs H.264 to stdout
        let mut child = Command::new("libcamera-vid")
            .args([
                "-t", "0",                          // Run indefinitely
                "--width", &width.to_string(),
                "--height", &height.to_string(),
                "--framerate", &fps.to_string(),
                "--codec", "h264",
                "--inline",                         // Include SPS/PPS with keyframes
                "-o", "-",                          // Output to stdout
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;

        let mut stdout = child.stdout.take().unwrap();

        // Spawn reader task
        tokio::task::spawn_blocking(move || {
            let mut buf = vec![0u8; 65536];
            loop {
                match stdout.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if tx.blocking_send(Bytes::copy_from_slice(&buf[..n])).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        Ok(rx)
    }
}

// For non-Pi development, provide a test pattern generator
#[cfg(feature = "test-source")]
pub mod test {
    use super::*;
    use tokio::time::{interval, Duration};

    pub fn start_test_source(fps: u32) -> mpsc::Receiver<Bytes> {
        let (tx, rx) = mpsc::channel(30);
        
        tokio::spawn(async move {
            let mut interval = interval(Duration::from_millis(1000 / fps as u64));
            let mut frame_num = 0u64;
            
            loop {
                interval.tick().await;
                // Send fake "frame" with frame number
                let data = frame_num.to_be_bytes().to_vec();
                if tx.send(Bytes::from(data)).await.is_err() {
                    break;
                }
                frame_num += 1;
            }
        });
        
        rx
    }
}
```

**File: `crates/yurei-capture/src/lib.rs`**

```rust
mod video;

pub use video::VideoCapture;

#[cfg(feature = "test-source")]
pub use video::test::start_test_source;
```

### Phase 1.4: Camera Binary (Day 13-15)

**File: `apps/yurei-camera/src/main.rs`**

```rust
use anyhow::Result;
use iroh::EndpointId;
use std::path::PathBuf;
use std::str::FromStr;
use tokio::time::Instant;
use tracing::{info, warn};
use yurei_capture::VideoCapture;
use yurei_core::{Frame, SourceId};
use yurei_relay::Relay;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    // Load config (hardcoded for POC)
    let server_endpoint_id = std::env::var("YUREI_SERVER_ENDPOINT_ID")
        .expect("Set YUREI_SERVER_ENDPOINT_ID env var");
    let server_endpoint_id = EndpointId::from_str(&server_endpoint_id)?;

    let key_path = PathBuf::from("/var/lib/yurei/camera.key");

    // Initialize relay (our Iroh endpoint)
    let relay = Relay::new(Some(&key_path)).await?;
    let source_id = SourceId::from_node_id_bytes(relay.endpoint_id().as_bytes());

    info!("Camera starting with EndpointId: {}", relay.endpoint_id());
    info!("Connecting to server: {}", server_endpoint_id);

    // Connect to server
    let conn = relay.connect(server_endpoint_id).await?;
    info!("Connected to server");

    // Start video capture
    let mut video_rx = VideoCapture::start(1280, 720, 30)?;
    info!("Video capture started");

    let start = Instant::now();

    // Stream frames to server
    while let Some(payload) = video_rx.recv().await {
        let timestamp_us = start.elapsed().as_micros() as u64;

        // TODO: Detect keyframes properly by parsing H.264 NAL units
        let keyframe = payload.len() > 10000; // Rough heuristic

        let mut frame = Frame::video(source_id, payload, keyframe);
        frame.timestamp_us = timestamp_us;

        if let Err(e) = conn.send_frame(&frame).await {
            warn!("Failed to send frame: {}", e);
            break;
        }
    }

    Ok(())
}
```

### Phase 1.5: Server Binary (Day 16-18)

**File: `apps/yurei-server-bin/src/main.rs`**

```rust
use anyhow::Result;
use iroh::EndpointId;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};
use tracing::{info, warn};
use yurei_core::Frame;
use yurei_relay::{Relay, RelayConnection};

type FrameBroadcast = broadcast::Sender<Frame>;

struct ServerState {
    /// Connected clients that want to receive frames
    clients: RwLock<HashMap<EndpointId, broadcast::Sender<Frame>>>,
    /// Broadcast channel for all incoming frames
    frame_tx: FrameBroadcast,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let key_path = PathBuf::from("./server.key");
    let relay = Relay::new(Some(&key_path)).await?;

    info!("Server starting with EndpointId: {}", relay.endpoint_id());
    info!("Waiting for connections...");

    let (frame_tx, _) = broadcast::channel(100);
    let state = Arc::new(ServerState {
        clients: RwLock::new(HashMap::new()),
        frame_tx,
    });

    loop {
        if let Some(conn) = relay.accept().await {
            let remote = conn.remote_endpoint_id();
            info!("New connection from: {}", remote);

            let state = state.clone();
            tokio::spawn(async move {
                handle_connection(conn, state).await;
            });
        }
    }
}

async fn handle_connection(conn: RelayConnection, state: Arc<ServerState>) {
    let remote = conn.remote_endpoint_id();

    // For POC: assume cameras send frames, clients receive them
    // In practice, we'd have a handshake to determine role

    // Try receiving frames (camera role)
    loop {
        match conn.recv_frame().await {
            Ok(frame) => {
                // Broadcast to all subscribers
                let _ = state.frame_tx.send(frame);
            }
            Err(e) => {
                warn!("Connection {} error: {}", remote, e);
                break;
            }
        }
    }

    info!("Connection {} closed", remote);
}
```

### Phase 1.6: Desktop Client (Day 19-25)

#### Task: Set up Tauri + Svelte project

```bash
# In apps/yurei-desktop
npm create tauri-app@latest -- --template svelte
```

**File: `apps/yurei-desktop/src/main.rs`** (Tauri backend)

```rust
use anyhow::Result;
use iroh::EndpointId;
use std::str::FromStr;
use std::sync::Arc;
use tauri::{Manager, State};
use tokio::sync::{mpsc, RwLock};
use yurei_core::Frame;
use yurei_relay::Relay;

struct AppState {
    relay: Relay,
    frame_rx: RwLock<Option<mpsc::Receiver<Frame>>>,
}

#[tauri::command]
async fn connect_to_server(
    server_endpoint_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    let endpoint_id = EndpointId::from_str(&server_endpoint_id)
        .map_err(|e| e.to_string())?;

    let conn = state.relay.connect(endpoint_id).await
        .map_err(|e| e.to_string())?;

    let (tx, rx) = mpsc::channel(30);
    *state.frame_rx.write().await = Some(rx);

    // Spawn frame receiver
    tokio::spawn(async move {
        loop {
            match conn.recv_frame().await {
                Ok(frame) => {
                    if tx.send(frame).await.is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    Ok(())
}

#[tauri::command]
async fn get_next_frame(
    state: State<'_, Arc<AppState>>,
) -> Result<Option<Vec<u8>>, String> {
    let mut rx_guard = state.frame_rx.write().await;
    if let Some(rx) = rx_guard.as_mut() {
        match rx.try_recv() {
            Ok(frame) => Ok(Some(frame.payload.to_vec())),
            Err(_) => Ok(None),
        }
    } else {
        Ok(None)
    }
}

#[tokio::main]
async fn main() {
    let relay = Relay::new(None).await.expect("Failed to create relay");

    let state = Arc::new(AppState {
        relay,
        frame_rx: RwLock::new(None),
    });

    tauri::Builder::default()
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            connect_to_server,
            get_next_frame,
        ])
        .run(tauri::generate_context!())
        .expect("error running tauri app");
}
```

**File: `apps/yurei-desktop/ui/src/App.svelte`**

```svelte
<script>
  import { invoke } from '@tauri-apps/api/tauri';
  import { onMount } from 'svelte';

  let serverEndpointId = '';
  let connected = false;
  let status = 'Disconnected';
  let frameCount = 0;

  // For POC: just count frames, actual video decode comes later

  async function connect() {
    try {
      await invoke('connect_to_server', { serverEndpointId });
      connected = true;
      status = 'Connected';
      startFrameLoop();
    } catch (e) {
      status = `Error: ${e}`;
    }
  }
  
  async function startFrameLoop() {
    while (connected) {
      try {
        const frame = await invoke('get_next_frame');
        if (frame) {
          frameCount++;
          // TODO: Decode H.264 and render
        }
      } catch (e) {
        console.error(e);
      }
      await new Promise(r => setTimeout(r, 16)); // ~60fps poll
    }
  }
</script>

<main>
  <h1>Yurei Desktop</h1>
  
  {#if !connected}
    <div>
      <input
        type="text"
        bind:value={serverEndpointId}
        placeholder="Server EndpointId"
      />
      <button on:click={connect}>Connect</button>
    </div>
  {/if}
  
  <p>Status: {status}</p>
  <p>Frames received: {frameCount}</p>
  
  <!-- Video canvas will go here -->
  <div id="video-container">
    <canvas id="video-canvas" width="1280" height="720"></canvas>
  </div>
</main>

<style>
  main {
    padding: 20px;
    font-family: system-ui;
  }
  
  input {
    padding: 8px;
    width: 400px;
    font-family: monospace;
  }
  
  #video-container {
    margin-top: 20px;
    background: #000;
    display: inline-block;
  }
  
  canvas {
    display: block;
  }
</style>
```

### Phase 1.7: Integration Testing (Day 26-28)

#### Task: End-to-end test script

**File: `scripts/test-e2e.sh`**

```bash
#!/bin/bash
set -e

echo "=== Yurei POC 1 E2E Test ==="

# Build all binaries
cargo build --release

# Start server
echo "Starting server..."
./target/release/yurei-server-bin &
SERVER_PID=$!
sleep 2

# Get server EndpointId from logs (hacky, improve later)
SERVER_ENDPOINT_ID=$(grep "EndpointId:" /tmp/server.log | cut -d' ' -f4)
echo "Server EndpointId: $SERVER_ENDPOINT_ID"

# Start camera (or test source on dev machine)
echo "Starting camera..."
YUREI_SERVER_ENDPOINT_ID=$SERVER_ENDPOINT_ID ./target/release/yurei-camera &
CAMERA_PID=$!
sleep 2

echo "Camera and server running. Start desktop app and connect to:"
echo "  $SERVER_ENDPOINT_ID"
echo ""
echo "Press Ctrl+C to stop"

wait
```

## Success Criteria Checklist

- [ ] `cargo build` succeeds for all crates
- [ ] Camera binary starts and prints its EndpointId
- [ ] Server binary starts, prints its EndpointId, accepts connections
- [ ] Camera connects to server and streams frames
- [ ] Desktop app connects to server
- [ ] Desktop app receives frames (frame counter increments)
- [ ] System runs stable for 10+ minutes without crashes

## Next Steps (POC 2)

After POC 1 validates core streaming:

1. Add H.264 decoding in desktop app (WebCodecs or ffmpeg)
2. Add audio channel (Opus)
3. Add mobile app (Tauri mobile)
4. Add multi-camera support
5. Add basic pairing (QR code)
6. Deploy server to cloud for testing

## Dependencies to Research

- `iroh` - ✅ Confirmed API for 0.96: uses `EndpointId`, `endpoint.id()`, discovery services
- `libcamera-rs` - Alternative to spawning `libcamera-vid` process
- Tauri video rendering - Best approach for H.264 decode in WebView
- Cross-compilation - Pi Zero 2W target setup

## Known Issues

- **Crypto dependency conflicts:** iroh 0.96 uses pre-release `curve25519-dalek` and `ed25519-dalek`
  which conflict with newer `digest` versions. Pin with:
  ```bash
  cargo update -p sha2 --precise 0.11.0-rc.4
  cargo update -p digest --precise 0.11.0-rc.9
  ```
