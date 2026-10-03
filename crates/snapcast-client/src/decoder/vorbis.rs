//! Vorbis decoder using symphonia.
//!
//! The snapserver sends Ogg/Vorbis stream headers as the CodecHeader,
//! then Ogg pages containing Vorbis audio data as WireChunk payloads.

use anyhow::{Result, bail};
use snapcast_proto::SampleFormat;
use snapcast_proto::message::codec_header::CodecHeader;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{CODEC_TYPE_VORBIS, CodecParameters, DecoderOptions};
use symphonia::core::formats::Packet;

use crate::decoder::Decoder;
use crate::stream::SampleEncoding;

/// Parse the Vorbis identification header from an Ogg bitstream.
///
/// The CodecHeader payload is a complete Ogg bitstream containing the 3 Vorbis
/// header packets. We parse the first Ogg page to find the Vorbis identification
/// header and extract sample rate and channels.
///
/// Default bit depth is 16 (Vorbis is float internally; the C++ code defaults to 16).
fn parse_vorbis_header(payload: &[u8]) -> Result<(SampleFormat, Vec<u8>)> {
    // Find OggS capture pattern
    if payload.len() < 28 || &payload[0..4] != b"OggS" {
        bail!("not an Ogg bitstream");
    }

    // Ogg page header: 27 bytes fixed + segment table
    let num_segments = payload[26] as usize;
    let header_size = 27 + num_segments;
    if payload.len() < header_size {
        bail!("Ogg page header truncated");
    }

    // First packet starts after the page header
    let packet_start = header_size;
    let remaining = &payload[packet_start..];

    // Vorbis identification header: type(1) + "vorbis"(6) + version(4) + channels(1) + rate(4)
    if remaining.len() < 16 {
        bail!("Vorbis identification header too small");
    }
    if remaining[0] != 1 || &remaining[1..7] != b"vorbis" {
        bail!("not a Vorbis identification header");
    }

    let channels = remaining[11] as u16;
    let sample_rate = u32::from_le_bytes(remaining[12..16].try_into().unwrap());

    if sample_rate == 0 || channels == 0 {
        bail!("invalid Vorbis header: rate={sample_rate}, channels={channels}");
    }

    // Vorbis is float internally; output as f32 to preserve full precision.
    let sf = SampleFormat::new(sample_rate, 32, channels);

    Ok((sf, payload.to_vec()))
}

/// Vorbis audio decoder using symphonia.
pub struct VorbisDecoder {
    decoder: Box<dyn symphonia::core::codecs::Decoder>,
    sample_format: SampleFormat,
    packet_id: u64,
}

impl VorbisDecoder {
    fn from_header(header: &CodecHeader) -> Result<Self> {
        let (sf, extra_data) = parse_vorbis_header(&header.payload)?;
        let mut params = CodecParameters::new();
        params
            .for_codec(CODEC_TYPE_VORBIS)
            .with_sample_rate(sf.rate())
            .with_channels(
                symphonia::core::audio::Channels::from_bits(((1u64 << sf.channels()) - 1) as u32)
                    .unwrap_or(symphonia::core::audio::Channels::FRONT_LEFT),
            )
            .with_extra_data(extra_data.into_boxed_slice());
        let decoder = symphonia::default::get_codecs()
            .make(&params, &DecoderOptions::default())
            .map_err(|e| anyhow::anyhow!("failed to create Vorbis decoder: {e}"))?;
        Ok(Self {
            decoder,
            sample_format: sf,
            packet_id: 0,
        })
    }
}

impl Decoder for VorbisDecoder {
    fn set_header(&mut self, header: &CodecHeader) -> Result<SampleFormat> {
        *self = Self::from_header(header)?;
        Ok(self.sample_format)
    }

    fn decode(&mut self, data: &mut Vec<u8>) -> Result<bool> {
        if data.is_empty() {
            return Ok(false);
        }

        tracing::trace!(
            codec = "vorbis",
            input_bytes = data.len(),
            packet_id = self.packet_id,
            "decode"
        );

        let packet = Packet::new_from_slice(0, self.packet_id, 0, data);
        self.packet_id += 1;

        let decoded = match self.decoder.decode(&packet) {
            Ok(buf) => buf,
            Err(e) => {
                tracing::warn!(codec = "vorbis", error = %e, "decode failed");
                return Ok(false);
            }
        };

        let spec = *decoded.spec();
        let frames = decoded.frames() as u64;

        let mut sample_buf = SampleBuffer::<f32>::new(frames, spec);
        sample_buf.copy_interleaved_ref(decoded);

        let mut out = Vec::with_capacity(sample_buf.samples().len() * 4);
        for &s in sample_buf.samples() {
            out.extend_from_slice(&s.to_le_bytes());
        }

        *data = out;
        Ok(true)
    }

