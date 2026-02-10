//! Frame multiplexing and demultiplexing
//!
//! Handles combining multiple frame streams (video, audio, telemetry)
//! from multiple sources into a single transport stream, and splitting
//! them back apart at the receiving end.

pub mod command;
pub mod frame;
pub mod muxer;
pub mod demuxer;

pub use muxer::{FrameBuffer, BackpressureSender, BufferStats};
pub use demuxer::Demuxer;
