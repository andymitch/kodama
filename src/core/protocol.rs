//! Protocol constants for Kodama

/// ALPN protocol identifier for Kodama connections.
///
/// Used during QUIC handshake to identify Kodama protocol version.
pub const ALPN: &[u8] = b"kodama/0";

/// Default port for camera relay endpoints
pub const DEFAULT_CAMERA_PORT: u16 = 7878;

/// Default port for server endpoints
pub const DEFAULT_SERVER_PORT: u16 = 7879;

/// Frame header size in bytes (source + channel + flags + timestamp)
pub const FRAME_HEADER_SIZE: usize = 8 + 1 + 1 + 8; // 18 bytes

/// Maximum frame payload size (16 MB, generous for high-bitrate video)
pub const MAX_FRAME_PAYLOAD_SIZE: usize = 16 * 1024 * 1024;