    fn output_encoding(&self) -> SampleEncoding {
        SampleEncoding::Float32
    }
}

/// Create a VorbisDecoder from a CodecHeader.
pub fn create(header: &CodecHeader) -> Result<VorbisDecoder> {
    VorbisDecoder::from_header(header)
}

#[cfg(test)]
mod tests {
    use super::*;
    use symphonia::core::audio::{AsAudioBufferRef, AudioBuffer, Channels, SignalSpec};
    use symphonia::core::codecs::{CodecDescriptor, CodecParameters, DecoderOptions, FinalizeResult};
    use symphonia::core::errors::Error as SymphoniaError;
    use symphonia::core::formats::Packet;

    struct AlwaysErrDecoder {
        params: CodecParameters,
        last: AudioBuffer<i16>,
    }

    impl AlwaysErrDecoder {
        fn new() -> Self {
            Self {
                params: CodecParameters::new(),
                last: AudioBuffer::new(0, SignalSpec::new(48_000, Channels::FRONT_LEFT)),
            }
        }
    }

    impl symphonia::core::codecs::Decoder for AlwaysErrDecoder {
        fn try_new(_params: &CodecParameters, _options: &DecoderOptions) -> symphonia::core::errors::Result<Self> {
            Ok(Self::new())
        }

        fn supported_codecs() -> &'static [CodecDescriptor] {
            &[]
        }

        fn reset(&mut self) {}

        fn codec_params(&self) -> &CodecParameters {
            &self.params
        }

