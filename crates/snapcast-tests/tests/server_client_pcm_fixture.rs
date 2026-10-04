use std::f32::consts::PI;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::PathBuf;

use snapcast_client::ClientEvent;
use snapcast_tests::{connect_client, expect_event, start_server};

const SAMPLE_RATE: u32 = 48_000;
const CHANNELS: usize = 2;
const BITS_PER_SAMPLE: u16 = 16;
const TONE_HZ: f32 = 1000.0;
const DURATION_SECS: u32 = 10;
const CHUNK_FRAMES: usize = 960; // 20 ms at 48 kHz

fn fixture_path() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("snapcast_fixture_{nanos}.pcm"))
}

fn write_stereo_pcm_fixture(path: &PathBuf) {
    let file = std::fs::File::create(path).expect("create fixture file");
    let mut writer = BufWriter::new(file);

    let total_frames = (SAMPLE_RATE * DURATION_SECS) as usize;
    let amp = 0.5_f32;

    for n in 0..total_frames {
        let t = n as f32 / SAMPLE_RATE as f32;
        let s = (amp * (2.0 * PI * TONE_HZ * t).sin() * i16::MAX as f32) as i16;
        let bytes = s.to_le_bytes();
        // Interleaved stereo: L, R with same sine sample.
        writer.write_all(&bytes).expect("write left sample");
        writer.write_all(&bytes).expect("write right sample");
    }

    writer.flush().expect("flush fixture file");
}

async fn stream_fixture_to_server(
    path: PathBuf,
    audio_tx: tokio::sync::mpsc::Sender<snapcast_server::AudioFrame>,
) {
    let file = std::fs::File::open(&path).expect("open fixture file");
    let mut reader = BufReader::new(file);

    let bytes_per_frame = CHANNELS * (BITS_PER_SAMPLE as usize / 8);
    let chunk_bytes = CHUNK_FRAMES * bytes_per_frame;
    let mut buf = vec![0_u8; chunk_bytes];

    let mut timestamp_usec: i64 = 1_000_000_000;
    let chunk_usecs: i64 = (CHUNK_FRAMES as i64 * 1_000_000) / SAMPLE_RATE as i64;

    loop {
        match reader.read_exact(&mut buf) {
            Ok(()) => {
                let mut samples = Vec::with_capacity(CHUNK_FRAMES * CHANNELS);
                for bytes in buf.chunks_exact(2) {
                    let s = i16::from_le_bytes([bytes[0], bytes[1]]);
                    samples.push(s as f32 / 32768.0);
                }

                audio_tx
                    .send(snapcast_server::AudioFrame {
                        data: snapcast_server::AudioData::F32(samples),
                        timestamp_usec,
                    })
                    .await
                    .expect("send audio frame to server");

                timestamp_usec += chunk_usecs;
            }
            Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(err) => panic!("failed reading fixture: {err}"),
        }
    }
}

#[tokio::test]
async fn server_distributes_and_client_consumes_pcm_fixture() {
    let path = fixture_path();
    write_stereo_pcm_fixture(&path);

    let server = start_server().await;
    let mut client = connect_client(server.port).await;

    let sender_path = path.clone();
    let sender = tokio::spawn(stream_fixture_to_server(sender_path, server.audio_tx.clone()));

    expect_event(&mut client.events, 10_000, |e| match e {
        ClientEvent::StreamStarted { codec, format } => {
            assert!(!codec.is_empty(), "codec should be announced");
            assert_eq!(format.rate(), SAMPLE_RATE);
            assert_eq!(format.channels(), CHANNELS as u16);
            Some(())
        }
        _ => None,
    })
    .await;

    // Confirm the client actually receives audio decoded from the fixture.
    let mut received_samples = 0usize;
    let target_samples = SAMPLE_RATE as usize * CHANNELS; // >= 1 second interleaved
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);

    while received_samples < target_samples {
        let frame = tokio::time::timeout_at(deadline, client.audio_rx.recv())
            .await
            .expect("timed out waiting for decoded audio")
            .expect("audio channel closed");

        assert_eq!(frame.sample_rate, SAMPLE_RATE);
        assert_eq!(frame.channels, CHANNELS as u16);
        received_samples += frame.samples.len();
    }

    // Full 10s playout is still real-time; waiting for complete drain can make
    // CI slower and occasionally flaky. Once we've proven decoded fixture audio
    // is flowing end-to-end, stop the sender task.
    sender.abort();
    let _ = sender.await;

    let _ = std::fs::remove_file(&path);
}
