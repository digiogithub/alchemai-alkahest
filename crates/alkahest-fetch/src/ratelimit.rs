//! Bandwidth cap shared by every stream of a [`crate::Downloader`] (ALK-T-0054).
//!
//! A virtual-scheduling token bucket without bursts: each chunk reserves the time it would
//! take to send at the configured rate, and its caller sleeps until its slot starts. Slots
//! are handed out under one lock, so any number of parallel parts (and of downloads on the same
//! downloader) together stay at or below the cap. Waiting is cancellable by the caller.

use std::time::Duration;

use parking_lot::Mutex;
use tokio::time::Instant;

/// Limits throughput to a number of bytes per second.
#[derive(Debug)]
pub(crate) struct RateLimiter {
    bytes_per_sec: u64,
    /// When the next reservation may start.
    next_slot: Mutex<Instant>,
}

impl RateLimiter {
    /// A limiter for `bytes_per_sec`; zero is treated as one so a misconfiguration slows the
    /// transfer instead of dividing by zero.
    pub(crate) fn new(bytes_per_sec: u64) -> Self {
        Self {
            bytes_per_sec: bytes_per_sec.max(1),
            next_slot: Mutex::new(Instant::now()),
        }
    }

    /// Reserves `bytes` and returns how long the caller must wait before using them.
    pub(crate) fn reserve(&self, bytes: u64) -> Duration {
        let now = Instant::now();
        // Chunk sizes and rates are far below 2^52, so the float conversion is exact.
        #[allow(clippy::cast_precision_loss)]
        let cost = Duration::from_secs_f64(bytes as f64 / self.bytes_per_sec as f64);
        let mut next = self.next_slot.lock();
        let start = (*next).max(now);
        *next = start + cost;
        start - now
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reservations_queue_up_at_the_configured_rate() {
        let limiter = RateLimiter::new(1000);
        assert!(limiter.reserve(500) < Duration::from_millis(50));
        let second = limiter.reserve(500);
        assert!(
            second > Duration::from_millis(400) && second <= Duration::from_millis(500),
            "{second:?}"
        );
        let third = limiter.reserve(1000);
        assert!(third > Duration::from_millis(900), "{third:?}");
    }

    #[test]
    fn zero_rate_is_clamped() {
        assert_eq!(RateLimiter::new(0).bytes_per_sec, 1);
    }
}
