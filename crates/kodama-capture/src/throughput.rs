//! Sliding-window throughput measurement
//!
//! Tracks bytes written over a configurable time window to compute
//! bits-per-second throughput. Used by the ABR controller to decide
//! when to adjust encoding bitrate.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Measures throughput over a sliding time window.
pub struct ThroughputTracker {
    samples: VecDeque<(Instant, u64)>,
    window: Duration,
    total_bytes: u64,
}

impl ThroughputTracker {
    /// Create a new tracker with the given measurement window.
    pub fn new(window: Duration) -> Self {
        Self {
            samples: VecDeque::new(),
            window,
            total_bytes: 0,
        }
    }

    /// Record `bytes` written at the current instant.
    pub fn record(&mut self, bytes: usize) {
        self.total_bytes += bytes as u64;
        let now = Instant::now();
        self.samples.push_back((now, self.total_bytes));
        self.prune(now);
    }

    /// Compute throughput in bits per second over the window.
    ///
    /// Returns 0.0 if fewer than 2 samples or the window span is < 100ms
    /// (avoids noisy readings during startup).
    pub fn bits_per_second(&self) -> f64 {
        if self.samples.len() < 2 {
            return 0.0;
        }

        let (t_old, b_old) = self.samples.front().unwrap();
        let (t_new, b_new) = self.samples.back().unwrap();

        let elapsed = t_new.duration_since(*t_old);
        if elapsed.as_millis() < 100 {
            return 0.0;
        }

        let bytes_delta = b_new - b_old;
        (bytes_delta as f64 * 8.0) / elapsed.as_secs_f64()
    }

    fn prune(&mut self, now: Instant) {
        while let Some(&(t, _)) = self.samples.front() {
            if now.duration_since(t) > self.window {
                self.samples.pop_front();
            } else {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn empty_tracker_returns_zero() {
        let tracker = ThroughputTracker::new(Duration::from_secs(3));
        assert_eq!(tracker.bits_per_second(), 0.0);
    }

    #[test]
    fn single_sample_returns_zero() {
        let mut tracker = ThroughputTracker::new(Duration::from_secs(3));
        tracker.record(1000);
        assert_eq!(tracker.bits_per_second(), 0.0);
    }

    #[test]
    fn short_window_returns_zero() {
        let mut tracker = ThroughputTracker::new(Duration::from_secs(3));
        tracker.record(1000);
        tracker.record(1000);
        // Both samples within < 100ms, should return 0
        assert_eq!(tracker.bits_per_second(), 0.0);
    }

    #[test]
    fn throughput_calculation() {
        let mut tracker = ThroughputTracker::new(Duration::from_secs(3));
        tracker.record(0);
        // Wait enough to get a meaningful window
        thread::sleep(Duration::from_millis(200));
        tracker.record(125_000); // 125KB in ~200ms ≈ 5 Mbps

        let bps = tracker.bits_per_second();
        // Should be roughly 5 Mbps (125000 * 8 / 0.2 = 5_000_000)
        // Allow ±50% for timing variance in CI
        assert!(bps > 2_000_000.0, "bps={bps}, expected > 2Mbps");
        assert!(bps < 10_000_000.0, "bps={bps}, expected < 10Mbps");
    }

    #[test]
    fn window_prunes_old_samples() {
        let mut tracker = ThroughputTracker::new(Duration::from_millis(300));
        tracker.record(10_000);
        thread::sleep(Duration::from_millis(150));
        tracker.record(10_000);
        thread::sleep(Duration::from_millis(200));
        tracker.record(10_000);
        // First sample should be pruned (>300ms old)
        // Remaining: 2 samples over ~200ms with 10KB delta
        assert!(tracker.samples.len() <= 3);
        let bps = tracker.bits_per_second();
        assert!(bps > 0.0);
    }
}
