//! Time utilities — server timestamp generation using the monotonic clock.
//!
//! The server delegates monotonic time retrieval to [`snapcast_proto::time`]
//! so protocol and server code use the same clock source.

/// Current monotonic time in microseconds.
pub fn now_usec() -> i64 {
    snapcast_proto::time::now_usec()
}

/// Generates evenly-spaced timestamps based on sample count.
/// This matches the C++ server behavior: timestamps reflect when audio
/// *should* be played, not when it was read from the source.
pub struct ChunkTimestamper {
    start_usec: i64,
    samples_written: u64,
    rate: u32,
}

impl ChunkTimestamper {
    /// Create a new timestamper anchored at the current time.
    pub fn new(rate: u32) -> Self {
        assert!(rate > 0, "sample rate must be > 0");
        Self {
            start_usec: now_usec(),
            samples_written: 0,
            rate,
        }
    }

    /// Get the timestamp for the next chunk of `frames` frames.
    pub fn next(&mut self, frames: u32) -> i64 {
        let elapsed_usec =
            ((self.samples_written as u128) * 1_000_000u128) / (self.rate as u128);
        let elapsed_usec = if elapsed_usec > i64::MAX as u128 {
            i64::MAX
        } else {
            elapsed_usec as i64
        };
        let ts = self.start_usec.saturating_add(elapsed_usec);
        self.samples_written += frames as u64;
        ts
    }

    /// Reset the timestamper (e.g. on stream restart).
    pub fn reset(&mut self) {
        self.start_usec = now_usec();
        self.samples_written = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_usec_is_positive_and_nondecreasing() {
        let a = now_usec();
        let b = now_usec();
        assert!(a > 0);
        assert!(b >= a);
    }

    #[test]
    fn first_chunk_is_anchored_at_start() {
        let mut ts = ChunkTimestamper::new(48_000);
        let start = ts.start_usec;
        assert_eq!(ts.next(1024), start);
    }

    #[test]
    fn timestamps_advance_by_exact_sample_duration() {
        let mut ts = ChunkTimestamper::new(48_000);
        let t0 = ts.next(48_000);
        let t1 = ts.next(48_000);
        // 48000 frames at 48 kHz is exactly one second (1,000,000 µs) apart.
        assert_eq!(t1 - t0, 1_000_000);
    }

    #[test]
    fn reset_rewinds_the_sample_counter() {
        let mut ts = ChunkTimestamper::new(44_100);
        ts.next(44_100);
        ts.next(44_100);
        ts.reset();
        // After reset the next chunk is anchored at the (new) start again.
        assert_eq!(ts.next(0), ts.start_usec);
    }

    #[test]
    #[should_panic(expected = "sample rate must be > 0")]
    fn new_rejects_zero_sample_rate() {
        let _ = ChunkTimestamper::new(0);
    }

    #[test]
    fn timestamps_remain_monotonic_for_fractional_steps() {
        let mut ts = ChunkTimestamper::new(44_100);
        let mut prev = ts.next(0);
        for _ in 0..1_000 {
            let cur = ts.next(1);
            assert!(cur >= prev);
            prev = cur;
        }
    }

    #[test]
    fn very_large_sample_counts_saturate_instead_of_wrapping() {
        let mut ts = ChunkTimestamper::new(48_000);
        ts.start_usec = i64::MAX - 10;
        ts.samples_written = u64::MAX;
        let stamp = ts.next(0);
        assert_eq!(stamp, i64::MAX);
    }
}
