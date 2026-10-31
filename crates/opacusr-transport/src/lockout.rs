//! Host-side authentication gate: a sliding-window counter of failed
//! password proofs with an escalating lockout.
//!
//! Every time the host rejects an `AuthResponse` proof (wrong password),
//! the failure is recorded here. Once `max_attempts` failures fall inside
//! the sliding `window`, new handshakes are refused until the lockout
//! expires. Each consecutive lockout doubles the wait (capped at
//! `max_lockout`), so an online brute-force cannot cycle the gate cheaply.
//!
//! Design notes (see also docs/security.md §8):
//! * the gate is deliberately **global per accept path** (signaling and
//!   LAN each get one): attacker-controlled identifiers — peer IDs,
//!   display names — rotate freely, so per-peer counters would not bound
//!   the guessing rate; a global one does. The trade-off is that a flood
//!   of wrong attempts also delays legitimate viewers; the lockout is
//!   short and escalates, so a human simply retries a minute later.
//! * only *wrong proofs* count. Protocol errors and timeouts do not: they
//!   are trivial for an attacker to trigger, say nothing about the
//!   password, and counting them would hand anyone a free denial of
//!   service.
//! * a successful authentication clears the counter and the escalation
//!   streak — the password is known, the gate did its job.
//! * `max_attempts = 0` (or a zero lockout) disables the gate entirely.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Tunables for [`AuthGate`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthGateConfig {
    /// Failed proofs allowed inside the sliding window before lockout.
    /// `0` disables the gate.
    pub max_attempts: u32,
    /// Sliding window over which failures are counted.
    pub window: Duration,
    /// Lockout applied when the budget is spent (first offense).
    pub lockout: Duration,
    /// Upper bound for the escalated lockout.
    pub max_lockout: Duration,
}

impl Default for AuthGateConfig {
    fn default() -> Self {
        Self {
            max_attempts: 5,
            window: Duration::from_secs(300),
            lockout: Duration::from_secs(60),
            max_lockout: Duration::from_secs(15 * 60),
        }
    }
}

/// Outcome of a pre-handshake gate check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateDecision {
    /// Proceed with the handshake.
    Allow,
    /// Refuse now; the caller should reject the session and tell the peer
    /// to retry after this long.
    Locked(Duration),
}

#[derive(Debug)]
struct GateState {
    cfg: AuthGateConfig,
    /// Timestamps of recent failed proofs, ascending.
    failures: VecDeque<Instant>,
    /// While set, every handshake is refused until this instant.
    locked_until: Option<Instant>,
    /// Completed lockouts in a row (no successful auth in between);
    /// drives the escalation.
    lockout_streak: u32,
}

impl GateState {
    fn disabled(&self) -> bool {
        self.cfg.max_attempts == 0 || self.cfg.lockout.is_zero()
    }

    /// Forget failures that aged out of the window and lockouts that have
    /// been served.
    fn drop_expired(&mut self, now: Instant) {
        while let Some(&t) = self.failures.front() {
            if now.duration_since(t) > self.cfg.window {
                self.failures.pop_front();
            } else {
                break;
            }
        }
        if let Some(until) = self.locked_until {
            if now >= until {
                self.locked_until = None;
            }
        }
    }

    /// Lockout for the next offense: base × 2^(consecutive lockouts so
    /// far), capped at `max_lockout`.
    fn escalated_lockout(&self) -> Duration {
        let shift = self.lockout_streak.min(20);
        self.cfg
            .lockout
            .checked_mul(1u32 << shift)
            .unwrap_or(Duration::MAX)
            .min(self.cfg.max_lockout)
    }
}

/// Shared, thread-safe gate. One instance per accept path; `Arc` it into
/// every [`crate::auth::host_handshake`] call of that path.
pub struct AuthGate {
    state: Mutex<GateState>,
}

impl std::fmt::Debug for AuthGate {
    /// Deliberately coarse: configuration and *whether* the gate is
    /// engaged, never timestamps or budgets an attacker could use.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        f.debug_struct("AuthGate")
            .field("max_attempts", &st.cfg.max_attempts)
            .field("enabled", &!st.disabled())
            .field("failures_in_window", &st.failures.len())
            .field("locked", &st.locked_until.is_some())
            .field("lockout_streak", &st.lockout_streak)
            .finish()
    }
}

impl AuthGate {
    pub fn new(cfg: AuthGateConfig) -> Self {
        Self {
            state: Mutex::new(GateState {
                cfg,
                failures: VecDeque::new(),
                locked_until: None,
                lockout_streak: 0,
            }),
        }
    }

    /// `true` unless the gate was disabled via the config.
    pub fn is_enabled(&self) -> bool {
        !self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .disabled()
    }

    /// Consult before starting a handshake; the host must refuse locked
    /// sessions *before* sending any challenge material.
    pub fn check(&self) -> GateDecision {
        self.check_at(Instant::now())
    }

    /// Record a failed proof. Returns `Some(lockout)` when this failure
    /// spent the budget and engaged a lockout.
    pub fn record_failure(&self) -> Option<Duration> {
        self.record_failure_at(Instant::now())
    }

