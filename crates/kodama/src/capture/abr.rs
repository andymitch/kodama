//! Adaptive Bitrate (ABR) controller
//!
//! Adjusts H.264 encoding bitrate based on measured network throughput.
//! Uses quality tiers with hysteresis to avoid oscillation:
//! - Fast downgrade (3s) when throughput drops well below encoding bitrate
//! - Slow upgrade (15s) when throughput can sustain a higher tier
//! - Cooldown period (5s) after any change
//!
//! Thresholds are designed for VBR encoding where actual throughput may be
//! significantly below the target bitrate even when the network is healthy.
//! Downgrade triggers at 50% of bitrate (real congestion), upgrade requires
//! 80% of the next tier's bitrate (enough headroom).

use std::fmt;
use std::time::{Duration, Instant};
use tracing::{debug, info};

/// Quality tiers for 720p @ 30fps encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum QualityTier {
    Minimum,
    Low,
    Medium,
    High,
}

impl QualityTier {
    /// Encoding bitrate for this tier in bits per second.
    pub fn bitrate_bps(self) -> u32 {
        match self {
            QualityTier::High => 4_000_000,
            QualityTier::Medium => 2_000_000,
            QualityTier::Low => 1_000_000,
            QualityTier::Minimum => 500_000,
        }
    }

    /// Throughput below this means the network can't keep up (bitrate * 0.5).
    ///
    /// VBR encoding often produces well under the target bitrate, so we only
    /// downgrade when throughput is drastically below the target — indicating
    /// real network congestion rather than VBR undershoot.
    pub fn downgrade_threshold_bps(self) -> f64 {
        self.bitrate_bps() as f64 * 0.5
    }

    /// Throughput above this for the *next* tier means we can safely upgrade
    /// (next_tier.bitrate * 0.8).
    pub fn upgrade_threshold_bps(self) -> f64 {
        self.bitrate_bps() as f64 * 0.8
    }

    /// Next tier up, if any.
    pub fn higher(self) -> Option<QualityTier> {
        match self {
            QualityTier::Minimum => Some(QualityTier::Low),
            QualityTier::Low => Some(QualityTier::Medium),
            QualityTier::Medium => Some(QualityTier::High),
            QualityTier::High => None,
        }
    }

    /// Next tier down, if any.
    pub fn lower(self) -> Option<QualityTier> {
        match self {
            QualityTier::High => Some(QualityTier::Medium),
            QualityTier::Medium => Some(QualityTier::Low),
            QualityTier::Low => Some(QualityTier::Minimum),
            QualityTier::Minimum => None,
        }
    }
}

impl fmt::Display for QualityTier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            QualityTier::High => write!(f, "High"),
            QualityTier::Medium => write!(f, "Medium"),
            QualityTier::Low => write!(f, "Low"),
            QualityTier::Minimum => write!(f, "Minimum"),
        }
    }
}

/// ABR configuration parameters.
#[derive(Debug, Clone)]
pub struct AbrConfig {
    /// How long throughput must be below threshold before downgrading.
    pub downgrade_after: Duration,
    /// How long throughput must be above threshold before upgrading.
    pub upgrade_after: Duration,
    /// Minimum time between tier changes.
    pub cooldown: Duration,
}

impl Default for AbrConfig {
    fn default() -> Self {
        Self {
            downgrade_after: Duration::from_secs(3),
            upgrade_after: Duration::from_secs(15),
            cooldown: Duration::from_secs(5),
        }
    }
}

/// Decision returned by the ABR controller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AbrDecision {
    /// Stay at the current tier.
    Hold,
    /// Switch to a different tier (includes new bitrate in bps).
    ChangeTo(QualityTier),
}

/// ABR decision engine.
pub struct AbrController {
    current_tier: QualityTier,
    downgrade_pressure_since: Option<Instant>,
    upgrade_pressure_since: Option<Instant>,
    last_change: Instant,
    config: AbrConfig,
}

impl AbrController {
    pub fn new(config: AbrConfig) -> Self {
        Self {
            current_tier: QualityTier::High,
            downgrade_pressure_since: None,
            upgrade_pressure_since: None,
            last_change: Instant::now(),
            config,
        }
    }

