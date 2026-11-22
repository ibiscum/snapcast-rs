//! Time-synchronized PCM audio stream buffer.

use std::collections::VecDeque;

use snapcast_proto::SampleFormat;
use snapcast_proto::types::Timeval;

use crate::double_buffer::DoubleBuffer;

/// In-memory representation of samples stored in a [`PcmChunk`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleEncoding {
    /// Signed little-endian integer PCM.
    PcmInt,
    /// Little-endian IEEE-754 `f32` samples.
    Float32,
}

/// A decoded PCM chunk with a server-time timestamp and a read cursor.
#[derive(Debug, Clone)]
pub struct PcmChunk {
    /// Server-time timestamp of this chunk.
    pub timestamp: Timeval,
    /// Raw PCM sample data.
    pub data: Vec<u8>,
    /// Sample format (rate, bits, channels).
    pub format: SampleFormat,
    /// Encoding of the sample bytes.
    pub encoding: SampleEncoding,
    read_pos: usize,
}

impl PcmChunk {
    /// Create a new PCM chunk.
    pub fn new(timestamp: Timeval, data: Vec<u8>, format: SampleFormat) -> Self {
        Self::new_with_encoding(timestamp, data, format, SampleEncoding::PcmInt)
    }

    /// Create a new chunk with an explicit sample encoding.
    pub fn new_with_encoding(
        timestamp: Timeval,
        data: Vec<u8>,
        format: SampleFormat,
        encoding: SampleEncoding,
    ) -> Self {
        Self {
            timestamp,
            data,
            format,
            encoding,
            read_pos: 0,
        }
    }

    /// Start time of this chunk in microseconds.
    pub fn start_usec(&self) -> i64 {
        self.timestamp.to_usec()
    }

    /// Duration of this chunk in microseconds.
    pub fn duration_usec(&self) -> i64 {
        if self.format.frame_size() == 0 || self.format.rate() == 0 {
            return 0;
        }
        let frames = self.data.len() as i64 / self.format.frame_size() as i64;
        frames * 1_000_000 / self.format.rate() as i64
    }

    /// Read up to `frames` frames into `output`, returning the number read.
    pub fn read_frames(&mut self, output: &mut [u8], frames: u32) -> u32 {
        let frame_size = self.format.frame_size() as usize;
        if frame_size == 0 {
            return 0;
        }
        let available_bytes = self.data.len().saturating_sub(self.read_pos);
        let available_frames = available_bytes / frame_size;
        let to_read = (frames as usize)
            .min(available_frames)
            .min(output.len() / frame_size);
        if to_read == 0 {
            return 0;
        }
        let bytes = to_read * frame_size;
        output[..bytes].copy_from_slice(&self.data[self.read_pos..self.read_pos + bytes]);
        self.read_pos += bytes;
        to_read as u32
    }

    /// Returns true if no complete frames remain.
    pub fn is_end(&self) -> bool {
        let frame_size = usize::from(self.format.frame_size());
        frame_size == 0 || self.data.len().saturating_sub(self.read_pos) < frame_size
    }

    /// Skip forward by `frames` frames.
    pub fn seek(&mut self, frames: u32) {
        let bytes = frames as usize * self.format.frame_size() as usize;
        self.read_pos = (self.read_pos + bytes).min(self.data.len());
    }
}

/// Correction threshold — soft sync starts when |short_median| > 100µs
const CORRECTION_BEGIN_USEC: i64 = 100;
/// Hard sync: |median| exceeds this (µs).
const HARD_SYNC_MEDIAN_USEC: i64 = 2000;
/// Hard sync: |short_median| exceeds this (µs).
const HARD_SYNC_SHORT_MEDIAN_USEC: i64 = 5000;
/// Hard sync: |mini_median| exceeds this (µs).
const HARD_SYNC_MINI_MEDIAN_USEC: i64 = 50000;
/// Hard sync: |age| exceeds this (µs).
const HARD_SYNC_AGE_USEC: i64 = 500_000;
/// Minimum |age| for hard sync re-trigger (µs).
const HARD_SYNC_MIN_AGE_USEC: i64 = 500;
/// Minimum |mini_median| for soft sync (µs).
const SOFT_SYNC_MIN_USEC: i64 = 50;
/// Maximum playback rate correction factor.
const MAX_RATE_CORRECTION: f64 = 0.0005;
/// Rate correction scaling factor.
const RATE_CORRECTION_SCALE: f64 = 0.00005;
/// DoubleBuffer capacity for mini (fast) drift detection.
const MINI_BUFFER_SIZE: usize = 20;
/// DoubleBuffer capacity for short-term drift detection.
const SHORT_BUFFER_SIZE: usize = 100;
/// DoubleBuffer capacity for long-term drift detection.
const BUFFER_SIZE: usize = 500;
/// Default buffer in milliseconds.
const DEFAULT_BUFFER_MS: i64 = 1000;