    /// Record a successful authentication: clears the failure counter and
    /// the escalation streak.
    pub fn record_success(&self) {
        self.record_success_at(Instant::now());
    }

    fn check_at(&self, now: Instant) -> GateDecision {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        st.drop_expired(now);
        if st.disabled() {
            return GateDecision::Allow;
        }
        match st.locked_until {
            Some(until) => {
                GateDecision::Locked(until.checked_duration_since(now).unwrap_or(Duration::ZERO))
            }
            None => GateDecision::Allow,
        }
    }

    fn record_failure_at(&self, now: Instant) -> Option<Duration> {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        st.drop_expired(now);
        if st.disabled() {
            return None;
        }
        st.failures.push_back(now);
        if st.failures.len() as u32 >= st.cfg.max_attempts {
            let lock = st.escalated_lockout();
            st.locked_until = Some(now + lock);
            st.lockout_streak = st.lockout_streak.saturating_add(1);
            // Budget spent: start fresh after the lockout. Older failures
            // would only make the next single mistake re-lock instantly.
            st.failures.clear();
            Some(lock)
        } else {
            None
        }
    }

    fn record_success_at(&self, now: Instant) {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        st.drop_expired(now);
        st.failures.clear();
        st.lockout_streak = 0;
        st.locked_until = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gate(max_attempts: u32, window: u64, lockout: u64, max_lockout: u64) -> AuthGate {
        AuthGate::new(AuthGateConfig {
            max_attempts,
            window: Duration::from_secs(window),
            lockout: Duration::from_secs(lockout),
            max_lockout: Duration::from_secs(max_lockout),
        })
    }

    #[test]
    fn locks_after_max_attempts() {
        let g = gate(3, 300, 60, 900);
        let t0 = Instant::now();
        assert_eq!(g.check_at(t0), GateDecision::Allow);
        assert_eq!(g.record_failure_at(t0 + secs(1)), None);
        assert_eq!(g.record_failure_at(t0 + secs(2)), None);
        assert_eq!(g.check_at(t0 + secs(3)), GateDecision::Allow);
        // Third failure spends the budget and locks for the base duration.
        assert_eq!(
            g.record_failure_at(t0 + secs(3)),
            Some(Duration::from_secs(60))
        );
        match g.check_at(t0 + secs(4)) {
            GateDecision::Locked(wait) => assert_eq!(wait, Duration::from_secs(59)),
            other => panic!("expected Locked, got {other:?}"),
        }
    }

    #[test]
    fn window_expiry_frees_the_budget() {
        let g = gate(2, 100, 50, 900);
        let t0 = Instant::now();
        assert_eq!(g.record_failure_at(t0 + secs(1)), None);
        assert_eq!(
            g.record_failure_at(t0 + secs(2)),
            Some(Duration::from_secs(50))
        );
        // Still locked near the end of the lockout…
        match g.check_at(t0 + secs(40)) {
            GateDecision::Locked(_) => {}
            other => panic!("expected Locked, got {other:?}"),
        }
        // …and free right after it expired at t0+52 (failures were
        // cleared when the lock engaged).
        match g.check_at(t0 + secs(51)) {
            GateDecision::Locked(_) => {}
            other => panic!("expected Locked, got {other:?}"),
        }
        assert_eq!(g.check_at(t0 + secs(52)), GateDecision::Allow);
        assert_eq!(g.record_failure_at(t0 + secs(52)), None);
    }

    #[test]
    fn lockout_escalates_and_caps() {
        let g = gate(1, 1000, 10, 25);
        let t0 = Instant::now();
        assert_eq!(
            g.record_failure_at(t0 + secs(1)),
            Some(Duration::from_secs(10))
        );
        assert_eq!(g.check_at(t0 + secs(11)), GateDecision::Allow);
        assert_eq!(
            g.record_failure_at(t0 + secs(12)),
            Some(Duration::from_secs(20))
        );
        assert_eq!(g.check_at(t0 + secs(33)), GateDecision::Allow);
        // 10 * 2^2 = 40 → capped at max_lockout = 25.
        assert_eq!(
            g.record_failure_at(t0 + secs(34)),
            Some(Duration::from_secs(25))
        );
    }

    #[test]
    fn success_resets_budget_and_streak() {
        let g = gate(2, 300, 60, 900);
        let t0 = Instant::now();
        assert_eq!(g.record_failure_at(t0 + secs(1)), None);
        g.record_success_at(t0 + secs(2));
        // Budget is full again after a success.
        assert_eq!(g.record_failure_at(t0 + secs(3)), None);
        assert_eq!(
            g.record_failure_at(t0 + secs(4)),
            Some(Duration::from_secs(60))
        );
    }

    #[test]
    fn disabled_gate_never_locks() {
        let g = gate(0, 300, 60, 900);
        let t0 = Instant::now();
        assert!(!g.is_enabled());
        for i in 0..10 {
            assert_eq!(g.record_failure_at(t0 + secs(i)), None);
            assert_eq!(g.check_at(t0 + secs(i)), GateDecision::Allow);
        }
    }

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }
}
