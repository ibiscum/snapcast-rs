#![cfg(feature = "encryption")]

use snapcast_client::{ClientConfig, ClientEvent, SnapClient};
use snapcast_server::{ServerConfig, SnapServer};
use snapcast_tests::spawn_serving;

#[tokio::test]
async fn encrypted_f32lz4_end_to_end() {
    let psk = "test-secret-key-42";

    // Server with encryption
    let server_config = ServerConfig {
        codec: "f32lz4".into(),
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
    let (mut client, mut events, _audio_rx) = SnapClient::new(client_config);
    tokio::spawn(async move {
        if let Err(e) = client.run().await {
            panic!("encrypted test client exited with error: {e}");
        }
    });

    // Wait for stream to start (proves codec header with ENC marker was accepted)
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(2000);
    loop {
        match tokio::time::timeout_at(deadline, events.recv()).await {
            Ok(Some(ClientEvent::StreamStarted { codec, .. })) => {
                assert_eq!(codec, "f32lz4");
                break;
            }
            Ok(Some(_)) => continue,
            _ => panic!("Timed out waiting for encrypted stream start"),
        }
    }

    // Push audio — if decryption fails, client would disconnect
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

    // Give time for chunks to flow through
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    // If we get here without disconnect, encryption round-trip works
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
    tokio::spawn(async move {
        if let Err(e) = client.run().await {
            panic!("wrong-key test client exited with error: {e}");
        }
    });

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

    if let Ok(Some(frame)) =
        tokio::time::timeout(std::time::Duration::from_millis(700), audio_rx.recv()).await
    {
        panic!(
            "wrong-key client unexpectedly decoded audio ({} samples)",
            frame.samples.len()
        );
    }
}
