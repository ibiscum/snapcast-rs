//! Server time synchronization.
//!
//! Port of the C++ `TimeProvider`. Receives client-to-server and server-to-client
//! latency pairs, computes the clock difference via a median buffer, and provides
//! `diff_to_server_usec()` — the estimated server/client clock offset.

use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Duration, Instant};

use snapcast_proto::Timeval;

use crate::double_buffer::DoubleBuffer;

/// Provides the estimated server time based on time sync messages.
pub struct TimeProvider {
    diff_buffer: DoubleBuffer,
    diff_to_server_usec: AtomicI64,
    last_sync: Option<Instant>,
}

impl Default for TimeProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl TimeProvider {
    /// Create a new time provider with default settings.
    pub fn new() -> Self {
        Self {
            diff_buffer: DoubleBuffer::new(200),
            diff_to_server_usec: AtomicI64::new(0),
            last_sync: None,
        }
    }

    /// Set the time diff from a c2s/s2c latency pair (as received in Time messages).
    ///
    /// The diff is computed as `(c2s - s2c) / 2` in whole microseconds, rounding
    /// toward zero. Symmetric network latency cancels out.
    pub fn set_diff(&mut self, c2s: &Timeval, s2c: &Timeval) {
        let diff_usec = (c2s.to_usec() - s2c.to_usec()) / 2;
        tracing::trace!(diff_usec, "set_diff");
        self.record_sample(diff_usec, Instant::now());
    }

    /// Set the time diff directly in milliseconds, truncated to whole microseconds.
    /// Non-finite or out-of-range values are ignored without refreshing the estimate.
    pub fn set_diff_ms(&mut self, ms: f64) {
        let usec = ms * 1_000.0;
        // The positive bound is exclusive: i64::MAX rounds to 2^63 in f64.
        let min_usec = i64::MIN as f64;
        if !(min_usec..-min_usec).contains(&usec) {
            tracing::warn!(ms, "ignoring invalid clock offset");
            return;
        }
        self.record_sample(usec as i64, Instant::now());
    }

    fn record_sample(&mut self, usec: i64, now: Instant) {
        if let Some(last) = self.last_sync
            && now.duration_since(last) > Duration::from_secs(60)
            && !self.diff_buffer.is_empty()
        {
            self.diff_buffer.clear();
        }
        self.last_sync = Some(now);

        self.diff_buffer.add(usec);
        let median = self.diff_buffer.median_simple();
        self.diff_to_server_usec.store(median, Ordering::Relaxed);
    }

    pub(crate) fn reset(&mut self) {
        self.diff_buffer.clear();
        self.last_sync = None;
        self.diff_to_server_usec.store(0, Ordering::Relaxed);
    }