    /// Create a new controller starting at the given tier.
    pub fn new_at(config: AbrConfig, tier: QualityTier) -> Self {
        Self {
            current_tier: tier,
            downgrade_pressure_since: None,
            upgrade_pressure_since: None,
            last_change: Instant::now(),
            config,
        }
    }

    pub fn current_tier(&self) -> QualityTier {
        self.current_tier
    }

    /// Evaluate throughput and decide whether to change tiers.
    ///
    /// Call this periodically (e.g. every 1 second) with the current
    /// measured throughput from `ThroughputTracker::bits_per_second()`.
    pub fn evaluate(&mut self, throughput_bps: f64) -> AbrDecision {
        let now = Instant::now();

        // Cooldown: no changes shortly after a tier switch
        if now.duration_since(self.last_change) < self.config.cooldown {
            debug!(
                "ABR: cooldown ({:.1}s remaining)",
                (self.config.cooldown - now.duration_since(self.last_change)).as_secs_f64()
            );
            return AbrDecision::Hold;
        }

        let downgrade_thresh = self.current_tier.downgrade_threshold_bps();

        // Check downgrade: throughput well below encoding bitrate (real congestion)
        if throughput_bps < downgrade_thresh {
            // Reset upgrade pressure
            self.upgrade_pressure_since = None;

            let pressure_start = *self.downgrade_pressure_since.get_or_insert(now);
            let pressure_duration = now.duration_since(pressure_start);

            debug!(
                "ABR: downgrade pressure {:.1}s (throughput {:.1} Mbps < threshold {:.1} Mbps)",
                pressure_duration.as_secs_f64(),
                throughput_bps / 1_000_000.0,
                downgrade_thresh / 1_000_000.0,
            );

            if pressure_duration >= self.config.downgrade_after {
                if let Some(lower) = self.current_tier.lower() {
                    info!(
                        "ABR: downgrade {} -> {} (throughput {:.2} Mbps)",
                        self.current_tier,
                        lower,
                        throughput_bps / 1_000_000.0,
                    );
                    self.current_tier = lower;
                    self.last_change = now;
                    self.downgrade_pressure_since = None;
                    return AbrDecision::ChangeTo(lower);
                }
            }
        } else {
            // No congestion, reset downgrade pressure
            self.downgrade_pressure_since = None;

            // Check upgrade: throughput sustains near current bitrate, meaning
            // the link has headroom. We use 90% of current tier as threshold
            // since we can't measure throughput above what we're sending.
            if let Some(higher) = self.current_tier.higher() {
                let upgrade_thresh = self.current_tier.bitrate_bps() as f64 * 0.9;

                if throughput_bps >= upgrade_thresh {
                    let pressure_start = *self.upgrade_pressure_since.get_or_insert(now);
                    let pressure_duration = now.duration_since(pressure_start);

                    debug!(
                        "ABR: upgrade pressure {:.1}s (throughput {:.1} Mbps >= threshold {:.1} Mbps)",
                        pressure_duration.as_secs_f64(),
                        throughput_bps / 1_000_000.0,
                        upgrade_thresh / 1_000_000.0,
                    );

                    if pressure_duration >= self.config.upgrade_after {
                        info!(
                            "ABR: upgrade {} -> {} (throughput {:.2} Mbps)",
                            self.current_tier,
                            higher,
                            throughput_bps / 1_000_000.0,
                        );
                        self.current_tier = higher;
                        self.last_change = now;
                        self.upgrade_pressure_since = None;
                        return AbrDecision::ChangeTo(higher);
                    }
                } else {
                    // Not enough for next tier, reset upgrade pressure
                    self.upgrade_pressure_since = None;
                }
            }
        }

        AbrDecision::Hold
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fast_config() -> AbrConfig {
        AbrConfig {
            downgrade_after: Duration::from_millis(100),
            upgrade_after: Duration::from_millis(200),
            cooldown: Duration::from_millis(50),
        }
    }

    #[test]
    fn starts_at_high() {
        let abr = AbrController::new(AbrConfig::default());
        assert_eq!(abr.current_tier(), QualityTier::High);
    }

    #[test]
    fn hold_when_throughput_above_downgrade_threshold() {
        let mut abr = AbrController::new(fast_config());
        std::thread::sleep(Duration::from_millis(60));
        // 3 Mbps is above High's downgrade threshold (2 Mbps) — no downgrade
        // even though it's below the 4 Mbps target (VBR undershoot is OK)
        let decision = abr.evaluate(3_000_000.0);
        assert_eq!(decision, AbrDecision::Hold);
        assert_eq!(abr.current_tier(), QualityTier::High);
    }

    #[test]
    fn downgrade_on_real_congestion() {
        let mut abr = AbrController::new(fast_config());
        std::thread::sleep(Duration::from_millis(60));

        // 1.5 Mbps < High downgrade threshold (2 Mbps) — real congestion
        abr.evaluate(1_500_000.0);
        std::thread::sleep(Duration::from_millis(120));
        let decision = abr.evaluate(1_500_000.0);
        assert_eq!(decision, AbrDecision::ChangeTo(QualityTier::Medium));
        assert_eq!(abr.current_tier(), QualityTier::Medium);
    }

    #[test]
    fn no_downgrade_during_cooldown() {
        let config = AbrConfig {
            downgrade_after: Duration::from_millis(10),
            upgrade_after: Duration::from_millis(200),
            cooldown: Duration::from_millis(500),
        };
        let mut abr = AbrController::new(config);

        // Immediately after creation, we're in cooldown
        let decision = abr.evaluate(100_000.0);
        assert_eq!(decision, AbrDecision::Hold);
    }

    #[test]
    fn upgrade_after_sustained_throughput() {
        let mut abr = AbrController::new(fast_config());
        // Force to Medium first
        abr.current_tier = QualityTier::Medium;
        abr.last_change = Instant::now() - Duration::from_secs(10);

        // 3.5 Mbps > Medium upgrade threshold (1.8 Mbps = 2M * 0.9)
        abr.evaluate(3_500_000.0);
        std::thread::sleep(Duration::from_millis(220));
        let decision = abr.evaluate(3_500_000.0);
        assert_eq!(decision, AbrDecision::ChangeTo(QualityTier::High));
    }

    #[test]
    fn no_upgrade_below_threshold() {
        let mut abr = AbrController::new(fast_config());
        abr.current_tier = QualityTier::Medium;
        abr.last_change = Instant::now() - Duration::from_secs(10);

        // 1.5 Mbps < Medium upgrade threshold (1.8 Mbps = 2M * 0.9) — not enough throughput
        abr.evaluate(1_500_000.0);
        std::thread::sleep(Duration::from_millis(220));
        let decision = abr.evaluate(1_500_000.0);
        assert_eq!(decision, AbrDecision::Hold);
        assert_eq!(abr.current_tier(), QualityTier::Medium);
    }

    #[test]
    fn pressure_resets_on_recovery() {
        let mut abr = AbrController::new(fast_config());
        abr.last_change = Instant::now() - Duration::from_secs(10);

        // Start downgrade pressure (below 2 Mbps threshold)
        abr.evaluate(1_000_000.0);
        std::thread::sleep(Duration::from_millis(50));

        // Throughput recovers — resets downgrade pressure
        abr.evaluate(3_000_000.0);

        // Wait past what would have been the downgrade threshold
        std::thread::sleep(Duration::from_millis(120));

        // Low again, but pressure timer restarted
        let decision = abr.evaluate(1_000_000.0);
        assert_eq!(decision, AbrDecision::Hold);
    }

    #[test]
    fn cannot_downgrade_below_minimum() {
        let mut abr = AbrController::new(fast_config());
        abr.current_tier = QualityTier::Minimum;
        abr.last_change = Instant::now() - Duration::from_secs(10);

        abr.evaluate(100_000.0);
        std::thread::sleep(Duration::from_millis(120));
        let decision = abr.evaluate(100_000.0);
        assert_eq!(decision, AbrDecision::Hold);
        assert_eq!(abr.current_tier(), QualityTier::Minimum);
    }

    #[test]
    fn cannot_upgrade_above_high() {
        let mut abr = AbrController::new(fast_config());
        abr.last_change = Instant::now() - Duration::from_secs(10);

        let decision = abr.evaluate(100_000_000.0);
        assert_eq!(decision, AbrDecision::Hold);
    }

    #[test]
    fn tier_bitrates() {
        assert_eq!(QualityTier::High.bitrate_bps(), 4_000_000);
        assert_eq!(QualityTier::Medium.bitrate_bps(), 2_000_000);
        assert_eq!(QualityTier::Low.bitrate_bps(), 1_000_000);
        assert_eq!(QualityTier::Minimum.bitrate_bps(), 500_000);
    }

    #[test]
    fn tier_downgrade_thresholds() {
        // 50% of bitrate
        assert_eq!(QualityTier::High.downgrade_threshold_bps(), 2_000_000.0);
        assert_eq!(QualityTier::Medium.downgrade_threshold_bps(), 1_000_000.0);
        assert_eq!(QualityTier::Low.downgrade_threshold_bps(), 500_000.0);
        assert_eq!(QualityTier::Minimum.downgrade_threshold_bps(), 250_000.0);
    }

    #[test]
    fn tier_upgrade_thresholds() {
        // 80% of bitrate
        assert_eq!(QualityTier::High.upgrade_threshold_bps(), 3_200_000.0);
        assert_eq!(QualityTier::Medium.upgrade_threshold_bps(), 1_600_000.0);
        assert_eq!(QualityTier::Low.upgrade_threshold_bps(), 800_000.0);
        assert_eq!(QualityTier::Minimum.upgrade_threshold_bps(), 400_000.0);
    }

    #[test]
    fn tier_navigation() {
        assert_eq!(QualityTier::High.higher(), None);
        assert_eq!(QualityTier::High.lower(), Some(QualityTier::Medium));
        assert_eq!(QualityTier::Minimum.lower(), None);
        assert_eq!(QualityTier::Minimum.higher(), Some(QualityTier::Low));
    }

    #[test]
    fn display_tiers() {
        assert_eq!(format!("{}", QualityTier::High), "High");
        assert_eq!(format!("{}", QualityTier::Medium), "Medium");
        assert_eq!(format!("{}", QualityTier::Low), "Low");
        assert_eq!(format!("{}", QualityTier::Minimum), "Minimum");
    }

    // ========== ABR + Throughput integration tests ==========

    use crate::capture::ThroughputTracker;
    use std::thread;

    /// Simulate the camera send loop: record bytes → measure throughput → feed to ABR.
    /// Verifies that sustained high throughput causes upgrades from Minimum → Low → Medium → High.
    #[test]
    fn throughput_driven_upgrade_ramp() {
        let config = AbrConfig {
            downgrade_after: Duration::from_millis(100),
            upgrade_after: Duration::from_millis(150),
            cooldown: Duration::from_millis(50),
        };
        let mut abr = AbrController::new_at(config, QualityTier::Minimum);
        abr.last_change = Instant::now() - Duration::from_secs(10);

        let mut tracker = ThroughputTracker::new(Duration::from_secs(1));

        // Simulate sending data at ~5 Mbps (625 KB/s) for a sustained period.
        // This is above the upgrade threshold for every tier up to High.
        // Minimum upgrade threshold = 500K * 0.9 = 450 Kbps
        // Low upgrade threshold = 1M * 0.9 = 900 Kbps
        // Medium upgrade threshold = 2M * 0.9 = 1.8 Mbps
        let bytes_per_tick = 62_500; // 62.5 KB per 100ms = 5 Mbps
        let mut upgrades = vec![];

        for _ in 0..30 {
            tracker.record(bytes_per_tick);
            thread::sleep(Duration::from_millis(100));

            let bps = tracker.bits_per_second();
            let decision = abr.evaluate(bps);
            if let AbrDecision::ChangeTo(tier) = decision {
                upgrades.push(tier);
                // Reset last_change to skip cooldown for test speed
                abr.last_change = Instant::now() - Duration::from_secs(10);
            }
        }

        assert!(
            upgrades.contains(&QualityTier::Low),
            "Should upgrade to Low, got: {upgrades:?}"
        );
        assert!(
            upgrades.contains(&QualityTier::Medium),
            "Should upgrade to Medium, got: {upgrades:?}"
        );
        assert!(
            upgrades.contains(&QualityTier::High),
            "Should upgrade to High, got: {upgrades:?}"
        );
        assert_eq!(abr.current_tier(), QualityTier::High);
    }

    /// Simulate congestion: start at High, feed low throughput → ABR downgrades.
    #[test]
    fn throughput_driven_downgrade_on_congestion() {
        let config = AbrConfig {
            downgrade_after: Duration::from_millis(150),
            upgrade_after: Duration::from_millis(500),
            cooldown: Duration::from_millis(50),
        };
        let mut abr = AbrController::new_at(config, QualityTier::High);
        abr.last_change = Instant::now() - Duration::from_secs(10);

        let mut tracker = ThroughputTracker::new(Duration::from_secs(1));

        // Simulate sending data at ~200 Kbps (25 KB/s) — well below every tier's
        // downgrade threshold. High downgrade = 2 Mbps, so 200 Kbps is way below.
        let bytes_per_tick = 2_500; // 2.5 KB per 100ms = 200 Kbps
        let mut downgrades = vec![];

        for _ in 0..30 {
            tracker.record(bytes_per_tick);
            thread::sleep(Duration::from_millis(100));

            let bps = tracker.bits_per_second();
            let decision = abr.evaluate(bps);
            if let AbrDecision::ChangeTo(tier) = decision {
                downgrades.push(tier);
                abr.last_change = Instant::now() - Duration::from_secs(10);
            }
        }

        assert!(
            downgrades.contains(&QualityTier::Medium),
            "Should downgrade to Medium, got: {downgrades:?}"
        );
        assert!(
            downgrades.contains(&QualityTier::Low),
            "Should downgrade to Low, got: {downgrades:?}"
        );
        assert!(
            downgrades.contains(&QualityTier::Minimum),
            "Should downgrade to Minimum, got: {downgrades:?}"
        );
        assert_eq!(abr.current_tier(), QualityTier::Minimum);
    }

    /// Simulate the real camera pattern: start at Minimum on reconnect,
    /// ramp throughput, then hit congestion and recover.
    #[test]
    fn throughput_driven_ramp_then_congestion_then_recovery() {
        let config = AbrConfig {
            downgrade_after: Duration::from_millis(150),
            upgrade_after: Duration::from_millis(150),
            cooldown: Duration::from_millis(50),
        };
        let mut abr = AbrController::new_at(config, QualityTier::Minimum);
        abr.last_change = Instant::now() - Duration::from_secs(10);

        let mut tracker = ThroughputTracker::new(Duration::from_millis(500));

        // Phase 1: High throughput → ramp up
        for _ in 0..20 {
            tracker.record(62_500); // ~5 Mbps
            thread::sleep(Duration::from_millis(100));
            let decision = abr.evaluate(tracker.bits_per_second());
            if let AbrDecision::ChangeTo(_) = decision {
                abr.last_change = Instant::now() - Duration::from_secs(10);
            }
        }
        let tier_after_ramp = abr.current_tier();
        assert!(
            tier_after_ramp >= QualityTier::Medium,
            "Should have ramped up, at {tier_after_ramp}"
        );

        // Phase 2: Congestion — throughput collapses
        // Use a fresh tracker so old high samples don't inflate the average
        let mut tracker2 = ThroughputTracker::new(Duration::from_millis(500));
        for _ in 0..20 {
            tracker2.record(1_000); // ~80 Kbps
            thread::sleep(Duration::from_millis(100));
            let decision = abr.evaluate(tracker2.bits_per_second());
            if let AbrDecision::ChangeTo(_) = decision {
                abr.last_change = Instant::now() - Duration::from_secs(10);
            }
        }
        assert_eq!(
            abr.current_tier(),
            QualityTier::Minimum,
            "Should have downgraded to Minimum under congestion"
        );

        // Phase 3: Recovery — throughput returns
        let mut tracker3 = ThroughputTracker::new(Duration::from_millis(500));
        for _ in 0..20 {
            tracker3.record(62_500); // ~5 Mbps
            thread::sleep(Duration::from_millis(100));
            let decision = abr.evaluate(tracker3.bits_per_second());
            if let AbrDecision::ChangeTo(_) = decision {
                abr.last_change = Instant::now() - Duration::from_secs(10);
            }
        }
        assert!(
            abr.current_tier() >= QualityTier::Medium,
            "Should have recovered, at {}",
            abr.current_tier()
        );
    }
}