        fn decode(&mut self, _packet: &Packet) -> symphonia::core::errors::Result<symphonia::core::audio::AudioBufferRef<'_>> {
            Err(SymphoniaError::DecodeError("forced decoder failure"))
        }

        fn finalize(&mut self) -> FinalizeResult {
            FinalizeResult::default()
        }

        fn last_decoded(&self) -> symphonia::core::audio::AudioBufferRef<'_> {
            self.last.as_audio_buffer_ref()
        }
    }

    fn make_test_decoder() -> VorbisDecoder {
        VorbisDecoder {
            decoder: Box::new(AlwaysErrDecoder::new()),
            sample_format: SampleFormat::new(44_100, 32, 2),
            packet_id: 0,
        }
    }

    /// Build a minimal Ogg page containing a Vorbis identification header.
    /// 44100 Hz, 2 channels.
    fn ogg_vorbis_header_44100_2() -> Vec<u8> {
        let mut page = Vec::new();

        // -- Vorbis identification header packet --
        let mut vorbis_id = Vec::new();
        vorbis_id.push(1u8); // packet type = identification
        vorbis_id.extend_from_slice(b"vorbis");
        vorbis_id.extend_from_slice(&0u32.to_le_bytes()); // version
        vorbis_id.push(2); // channels
        vorbis_id.extend_from_slice(&44100u32.to_le_bytes()); // sample rate
        vorbis_id.extend_from_slice(&0i32.to_le_bytes()); // bitrate max
        vorbis_id.extend_from_slice(&128000i32.to_le_bytes()); // bitrate nominal
        vorbis_id.extend_from_slice(&0i32.to_le_bytes()); // bitrate min
        vorbis_id.push(0xb8); // blocksize 0=256, 1=2048 → (8<<4)|11 = 0xb8
        vorbis_id.push(1); // framing bit

        let packet_len = vorbis_id.len();

        // -- Ogg page header --
        page.extend_from_slice(b"OggS"); // capture pattern
        page.push(0); // version
        page.push(0x02); // header type: beginning of stream
        page.extend_from_slice(&0u64.to_le_bytes()); // granule position
        page.extend_from_slice(&1u32.to_le_bytes()); // serial number
        page.extend_from_slice(&0u32.to_le_bytes()); // page sequence
        page.extend_from_slice(&0u32.to_le_bytes()); // CRC (skip for test)
        page.push(1); // 1 segment
        page.push(packet_len as u8); // segment size

        // -- Packet data --
        page.extend_from_slice(&vorbis_id);

        page
    }

    #[test]
    fn parse_header_44100_2() {
        let payload = ogg_vorbis_header_44100_2();
        let (sf, _) = parse_vorbis_header(&payload).unwrap();
        assert_eq!(sf.rate(), 44100);
        assert_eq!(sf.channels(), 2);
        assert_eq!(sf.bits(), 32);
    }

    #[test]
    fn parse_header_48000_6() {
        let mut page = Vec::new();
        let mut vorbis_id = Vec::new();
        vorbis_id.push(1u8);
        vorbis_id.extend_from_slice(b"vorbis");
        vorbis_id.extend_from_slice(&0u32.to_le_bytes());
        vorbis_id.push(6); // 6 channels
        vorbis_id.extend_from_slice(&48000u32.to_le_bytes());
        // pad to 16 bytes minimum
        vorbis_id.resize(30, 0);

        let packet_len = vorbis_id.len();
        page.extend_from_slice(b"OggS");
        page.push(0);
        page.push(0x02);
        page.extend_from_slice(&0u64.to_le_bytes());
        page.extend_from_slice(&1u32.to_le_bytes());
        page.extend_from_slice(&0u32.to_le_bytes());
        page.extend_from_slice(&0u32.to_le_bytes());
        page.push(1);
        page.push(packet_len as u8);
        page.extend_from_slice(&vorbis_id);

        let (sf, _) = parse_vorbis_header(&page).unwrap();
        assert_eq!(sf.rate(), 48000);
        assert_eq!(sf.channels(), 6);
    }

    #[test]
    fn not_ogg_fails() {
        assert!(parse_vorbis_header(b"NOPE_not_ogg_data_at_all!!!!!").is_err());
    }

    #[test]
    fn not_vorbis_fails() {
        let mut page = Vec::new();
        page.extend_from_slice(b"OggS");
        page.push(0);
        page.push(0);
        page.extend_from_slice(&[0; 20]); // rest of ogg header
        page.push(1); // 1 segment
        page.push(16); // segment size
        page.extend_from_slice(&[0; 16]); // not a vorbis packet
        assert!(parse_vorbis_header(&page).is_err());
    }

    #[test]
    fn ogg_page_header_truncated_fails() {
        let mut page = Vec::new();
        page.extend_from_slice(b"OggS");
        page.resize(27, 0);
        page[26] = 4; // claims 4 lacing bytes, but none follow
        assert!(parse_vorbis_header(&page).is_err());
    }

    #[test]
    fn invalid_vorbis_header_zero_channels_fails() {
        let mut page = ogg_vorbis_header_44100_2();
        // Identification header channel byte offset in this helper layout.
        let channels_offset = 28 + 1 + 6 + 4;
        page[channels_offset] = 0;
        assert!(parse_vorbis_header(&page).is_err());
    }

    #[test]
    fn invalid_vorbis_header_zero_rate_fails() {
        let mut page = ogg_vorbis_header_44100_2();
        // Identification header sample-rate bytes in this helper layout.
        let sample_rate_offset = 27 + 1 + 6 + 4 + 1;
        page[sample_rate_offset..sample_rate_offset + 4].copy_from_slice(&0u32.to_le_bytes());
        assert!(parse_vorbis_header(&page).is_err());
    }

    #[test]
    fn output_encoding_is_float32() {
        let dec = make_test_decoder();
        assert_eq!(dec.output_encoding(), SampleEncoding::Float32);
    }

    #[test]
    fn decode_empty_input_returns_false() {
        let mut dec = make_test_decoder();
        let mut data = Vec::new();
        assert!(!dec.decode(&mut data).unwrap());
    }

    #[test]
    fn decode_invalid_frame_returns_false() {
        let mut dec = make_test_decoder();
        let mut data = vec![0xFF, 0x00, 0xAA, 0x55];
        assert!(!dec.decode(&mut data).unwrap());
    }

    #[test]
    fn set_header_error_keeps_existing_state() {
        let mut dec = make_test_decoder();
        let header = CodecHeader {
            codec: "vorbis".into(),
            payload: b"not an ogg stream".to_vec(),
        };
        assert!(dec.set_header(&header).is_err());
        assert_eq!(dec.sample_format.rate(), 44_100);
        assert_eq!(dec.sample_format.bits(), 32);
        assert_eq!(dec.sample_format.channels(), 2);
    }
}