/// Time-synchronized PCM stream buffer.
///
/// The `Stream` is responsible for buffering decoded PCM chunks and delivering them to the
/// audio player in a way that remains synchronized with the server's time. It implements
/// the same synchronization strategy as the original C++ Snapcast client.
///
/// ### Synchronization Strategy
///
/// There are two main modes of synchronization:
///
/// 1. **Hard Sync**: Used when the client is far out of sync (> 50ms) or just starting.
///    In this mode, the stream skips forward in the buffer or inserts silence to reach
///    the desired target time exactly.
/// 2. **Soft Sync**: Used for fine-tuning when the drift is small (typically < 10ms).
///    Instead of jumping, the stream subtly adjusts the playback rate (e.g., by 0.05%)
///    by adding or removing single samples at regular intervals. This is inaudible to
///    most listeners.
///
/// ### Drift Detection
///
/// Synchronization is based on "age", which is the difference between when a sample
/// *should* have been played (server time) and when it *is* being played (now).
///
/// The stream maintains three `DoubleBuffer` instances to track drift over different timescales:
/// - **Mini Buffer** (20 samples): Fast reaction to sudden network or system jitter.
/// - **Short Buffer** (100 samples): Used for calculating soft sync rate corrections.
/// - **Long Buffer** (500 samples): Long-term stability tracking and hard sync re-triggering.
///
/// The median value of these buffers is used to filter out outliers and ensure stable
/// synchronization even in unstable network conditions.
pub struct Stream {
    /// Nominal format of the incoming PCM data.
    format: SampleFormat,
    /// Encoding of samples stored in chunks.
    encoding: SampleEncoding,
    /// Queue of pending PCM chunks.
    chunks: VecDeque<PcmChunk>,
    /// The chunk currently being read from.
    current: Option<PcmChunk>,
    /// Target buffer size in milliseconds.
    buffer_ms: i64,
    /// Whether we are currently in hard sync mode.
    hard_sync: bool,

    // Drift detection buffers
    mini_buffer: DoubleBuffer,
    short_buffer: DoubleBuffer,
    buffer: DoubleBuffer,
    /// Long-term median drift in microseconds.
    median: i64,
    /// Short-term median drift in microseconds.
    short_median: i64,

    // Soft sync (rate correction) state
    /// Number of frames played at the current (corrected) rate.
    played_frames: u32,
    /// How many frames to play before adding/removing a single sample (0 if no correction).
    correct_after_x_frames: i32,
    /// Cumulative difference in frames caused by rate correction (for logging).
    frame_delta: i32,
    /// Internal buffer used for sample insertion/removal.
    read_buf: Vec<u8>,

    /// Last time (in server seconds) that stats were logged.
    last_log_sec: i64,
}

impl Stream {
    /// Create a new stream for the given sample format.
    pub fn new(format: SampleFormat) -> Self {
        Self::with_encoding(format, SampleEncoding::PcmInt)
    }

    /// Create a new stream for the given sample format and sample encoding.
    pub fn with_encoding(format: SampleFormat, encoding: SampleEncoding) -> Self {
        Self {
            format,
            encoding,
            chunks: VecDeque::new(),
            current: None,
            buffer_ms: DEFAULT_BUFFER_MS,
            hard_sync: true,
            mini_buffer: DoubleBuffer::new(MINI_BUFFER_SIZE),
            short_buffer: DoubleBuffer::new(SHORT_BUFFER_SIZE),
            buffer: DoubleBuffer::new(BUFFER_SIZE),
            median: 0,
            short_median: 0,
            played_frames: 0,
            correct_after_x_frames: 0,
            frame_delta: 0,
            read_buf: Vec::new(),
            last_log_sec: 0,
        }
    }

    /// Returns the sample format.
    pub fn format(&self) -> SampleFormat {
        self.format
    }

    /// Returns the sample byte encoding.
    pub fn encoding(&self) -> SampleEncoding {
        self.encoding
    }

    /// Set the target buffer size in milliseconds.
    pub fn set_buffer_ms(&mut self, ms: i64) {
        self.buffer_ms = ms;
    }

    /// Current target buffer size in milliseconds.
    pub fn buffer_ms(&self) -> i64 {
        self.buffer_ms
    }

    /// Enqueue a decoded PCM chunk. Chunks with a different format or encoding are dropped.
    pub fn add_chunk(&mut self, chunk: PcmChunk) {
        if chunk.format != self.format || chunk.encoding != self.encoding {
            tracing::warn!("dropping PCM chunk with a mismatched format or encoding");
            return;
        }
        self.chunks.push_back(chunk);
    }

