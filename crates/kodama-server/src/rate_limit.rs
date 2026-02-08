//! Per-connection rate limiting for camera frame streams
//!
//! Uses lock-free atomics on the hot path (matching `AtomicRouterStats` pattern).
//! Each camera connection gets its own `ConnectionRateLimiter` instance — no
//! shared state, no contention.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use kodama_core::Channel;

/// Per-channel FPS limits, memory budget, and abuse threshold.
#[derive(Debug, Clone)]
pub struct RateLimitConfig {
    /// Max frames per second for video channel
    pub video_fps: u32,
    /// Max frames per second for audio channel
    pub audio_fps: u32,
    /// Max frames per second for telemetry channel
    pub telemetry_fps: u32,
    /// Max frames per second for unknown channels
    pub unknown_fps: u32,
    /// Max outstanding bytes across all channels before dropping
    pub memory_budget: u64,
    /// Seconds of sustained abuse before recommending disconnect
    pub abuse_threshold_secs: u64,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            video_fps: 120,
            audio_fps: 100,
            telemetry_fps: 10,
            unknown_fps: 10,
            memory_budget: 32 * 1024 * 1024, // 32 MB
            abuse_threshold_secs: 10,
        }
    }
}

impl RateLimitConfig {
    fn fps_for_channel(&self, channel: &Channel) -> u32 {
        match channel {
            Channel::Video => self.video_fps,
            Channel::Audio => self.audio_fps,
            Channel::Telemetry => self.telemetry_fps,
            Channel::Unknown(_) => self.unknown_fps,
        }
    }
}

/// Sliding-window frame counter for a single channel.
///
/// Each 1-second window tracks frame count. When the window expires,
/// the counter resets via CAS.
struct ChannelWindow {
    /// Frame count in the current window
    count: AtomicU64,
    /// Window start time in milliseconds since `epoch`
    window_start_ms: AtomicU64,
}

impl ChannelWindow {
    fn new() -> Self {
        Self {
            count: AtomicU64::new(0),
            window_start_ms: AtomicU64::new(0),
        }
    }

    /// Record a frame arrival. Returns the frame count in the current window
    /// (after increment). `now_ms` is milliseconds since the limiter's epoch.
    fn record(&self, now_ms: u64) -> u64 {
        let window = self.window_start_ms.load(Ordering::Relaxed);
        if now_ms.saturating_sub(window) >= 1000 {
            // Window expired — try to reset via CAS
            if self
                .window_start_ms
                .compare_exchange(window, now_ms, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                self.count.store(1, Ordering::Relaxed);
                return 1;
            }
            // Another thread won the CAS — fall through and just increment
        }
        self.count.fetch_add(1, Ordering::Relaxed) + 1
    }
}

/// Result of a rate-limit check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateCheck {
    /// Frame is within limits
    Allowed,
    /// Frame was dropped (rate or memory exceeded)
    Dropped,
}

/// Per-camera-connection rate limiter.
///
/// Created locally in each `handle_camera` loop — no shared state needed.
pub struct ConnectionRateLimiter {
    config: RateLimitConfig,
    epoch: Instant,
    /// Per-channel windows: [video, audio, telemetry, unknown]
    windows: [ChannelWindow; 4],
    /// Outstanding bytes not yet confirmed sent
    bytes_outstanding: AtomicU64,
    /// Total frames dropped across this connection
    total_dropped: AtomicU64,
    /// Consecutive seconds with drops (for abuse detection)
    abuse_seconds: AtomicU64,
    /// Last second in which we observed a drop (ms since epoch / 1000)
    last_drop_second: AtomicU64,
    /// Last second in which we logged a drop warning
    last_log_second: AtomicU64,
}

impl ConnectionRateLimiter {
    /// Create a new rate limiter with the given config.
    pub fn new(config: RateLimitConfig) -> Self {
        Self {
            config,
            epoch: Instant::now(),
            windows: [
                ChannelWindow::new(),
                ChannelWindow::new(),
                ChannelWindow::new(),
                ChannelWindow::new(),
            ],
            bytes_outstanding: AtomicU64::new(0),
            total_dropped: AtomicU64::new(0),
            abuse_seconds: AtomicU64::new(0),
            last_drop_second: AtomicU64::new(u64::MAX),
            last_log_second: AtomicU64::new(u64::MAX),
        }
    }

