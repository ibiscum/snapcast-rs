#![cfg(feature = "encryption")]

use std::time::Duration;

use snapcast_client::{ClientCommand, ClientConfig, ClientEvent, SampleFormat, SnapClient};
use snapcast_server::{ServerConfig, SnapServer};
use snapcast_tests::spawn_serving;

#[tokio::test]
async fn encrypted_f32lz4_end_to_end() {
    let psk = "test-secret-key-42";

    // Server with encryption
    let server_config = ServerConfig {
        codec: "f32lz4".into(),
        sample_format: "48000:16:2".into(),
        encryption_psk: Some(psk.into()),
        ..ServerConfig::default()
    };
    let (mut server, _events) = SnapServer::new(server_config);
    let audio_tx = server.add_stream("default");
    let port = spawn_serving(server).await;

    // Client with matching key
    let client_config = ClientConfig {
        host: "127.0.0.1".into(),
        port,
        encryption_psk: Some(psk.into()),
        ..ClientConfig::default()
    };
    let (mut client, mut events, mut audio_rx) = SnapClient::new(client_config);
    let cmd = client.command_sender();
    let client_task = tokio::spawn(async move { client.run().await });

    // Wait for stream to start (proves codec header with ENC marker was accepted)
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(2000);
    loop {
        match tokio::time::timeout_at(deadline, events.recv()).await {
            Ok(Some(ClientEvent::StreamStarted { codec, format })) => {
                assert_eq!(codec, "f32lz4");
                assert_eq!(format, SampleFormat::new(48_000, 32, 2));
                break;
            }
            Ok(Some(_)) => continue,
            _ => panic!("Timed out waiting for encrypted stream start"),
        }
    }

    let samples: Vec<f32> = (0..960).map(|i| (i as f32 / 960.0) * 2.0 - 1.0).collect();
    for _ in 0..5 {
        audio_tx
            .send(snapcast_server::AudioFrame {
                data: snapcast_server::AudioData::F32(samples.clone()),
                timestamp_usec: 0,
            })
            .await
            .unwrap();
    }

    let frame = tokio::time::timeout(Duration::from_secs(2), audio_rx.recv())
        .await
        .expect("matching-key client did not decode audio")
        .expect("matching-key client exited before delivering audio");
    assert_eq!(frame.sample_rate, 48_000);
    assert_eq!(frame.channels, 2);
    assert_eq!(frame.samples, samples);
    stop_client(cmd, client_task).await;
}

#[tokio::test]
async fn wrong_key_drops_encrypted_audio() {
    // Server with encryption
    let server_config = ServerConfig {
        codec: "f32lz4".into(),
        encryption_psk: Some("server-key".into()),
        ..ServerConfig::default()
    };
    let (mut server, _events) = SnapServer::new(server_config);
    let audio_tx = server.add_stream("default");
    let port = spawn_serving(server).await;

    // Client with WRONG key
    let client_config = ClientConfig {
        host: "127.0.0.1".into(),
        port,
        encryption_psk: Some("wrong-key".into()),
        ..ClientConfig::default()
    };
    let (mut client, mut events, mut audio_rx) = SnapClient::new(client_config);
    let cmd = client.command_sender();
    let client_task = tokio::spawn(async move { client.run().await });

    // Client should still connect (encryption is on the codec, not the handshake)
    // But audio decryption will fail silently (chunks dropped)
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(2000);
    loop {
        match tokio::time::timeout_at(deadline, events.recv()).await {
            Ok(Some(ClientEvent::StreamStarted { .. })) => break, // connected OK
            Ok(Some(_)) => continue,
            _ => panic!("Timed out — client should still connect with wrong key"),
        }
    }

    // Push encrypted audio from server. Wrong-key decryption must fail and drop
    // chunks, so the client should decode nothing onto audio_rx.
    let samples: Vec<f32> = (0..960).map(|i| (i as f32 / 960.0) * 2.0 - 1.0).collect();
    for i in 0..10 {
        audio_tx
            .send(snapcast_server::AudioFrame {
                data: snapcast_server::AudioData::F32(samples.clone()),
                timestamp_usec: 1_000_000 + (i as i64) * 10_000,
            })
            .await
            .unwrap();
    }

    let deadline = tokio::time::sleep(Duration::from_millis(700));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            biased;
            audio = audio_rx.recv() => match audio {
                Some(frame) => panic!("wrong-key client decoded {} samples", frame.samples.len()),
                None => panic!("wrong-key client exited instead of dropping audio"),
            },
            event = events.recv() => match event {
                Some(ClientEvent::Disconnected { reason }) => panic!("wrong-key client disconnected: {reason}"),
                None => panic!("wrong-key client closed the event channel"),
                _ => {}
            },
            () = &mut deadline => break,
        }
    }
    stop_client(cmd, client_task).await;
}

async fn stop_client(
    cmd: tokio::sync::mpsc::Sender<ClientCommand>,
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
) {
    tokio::time::timeout(Duration::from_secs(2), async {
        cmd.send(ClientCommand::Stop).await.unwrap();
        task.await.unwrap().unwrap();
    })
    .await
    .expect("client did not stop");
}