    /// Number of queued chunks.
    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }

    /// Clear all queued chunks and reset sync state.
    pub fn clear(&mut self) {
        self.chunks.clear();
        self.current = None;
        self.hard_sync = true;
        self.reset_sync_history();
    }

    fn reset_sync_history(&mut self) {
        self.buffer.clear();
        self.mini_buffer.clear();
        self.short_buffer.clear();
        self.median = 0;
        self.short_median = 0;
        self.played_frames = 0;
        self.correct_after_x_frames = 0;
        self.frame_delta = 0;
        self.last_log_sec = 0;
    }

    fn advance_to_playable_chunk(&mut self) -> bool {
        while self.current.as_ref().is_none_or(PcmChunk::is_end) {
            self.current = self.chunks.pop_front();
            if self.current.is_none() {
                return false;
            }
        }
        true
    }

    fn update_buffers(&mut self, age: i64) {
        self.buffer.add(age);
        self.mini_buffer.add(age);
        self.short_buffer.add(age);
    }

    fn set_real_sample_rate(&mut self, sample_rate: f64) {
        let nominal = self.format.rate() as f64;
        if (sample_rate - nominal).abs() < f64::EPSILON {
            self.correct_after_x_frames = 0;
        } else {
            let ratio = nominal / sample_rate;
            self.correct_after_x_frames = (ratio / (ratio - 1.0)).round() as i32;
        }
    }

    /// Fill the requested frames with synchronized PCM, padding a short read with silence.
    ///
    /// Returns true for playable audio (including a partial read) or intentional
    /// silence while waiting for an early chunk. Returns false if playback is
    /// unavailable, out of sync, or the output buffer is too small.
    pub fn get_player_chunk(
        &mut self,
        server_now_usec: i64,
        output_buffer_dac_time_usec: i64,
        output: &mut [u8],
        frames: u32,
    ) -> bool {
        let frame_size = usize::from(self.format.frame_size());
        if self.format.rate() == 0 || frame_size == 0 || frames == 0 {
            return false;
        }
        if output.len() / frame_size < frames as usize {
            tracing::warn!(frames, bytes = output.len(), "PCM output buffer too small");
            return false;
        }
        if !self.advance_to_playable_chunk() {
            return false;
        }

        // --- Hard sync: initial alignment ---
        if self.hard_sync {
            let rate = i64::from(self.format.rate());
            let target_usec = server_now_usec + output_buffer_dac_time_usec - self.buffer_ms * 1000;
            let silent_frames = loop {
                let chunk = self.current.as_mut().unwrap();
                let age = target_usec - chunk.start_usec();
                let consumed = chunk.read_pos / frame_size;
                let cursor_usec = consumed as i64 * 1_000_000 / rate;
                if age < cursor_usec {
                    let gap = i128::from(cursor_usec) - i128::from(age);
                    break (gap * i128::from(rate) / 1_000_000).min(i128::from(frames)) as u32;
                }
                let target_frame = i128::from(age) * i128::from(rate) / 1_000_000;
                if target_frame < (chunk.data.len() / frame_size) as i128 {
                    chunk.read_pos = (target_frame as usize).max(consumed) * frame_size;
                    break 0;
                }
                self.current = None;
                if !self.advance_to_playable_chunk() {
                    return false;
                }
            };

            let offset = silent_frames as usize * frame_size;
            output[..offset].fill(0);
            let remaining = frames - silent_frames;
            if remaining == 0 {
                return true;
            }
            if self.read_next(&mut output[offset..], remaining).is_none() {
                return false;
            }
            self.hard_sync = false;
            self.reset_sync_history();
            return true;
        }

        // --- Normal playback with drift correction ---

        // Compute frames correction from current rate adjustment
        let mut frames_correction: i32 = 0;
        if self.correct_after_x_frames != 0 {
            self.played_frames += frames;
            if self.played_frames >= self.correct_after_x_frames.unsigned_abs() {
                frames_correction = self.played_frames as i32 / self.correct_after_x_frames;
                self.played_frames %= self.correct_after_x_frames.unsigned_abs();
            }
        }

        // Read with correction (or plain read if correction == 0)
        let chunk_start = match self.read_with_correction(output, frames, frames_correction) {
            Some(ts) => ts,
            None => return false,
        };

        let age_usec =
            server_now_usec - chunk_start - self.buffer_ms * 1000 + output_buffer_dac_time_usec;

        // Reset sample rate to nominal, soft sync may override below
        self.set_real_sample_rate(self.format.rate() as f64);

        // Hard sync re-trigger thresholds (matching C++)
        if self.buffer.full()
            && self.median.abs() > HARD_SYNC_MEDIAN_USEC
            && age_usec.abs() > HARD_SYNC_MIN_AGE_USEC
        {
            tracing::info!(
                median = self.median,
                "Hard sync: buffer full, |median| > 2ms"
            );
            self.hard_sync = true;
        } else if self.short_buffer.full()
            && self.short_median.abs() > HARD_SYNC_SHORT_MEDIAN_USEC
            && age_usec.abs() > HARD_SYNC_MIN_AGE_USEC
        {
            tracing::info!(
                short_median = self.short_median,
                "Hard sync: short buffer full, |short_median| > 5ms"
            );
            self.hard_sync = true;
        } else if self.mini_buffer.full()
            && self.mini_buffer.median_simple().abs() > HARD_SYNC_MINI_MEDIAN_USEC
            && age_usec.abs() > HARD_SYNC_MIN_AGE_USEC
        {
            tracing::info!(
                age_usec,
                mini_median = self.mini_buffer.median_simple(),
                "Hard sync: mini buffer full, |mini_median| > 50ms"
            );
            self.hard_sync = true;
        } else if age_usec.abs() > HARD_SYNC_AGE_USEC {
            tracing::info!(age_usec, "Hard sync: |age| > 500ms");
            self.hard_sync = true;
        } else if self.short_buffer.full() {
            // Soft sync: adjust playback speed based on drift
            let mini_median = self.mini_buffer.median_simple();
            if self.short_median > CORRECTION_BEGIN_USEC
                && mini_median > SOFT_SYNC_MIN_USEC
                && age_usec > SOFT_SYNC_MIN_USEC
            {
                let rate = (self.short_median as f64 / 100.0) * RATE_CORRECTION_SCALE;
                let rate = 1.0 - rate.min(MAX_RATE_CORRECTION);
                self.set_real_sample_rate(self.format.rate() as f64 * rate);
            } else if self.short_median < -CORRECTION_BEGIN_USEC
                && mini_median < -SOFT_SYNC_MIN_USEC
                && age_usec < -SOFT_SYNC_MIN_USEC
            {
                let rate = (-self.short_median as f64 / 100.0) * RATE_CORRECTION_SCALE;
                let rate = 1.0 + rate.min(MAX_RATE_CORRECTION);
                self.set_real_sample_rate(self.format.rate() as f64 * rate);
            }
        }

        self.update_buffers(age_usec);

        // Stats logging (once per second)
        let now_sec = server_now_usec / 1_000_000;
        if now_sec != self.last_log_sec {
            self.last_log_sec = now_sec;
            self.median = self.buffer.median_simple();
            self.short_median = self.short_buffer.median_simple();
            tracing::debug!(
                target: "Stats",
                "Chunk: {}\t{}\t{}\t{}\t{}\t{}\t{}",
                age_usec,
                self.mini_buffer.median_simple(),
                self.short_median,
                self.median,
                self.buffer.len(),
                output_buffer_dac_time_usec / 1000,
                self.frame_delta,
            );
            self.frame_delta = 0;
        }

        age_usec.abs() < 500_000
    }

    /// Fill `output` with silence.
    pub fn get_silence(&self, output: &mut [u8], frames: u32) {
        let bytes = frames as usize * self.format.frame_size() as usize;
        let len = bytes.min(output.len());
        output[..len].fill(0);
    }

    /// Like [`get_player_chunk`](Self::get_player_chunk), but fills silence on failure.
    pub fn get_player_chunk_or_silence(
        &mut self,
        server_now_usec: i64,
        output_buffer_dac_time_usec: i64,
        output: &mut [u8],
        frames: u32,
    ) -> bool {
        let result =
            self.get_player_chunk(server_now_usec, output_buffer_dac_time_usec, output, frames);
        if !result {
            self.get_silence(output, frames);
        }
        result
    }

    fn read_next(&mut self, output: &mut [u8], frames: u32) -> Option<i64> {
        let frame_size = usize::from(self.format.frame_size());
        let output = &mut output[..frames as usize * frame_size];
        let mut ts = None;
        let mut read = 0u32;
        while read < frames && self.advance_to_playable_chunk() {
            let chunk = self.current.as_mut().unwrap();
            let consumed_frames = chunk.read_pos / frame_size;
            let offset = read as usize * frame_size;
            let n = chunk.read_frames(&mut output[offset..], frames - read);
            if n == 0 {
                break;
            }
            ts.get_or_insert_with(|| {
                chunk.start_usec()
                    + consumed_frames as i64 * 1_000_000 / i64::from(self.format.rate())
            });
            read += n;
        }
        output[read as usize * frame_size..].fill(0);
        ts
    }

    fn read_with_correction(
        &mut self,
        output: &mut [u8],
        frames: u32,
        correction: i32,
    ) -> Option<i64> {
        if correction == 0 {
            return self.read_next(output, frames);
        }

        // Clamp correction to avoid underflow
        let correction = correction.max(-(frames as i32) + 1);

        self.frame_delta -= correction;
        let to_read = (frames as i32 + correction) as u32;
        let frame_size = self.format.frame_size() as usize;

        self.read_buf.resize(to_read as usize * frame_size, 0);
        let mut read_buf = std::mem::take(&mut self.read_buf);
        let ts = self.read_next(&mut read_buf, to_read);

        let max = if correction < 0 {
            frames as usize
        } else {
            to_read as usize
        };
        let slices = (correction.unsigned_abs() as usize + 1).min(max);
        let slice_size = max / slices;

        let mut pos = 0usize;
        for n in 0..slices {
            let size = if n + 1 == slices {
                max - pos
            } else {
                slice_size
            };

            if correction < 0 {
                let src_start = (pos - n) * frame_size;
                let dst_start = pos * frame_size;
                let len = size * frame_size;
                output[dst_start..dst_start + len]
                    .copy_from_slice(&read_buf[src_start..src_start + len]);
            } else {
                let src_start = pos * frame_size;
                let dst_start = (pos - n) * frame_size;
                let len = size * frame_size;
                output[dst_start..dst_start + len]
                    .copy_from_slice(&read_buf[src_start..src_start + len]);
            }
            pos += size;
        }

        self.read_buf = read_buf;
        ts
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt() -> SampleFormat {
        SampleFormat::new(48000, 16, 2)
    }

    fn make_chunk(sec: i32, usec: i32, frames: u32, format: SampleFormat) -> PcmChunk {
        let bytes = frames as usize * format.frame_size() as usize;
        let data: Vec<u8> = (0..bytes).map(|i| (i % 256) as u8).collect();
        PcmChunk::new(Timeval { sec, usec }, data, format)
    }

    fn stereo_frames(ids: impl IntoIterator<Item = u16>) -> Vec<u8> {
        ids.into_iter()
            .flat_map(|id| [id, id + 100])
            .flat_map(u16::to_le_bytes)
            .collect()
    }

    #[test]
    fn pcm_chunk_duration() {
        let f = fmt();
        let chunk = make_chunk(0, 0, 480, f);
        assert_eq!(chunk.duration_usec(), 10_000);
    }

    #[test]
    fn pcm_chunk_read_frames() {
        let f = fmt();
        let mut chunk = make_chunk(0, 0, 100, f);
        let mut buf = vec![0u8; 50 * f.frame_size() as usize];
        let read = chunk.read_frames(&mut buf, 50);
        assert_eq!(read, 50);
        assert!(!chunk.is_end());
        let read = chunk.read_frames(&mut buf, 50);
        assert_eq!(read, 50);
        assert!(chunk.is_end());
    }

    #[test]
    fn read_frames_bounded_by_output() {
        // A chunk with more data than the output can hold must fill only the
        // output (no panic), returning the number of whole frames written.
        let f = fmt();
        let mut chunk = make_chunk(0, 0, 100, f); // 100 frames available
        let mut buf = vec![0u8; 10 * f.frame_size() as usize]; // room for 10
        assert_eq!(chunk.read_frames(&mut buf, 100), 10);
    }

    #[test]
    fn pcm_chunk_seek() {
        let f = fmt();
        let mut chunk = make_chunk(0, 0, 100, f);
        chunk.seek(90);
        let mut buf = vec![0u8; 100 * f.frame_size() as usize];
        let read = chunk.read_frames(&mut buf, 100);
        assert_eq!(read, 10);
    }

    #[test]
    fn stream_add_and_count() {
        let f = fmt();
        let mut stream = Stream::new(f);
        assert_eq!(stream.chunk_count(), 0);
        stream.add_chunk(make_chunk(100, 0, 480, f));
        stream.add_chunk(make_chunk(100, 10_000, 480, f));
        assert_eq!(stream.chunk_count(), 2);
    }

    #[test]
    fn stream_clear() {
        let f = fmt();
        let mut stream = Stream::new(f);
        stream.add_chunk(make_chunk(100, 0, 480, f));
        stream.update_buffers(500);
        stream.short_median = 500;
        stream.median = 500;
        stream.correct_after_x_frames = -2;
        stream.played_frames = 1;
        stream.clear();
        assert_eq!(stream.chunk_count(), 0);
        assert!(stream.hard_sync);
        assert!(stream.buffer.is_empty());
        assert!(stream.short_buffer.is_empty());
        assert!(stream.mini_buffer.is_empty());
        assert_eq!((stream.median, stream.short_median), (0, 0));
        assert_eq!(
            (stream.correct_after_x_frames, stream.played_frames),
            (0, 0)
        );
    }

    #[test]
    fn stream_silence_when_empty() {
        let f = fmt();
        let mut stream = Stream::new(f);
        let mut buf = vec![0xFFu8; 480 * f.frame_size() as usize];
        let result = stream.get_player_chunk(100_000_000, 0, &mut buf, 480);
        assert!(!result);
    }

    #[test]
    fn stream_hard_sync_plays_silence_when_too_early() {
        let f = fmt();
        let mut stream = Stream::new(f);
        stream.set_buffer_ms(1000);
        stream.add_chunk(make_chunk(100, 0, 4800, f));
        let server_now = 100_000_000i64;
        let mut buf = vec![0xFFu8; 480 * f.frame_size() as usize];
        let result = stream.get_player_chunk(server_now, 0, &mut buf, 480);
        assert!(result);
        assert!(buf.iter().all(|&b| b == 0));
    }

    #[test]
    fn stream_hard_sync_plays_data_when_aligned() {
        let f = fmt();
        let mut stream = Stream::new(f);
        stream.set_buffer_ms(1000);
        stream.add_chunk(make_chunk(99, 0, 4800, f));
        let server_now = 100_000_000i64;
        let mut buf = vec![0u8; 480 * f.frame_size() as usize];
        let result = stream.get_player_chunk(server_now, 0, &mut buf, 480);
        assert!(result);
        assert!(buf.iter().any(|&b| b != 0));
    }

    #[test]
    fn set_real_sample_rate_correction() {
        let f = fmt();
        let mut stream = Stream::new(f);
        stream.set_real_sample_rate(48000.0);
        assert_eq!(stream.correct_after_x_frames, 0);

        stream.set_real_sample_rate(47999.0);
        assert_ne!(stream.correct_after_x_frames, 0);
    }

    #[test]
    fn read_with_correction_remove_one_frame() {
        let f = fmt();
        let mut stream = Stream::new(f);
        stream.add_chunk(PcmChunk::new(
            Timeval { sec: 100, usec: 0 },
            stereo_frames(0..12),
            f,
        ));
        let mut output = [0xFF; 36];
        assert_eq!(
            stream.read_with_correction(&mut output, 9, 1),
            Some(100_000_000)
        );
        assert_eq!(
            output.as_slice(),
            stereo_frames([0, 1, 2, 3, 5, 6, 7, 8, 9])
        );
        stream.read_next(&mut output[..4], 1).unwrap();
        assert_eq!(output[..4], stereo_frames([10]));
    }

    #[test]
    fn read_with_correction_zero_is_passthrough() {
        let f = fmt();
        let mut stream = Stream::new(f);
        stream.add_chunk(make_chunk(100, 0, 100, f));
        stream.current = stream.chunks.pop_front();

        let mut out1 = vec![0u8; 50 * f.frame_size() as usize];
        stream.read_with_correction(&mut out1, 50, 0);

        stream.add_chunk(make_chunk(100, 0, 100, f));
        stream.current = stream.chunks.pop_front();

        let mut out2 = vec![0u8; 50 * f.frame_size() as usize];
        stream.read_next(&mut out2, 50);

        assert_eq!(out1, out2);
    }

    #[test]
    fn read_with_correction_negative_repeats_whole_stereo_frames() {
        let f = fmt();
        let mut stream = Stream::new(f);
        stream.add_chunk(PcmChunk::new(
            Timeval { sec: 100, usec: 0 },
            stereo_frames(0..12),
            f,
        ));
        let mut output = [0xFF; 40];
        assert_eq!(
            stream.read_with_correction(&mut output, 10, -2),
            Some(100_000_000)
        );
        assert_eq!(
            output.as_slice(),
            stereo_frames([0, 1, 2, 2, 3, 4, 4, 5, 6, 7])
        );
        stream.read_next(&mut output[..4], 1).unwrap();
        assert_eq!(output[..4], stereo_frames([8]));
    }

    #[test]
    fn corrected_short_read_does_not_replay_scratch_buffer() {
        let f = fmt();
        let mut stream = Stream::new(f);
        stream.add_chunk(PcmChunk::new(
            Timeval { sec: 100, usec: 0 },
            stereo_frames(1..=6),
            f,
        ));
        let mut output = [0xFF; 16];
        assert!(stream.read_with_correction(&mut output, 4, 1).is_some());
        assert!(stream.read_with_correction(&mut output, 4, 1).is_some());
        assert_eq!(output[..4], stereo_frames([6]));
        assert_eq!(output[4..], [0; 12]);
    }

    #[test]
    fn read_with_correction_clamps_large_negative_values() {
        let f = fmt();
        let mut stream = Stream::new(f);
        stream.add_chunk(make_chunk(100, 0, 10, f));
        stream.current = stream.chunks.pop_front();

        let mut output = vec![0u8; 10 * f.frame_size() as usize];
        // correction is clamped to -(frames-1), so this must not underflow/panic.
        let ts = stream.read_with_correction(&mut output, 10, -10_000);
        assert!(ts.is_some());
    }

    // ---- PcmChunk edge cases ----

    #[test]
    fn pcm_chunk_duration_zero_rate() {
        // rate == 0 short-circuits to a 0 duration (guards a divide-by-zero).
        let f = SampleFormat::new(0, 16, 2);
        let chunk = PcmChunk::new(Timeval { sec: 0, usec: 0 }, vec![0u8; 16], f);
        assert_eq!(chunk.duration_usec(), 0);
    }

    #[test]
    fn pcm_chunk_zero_frame_size_is_safe() {
        // channels == 0 => frame_size 0 => read_frames and duration are no-ops.
        let f = SampleFormat::new(48000, 16, 0);
        let mut chunk = PcmChunk::new(Timeval { sec: 0, usec: 0 }, vec![0u8; 16], f);
        assert_eq!(chunk.duration_usec(), 0);
        let mut buf = vec![0u8; 16];
        assert_eq!(chunk.read_frames(&mut buf, 4), 0);
    }

    #[test]
    fn pcm_chunk_seek_past_end_clamps() {
        let f = fmt();
        let mut chunk = make_chunk(0, 0, 10, f);
        chunk.seek(1000); // far past the 10 available frames
        assert!(chunk.is_end());
        let mut buf = vec![0u8; f.frame_size() as usize];
        assert_eq!(chunk.read_frames(&mut buf, 1), 0);
    }

    #[test]
    fn pcm_chunk_is_end_lifecycle() {
        let f = fmt();
        let mut chunk = make_chunk(0, 0, 5, f);
        assert!(!chunk.is_end());
        let mut buf = vec![0u8; 5 * f.frame_size() as usize];
        chunk.read_frames(&mut buf, 5);
        assert!(chunk.is_end());
    }

    // ---- Stream accessors / encoding ----

    #[test]
    fn stream_accessors_report_format_and_encoding() {
        let s = Stream::new(SampleFormat::new(44100, 16, 1));
        assert_eq!(s.format().rate(), 44100);
        assert_eq!(s.format().channels(), 1);
        assert_eq!(s.encoding(), SampleEncoding::PcmInt);
    }

    #[test]
    fn stream_with_float32_encoding() {
        let s = Stream::with_encoding(fmt(), SampleEncoding::Float32);
        assert_eq!(s.encoding(), SampleEncoding::Float32);
    }

    // ---- silence helpers ----

    #[test]
    fn get_silence_zeroes_output() {
        let f = fmt();
        let s = Stream::new(f);
        let mut buf = vec![0xAAu8; 480 * f.frame_size() as usize];
        s.get_silence(&mut buf, 480);
        assert!(buf.iter().all(|&b| b == 0));
    }

    #[test]
    fn or_silence_fills_when_empty() {
        let f = fmt();
        let mut s = Stream::new(f);
        let mut buf = vec![0xAAu8; 480 * f.frame_size() as usize];
        let result = s.get_player_chunk_or_silence(100_000_000, 0, &mut buf, 480);
        assert!(!result, "empty stream underruns");
        assert!(
            buf.iter().all(|&b| b == 0),
            "silence is written on underrun"
        );
    }

    // ---- hard-sync chunk skipping (age > 0 path) ----

    #[test]
    fn hard_sync_skips_stale_chunk_and_aligns_to_next() {
        let f = fmt();
        let mut s = Stream::new(f);
        s.set_buffer_ms(1000);
        // Stale chunk (age far beyond its own duration) then an aligned one (age 0).
        s.add_chunk(make_chunk(90, 0, 4800, f));
        s.add_chunk(make_chunk(99, 0, 4800, f));
        let mut buf = vec![0u8; 480 * f.frame_size() as usize];
        let result = s.get_player_chunk(100_000_000, 0, &mut buf, 480);
        assert!(
            result,
            "should align to the good chunk after skipping the stale one"
        );
        assert!(
            buf.iter().any(|&b| b != 0),
            "expected real audio, not silence"
        );
    }

    #[test]
    fn hard_sync_returns_false_when_all_chunks_stale() {
        let f = fmt();
        let mut s = Stream::new(f);
        s.set_buffer_ms(1000);
        // Every chunk is older than its own duration → skip loop exhausts, no alignment.
        s.add_chunk(make_chunk(90, 0, 480, f));
        s.add_chunk(make_chunk(91, 0, 480, f));
        let mut buf = vec![0u8; 480 * f.frame_size() as usize];
        assert!(!s.get_player_chunk(100_000_000, 0, &mut buf, 480));
    }

    #[test]
    fn hard_sync_plays_from_inside_an_overlapping_chunk() {
        let format = SampleFormat::new(1000, 16, 2);
        for prepend_stale in [false, true] {
            let mut stream = Stream::new(format);
            if prepend_stale {
                stream.add_chunk(PcmChunk::new(
                    Timeval { sec: 98, usec: 0 },
                    stereo_frames([90, 91]),
                    format,
                ));
            }
            stream.add_chunk(PcmChunk::new(
                Timeval { sec: 99, usec: 0 },
                stereo_frames(1..=5),
                format,
            ));
            let mut output = [0xFF; 8];
            assert!(stream.get_player_chunk(100_001_000, 1_000, &mut output, 2));
            assert_eq!(output.as_slice(), stereo_frames([3, 4]));
            assert!(stream.get_player_chunk(100_003_000, 1_000, &mut output[..4], 1));
            assert_eq!(output[..4], stereo_frames([5]));
        }
    }

    #[test]
    fn stream_skips_incomplete_frames_and_pads_short_reads() {
        let format = SampleFormat::new(1000, 16, 2);
        for initial_read in [false, true] {
            let mut stream = Stream::new(format);
            let mut data = stereo_frames([1, 2]);
            data.push(0xAA);
            stream.add_chunk(PcmChunk::new(Timeval { sec: 99, usec: 0 }, data, format));
            stream.add_chunk(PcmChunk::new(
                Timeval {
                    sec: 99,
                    usec: 2_000,
                },
                vec![0xBB; 3],
                format,
            ));
            stream.add_chunk(PcmChunk::new(
                Timeval {
                    sec: 99,
                    usec: 2_000,
                },
                stereo_frames([3]),
                format,
            ));
            let now = 100_000_000;
            if initial_read {
                let mut first = [0xFF; 4];
                assert!(stream.get_player_chunk(now, 0, &mut first, 1));
                assert_eq!(first.as_slice(), stereo_frames([1]));
            }
            let consumed = usize::from(initial_read);
            let mut output = [0xFF; 17];
            assert!(stream.get_player_chunk_or_silence(
                now + consumed as i64 * 1_000,
                0,
                &mut output,
                4,
            ));
            let expected = stereo_frames((1 + consumed as u16)..=3);
            assert_eq!(output[..expected.len()], expected);
            assert!(output[expected.len()..16].iter().all(|&byte| byte == 0));
            assert_eq!(output[16], 0xFF);
        }
    }

    #[test]
    fn stream_drops_chunks_with_mismatched_format_or_encoding() {
        let format = SampleFormat::new(1000, 32, 2);
        let mut stream = Stream::with_encoding(format, SampleEncoding::Float32);
        let timestamp = Timeval { sec: 99, usec: 0 };
        stream.add_chunk(PcmChunk::new(timestamp, vec![0xAA; 8], format));
        stream.add_chunk(PcmChunk::new_with_encoding(
            timestamp,
            vec![0xBB; 4],
            SampleFormat::new(1000, 32, 1),
            SampleEncoding::Float32,
        ));
        let samples = [0.25f32.to_le_bytes(), (-0.5f32).to_le_bytes()].concat();
        stream.add_chunk(PcmChunk::new_with_encoding(
            timestamp,
            samples.clone(),
            format,
            SampleEncoding::Float32,
        ));
        let mut output = [0xFF; 8];
        assert!(stream.get_player_chunk(100_000_000, 0, &mut output, 1));
        assert_eq!(output.as_slice(), samples);
    }

    #[test]
    fn undersized_output_does_not_consume_audio_or_complete_sync() {
        let format = SampleFormat::new(1000, 16, 2);
        let mut stream = Stream::new(format);
        stream.add_chunk(PcmChunk::new(
            Timeval {
                sec: 99,
                usec: 1_000,
            },
            stereo_frames([1, 2, 3]),
            format,
        ));
        assert!(!stream.get_player_chunk(100_000_000, 0, &mut [0xFF; 3], 3));
        assert!(stream.hard_sync);

        let mut output = [0xFF; 12];
        assert!(stream.get_player_chunk(100_000_000, 0, &mut output, 3));
        assert_eq!(output[..4], [0; 4]);
        assert_eq!(output[4..], stereo_frames([1, 2]));
        assert!(!stream.get_player_chunk(100_003_000, 0, &mut [0xFF; 3], 1));
        assert!(stream.get_player_chunk(100_003_000, 0, &mut output[..4], 1));
        assert_eq!(output[..4], stereo_frames([3]));
    }

    #[test]
    fn hard_sync_completion_discards_previous_rate_correction() {
        let format = SampleFormat::new(1000, 16, 2);
        let mut stream = Stream::new(format);
        stream.correct_after_x_frames = -2;
        stream.played_frames = 1;
        stream.add_chunk(PcmChunk::new(
            Timeval { sec: 99, usec: 0 },
            stereo_frames(1..=4),
            format,
        ));
        let mut output = [0xFF; 8];
        assert!(stream.get_player_chunk(100_000_000, 0, &mut output, 2));
        assert_eq!(output.as_slice(), stereo_frames([1, 2]));
        assert!(stream.get_player_chunk(100_002_000, 0, &mut output, 2));
        assert_eq!(output.as_slice(), stereo_frames([3, 4]));
    }

    // ---- normal playback (after hard sync completes) ----

    #[test]
    fn normal_playback_after_hard_sync() {
        let f = fmt();
        let mut s = Stream::new(f);
        s.set_buffer_ms(1000);
        s.add_chunk(make_chunk(99, 0, 48000, f)); // one second of audio, aligned at age 0
        let mut buf = vec![0u8; 480 * f.frame_size() as usize];
        assert!(s.get_player_chunk(100_000_000, 0, &mut buf, 480));
        assert!(
            !s.hard_sync,
            "hard sync completes on the first aligned call"
        );
        // Subsequent calls take the normal drift-correction playback path.
        let mut played = false;
        for i in 1..10i64 {
            let now = 100_000_000 + i * 10_000; // +10 ms == 480 frames @ 48 kHz
            if s.get_player_chunk(now, 0, &mut buf, 480) {
                played = true;
            }
        }
        assert!(played, "normal playback should deliver frames");
    }

    #[test]
    fn normal_playback_retriggers_hard_sync_on_large_age() {
        let f = fmt();
        let mut s = Stream::new(f);
        s.set_buffer_ms(1000);
        s.add_chunk(make_chunk(99, 0, 48000, f));
        let mut buf = vec![0u8; 480 * f.frame_size() as usize];
        assert!(s.get_player_chunk(100_000_000, 0, &mut buf, 480));
        assert!(!s.hard_sync);
        // Jump the clock >500 ms forward → age exceeds the hard-sync age threshold.
        let result = s.get_player_chunk(100_700_000, 0, &mut buf, 480);
        assert!(s.hard_sync, "a >500 ms age must re-trigger hard sync");
        assert!(!result, "an out-of-range age returns false");
    }

    #[test]
    fn out_of_order_timestamps_do_not_panic() {
        let f = fmt();
        let mut s = Stream::new(f);
        s.set_buffer_ms(1000);
        // Decreasing then increasing timestamps must never panic the sync loop.
        s.add_chunk(make_chunk(101, 0, 480, f));
        s.add_chunk(make_chunk(99, 0, 480, f));
        s.add_chunk(make_chunk(100, 0, 480, f));
        let mut buf = vec![0u8; 480 * f.frame_size() as usize];
        for i in 0..5i64 {
            let _ = s.get_player_chunk(100_000_000 + i * 10_000, 0, &mut buf, 480);
        }
    }

    #[test]
    fn zero_rate_stream_does_not_panic_and_underruns() {
        let f = SampleFormat::new(0, 16, 2);
        let mut s = Stream::new(f);
        s.set_buffer_ms(1000);
        s.add_chunk(make_chunk(100, 0, 480, f));
        let mut buf = vec![0u8; 480 * 4];
        // Zero sample-rate currently cannot produce playable timing; contract here is
        // "no panic" and a graceful false return.
        assert!(!s.get_player_chunk(100_000_000, 0, &mut buf, 480));
    }
}
