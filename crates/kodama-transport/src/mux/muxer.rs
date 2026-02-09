//! Frame buffering with backpressure handling
//!
//! Provides smart frame dropping when the network can't keep up:
//! - Drops non-keyframes first
//! - Always keeps the most recent keyframe
//! - Provides metrics on dropped frames

use std::collections::VecDeque;
use tokio::sync::mpsc;
use tracing::{debug, warn};

use kodama_core::Frame;

/// Statistics about frame dropping
#[derive(Debug, Clone, Default)]
pub struct BufferStats {
    /// Total frames received
    pub frames_received: u64,
    /// Frames dropped due to buffer overflow
    pub frames_dropped: u64,
    /// Keyframes received
    pub keyframes_received: u64,
    /// Keyframes dropped (should be rare)
    pub keyframes_dropped: u64,
}

impl BufferStats {
    /// Get the drop rate as a percentage
    pub fn drop_rate(&self) -> f64 {
        if self.frames_received == 0 {
            0.0
        } else {
            (self.frames_dropped as f64 / self.frames_received as f64) * 100.0
        }
    }
}

/// A frame buffer that handles backpressure by selectively dropping frames
///
/// Strategy:
/// 1. If buffer is full and a non-keyframe arrives, drop the oldest non-keyframe
/// 2. If buffer is full and a keyframe arrives, drop the oldest non-keyframe to make room
/// 3. Only drop keyframes as a last resort
pub struct FrameBuffer {
    buffer: VecDeque<Frame>,
    capacity: usize,
    stats: BufferStats,
    last_keyframe_idx: Option<usize>,
}

impl FrameBuffer {
    /// Create a new frame buffer with the given capacity
    pub fn new(capacity: usize) -> Self {
        Self {
            buffer: VecDeque::with_capacity(capacity),
            capacity,
            stats: BufferStats::default(),
            last_keyframe_idx: None,
        }
    }

    /// Push a frame into the buffer, dropping frames if necessary
    ///
    /// Returns true if the frame was kept, false if it was dropped.
    pub fn push(&mut self, frame: Frame) -> bool {
        let is_keyframe = frame.flags.is_keyframe();

        self.stats.frames_received += 1;
        if is_keyframe {
            self.stats.keyframes_received += 1;
        }

        if self.buffer.len() < self.capacity {
            // Buffer has room
            if is_keyframe {
                self.last_keyframe_idx = Some(self.buffer.len());
            }
            self.buffer.push_back(frame);
            return true;
        }

        // Buffer is full - need to drop something
        if is_keyframe {
            // Keyframe arriving - try to drop a non-keyframe
            if let Some(idx) = self.find_droppable_non_keyframe() {
                self.drop_at(idx);
                self.last_keyframe_idx = Some(self.buffer.len());
                self.buffer.push_back(frame);
                return true;
            }
            // No non-keyframes to drop - drop oldest keyframe
            warn!("Dropping keyframe due to buffer overflow");
            self.drop_oldest();
            self.last_keyframe_idx = Some(self.buffer.len());
            self.buffer.push_back(frame);
            true
        } else {
            // Non-keyframe arriving - try to drop another non-keyframe
            if let Some(idx) = self.find_droppable_non_keyframe() {
                self.drop_at(idx);
                self.buffer.push_back(frame);
                return true;
            }
            // Only keyframes in buffer - drop this non-keyframe
            debug!("Dropping non-keyframe due to buffer full of keyframes");
            self.stats.frames_dropped += 1;
            false
        }
    }

    /// Pop the oldest frame from the buffer
    pub fn pop(&mut self) -> Option<Frame> {
        let frame = self.buffer.pop_front()?;

        // Update keyframe index
        if let Some(idx) = &mut self.last_keyframe_idx {
            if *idx == 0 {
                // We just popped the last keyframe
                self.last_keyframe_idx = None;
            } else {
                *idx -= 1;
            }
        }

        Some(frame)
    }

    /// Check if the buffer is empty
    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    /// Get the number of frames in the buffer
    pub fn len(&self) -> usize {
        self.buffer.len()
    }

    /// Get buffer statistics
    pub fn stats(&self) -> &BufferStats {
        &self.stats
    }

    /// Find a non-keyframe that can be dropped (prefer oldest)
    fn find_droppable_non_keyframe(&self) -> Option<usize> {
        for (i, frame) in self.buffer.iter().enumerate() {
            if !frame.flags.is_keyframe() {
                return Some(i);
            }
        }
        None
    }

