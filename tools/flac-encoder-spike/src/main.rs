//! Verification spike for the pure-Rust FLAC encoder evaluation.
//!
//! See `docs/flac-encoder-evaluation.md` for the full write-up. In short:
//! does a chunk-by-chunk "header once, frames per chunk" encode via flacenc /
//! oxideav-flac actually round-trip correctly, matching how snapcast-rs's
//! production server feeds FLAC (1152-frame blocks, live stream, placeholder
//! STREAMINFO stats)? This is a standalone evaluation tool, not part of the
//! snapcast-rs workspace or build — see the `[workspace]` marker in
//! `Cargo.toml`. Run with `cargo run --release` from this directory.

use std::fs;
use std::io::{self, Cursor, Write};
use std::path::PathBuf;
use std::process::{Command, ExitCode};

const SAMPLE_RATE: u32 = 48_000;
const CHANNELS: usize = 2;
const BITS: usize = 16;
const BLOCK_FRAMES: usize = 1152; // matches snapserver-rs's FLAC_BLOCK_FRAMES
const ALIGNED_FRAMES: usize = 125 * BLOCK_FRAMES;
const PARTIAL_FRAMES: usize = ALIGNED_FRAMES + 240;

type EncodeResult = Result<Encoded, Box<dyn std::error::Error>>;
type Encoder = fn(&[i32]) -> EncodeResult;
const CANDIDATES: [(&str, Encoder); 2] = [
    ("flacenc", via_flacenc::encode),
    ("oxideav-flac", via_oxideav::encode),
];

#[derive(Default)]
struct Encoded {
    bytes: Vec<u8>,
    feeds_without_packets: usize,
    packets_before_flush: usize,
    packets_on_flush: usize,
}

/// Deterministic test signal: a few mixed sine tones (not pure silence/DC,
/// which some encoders special-case) at a known sample count.
fn generate_pcm(total_frames: usize) -> Vec<i32> {
    let mut out = Vec::with_capacity(total_frames * CHANNELS);
    for n in 0..total_frames {
        let t = n as f64 / SAMPLE_RATE as f64;
        let l = (0.3 * (2.0 * std::f64::consts::PI * 440.0 * t).sin()
            + 0.15 * (2.0 * std::f64::consts::PI * 1500.0 * t).sin())
            * i16::MAX as f64;
        let r = (0.3 * (2.0 * std::f64::consts::PI * 523.25 * t).sin()
            + 0.1 * (2.0 * std::f64::consts::PI * 2000.0 * t).sin())
            * i16::MAX as f64;
        out.push(l as i32);
        out.push(r as i32);
    }
    out
}

fn i32_to_i16_saturating(sample: i32) -> i16 {
    match i16::try_from(sample) {
        Ok(s) => s,
        Err(_) => sample.clamp(i16::MIN as i32, i16::MAX as i32) as i16,
    }
}

// ---------------------------------------------------------------------
// Candidate A: flacenc — header via Stream::write (0 frames), frames via
// the lower-level encode_fixed_size_frame() + FrameBuf::fill_interleaved.
// ---------------------------------------------------------------------
mod via_flacenc {
    use super::*;
    use flacenc::component::{BitRepr, Stream, StreamInfo};
    use flacenc::config;
    use flacenc::error::Verify;
    use flacenc::source::{Fill, FrameBuf};

