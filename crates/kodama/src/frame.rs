//! Frame types for video, audio, and telemetry data

use bytes::Bytes;
use serde::{Deserialize, Serialize};

/// Identifies a camera/source in the system.
///
/// Derived from the first 8 bytes of an Iroh NodeId for compactness
/// while maintaining uniqueness within a deployment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SourceId(pub [u8; 8]);

impl SourceId {
    /// Create a new SourceId from raw bytes
    pub fn new(id: [u8; 8]) -> Self {
        Self(id)
    }

    /// Generate from Iroh NodeId bytes (truncates to first 8 bytes)
    pub fn from_node_id_bytes(bytes: &[u8]) -> Self {
        assert!(
            bytes.len() >= 8,
            "NodeId bytes must be at least 8 bytes, got {}",
            bytes.len()
        );
        let mut id = [0u8; 8];
        id.copy_from_slice(&bytes[..8]);
        Self(id)
    }

    /// Get the raw bytes
    pub fn as_bytes(&self) -> &[u8; 8] {
        &self.0
    }
}

impl std::fmt::Display for SourceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Display as hex for readability
        for byte in &self.0 {
            write!(f, "{:02x}", byte)?;
        }
        Ok(())
    }
}

/// Channel types within a source.
///
/// Each camera outputs multiple channels that are muxed by the relay layer.
/// The `Unknown` variant provides forward compatibility: frames with
/// unrecognized channel types are accepted and relayed without error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Channel {
    /// H.264 video frames
    Video,
    /// Opus audio packets
    Audio,
    /// MessagePack-encoded telemetry (CPU, temp, etc.)
    Telemetry,
    /// Forward-compatible: silently accept unknown channel types
    Unknown(u8),
}

impl Channel {
    /// Convert to the wire-format byte value.
    pub fn as_u8(&self) -> u8 {
        match self {
            Channel::Video => 0,
            Channel::Audio => 1,
            Channel::Telemetry => 2,
            Channel::Unknown(v) => *v,
        }
    }
}

impl From<u8> for Channel {
    fn from(value: u8) -> Self {
        match value {
            0 => Channel::Video,
            1 => Channel::Audio,
            2 => Channel::Telemetry,
            v => Channel::Unknown(v),
        }
    }
}

/// Frame flags for metadata.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameFlags(pub u8);

impl FrameFlags {
    /// Flag indicating this is a keyframe (I-frame for video)
    pub const KEYFRAME: u8 = 0b0000_0001;

    /// Check if this is a keyframe
    pub fn is_keyframe(&self) -> bool {
        self.0 & Self::KEYFRAME != 0
    }

    /// Set the keyframe flag
    pub fn set_keyframe(&mut self) {
        self.0 |= Self::KEYFRAME;
    }

    /// Create flags with keyframe set
    pub fn keyframe() -> Self {
        Self(Self::KEYFRAME)
    }
}

/// A single frame of data (video, audio, or telemetry).
///
/// Wire format (22 bytes header + payload):
/// ```text
/// ┌──────────────┬──────────────┬──────────────┬──────────────┬─────────────┐
/// │   source     │   channel    │    flags     │  timestamp   │   payload   │
/// │   (8 bytes)  │   (1 byte)   │   (1 byte)   │   (8 bytes)  │   (var)     │
/// └──────────────┴──────────────┴──────────────┴──────────────┴─────────────┘
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Frame {
    /// Source camera/device ID
    pub source: SourceId,
    /// Channel type (video, audio, telemetry)
    pub channel: Channel,
    /// Frame flags (keyframe, etc.)
    pub flags: FrameFlags,
    /// Timestamp in microseconds since stream start
    pub timestamp_us: u64,
    /// Frame payload (H.264 NAL units, Opus packets, or MessagePack)
    pub payload: Bytes,
}

impl Frame {
    /// Create a video frame
    pub fn video(source: SourceId, payload: Bytes, keyframe: bool) -> Self {
        let flags = if keyframe {
            FrameFlags::keyframe()
        } else {
            FrameFlags::default()
        };
        Self {
            source,
            channel: Channel::Video,
            flags,
            timestamp_us: 0, // Caller should set
            payload,
        }
    }

