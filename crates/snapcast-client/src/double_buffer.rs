//! Size-limited circular buffer with median/mean filtering.
//!
//! Port of the C++ `DoubleBuffer` template. Used by TimeProvider and Stream
//! for drift detection via median filtering.

use std::collections::VecDeque;

/// A fixed-capacity circular buffer that computes median over its contents.
#[derive(Debug, Clone)]
pub struct DoubleBuffer {
    buf: VecDeque<i64>,
    capacity: usize,
}

impl DoubleBuffer {
    /// Create a buffer with the given capacity.
    pub fn new(capacity: usize) -> Self {
        Self {
            buf: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    /// Add a value. Drops the oldest if full.
    pub fn add(&mut self, value: i64) {
        if self.buf.len() >= self.capacity {
            self.buf.pop_front();
        }
        self.buf.push_back(value);
    }

    /// Median value. If `mean_count > 1`, averages `mean_count` sorted values centered
    /// around the median (for even `mean_count`, slightly biased to upper-middle).
    pub fn median(&self, mean_count: usize) -> i64 {
        if self.buf.is_empty() {
            return 0;
        }
        let mut sorted: Vec<i64> = self.buf.iter().copied().collect();
        sorted.sort_unstable();

        if mean_count <= 1 || sorted.len() < mean_count {
            sorted[sorted.len() / 2]
        } else {
            let mid = sorted.len() / 2;
            let mut low = mid.saturating_sub(mean_count / 2);
            if low + mean_count > sorted.len() {
                low = sorted.len() - mean_count;
            }
            let high = low + mean_count;
            let sum: i64 = sorted[low..high].iter().sum();
            sum / mean_count as i64
        }
    }

    /// Simple median (mean_count=1).
    pub fn median_simple(&self) -> i64 {
        self.median(1)
    }

    /// Remove all values.
    pub fn clear(&mut self) {
        self.buf.clear();
    }

    /// Returns true if the buffer contains no values.
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// Number of values currently stored.
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    /// Returns true if the buffer has reached capacity.
    pub fn full(&self) -> bool {
        self.buf.len() >= self.capacity
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_median_is_zero() {
        let db = DoubleBuffer::new(10);
        assert_eq!(db.median_simple(), 0);
    }

    #[test]
    fn single_element() {
        let mut db = DoubleBuffer::new(10);
        db.add(42);
        assert_eq!(db.median_simple(), 42);
    }

    #[test]
    fn odd_count_median() {
        let mut db = DoubleBuffer::new(10);
        for v in [3, 1, 4, 1, 5] {
            db.add(v);
        }
        // sorted: [1, 1, 3, 4, 5], median index 2 → 3
        assert_eq!(db.median_simple(), 3);
    }

    #[test]
    fn even_count_median() {
        let mut db = DoubleBuffer::new(10);
        for v in [10, 20, 30, 40] {
            db.add(v);
        }
        // sorted: [10, 20, 30, 40], index 2 → 30
        assert_eq!(db.median_simple(), 30);
    }

    #[test]
    fn capacity_evicts_oldest() {
        let mut db = DoubleBuffer::new(3);
        db.add(100);
        db.add(200);
        db.add(300);
        assert!(db.full());
        db.add(400); // evicts 100
        assert_eq!(db.len(), 3);
        // contents: [200, 300, 400], sorted: [200, 300, 400], median → 300
        assert_eq!(db.median_simple(), 300);
    }

    #[test]
    fn median_with_mean() {
        let mut db = DoubleBuffer::new(10);
        for v in [1, 2, 3, 4, 5, 6, 7] {
            db.add(v);
        }
        // sorted: [1,2,3,4,5,6,7], mid=3, mean_count=3 → avg of [3,4,5] = 4
        assert_eq!(db.median(3), 4);
    }

    #[test]
    fn median_with_even_mean_count_is_stable() {
        let mut db = DoubleBuffer::new(10);
        for v in [10, 20, 30, 40] {
            db.add(v);
        }
        // sorted: [10,20,30,40], mean_count=4 -> avg of all = 25
        assert_eq!(db.median(4), 25);
    }

    #[test]
    fn median_mean_count_larger_than_len_falls_back_to_simple_median() {
        let mut db = DoubleBuffer::new(10);
        for v in [5, 1, 9] {
            db.add(v);
        }
        assert_eq!(db.median(10), db.median_simple());
    }

    #[test]
    fn zero_capacity_retains_only_latest_item() {
        let mut db = DoubleBuffer::new(0);
        assert!(db.full());
        db.add(7);
        assert_eq!(db.len(), 1);
        assert_eq!(db.median_simple(), 7);
        db.add(9);
        assert_eq!(db.len(), 1);
        assert_eq!(db.median_simple(), 9);
    }

    #[test]
    fn clear_resets() {
        let mut db = DoubleBuffer::new(10);
        db.add(1);
        db.add(2);
        db.clear();
        assert!(db.is_empty());
        assert_eq!(db.median_simple(), 0);
    }

    #[test]
    fn negative_values() {
        let mut db = DoubleBuffer::new(5);
        for v in [-100, -50, 0, 50, 100] {
            db.add(v);
        }
        assert_eq!(db.median_simple(), 0);
    }
}