    pub fn encode(pcm: &[i32]) -> EncodeResult {
        let config = config::Encoder::default()
            .into_verified()
            .map_err(|(_, error)| error)?;
        let stream_info = StreamInfo::new(SAMPLE_RATE as usize, CHANNELS, BITS)?;

        // Header: marker + STREAMINFO, zero frames (placeholder stats,
        // matching how a live/unbounded stream's header looks). Block size
        // IS known up front for a live stream (we choose it), so set it
        // explicitly — matches what encode_with_fixed_block_size does.
        let mut header_stream = Stream::new(SAMPLE_RATE as usize, CHANNELS, BITS)?;
        header_stream
            .stream_info_mut()
            .set_block_sizes(BLOCK_FRAMES, BLOCK_FRAMES)?;
        let mut header_sink = flacenc::bitsink::MemSink::<u8>::new();
        header_stream.write(&mut header_sink)?;
        let mut out = Encoded {
            bytes: header_sink.into_inner(),
            ..Encoded::default()
        };

        for (frame_number, chunk) in pcm.chunks(BLOCK_FRAMES * CHANNELS).enumerate() {
            let frames_here = chunk.len() / CHANNELS;
            let mut framebuf = FrameBuf::with_size(CHANNELS, frames_here)?;
            framebuf.fill_interleaved(chunk)?;

            let frame =
                flacenc::encode_fixed_size_frame(&config, &framebuf, frame_number, &stream_info)?;

            let mut frame_sink = flacenc::bitsink::MemSink::<u8>::new();
            frame.write(&mut frame_sink)?;
            out.bytes.extend(frame_sink.into_inner());
            out.packets_before_flush += 1;
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------------
// Candidate B: oxideav-flac — header via output_params().extradata
// (read immediately after construction, before any frame), frames via
// send_frame()/receive_packet() push-pull.
// ---------------------------------------------------------------------
mod via_oxideav {
    use super::*;
    use oxideav_core::format::{MediaType, SampleFormat};
    use oxideav_core::frame::{AudioFrame, Frame};
    use oxideav_core::stream::{CodecId, CodecParameters};
    use oxideav_flac::metadata::FLAC_MAGIC;

    pub fn encode(pcm: &[i32]) -> EncodeResult {
        let mut params = CodecParameters::audio(CodecId::new("flac"));
        params.media_type = MediaType::Audio;
        params.sample_rate = Some(SAMPLE_RATE);
        params.channels = Some(CHANNELS as u16);
        params.sample_format = Some(SampleFormat::S16);

        let mut encoder = oxideav_flac::encoder::make_encoder(&params)?;

        // Header read immediately after construction — before any frame.
        let mut out = Encoded::default();
        out.bytes.extend_from_slice(&FLAC_MAGIC);
        out.bytes
            .extend_from_slice(&encoder.output_params().extradata);

        for chunk in pcm.chunks(BLOCK_FRAMES * CHANNELS) {
            let frames_here = (chunk.len() / CHANNELS) as u32;
            let mut bytes = Vec::with_capacity(chunk.len() * 2);
            for &s in chunk {
                bytes.extend_from_slice(&i32_to_i16_saturating(s).to_le_bytes());
            }
            let af = AudioFrame {
                samples: frames_here,
                pts: None,
                data: vec![bytes],
            };
            encoder.send_frame(&Frame::Audio(af))?;
            let packets = drain_packets(&mut out.bytes, || encoder.receive_packet())?;
            out.feeds_without_packets += usize::from(packets == 0);
            out.packets_before_flush += packets;
        }
        // Drain any final partial frame.
        encoder.flush()?;
        out.packets_on_flush = drain_packets(&mut out.bytes, || encoder.receive_packet())?;
        Ok(out)
    }
}

fn drain_packets(
    bytes: &mut Vec<u8>,
    mut receive: impl FnMut() -> oxideav_core::Result<oxideav_core::Packet>,
) -> oxideav_core::Result<usize> {
    let mut count = 0;
    loop {
        match receive() {
            Ok(packet) => {
                bytes.extend_from_slice(&packet.data);
                count += 1;
            }
            Err(oxideav_core::Error::NeedMore) => return Ok(count),
            Err(error) => return Err(error),
        }
    }
}

// ---------------------------------------------------------------------
// Verification: decode via symphonia (independent of both candidates,
// and literally what the real snapcast-client uses in production).
// ---------------------------------------------------------------------
fn decode_with_symphonia(flac_bytes: Vec<u8>) -> Result<Vec<i16>, String> {
    use symphonia::core::codecs::DecoderOptions;
    use symphonia::core::errors::Error as SymError;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;
    let cursor = Cursor::new(flac_bytes);
    let mss = MediaSourceStream::new(Box::new(cursor), Default::default());
    let mut hint = Hint::new();
    hint.with_extension("flac");

    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| format!("probe failed: {e}"))?;
    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != symphonia::core::codecs::CODEC_TYPE_NULL)
        .ok_or("no audio track")?
        .clone();
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| format!("decoder init failed: {e}"))?;

    let mut out: Vec<i16> = Vec::new();
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(format!("packet read failed: {e}")),
        };
        match decoder.decode(&packet) {
            Ok(decoded) => {
                let spec = *decoded.spec();
                let mut sample_buf = symphonia::core::audio::SampleBuffer::<i16>::new(
                    decoded.capacity() as u64,
                    spec,
                );
                sample_buf.copy_interleaved_ref(decoded);
                out.extend_from_slice(sample_buf.samples());
            }
            Err(SymError::DecodeError(e)) => return Err(format!("DECODE ERROR: {e}")),
            Err(e) => return Err(format!("decode failed: {e}")),
        }
    }
    Ok(out)
}