    /// Get the current diff to server in microseconds.
    pub fn diff_to_server_usec(&self) -> i64 {
        self.diff_to_server_usec.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_diff_is_zero() {
        let tp = TimeProvider::new();
        assert_eq!(tp.diff_to_server_usec(), 0);
    }

    #[test]
    fn set_diff_from_latency_pair() {
        let mut tp = TimeProvider::new();

        // c2s = 10ms, s2c = 8ms → diff = (10 - 8) / 2 = 1ms = 1000 usec
        let c2s = Timeval {
            sec: 0,
            usec: 10_000,
        };
        let s2c = Timeval {
            sec: 0,
            usec: 8_000,
        };
        tp.set_diff(&c2s, &s2c);
        assert_eq!(tp.diff_to_server_usec(), 1000);
    }

    #[test]
    fn median_stabilizes() {
        let mut tp = TimeProvider::new();

        // Feed several values, one outlier
        for _ in 0..10 {
            tp.set_diff_ms(5.0); // 5ms = 5000 usec
        }
        tp.set_diff_ms(100.0); // outlier

        // Median should still be close to 5000 usec
        assert_eq!(tp.diff_to_server_usec(), 5000);
    }

    #[test]
    fn negative_diff() {
        let mut tp = TimeProvider::new();
        tp.set_diff_ms(-3.5);
        assert_eq!(tp.diff_to_server_usec(), -3500);
    }

    #[test]
    fn set_diff_symmetric_latency_cancels() {
        let mut tp = TimeProvider::new();

        // If c2s == s2c, the diff should be 0 (symmetric network)
        let c2s = Timeval { sec: 0, usec: 5000 };
        let s2c = Timeval { sec: 0, usec: 5000 };
        tp.set_diff(&c2s, &s2c);
        assert_eq!(tp.diff_to_server_usec(), 0);
    }

    #[test]
    fn sync_history_expires_only_after_more_than_sixty_seconds() {
        let mut tp = TimeProvider::new();
        let start = Instant::now();
        for _ in 0..5 {
            tp.record_sample(5_000, start);
        }

        let boundary = start + Duration::from_secs(60);
        tp.record_sample(100_000, boundary);
        assert_eq!(tp.diff_to_server_usec(), 5000);

        let expired = boundary + Duration::from_secs(60) + Duration::from_nanos(1);
        tp.record_sample(200_000, expired);
        assert_eq!(tp.diff_to_server_usec(), 200_000);
        tp.record_sample(100_000, expired + Duration::from_millis(1));
        assert_eq!(tp.diff_to_server_usec(), 200_000);
        tp.record_sample(150_000, expired + Duration::from_millis(2));
        assert_eq!(tp.diff_to_server_usec(), 150_000);
    }

    #[test]
    fn rolling_median_evicts_the_oldest_sample() {
        let mut tp = TimeProvider::new();
        let now = Instant::now();
        for _ in 0..101 {
            tp.record_sample(3, now);
        }
        for _ in 0..99 {
            tp.record_sample(10, now);
        }
        assert_eq!(tp.diff_to_server_usec(), 3);
        tp.record_sample(10, now);
        assert_eq!(tp.diff_to_server_usec(), 10);
    }

    #[test]
    fn invalid_offsets_do_not_change_estimate_or_history() {
        let mut tp = TimeProvider::new();
        let last = Instant::now();
        tp.record_sample(12_000, last);
        for invalid in [
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::MAX,
            -f64::MAX,
            -(i64::MIN as f64) / 1_000.0,
        ] {
            tp.set_diff_ms(invalid);
            assert_eq!(tp.diff_to_server_usec(), 12_000);
            assert_eq!(tp.last_sync, Some(last));
            assert_eq!(tp.diff_buffer.len(), 1);
        }
    }

    #[test]
    fn even_sample_count_uses_upper_middle_median() {
        let mut tp = TimeProvider::new();
        tp.set_diff_ms(1.0);
        tp.set_diff_ms(3.0);
        // DoubleBuffer median_simple() for even count uses sorted[len/2].
        assert_eq!(tp.diff_to_server_usec(), 3000);
    }

    #[test]
    fn set_diff_ms_fractional_truncates_toward_zero() {
        let mut tp = TimeProvider::new();
        tp.set_diff_ms(0.0015); // 1.5 usec -> 1
        assert_eq!(tp.diff_to_server_usec(), 1);

        let mut tp = TimeProvider::new();
        tp.set_diff_ms(-0.0015); // -1.5 usec -> -1
        assert_eq!(tp.diff_to_server_usec(), -1);
    }

    #[test]
    fn set_diff_handles_mixed_seconds_and_usec_precision() {
        let mut tp = TimeProvider::new();
        let c2s = Timeval { sec: 1, usec: 250 };
        let s2c = Timeval { sec: 0, usec: 750 };
        // ((1000.25ms - 0.75ms) / 2) = 499.75ms = 499_750 usec
        tp.set_diff(&c2s, &s2c);
        assert_eq!(tp.diff_to_server_usec(), 499_750);
    }

    #[test]
    fn latency_pair_has_exact_signed_microsecond_precision() {
        for (c2s, s2c, expected) in [
            (1_000_250, 998_248, 1_001),
            (998_248, 1_000_250, -1_001),
            (1_000_250, 998_247, 1_001),
            (998_247, 1_000_250, -1_001),
        ] {
            let mut tp = TimeProvider::new();
            tp.set_diff(&Timeval::from_usec(c2s), &Timeval::from_usec(s2c));
            assert_eq!(tp.diff_to_server_usec(), expected);
        }
    }
}
