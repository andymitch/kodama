//! Frame multiplexing and demultiplexing
//!
//! Handles combining multiple frame streams (video, audio, telemetry)
//! from multiple sources into a single transport stream, and splitting
//! them back apart at the receiving end.

pub mod command;
pub mod demuxer;
pub mod frame;
pub mod muxer;

pub use demuxer::Demuxer;
pub use muxer::{BackpressureSender, BufferStats, FrameBuffer};
