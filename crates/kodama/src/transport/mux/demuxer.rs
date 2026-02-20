//! Frame demultiplexer (stub)
//!
//! Splits a multiplexed frame stream by source ID and channel type
//! for routing to appropriate handlers.

use std::collections::HashMap;
use tokio::sync::mpsc;

use crate::{Channel, Frame, SourceId};

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
    use crate::FrameFlags;
    use bytes::Bytes;

    fn make_frame(source: SourceId, channel: Channel) -> Frame {
        Frame {
            source,
            channel,
            flags: FrameFlags::default(),
            timestamp_us: 0,
            payload: Bytes::from_static(b"test"),
        }
    }

    #[tokio::test]
    async fn test_demuxer_source_subscription() {
        let mut demuxer = Demuxer::new();
        let source = SourceId::new([1, 2, 3, 4, 5, 6, 7, 8]);
        let mut rx = demuxer.subscribe_source(source);

        let frame = make_frame(source, Channel::Video);
        let count = demuxer.route(frame.clone()).await;
        assert_eq!(count, 1);

        let received = rx.recv().await.unwrap();
        assert_eq!(received.source, source);
    }

    #[tokio::test]
    async fn multi_source_routing() {
        let mut demuxer = Demuxer::new();
        let source1 = SourceId::new([1, 0, 0, 0, 0, 0, 0, 0]);
        let source2 = SourceId::new([2, 0, 0, 0, 0, 0, 0, 0]);

        let mut rx1 = demuxer.subscribe_source(source1);
        let mut rx2 = demuxer.subscribe_source(source2);

        // Route frame from source1 — only rx1 should receive
        let count = demuxer.route(make_frame(source1, Channel::Video)).await;
        assert_eq!(count, 1);

        // Route frame from source2 — only rx2 should receive
        let count = demuxer.route(make_frame(source2, Channel::Audio)).await;
        assert_eq!(count, 1);

        assert_eq!(rx1.recv().await.unwrap().source, source1);
        assert_eq!(rx2.recv().await.unwrap().source, source2);

        // No cross-contamination
        assert!(rx1.try_recv().is_err());
        assert!(rx2.try_recv().is_err());
    }

    #[tokio::test]
    async fn channel_subscriber_receives_all_sources() {
        let mut demuxer = Demuxer::new();
        let source1 = SourceId::new([1, 0, 0, 0, 0, 0, 0, 0]);
        let source2 = SourceId::new([2, 0, 0, 0, 0, 0, 0, 0]);

        let mut video_rx = demuxer.subscribe_channel(Channel::Video);

        // Both sources send video
        demuxer.route(make_frame(source1, Channel::Video)).await;
        demuxer.route(make_frame(source2, Channel::Video)).await;

        // Audio should not be received
        demuxer.route(make_frame(source1, Channel::Audio)).await;

        assert_eq!(video_rx.recv().await.unwrap().source, source1);
        assert_eq!(video_rx.recv().await.unwrap().source, source2);
        assert!(video_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn global_subscriber_receives_everything() {
        let mut demuxer = Demuxer::new();
        let source = SourceId::new([1, 0, 0, 0, 0, 0, 0, 0]);

        let mut all_rx = demuxer.subscribe_all();

        demuxer.route(make_frame(source, Channel::Video)).await;
        demuxer.route(make_frame(source, Channel::Audio)).await;
        demuxer.route(make_frame(source, Channel::Telemetry)).await;

        // All three should be received
        all_rx.recv().await.unwrap();
        all_rx.recv().await.unwrap();
        all_rx.recv().await.unwrap();
        assert!(all_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn overlapping_subscribers_all_receive() {
        let mut demuxer = Demuxer::new();
        let source = SourceId::new([1, 0, 0, 0, 0, 0, 0, 0]);

        let mut source_rx = demuxer.subscribe_source(source);
        let mut channel_rx = demuxer.subscribe_channel(Channel::Video);
        let mut global_rx = demuxer.subscribe_all();

        // One frame hits all three subscriber types
        let count = demuxer.route(make_frame(source, Channel::Video)).await;
        assert_eq!(count, 3);

        assert!(source_rx.recv().await.is_some());
        assert!(channel_rx.recv().await.is_some());
        assert!(global_rx.recv().await.is_some());
    }

    #[tokio::test]
    async fn unmatched_frame_returns_zero() {
        let mut demuxer = Demuxer::new();
        let subscribed = SourceId::new([1, 0, 0, 0, 0, 0, 0, 0]);
        let unsubscribed = SourceId::new([9, 9, 9, 9, 9, 9, 9, 9]);

        let _rx = demuxer.subscribe_source(subscribed);

        // Route frame from a source nobody subscribed to
        let count = demuxer
            .route(make_frame(unsubscribed, Channel::Video))
            .await;
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn cleanup_removes_dropped_receivers() {
        let mut demuxer = Demuxer::new();
        let source = SourceId::new([1, 0, 0, 0, 0, 0, 0, 0]);

        let rx = demuxer.subscribe_source(source);
        drop(rx); // Drop the receiver

        demuxer.cleanup();

        // Should not panic or error — subscriber was cleaned up
        let count = demuxer.route(make_frame(source, Channel::Video)).await;
        assert_eq!(count, 0);
    }
}
