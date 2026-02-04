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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum Channel {
    /// H.264 video frames
    Video = 0,
    /// Opus audio packets
    Audio = 1,
    /// MessagePack-encoded telemetry (CPU, temp, etc.)
    Telemetry = 2,
}

impl TryFrom<u8> for Channel {
    type Error = ();

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Channel::Video),
            1 => Ok(Channel::Audio),
            2 => Ok(Channel::Telemetry),
            _ => Err(()),
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
        assert_eq!(Channel::try_from(0), Ok(Channel::Video));
        assert_eq!(Channel::try_from(1), Ok(Channel::Audio));
        assert_eq!(Channel::try_from(2), Ok(Channel::Telemetry));
        assert_eq!(Channel::try_from(3), Err(()));
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
}