    /// Create an audio frame
    pub fn audio(source: SourceId, payload: Bytes) -> Self {
        Self {
            source,
            channel: Channel::Audio,
            flags: FrameFlags::default(),
            timestamp_us: 0,
            payload,
        }
    }

    /// Create a telemetry frame
    pub fn telemetry(source: SourceId, payload: Bytes) -> Self {
        Self {
            source,
            channel: Channel::Telemetry,
            flags: FrameFlags::default(),
            timestamp_us: 0,
            payload,
        }
    }

    /// Set the timestamp and return self (builder pattern)
    pub fn with_timestamp(mut self, timestamp_us: u64) -> Self {
        self.timestamp_us = timestamp_us;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_source_id_display() {
        let id = SourceId::new([0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef]);
        assert_eq!(format!("{}", id), "0123456789abcdef");
    }

    #[test]
    fn test_channel_conversion() {
        assert_eq!(Channel::from(0), Channel::Video);
        assert_eq!(Channel::from(1), Channel::Audio);
        assert_eq!(Channel::from(2), Channel::Telemetry);
        assert_eq!(Channel::from(3), Channel::Unknown(3));
        assert_eq!(Channel::from(255), Channel::Unknown(255));
    }

    #[test]
    fn test_channel_as_u8() {
        assert_eq!(Channel::Video.as_u8(), 0);
        assert_eq!(Channel::Audio.as_u8(), 1);
        assert_eq!(Channel::Telemetry.as_u8(), 2);
        assert_eq!(Channel::Unknown(42).as_u8(), 42);
    }

    #[test]
    fn test_frame_flags() {
        let mut flags = FrameFlags::default();
        assert!(!flags.is_keyframe());

        flags.set_keyframe();
        assert!(flags.is_keyframe());

        let keyframe_flags = FrameFlags::keyframe();
        assert!(keyframe_flags.is_keyframe());
    }

    #[test]
    fn test_frame_constructors() {
        let source = SourceId::new([0; 8]);
        let payload = Bytes::from_static(b"test");

        let video = Frame::video(source, payload.clone(), true);
        assert_eq!(video.channel, Channel::Video);
        assert!(video.flags.is_keyframe());

        let audio = Frame::audio(source, payload.clone());
        assert_eq!(audio.channel, Channel::Audio);

        let telemetry = Frame::telemetry(source, payload);
        assert_eq!(telemetry.channel, Channel::Telemetry);
    }

    // ========== SourceId edge cases ==========

    #[test]
    fn source_id_from_node_id_exact_8_bytes() {
        let bytes = [10u8; 8];
        let id = SourceId::from_node_id_bytes(&bytes);
        assert_eq!(id.0, bytes);
    }

    #[test]
    fn source_id_from_node_id_truncates_longer_input() {
        let bytes = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
        let id = SourceId::from_node_id_bytes(&bytes);
        assert_eq!(id.0, [1, 2, 3, 4, 5, 6, 7, 8]);
    }

    #[test]
    #[should_panic(expected = "NodeId bytes must be at least 8 bytes")]
    fn source_id_from_node_id_panics_on_short_input() {
        SourceId::from_node_id_bytes(&[1, 2, 3]);
    }

    #[test]
    #[should_panic(expected = "NodeId bytes must be at least 8 bytes")]
    fn source_id_from_node_id_panics_on_empty_input() {
        SourceId::from_node_id_bytes(&[]);
    }

    #[test]
    fn source_id_all_zeros_display() {
        let id = SourceId::new([0; 8]);
        assert_eq!(format!("{}", id), "0000000000000000");
    }

    #[test]
    fn source_id_equality_and_hash() {
        use std::collections::HashSet;

        let a = SourceId::new([1, 2, 3, 4, 5, 6, 7, 8]);
        let b = SourceId::new([1, 2, 3, 4, 5, 6, 7, 8]);
        let c = SourceId::new([8, 7, 6, 5, 4, 3, 2, 1]);

        assert_eq!(a, b);
        assert_ne!(a, c);

        let mut set = HashSet::new();
        set.insert(a);
        assert!(set.contains(&b));
        assert!(!set.contains(&c));
    }
}