    #[cfg(test)]
    fn new_with_epoch(config: RateLimitConfig, epoch: Instant) -> Self {
        Self {
            config,
            epoch,
            windows: [
                ChannelWindow::new(),
                ChannelWindow::new(),
                ChannelWindow::new(),
                ChannelWindow::new(),
            ],
            bytes_outstanding: AtomicU64::new(0),
            total_dropped: AtomicU64::new(0),
            abuse_seconds: AtomicU64::new(0),
            last_drop_second: AtomicU64::new(u64::MAX),
            last_log_second: AtomicU64::new(u64::MAX),
        }
    }

    fn now_ms(&self) -> u64 {
        self.epoch.elapsed().as_millis() as u64
    }

    fn window_index(channel: &Channel) -> usize {
        match channel {
            Channel::Video => 0,
            Channel::Audio => 1,
            Channel::Telemetry => 2,
            Channel::Unknown(_) => 3,
        }
    }

    /// Check whether a frame should be allowed or dropped.
    ///
    /// This is the hot path — all operations are lock-free atomics.
    pub fn check_frame(&self, channel: &Channel, payload_len: usize) -> RateCheck {
        let now_ms = self.now_ms();

        // Check memory budget first
        let outstanding = self.bytes_outstanding.load(Ordering::Relaxed);
        if outstanding.saturating_add(payload_len as u64) > self.config.memory_budget {
            self.record_drop(now_ms);
            return RateCheck::Dropped;
        }

        // Check per-channel FPS
        let limit = self.config.fps_for_channel(channel);
        let idx = Self::window_index(channel);
        let count = self.windows[idx].record(now_ms);

        if count > limit as u64 {
            self.record_drop(now_ms);
            return RateCheck::Dropped;
        }

        // Track outstanding bytes
        self.bytes_outstanding
            .fetch_add(payload_len as u64, Ordering::Relaxed);

        RateCheck::Allowed
    }

    /// Called after a frame has been broadcast (or otherwise consumed),
    /// releasing bytes from the outstanding budget.
    pub fn frame_sent(&self, payload_len: usize) {
        // Saturating subtract via fetch_sub — if somehow underflows,
        // the u64 wrap is caught by the budget check's saturating_add.
        let prev = self
            .bytes_outstanding
            .fetch_sub(payload_len as u64, Ordering::Relaxed);
        // Guard against underflow (shouldn't happen in practice)
        if (prev as i64) < (payload_len as i64) {
            self.bytes_outstanding.store(0, Ordering::Relaxed);
        }
    }

    fn record_drop(&self, now_ms: u64) {
        self.total_dropped.fetch_add(1, Ordering::Relaxed);
        let current_second = now_ms / 1000;
        self.last_drop_second
            .store(current_second, Ordering::Relaxed);
    }

    /// Check for sustained abuse. Call roughly once per second.
    ///
    /// Returns `true` if the connection should be disconnected due to
    /// sustained rate-limit violations.
    pub fn tick_abuse_check(&self) -> bool {
        let now_ms = self.now_ms();
        let current_second = now_ms / 1000;
        let last_drop = self.last_drop_second.load(Ordering::Relaxed);

        if last_drop == current_second || last_drop == current_second.saturating_sub(1) {
            // Drops happened recently — increment abuse counter
            let secs = self.abuse_seconds.fetch_add(1, Ordering::Relaxed) + 1;
            secs >= self.config.abuse_threshold_secs
        } else {
            // No recent drops — reset abuse counter
            self.abuse_seconds.store(0, Ordering::Relaxed);
            false
        }
    }

