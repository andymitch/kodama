//! Frame demultiplexer (stub)
//!
//! Splits a multiplexed frame stream by source ID and channel type
//! for routing to appropriate handlers.

use std::collections::HashMap;
use tokio::sync::mpsc;

use crate::core::{Channel, Frame, SourceId};

/// Demultiplexer for splitting frames by source and channel
pub struct Demuxer {
    /// Subscribers by source ID
    source_subscribers: HashMap<SourceId, mpsc::Sender<Frame>>,
    /// Subscribers by channel type (receive all sources)
    channel_subscribers: HashMap<Channel, mpsc::Sender<Frame>>,
    /// Global subscribers (receive everything)
    global_subscribers: Vec<mpsc::Sender<Frame>>,
}

impl Demuxer {
    /// Create a new demuxer
    pub fn new() -> Self {
        Self {
            source_subscribers: HashMap::new(),
            channel_subscribers: HashMap::new(),
            global_subscribers: Vec::new(),
        }
    }

    /// Subscribe to frames from a specific source
    pub fn subscribe_source(&mut self, source: SourceId) -> mpsc::Receiver<Frame> {
        let (tx, rx) = mpsc::channel(64);
        self.source_subscribers.insert(source, tx);
        rx
    }

    /// Subscribe to frames of a specific channel type (all sources)
    pub fn subscribe_channel(&mut self, channel: Channel) -> mpsc::Receiver<Frame> {
        let (tx, rx) = mpsc::channel(64);
        self.channel_subscribers.insert(channel, tx);
        rx
    }

    /// Subscribe to all frames
    pub fn subscribe_all(&mut self) -> mpsc::Receiver<Frame> {
        let (tx, rx) = mpsc::channel(64);
        self.global_subscribers.push(tx);
        rx
    }

    /// Route a frame to appropriate subscribers
    ///
    /// Returns the number of subscribers that received the frame.
    pub async fn route(&self, frame: Frame) -> usize {
        let mut count = 0;

        // Route to source-specific subscribers
        if let Some(tx) = self.source_subscribers.get(&frame.source) {
            if tx.send(frame.clone()).await.is_ok() {
                count += 1;
            }
        }

        // Route to channel-specific subscribers
        if let Some(tx) = self.channel_subscribers.get(&frame.channel) {
            if tx.send(frame.clone()).await.is_ok() {
                count += 1;
            }
        }

        // Route to global subscribers
        for tx in &self.global_subscribers {
            if tx.send(frame.clone()).await.is_ok() {
                count += 1;
            }
        }

        count
    }

    /// Remove closed subscribers
    pub fn cleanup(&mut self) {
        self.source_subscribers.retain(|_, tx| !tx.is_closed());
        self.channel_subscribers.retain(|_, tx| !tx.is_closed());
        self.global_subscribers.retain(|tx| !tx.is_closed());
    }
}

impl Default for Demuxer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use crate::core::FrameFlags;

    #[tokio::test]
    async fn test_demuxer_source_subscription() {
        let mut demuxer = Demuxer::new();
        let source = SourceId::new([1, 2, 3, 4, 5, 6, 7, 8]);
        let mut rx = demuxer.subscribe_source(source);

        let frame = Frame {
            source,
            channel: Channel::Video,
            flags: FrameFlags::default(),
            timestamp_us: 0,
            payload: Bytes::from_static(b"test"),
        };

        let count = demuxer.route(frame.clone()).await;
        assert_eq!(count, 1);

        let received = rx.recv().await.unwrap();
        assert_eq!(received.source, source);
    }
}
