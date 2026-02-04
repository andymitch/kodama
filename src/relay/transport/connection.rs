//! Persistent frame streams for efficient video streaming
//!
//! Instead of opening a new QUIC stream per frame (overhead at 30fps+),
//! this module provides persistent bidirectional streams that multiplex
//! frames with length-prefix framing.

use anyhow::Result;
use iroh::endpoint::{RecvStream, SendStream};
use tokio::sync::Mutex;

use crate::core::Frame;
use crate::relay::mux::frame::{read_frame, write_frame};

/// A sender for streaming frames over a persistent QUIC stream
pub struct FrameSender {
    send: Mutex<SendStream>,
}

impl FrameSender {
    /// Create a new frame sender from a QUIC send stream
    pub fn new(send: SendStream) -> Self {
        Self {
            send: Mutex::new(send),
        }
    }

    /// Send a frame over the persistent stream
    pub async fn send(&self, frame: &Frame) -> Result<()> {
        let mut send = self.send.lock().await;
        write_frame(&mut *send, frame).await
    }

    /// Finish the stream (no more frames will be sent)
    pub async fn finish(self) -> Result<()> {
        let mut send = self.send.into_inner();
        send.finish()?;
        Ok(())
    }
}

/// A receiver for streaming frames from a persistent QUIC stream
pub struct FrameReceiver {
    recv: Mutex<RecvStream>,
}

impl FrameReceiver {
    /// Create a new frame receiver from a QUIC receive stream
    pub fn new(recv: RecvStream) -> Self {
        Self {
            recv: Mutex::new(recv),
        }
    }

    /// Receive the next frame from the stream
    ///
    /// Returns None if the stream is finished.
    pub async fn recv(&self) -> Result<Option<Frame>> {
        let mut recv = self.recv.lock().await;
        match read_frame(&mut *recv).await {
            Ok(frame) => Ok(Some(frame)),
            Err(e) => {
                // Check if this is a clean stream close
                let err_str = e.to_string();
                if err_str.contains("end of stream") || err_str.contains("eof") {
                    Ok(None)
                } else {
                    Err(e)
                }
            }
        }
    }
}

/// A bidirectional frame stream for request/response patterns
pub struct FrameStream {
    pub sender: FrameSender,
    pub receiver: FrameReceiver,
}

impl FrameStream {
    /// Create a new bidirectional frame stream
    pub fn new(send: SendStream, recv: RecvStream) -> Self {
        Self {
            sender: FrameSender::new(send),
            receiver: FrameReceiver::new(recv),
        }
    }

    /// Send a frame
    pub async fn send(&self, frame: &Frame) -> Result<()> {
        self.sender.send(frame).await
    }

    /// Receive a frame
    pub async fn recv(&self) -> Result<Option<Frame>> {
        self.receiver.recv().await
    }
}