    /// Returns true if a drop warning should be logged now (at most 1/sec).
    pub fn should_log_drop(&self) -> bool {
        let now_ms = self.now_ms();
        let current_second = now_ms / 1000;
        let last = self.last_log_second.load(Ordering::Relaxed);
        if current_second != last {
            self.last_log_second
                .compare_exchange(last, current_second, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
        } else {
            false
        }
    }

    /// Total frames dropped on this connection.
    pub fn frames_dropped(&self) -> u64 {
        self.total_dropped.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn make_config() -> RateLimitConfig {
        RateLimitConfig {
            video_fps: 5,
            audio_fps: 5,
            telemetry_fps: 2,
            unknown_fps: 2,
            memory_budget: 1024,
            abuse_threshold_secs: 3,
        }
    }

    #[test]
    fn frames_within_limit_are_allowed() {
        let limiter = ConnectionRateLimiter::new(make_config());
        for _ in 0..5 {
            assert_eq!(
                limiter.check_frame(&Channel::Video, 10),
                RateCheck::Allowed
            );
        }
        assert_eq!(limiter.frames_dropped(), 0);
    }

    #[test]
    fn frames_over_limit_are_dropped() {
        let limiter = ConnectionRateLimiter::new(make_config());
        // Fill the window (5 allowed)
        for _ in 0..5 {
            assert_eq!(
                limiter.check_frame(&Channel::Video, 10),
                RateCheck::Allowed
            );
        }
        // 6th should be dropped
        assert_eq!(
            limiter.check_frame(&Channel::Video, 10),
            RateCheck::Dropped
        );
        assert_eq!(limiter.frames_dropped(), 1);
    }

    #[test]
    fn window_resets_after_one_second() {
        let epoch = Instant::now();
        let limiter = ConnectionRateLimiter::new_with_epoch(make_config(), epoch);

        // Fill the window
        for _ in 0..5 {
            assert_eq!(
                limiter.check_frame(&Channel::Video, 10),
                RateCheck::Allowed
            );
        }
        assert_eq!(
            limiter.check_frame(&Channel::Video, 10),
            RateCheck::Dropped
        );

        // Simulate time passing by creating a new limiter with an older epoch
        let old_epoch = Instant::now() - Duration::from_millis(1100);
        let limiter2 = ConnectionRateLimiter::new_with_epoch(make_config(), old_epoch);
        // Fill first window
        for _ in 0..5 {
            assert_eq!(
                limiter2.check_frame(&Channel::Video, 10),
                RateCheck::Allowed
            );
        }
        assert_eq!(
            limiter2.check_frame(&Channel::Video, 10),
            RateCheck::Dropped
        );

        // Wait for real time to advance past the window
        std::thread::sleep(Duration::from_millis(1050));
        // Now the window should have reset
        assert_eq!(
            limiter2.check_frame(&Channel::Video, 10),
            RateCheck::Allowed
        );
    }

    #[test]
    fn memory_budget_enforcement() {
        let limiter = ConnectionRateLimiter::new(make_config());
        // Budget is 1024 bytes. Send a frame that nearly fills it.
        assert_eq!(
            limiter.check_frame(&Channel::Video, 1000),
            RateCheck::Allowed
        );
        // Another 100 bytes would exceed budget
        assert_eq!(
            limiter.check_frame(&Channel::Video, 100),
            RateCheck::Dropped
        );
        // Release the first frame's bytes
        limiter.frame_sent(1000);
        // Now 100 bytes should fit
        assert_eq!(
            limiter.check_frame(&Channel::Video, 100),
            RateCheck::Allowed
        );
    }

    #[test]
    fn sustained_abuse_triggers_disconnect() {
        let limiter = ConnectionRateLimiter::new(make_config());
        // Simulate drops happening every tick
        // Each tick increments abuse_seconds. Threshold is 3.
        for _ in 0..5 {
            limiter.check_frame(&Channel::Video, 10);
        }
        // Cause a drop
        limiter.check_frame(&Channel::Video, 10);

        assert!(!limiter.tick_abuse_check()); // 1 second
        assert!(!limiter.tick_abuse_check()); // 2 seconds
        assert!(limiter.tick_abuse_check()); // 3 seconds → disconnect
    }

    #[test]
    fn channels_are_independent() {
        let limiter = ConnectionRateLimiter::new(make_config());
        // Fill video (limit=5)
        for _ in 0..5 {
            assert_eq!(
                limiter.check_frame(&Channel::Video, 10),
                RateCheck::Allowed
            );
        }
        assert_eq!(
            limiter.check_frame(&Channel::Video, 10),
            RateCheck::Dropped
        );

        // Audio should still work (separate window, limit=5)
        for _ in 0..5 {
            assert_eq!(
                limiter.check_frame(&Channel::Audio, 10),
                RateCheck::Allowed
            );
        }

        // Telemetry should also work (limit=2)
        assert_eq!(
            limiter.check_frame(&Channel::Telemetry, 10),
            RateCheck::Allowed
        );
    }

    #[test]
    fn abuse_counter_resets_on_good_behavior() {
        let limiter = ConnectionRateLimiter::new(make_config());
        // Cause drops to build up abuse
        for _ in 0..5 {
            limiter.check_frame(&Channel::Video, 10);
        }
        limiter.check_frame(&Channel::Video, 10); // dropped

        assert!(!limiter.tick_abuse_check()); // abuse_seconds = 1
        assert!(!limiter.tick_abuse_check()); // abuse_seconds = 2

        // Simulate "no recent drops" by setting last_drop_second far in the past.
        // current_second is ~0 (test runs fast), so use u64::MAX which won't
        // match current_second or current_second-1.
        limiter.last_drop_second.store(u64::MAX, Ordering::Relaxed);

        // tick should reset abuse counter
        assert!(!limiter.tick_abuse_check()); // resets to 0
        assert!(!limiter.tick_abuse_check()); // still 0 (no recent drops)
    }
}
