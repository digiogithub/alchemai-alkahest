//! Restart policy hook for the scheduler.
//!
//! ALK-SP-0004.R6 reloads a crashed model on the next request. A model that crashes on every
//! load would then crash-loop with every request, so the scheduler asks a [`RestartTracker`]
//! before reloading: after too many crashes in a window it refuses and reports the last exit
//! instead of spawning again.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Limits on reloading a crashing engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RestartPolicy {
    /// Crashes tolerated inside `window` before reloads are refused.
    pub max_crashes: u32,
    /// Sliding window over which crashes count.
    pub window: Duration,
    /// Delay before the first reload; doubles with every crash in the window.
    pub base_backoff: Duration,
    /// Upper bound of the delay.
    pub max_backoff: Duration,
}

impl Default for RestartPolicy {
    /// Five crashes in five minutes, backoff from 1 s up to 30 s.
    fn default() -> Self {
        Self {
            max_crashes: 5,
            window: Duration::from_secs(300),
            base_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(30),
        }
    }
}

/// What the scheduler should do after a crash.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestartDecision {
    /// Reload on the next request, not before `after` has passed since the crash.
    Restart {
        /// Minimum delay before the reload.
        after: Duration,
    },
    /// Too many crashes: fail requests with the last exit until the window clears.
    GiveUp {
        /// Crashes counted inside the window.
        crashes: u32,
    },
}

/// Crash history of one model.
#[derive(Debug, Clone)]
pub struct RestartTracker {
    policy: RestartPolicy,
    crashes: VecDeque<Instant>,
}

impl RestartTracker {
    /// A tracker with no crashes.
    pub fn new(policy: RestartPolicy) -> Self {
        Self {
            policy,
            crashes: VecDeque::new(),
        }
    }

    /// Records a crash at `now` and decides.
    pub fn record_crash(&mut self, now: Instant) -> RestartDecision {
        self.crashes.push_back(now);
        self.decide(now)
    }

    /// The decision at `now` without recording anything (crashes age out of the window).
    pub fn decide(&mut self, now: Instant) -> RestartDecision {
        while let Some(first) = self.crashes.front() {
            if now.saturating_duration_since(*first) > self.policy.window {
                self.crashes.pop_front();
            } else {
                break;
            }
        }
        let n = u32::try_from(self.crashes.len()).unwrap_or(u32::MAX);
        if n > self.policy.max_crashes {
            return RestartDecision::GiveUp { crashes: n };
        }
        if n == 0 {
            return RestartDecision::Restart {
                after: Duration::ZERO,
            };
        }
        let factor = 1u32.checked_shl(n - 1).unwrap_or(u32::MAX);
        let after = self
            .policy
            .base_backoff
            .saturating_mul(factor)
            .min(self.policy.max_backoff);
        RestartDecision::Restart { after }
    }

    /// Forgets the history (after a clean stop or a long healthy run).
    pub fn reset(&mut self) {
        self.crashes.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_then_gives_up_then_recovers() {
        let policy = RestartPolicy {
            max_crashes: 2,
            window: Duration::from_secs(60),
            base_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(10),
        };
        let mut t = RestartTracker::new(policy);
        let t0 = Instant::now();
        assert_eq!(
            t.record_crash(t0),
            RestartDecision::Restart {
                after: Duration::from_secs(1)
            }
        );
        assert_eq!(
            t.record_crash(t0 + Duration::from_secs(1)),
            RestartDecision::Restart {
                after: Duration::from_secs(2)
            }
        );
        assert_eq!(
            t.record_crash(t0 + Duration::from_secs(2)),
            RestartDecision::GiveUp { crashes: 3 }
        );
        assert_eq!(
            t.decide(t0 + Duration::from_secs(120)),
            RestartDecision::Restart {
                after: Duration::ZERO
            }
        );
    }
}