    /// Drop frame at index and update stats
    fn drop_at(&mut self, idx: usize) {
        if let Some(frame) = self.buffer.remove(idx) {
            self.stats.frames_dropped += 1;
            if frame.flags.is_keyframe() {
                self.stats.keyframes_dropped += 1;
            }

            // Update keyframe index if needed
            if let Some(ki) = &mut self.last_keyframe_idx {
                if idx < *ki {
                    *ki -= 1;
                } else if idx == *ki {
                    self.last_keyframe_idx = None;
                }
            }
        }
    }

    /// Drop the oldest frame
    fn drop_oldest(&mut self) {
        if let Some(frame) = self.buffer.pop_front() {
            self.stats.frames_dropped += 1;
            if frame.flags.is_keyframe() {
                self.stats.keyframes_dropped += 1;
            }

            // Update keyframe index
            if let Some(idx) = &mut self.last_keyframe_idx {
                if *idx == 0 {
                    self.last_keyframe_idx = None;
                } else {
                    *idx -= 1;
                }
            }
        }
    }
}

/// A channel sender with backpressure handling
///
/// Wraps an mpsc sender and handles full channel by buffering
/// and selectively dropping frames.
pub struct BackpressureSender {
    tx: mpsc::Sender<Frame>,
    buffer: FrameBuffer,
}

impl BackpressureSender {
    /// Create a new backpressure sender
    ///
    /// `buffer_size` is the number of frames to buffer when the channel is full.
    pub fn new(tx: mpsc::Sender<Frame>, buffer_size: usize) -> Self {
        Self {
            tx,
            buffer: FrameBuffer::new(buffer_size),
        }
    }

    /// Send a frame, buffering and dropping as needed
    ///
    /// Returns Ok(true) if the frame was sent or buffered,
    /// Ok(false) if the frame was dropped,
    /// Err if the channel is closed.
    pub async fn send(&mut self, frame: Frame) -> Result<bool, mpsc::error::SendError<Frame>> {
        // First try to flush any buffered frames
        self.flush_buffer().await?;

        // Try to send directly
        match self.tx.try_send(frame) {
            Ok(()) => Ok(true),
            Err(mpsc::error::TrySendError::Full(frame)) => {
                // Channel full - buffer the frame
                Ok(self.buffer.push(frame))
            }
            Err(mpsc::error::TrySendError::Closed(frame)) => Err(mpsc::error::SendError(frame)),
        }
    }

    /// Try to flush buffered frames to the channel
    async fn flush_buffer(&mut self) -> Result<(), mpsc::error::SendError<Frame>> {
        while let Some(frame) = self.buffer.pop() {
            match self.tx.try_send(frame) {
                Ok(()) => continue,
                Err(mpsc::error::TrySendError::Full(frame)) => {
                    // Put it back - channel still full
                    self.buffer.push(frame);
                    break;
                }
                Err(mpsc::error::TrySendError::Closed(frame)) => {
                    return Err(mpsc::error::SendError(frame));
                }
            }
        }
        Ok(())
    }

