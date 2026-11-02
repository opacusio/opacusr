//! Adaptive stream quality driven by viewer link reports.
//!
//! The viewer measures RTT (`Ping`/`Pong`) and the frame rate it actually
//! receives, and reports both every few seconds via `LinkStats`. The host
//! maps that onto a *tier* (0 = full quality … 3 = very constrained link)
//! and applies the tier's `fps`/`quality`/`scale` parameters with
//! hysteresis:
//!
//! * degrading (link got worse) takes effect after **one** report;
//! * upgrading (link got better) requires **three** consecutive reports —
//!   flapping back up on a lucky measurement is worse than staying low
//!   for a few seconds;
//! * a manual `QualitySet` from the viewer UI disables the controller for
//!   the rest of the session.
//!
//! Tier boundaries are conservative: JPEG-over-data-channel is not
//! real-time video, so we prefer slightly blurry over a growing backlog.

/// RTT thresholds (ms) separating tiers: `<60` → 0, `<120` → 1,
/// `<250` → 2, `>=250` → 3.
pub const TIER_RTT_MS: [u32; 3] = [60, 120, 250];

/// Frame-arrival jitter thresholds (ms) that pinch the link even when
/// RTT looks fine: `<120` → no penalty, `<250` → at least tier 1,
/// `>=250` → at least tier 2. Jitter alone never reaches tier 3 — that
/// is reserved for sustained RTT/throughput problems.
pub const TIER_JITTER_MS: [u16; 2] = [120, 250];

/// Below this received-FPS a *good-RTT* link is still considered
/// throughput-starved and pushed down a tier.
const STARVED_FPS: u16 = 5;

/// Consecutive good reports required before stepping up a tier.
const UPGRADE_CONFIRMATIONS: u8 = 3;

/// One tier's streaming parameters (relative to the configured base).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamParams {
    pub fps: u8,
    pub jpeg_quality: u8,
    pub scale_percent: u16,
}

#[derive(Debug)]
pub struct AdaptiveQuality {
    enabled: bool,
    base: StreamParams,
    tier: usize,
    candidate: Option<usize>,
    confirmations: u8,
    disabled_reason: Option<&'static str>,
}

impl AdaptiveQuality {
    pub fn new(enabled: bool, base_fps: u8, base_quality: u8, base_scale: u16) -> Self {
        Self {
            enabled,
            base: StreamParams {
                fps: base_fps.max(1),
                jpeg_quality: base_quality.clamp(1, 100),
                scale_percent: base_scale.clamp(10, 100),
            },
            tier: 0,
            candidate: None,
            confirmations: 0,
            disabled_reason: None,
        }
    }

    /// Feed one viewer report. Returns the new parameters when the tier
    /// changed, `None` otherwise (steady state, disabled, or hysteresis
    /// still pending).
    pub fn on_link_stats(
        &mut self,
        rtt_ms: u32,
        recv_fps: u16,
        jitter_ms: u16,
    ) -> Option<StreamParams> {
        if !self.enabled || self.disabled_reason.is_some() {
            return None;
        }

        // The worse of the two signals decides the tier: sustained RTT
        // and arrival jitter each indicate congestion, whichever fires
        // first.
        let mut want = Self::tier_for_rtt(rtt_ms).max(Self::tier_for_jitter(jitter_ms));
        // Throughput starvation: RTT looks fine but the viewer receives
        // far fewer frames than we produce — treat the link as worse.
        if want == 0 && recv_fps > 0 && recv_fps < STARVED_FPS {
            want = 1;
        }

        if want == self.tier {
            self.candidate = None;
            self.confirmations = 0;
            return None;
        }

        let upgrade = want < self.tier;
        if self.candidate == Some(want) {
            self.confirmations += 1;
        } else {
            self.candidate = Some(want);
            self.confirmations = 1;
        }

        let needed = if upgrade {
            UPGRADE_CONFIRMATIONS
        } else {
            1 // degrade fast
        };
        if self.confirmations >= needed {
            self.tier = want;
            self.candidate = None;
            self.confirmations = 0;
            return Some(self.params());
        }
        None
    }

    /// The viewer set quality manually — stop adapting.
    pub fn manual_override(&mut self) {
        if self.enabled {
            self.disabled_reason = Some("manual quality setting");
        }
    }

    /// Parameters for the current tier (also the initial state).
    pub fn params(&self) -> StreamParams {
        self.tier_params(self.tier)
    }

    pub fn tier(&self) -> usize {
        self.tier
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled && self.disabled_reason.is_none()
    }

    fn tier_params(&self, tier: usize) -> StreamParams {
        let b = self.base;
        match tier {
            0 => b,
            1 => StreamParams {
                fps: b.fps.min(20),
                jpeg_quality: b.jpeg_quality.saturating_sub(15).max(40),
                scale_percent: b.scale_percent,
            },
            2 => StreamParams {
                fps: b.fps.min(12),
                jpeg_quality: b.jpeg_quality.saturating_sub(30).max(30),
                scale_percent: b.scale_percent.min(75),
            },
            _ => StreamParams {
                fps: b.fps.min(8),
                jpeg_quality: b.jpeg_quality.saturating_sub(45).max(20),
                scale_percent: b.scale_percent.min(60),
            },
        }
    }

