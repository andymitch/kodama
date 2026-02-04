//! Wire format for frames over QUIC streams
//!
//! Each frame is sent as:
//! - 4 bytes: payload length (big-endian u32)
//! - N bytes: frame header + payload
//!
//! Frame header (18 bytes):
//! - 8 bytes: source ID
//! - 1 byte: channel type
//! - 1 byte: flags
//! - 8 bytes: timestamp (microseconds, big-endian u64)
//!
//! Followed by variable-length payload.

use anyhow::Result;
use bytes::{Buf, BufMut, Bytes, BytesMut};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::core::{Channel, Frame, FrameFlags, SourceId, FRAME_HEADER_SIZE, MAX_FRAME_PAYLOAD_SIZE};

/// Serialize a frame to bytes (header + payload)
pub fn frame_to_bytes(frame: &Frame) -> Bytes {
    let mut buf = BytesMut::with_capacity(FRAME_HEADER_SIZE + frame.payload.len());

    // Header
    buf.put_slice(frame.source.as_bytes());
    buf.put_u8(frame.channel as u8);
    buf.put_u8(frame.flags.0);
    buf.put_u64(frame.timestamp_us);

    // Payload
    buf.put_slice(&frame.payload);

    buf.freeze()
}

/// Deserialize a frame from bytes
pub fn frame_from_bytes(mut buf: Bytes) -> Result<Frame> {
    if buf.len() < FRAME_HEADER_SIZE {
        anyhow::bail!(
            "Buffer too small for frame header: {} < {}",
            buf.len(),
            FRAME_HEADER_SIZE
        );
    }

    // Parse header
    let mut source_bytes = [0u8; 8];
    buf.copy_to_slice(&mut source_bytes);
    let source = SourceId::new(source_bytes);

    let channel = Channel::try_from(buf.get_u8())
        .map_err(|_| anyhow::anyhow!("Unknown channel type"))?;

    let flags = FrameFlags(buf.get_u8());
    let timestamp_us = buf.get_u64();

    // Remaining bytes are payload
    let payload = buf;

    Ok(Frame {
        source,
        channel,
        flags,
        timestamp_us,
        payload,
    })
}

/// Write a frame to an async writer (prepends length prefix)
pub async fn write_frame<W: AsyncWrite + Unpin>(writer: &mut W, frame: &Frame) -> Result<()> {
    let bytes = frame_to_bytes(frame);

    if bytes.len() > MAX_FRAME_PAYLOAD_SIZE {
        anyhow::bail!(
            "Frame too large: {} > {}",
            bytes.len(),
            MAX_FRAME_PAYLOAD_SIZE
        );
    }

    // Write length prefix
    writer.write_all(&(bytes.len() as u32).to_be_bytes()).await?;

    // Write frame data
    writer.write_all(&bytes).await?;

    Ok(())
}

/// Read a frame from an async reader (expects length prefix)
pub async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Frame> {
    // Read length prefix
    let mut len_bytes = [0u8; 4];
    reader.read_exact(&mut len_bytes).await?;
    let len = u32::from_be_bytes(len_bytes) as usize;

    if len > MAX_FRAME_PAYLOAD_SIZE {
        anyhow::bail!("Frame length exceeds maximum: {} > {}", len, MAX_FRAME_PAYLOAD_SIZE);
    }

    if len < FRAME_HEADER_SIZE {
        anyhow::bail!("Frame length too small for header: {} < {}", len, FRAME_HEADER_SIZE);
    }

    // Read frame data
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf).await?;

    frame_from_bytes(Bytes::from(buf))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_frame_roundtrip() {
        let source = SourceId::new([1, 2, 3, 4, 5, 6, 7, 8]);
        let payload = Bytes::from_static(b"hello world");

        let frame = Frame {
            source,
            channel: Channel::Video,
            flags: FrameFlags::keyframe(),
            timestamp_us: 123456789,
            payload,
        };

        let bytes = frame_to_bytes(&frame);
        let decoded = frame_from_bytes(bytes).unwrap();

        assert_eq!(decoded.source, frame.source);
        assert_eq!(decoded.channel, frame.channel);
        assert_eq!(decoded.flags, frame.flags);
        assert_eq!(decoded.timestamp_us, frame.timestamp_us);
        assert_eq!(decoded.payload, frame.payload);
    }

    #[tokio::test]
    async fn test_read_write_frame() {
        let source = SourceId::new([1, 2, 3, 4, 5, 6, 7, 8]);
        let payload = Bytes::from_static(b"test payload");

        let frame = Frame {
            source,
            channel: Channel::Audio,
            flags: FrameFlags::default(),
            timestamp_us: 999,
            payload,
        };

        // Write to buffer
        let mut buf = Vec::new();
        write_frame(&mut buf, &frame).await.unwrap();

        // Read back
        let mut cursor = std::io::Cursor::new(buf);
        let decoded = read_frame(&mut cursor).await.unwrap();

        assert_eq!(decoded.source, frame.source);
        assert_eq!(decoded.channel, frame.channel);
        assert_eq!(decoded.timestamp_us, frame.timestamp_us);
        assert_eq!(decoded.payload, frame.payload);
    }
}