    /// Get buffer statistics
    pub fn stats(&self) -> &BufferStats {
        self.buffer.stats()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use kodama_core::{Channel, FrameFlags, SourceId};

    fn make_frame(keyframe: bool) -> Frame {
        Frame {
            source: SourceId::new([0; 8]),
            channel: Channel::Video,
            flags: if keyframe {
                FrameFlags::keyframe()
            } else {
                FrameFlags::default()
            },
            timestamp_us: 0,
            payload: Bytes::new(),
        }
    }

    #[test]
    fn test_buffer_basic() {
        let mut buffer = FrameBuffer::new(3);

        assert!(buffer.push(make_frame(true))); // keyframe
        assert!(buffer.push(make_frame(false))); // non-keyframe
        assert!(buffer.push(make_frame(false))); // non-keyframe

        assert_eq!(buffer.len(), 3);
        assert_eq!(buffer.stats().frames_dropped, 0);
    }

    #[test]
    fn test_buffer_drops_non_keyframe() {
        let mut buffer = FrameBuffer::new(2);

        buffer.push(make_frame(true)); // keyframe
        buffer.push(make_frame(false)); // non-keyframe
        buffer.push(make_frame(false)); // another non-keyframe - should drop one

        assert_eq!(buffer.len(), 2);
        assert_eq!(buffer.stats().frames_dropped, 1);
        assert_eq!(buffer.stats().keyframes_dropped, 0);
    }

    #[test]
    fn test_buffer_preserves_keyframe() {
        let mut buffer = FrameBuffer::new(2);

        buffer.push(make_frame(true)); // keyframe
        buffer.push(make_frame(false)); // non-keyframe
        buffer.push(make_frame(true)); // new keyframe - should drop non-keyframe

        assert_eq!(buffer.len(), 2);
        assert_eq!(buffer.stats().frames_dropped, 1);
        assert_eq!(buffer.stats().keyframes_dropped, 0);

        // Both remaining should be keyframes
        let f1 = buffer.pop().unwrap();
        let f2 = buffer.pop().unwrap();
        assert!(f1.flags.is_keyframe());
        assert!(f2.flags.is_keyframe());
    }

    #[test]
    fn all_keyframes_overflow_drops_incoming_nonkeyframe() {
        // Buffer full of only keyframes — incoming non-keyframe gets dropped
        let mut buffer = FrameBuffer::new(2);
        buffer.push(make_frame(true));
        buffer.push(make_frame(true));

        // Non-keyframe should be dropped (no non-keyframes to evict)
        assert!(!buffer.push(make_frame(false)));
        assert_eq!(buffer.len(), 2);
        assert_eq!(buffer.stats().frames_dropped, 1);
        assert_eq!(buffer.stats().keyframes_dropped, 0);
    }

    #[test]
    fn all_keyframes_overflow_drops_oldest_keyframe_for_new_keyframe() {
        // Buffer full of keyframes — incoming keyframe evicts oldest keyframe
        let mut buffer = FrameBuffer::new(2);
        buffer.push(make_frame(true));
        buffer.push(make_frame(true));

        assert!(buffer.push(make_frame(true)));
        assert_eq!(buffer.len(), 2);
        assert_eq!(buffer.stats().frames_dropped, 1);
        assert_eq!(buffer.stats().keyframes_dropped, 1);
    }

    #[test]
    fn pop_tracks_keyframe_index_correctly() {
        let mut buffer = FrameBuffer::new(4);
        buffer.push(make_frame(false)); // idx 0
        buffer.push(make_frame(false)); // idx 1
        buffer.push(make_frame(true));  // idx 2 — keyframe

        // Pop twice, keyframe index should track
        buffer.pop(); // removes idx 0, keyframe now at idx 1
        buffer.pop(); // removes idx 1, keyframe now at idx 0

        let f = buffer.pop().unwrap();
        assert!(f.flags.is_keyframe());
        assert!(buffer.is_empty());
    }

    #[test]
    fn stats_consistency_after_mixed_operations() {
        let mut buffer = FrameBuffer::new(3);

        // Fill buffer: KF, NKF, NKF
        buffer.push(make_frame(true));
        buffer.push(make_frame(false));
        buffer.push(make_frame(false));
        assert_eq!(buffer.stats().frames_received, 3);

        // Push another NKF — drops oldest NKF
        buffer.push(make_frame(false));
        assert_eq!(buffer.stats().frames_received, 4);
        assert_eq!(buffer.stats().frames_dropped, 1);

        // Push a KF — drops a NKF to make room
        buffer.push(make_frame(true));
        assert_eq!(buffer.stats().frames_received, 5);
        assert_eq!(buffer.stats().frames_dropped, 2);
        assert_eq!(buffer.stats().keyframes_received, 2);
        assert_eq!(buffer.stats().keyframes_dropped, 0);
    }

    #[test]
    fn drop_rate_zero_when_no_frames() {
        let stats = BufferStats::default();
        assert_eq!(stats.drop_rate(), 0.0);
    }

    #[test]
    fn drop_rate_accurate() {
        let mut buffer = FrameBuffer::new(1);
        buffer.push(make_frame(true));  // kept
        buffer.push(make_frame(true));  // evicts oldest keyframe
        buffer.push(make_frame(false)); // dropped (buffer full of keyframes... actually just 1 KF)

        // 3 received, some dropped
        let rate = buffer.stats().drop_rate();
        assert!(rate > 0.0);
        assert!(rate <= 100.0);
    }

    #[test]
    fn pop_on_empty_buffer_returns_none() {
        let mut buffer = FrameBuffer::new(4);
        assert!(buffer.pop().is_none());
        assert!(buffer.is_empty());
    }

    #[test]
    fn capacity_one_buffer() {
        let mut buffer = FrameBuffer::new(1);

        buffer.push(make_frame(true)); // fills buffer
        assert_eq!(buffer.len(), 1);

        // Another keyframe evicts the old one
        buffer.push(make_frame(true));
        assert_eq!(buffer.len(), 1);
        assert_eq!(buffer.stats().keyframes_dropped, 1);

        // Non-keyframe gets dropped (only keyframe in buffer)
        assert!(!buffer.push(make_frame(false)));
        assert_eq!(buffer.stats().frames_dropped, 2); // 1 KF + 1 NKF dropped
    }
}