fn temp_flac_path(label: &str) -> PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!(
        "flac-spike-{label}-{}-{nonce}.flac",
        std::process::id()
    ))
}

fn run_external_flac_check(label: &str, bytes: &[u8]) -> Result<(), String> {
    let path = temp_flac_path(label);
    let file = fs::File::create_new(&path)
        .map_err(|e| format!("cannot create {}: {e}", path.display()))?;
    let result = (|| {
        let mut file = file;
        file.write_all(bytes)
            .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        drop(file);
        println!("  -> wrote {} ({} bytes)", path.display(), bytes.len());
        let check = match Command::new("flac")
            .args(["-t", "--totally-silent"])
            .arg(&path)
            .status()
        {
            Ok(status) if status.success() => {
                println!("  -> `flac -t` (reference C decoder): OK");
                Ok(())
            }
            Ok(status) => Err(format!("`flac -t` failed: {status}")),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                println!("  -> `flac -t`: SKIPPED (optional executable not found)");
                Ok(())
            }
            Err(e) => Err(format!("could not run `flac -t`: {e}")),
        };

        match Command::new("metaflac")
            .arg("--show-md5sum")
            .arg(&path)
            .output()
        {
            Ok(output) if output.status.success() => {
                let md5 = String::from_utf8_lossy(&output.stdout);
                println!("  -> embedded STREAMINFO md5: {}", md5.trim());
            }
            Ok(output) => eprintln!("  -> optional `metaflac` failed: {}", output.status),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => eprintln!("  -> could not run optional `metaflac`: {e}"),
        }
        check
    })();

    if std::env::var_os("FLAC_SPIKE_KEEP_TMP").is_none() {
        if let Err(e) = fs::remove_file(&path) {
            eprintln!("  -> could not remove {}: {e}", path.display());
        }
    }
    result
}

fn pcm_as_i16(pcm: &[i32]) -> Vec<i16> {
    pcm.iter().map(|&s| i32_to_i16_saturating(s)).collect()
}

fn verify_pcm(decoded: &[i16], expected: &[i16]) -> Result<(), String> {
    if decoded.len() != expected.len() {
        return Err(format!(
            "sample count mismatch (got {}, want {})",
            decoded.len(),
            expected.len()
        ));
    }
    if let Some(first) = decoded.iter().zip(expected).position(|(a, b)| a != b) {
        return Err(format!(
            "sample mismatch at index {first} (got {}, want {})",
            decoded[first], expected[first]
        ));
    }
    Ok(())
}

fn run_case(
    case: &str,
    frames: usize,
    candidates: &[(&str, Encoder)],
    external_check: impl Fn(&str, &[u8]) -> Result<(), String>,
) -> bool {
    let pcm = generate_pcm(frames);
    println!(
        "\n{case}: {frames} frames ({:.3}s) @ {} Hz, {} ch, {}-bit, {} input chunks (up to {} frames)",
        frames as f64 / f64::from(SAMPLE_RATE),
        SAMPLE_RATE,
        CHANNELS,
        BITS,
        frames.div_ceil(BLOCK_FRAMES),
        BLOCK_FRAMES,
    );
    let expected = pcm_as_i16(&pcm);
    let raw_pcm_bytes = expected.len() * std::mem::size_of::<i16>();

    let mut passed = true;
    for &(label, encode_fn) in candidates {
        println!("\n=== {label} ===");
        let encoded = match encode_fn(&pcm) {
            Ok(encoded) => encoded,
            Err(error) => {
                eprintln!("  -> {label} encode failed: {error}");
                passed = false;
                continue;
            }
        };
        println!(
            "  encoded: {} bytes (from {raw_pcm_bytes} bytes raw PCM)",
            encoded.bytes.len()
        );
        println!(
            "  packets: {} before flush, {} on flush; {} input chunks emitted no packet",
            encoded.packets_before_flush, encoded.packets_on_flush, encoded.feeds_without_packets
        );

        if let Err(error) = external_check(label, &encoded.bytes) {
            eprintln!("  -> {error}");
            passed = false;
        }
        match decode_with_symphonia(encoded.bytes)
            .and_then(|decoded| verify_pcm(&decoded, &expected))
        {
            Ok(()) => println!(
                "  -> symphonia decode: BIT-EXACT MATCH ({} samples)",
                expected.len()
            ),
            Err(error) => {
                eprintln!("  -> symphonia verification failed: {error}");
                passed = false;
            }
        }
    }
    passed
}