    fn tier_for_rtt(rtt_ms: u32) -> usize {
        let mut tier = 0;
        for &t in &TIER_RTT_MS {
            if rtt_ms >= t {
                tier += 1;
            }
        }
        tier
    }

    fn tier_for_jitter(jitter_ms: u16) -> usize {
        let mut tier = 0;
        for &t in &TIER_JITTER_MS {
            if jitter_ms >= t {
                tier += 1;
            }
        }
        tier
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn controller() -> AdaptiveQuality {
        AdaptiveQuality::new(true, 30, 80, 100)
    }

    #[test]
    fn starts_at_base_params() {
        let c = controller();
        assert_eq!(
            c.params(),
            StreamParams {
                fps: 30,
                jpeg_quality: 80,
                scale_percent: 100
            }
        );
        assert_eq!(c.tier(), 0);
    }

    #[test]
    fn degrades_immediately_on_bad_rtt() {
        let mut c = controller();
        let p = c.on_link_stats(300, 15, 0);
        assert_eq!(p.map(|p| p.fps), Some(8));
        assert_eq!(c.tier(), 3);
    }

    #[test]
    fn upgrades_only_after_three_reports() {
        let mut c = controller();
        c.on_link_stats(300, 10, 0);
        // One good report must NOT upgrade.
        assert!(c.on_link_stats(20, 15, 0).is_none());
        assert!(c.on_link_stats(20, 15, 0).is_none());
        let p = c.on_link_stats(20, 15, 0);
        assert_eq!(c.tier(), 0);
        assert_eq!(p.map(|p| p.fps), Some(30));
    }

    #[test]
    fn starved_link_downgrades_despite_good_rtt() {
        let mut c = controller();
        let p = c.on_link_stats(10, 2, 0);
        assert_eq!(p.map(|p| p.fps), Some(20)); // tier 1
        assert_eq!(c.tier(), 1);
    }

    #[test]
    fn jittery_link_downgrades_despite_good_rtt() {
        let mut c = controller();
        assert!(c.on_link_stats(10, 15, 0).is_none()); // tier 0, steady
                                                       // 260 ms of arrival jitter → at least tier 2, immediately.
        let p = c.on_link_stats(10, 15, 260);
        assert_eq!(p.map(|p| p.fps), Some(12));
        assert_eq!(c.tier(), 2);
    }

    #[test]
    fn jitter_below_threshold_is_ignored() {
        let mut c = controller();
        assert!(c.on_link_stats(10, 15, 119).is_none());
        assert_eq!(c.tier(), 0);
    }

    #[test]
    fn jitter_alone_never_reaches_tier_three() {
        let mut c = controller();
        let p = c.on_link_stats(10, 15, 600);
        assert_eq!(c.tier(), 2);
        assert_eq!(p.map(|p| p.fps), Some(12));
    }

    #[test]
    fn rtt_and_jitter_take_the_worse_tier() {
        let mut c = controller();
        // RTT alone → tier 1 (60 ≤ rtt < 120); jitter adds nothing.
        c.on_link_stats(70, 15, 0);
        assert_eq!(c.tier(), 1);
        // Jitter alone would want tier 2; the max wins.
        let p = c.on_link_stats(70, 15, 300);
        assert_eq!(c.tier(), 2);
        assert!(p.is_some());
    }

    #[test]
    fn mixed_readings_reset_hysteresis() {
        let mut c = controller();
        assert!(c.on_link_stats(300, 10, 0).is_some()); // degrade → tier 3
        assert!(c.on_link_stats(20, 15, 0).is_none()); // good #1 (upgrade pending)
                                                       // A bad reading resets the upgrade counter (and, being a no-op on
                                                       // tier 3, emits nothing).
        assert!(c.on_link_stats(400, 10, 0).is_none());
        // Upgrade counter was reset: needs 3 fresh good reports.
        assert!(c.on_link_stats(20, 15, 0).is_none());
        assert!(c.on_link_stats(20, 15, 0).is_none());
        let p = c.on_link_stats(20, 15, 0);
        assert!(p.is_some());
        assert_eq!(c.tier(), 0);
    }

    #[test]
    fn manual_override_disables_adaptation() {
        let mut c = controller();
        c.manual_override();
        assert!(!c.is_enabled());
        assert!(c.on_link_stats(300, 10, 0).is_none());
        assert_eq!(c.tier(), 0);
    }

    #[test]
    fn disabled_controller_is_inert() {
        let mut c = AdaptiveQuality::new(false, 30, 80, 100);
        assert!(c.on_link_stats(300, 10, 0).is_none());
    }

    #[test]
    fn tier_bounds_respect_base() {
        // A base below the tier cap must not be raised.
        let mut c = AdaptiveQuality::new(true, 10, 25, 50);
        c.on_link_stats(300, 10, 0);
        assert_eq!(
            c.params(),
            StreamParams {
                fps: 8,
                jpeg_quality: 20, // 25-45 saturated → floored at 20
                scale_percent: 50,
            }
        );
    }
}