fn main() -> ExitCode {
    let aligned = run_case(
        "Aligned input",
        ALIGNED_FRAMES,
        &CANDIDATES,
        run_external_flac_check,
    );
    let partial = run_case(
        "Partial input tail",
        PARTIAL_FRAMES,
        &CANDIDATES,
        run_external_flac_check,
    );
    if aligned && partial {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_pcm_has_expected_interleaved_length() {
        let pcm = generate_pcm(17);
        assert_eq!(pcm.len(), 17 * CHANNELS);
        assert_eq!(pcm.len() % CHANNELS, 0);
    }

    #[test]
    fn both_encoders_round_trip_aligned_and_partial_input() {
        for &(label, encode) in &CANDIDATES {
            for frames in [4 * BLOCK_FRAMES, 4 * BLOCK_FRAMES + 240] {
                let pcm = generate_pcm(frames);
                let encoded = encode(&pcm).unwrap();
                if label == "oxideav-flac" {
                    assert_eq!(encoded.packets_before_flush, 1);
                    assert_eq!(encoded.packets_on_flush, 1);
                    assert_eq!(
                        encoded.feeds_without_packets,
                        frames.div_ceil(BLOCK_FRAMES) - 1
                    );
                } else {
                    assert_eq!(encoded.packets_before_flush, frames.div_ceil(BLOCK_FRAMES));
                    assert_eq!(encoded.packets_on_flush, 0);
                    assert_eq!(encoded.feeds_without_packets, 0);
                }
                let decoded = decode_with_symphonia(encoded.bytes).unwrap();
                verify_pcm(&decoded, &pcm_as_i16(&pcm))
                    .unwrap_or_else(|error| panic!("{label}, {frames} frames: {error}"));
            }
        }
    }

    #[test]
    fn verification_rejects_a_truncated_or_changed_signal() {
        let pcm = generate_pcm(BLOCK_FRAMES + 240);
        let encoded = via_flacenc::encode(&pcm).unwrap();
        let decoded = decode_with_symphonia(encoded.bytes).unwrap();
        let mut expected = pcm_as_i16(&pcm);
        assert!(verify_pcm(&decoded[..decoded.len() - CHANNELS], &expected)
            .unwrap_err()
            .contains("sample count mismatch"));
        expected[7] += 1;
        assert!(verify_pcm(&decoded, &expected)
            .unwrap_err()
            .contains("index 7"));
    }

    #[test]
    fn drain_only_stops_normally_on_need_more() {
        use oxideav_core::Error;

        let mut bytes = Vec::new();
        assert_eq!(
            drain_packets(&mut bytes, || Err(Error::NeedMore)).unwrap(),
            0
        );
        assert!(matches!(
            drain_packets(&mut bytes, || Err(Error::InvalidData("bad packet".into()))),
            Err(Error::InvalidData(_))
        ));
    }

    #[test]
    fn a_failed_check_marks_the_case_failed_without_skipping_other_candidates() {
        fn altered_encoder(pcm: &[i32]) -> EncodeResult {
            let mut altered = pcm.to_vec();
            altered[0] += 1;
            via_flacenc::encode(&altered)
        }

        let checked = std::cell::Cell::new(0);
        let candidates: [(&str, Encoder); 2] =
            [("altered", altered_encoder), ("good", via_flacenc::encode)];
        assert!(!run_case("mismatch", BLOCK_FRAMES, &candidates, |_, _| {
            checked.set(checked.get() + 1);
            Ok(())
        }));
        assert_eq!(checked.get(), 2);

        let good = &candidates[1..];
        assert!(!run_case(
            "external rejection",
            BLOCK_FRAMES,
            good,
            |_, _| { Err("reference decoder rejected stream".into()) }
        ));
        assert!(run_case("success", BLOCK_FRAMES, good, |_, _| Ok(())));
    }

    #[test]
    fn i32_to_i16_conversion_is_saturating() {
        let src = [0, 123, i16::MAX as i32, i16::MIN as i32, 40000, -50000];
        let got = pcm_as_i16(&src);
        assert_eq!(got[0], 0);
        assert_eq!(got[1], 123);
        assert_eq!(got[2], i16::MAX);
        assert_eq!(got[3], i16::MIN);
        assert_eq!(got[4], i16::MAX);
        assert_eq!(got[5], i16::MIN);
    }
}
