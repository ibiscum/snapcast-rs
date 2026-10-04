mod cli;
mod logging;
mod mixer;
mod player;

use clap::Parser;
use snapcast_client::{ClientCommand, ClientConfig, ClientEvent, SnapClient};

fn main() -> anyhow::Result<()> {
    let cli = cli::Cli::parse();

    logging::init(&cli.logsink, &cli.logfilter)?;

    if cli.list {
        list_devices(&cli.player);
        return Ok(());
    }

    #[cfg(feature = "encryption")]
    let encryption_psk = cli.encryption_psk.clone();
    #[cfg(feature = "mdns")]
    let mut settings = cli.into_settings()?;
    #[cfg(not(feature = "mdns"))]
    let settings = cli.into_settings()?;

    #[cfg(unix)]
    if let Some(ref daemon) = settings.daemon {
        daemonize(daemon)?;
    }

    // mDNS discovery if no host specified
    #[cfg(feature = "mdns")]
    if settings.server.host.is_empty() {
        tracing::info!("No server specified, browsing mDNS for _snapcast._tcp...");
        match discover_snapcast() {
            Ok((host, port)) => {
                settings.server.host = host;
                settings.server.port = port;
            }
            Err(e) => anyhow::bail!("mDNS discovery failed: {e}"),
        }
    }

    tracing::info!(
        server = %format!(
            "{}://{}:{}",
            settings.server.scheme, settings.server.host, settings.server.port
        ),
        instance = settings.instance,
        "snapclient-rs starting"
    );

    let mixer_str = mixer_spec(
        settings.player.mixer.mode,
        &settings.player.mixer.parameter,
    );
    let (mixer, volume_state) = mixer::Mixer::from_str(&mixer_str);
    let mixer = std::sync::Arc::new(mixer);

    let config = ClientConfig {
        scheme: settings.server.scheme.clone(),
        host: settings.server.host.clone(),
        port: settings.server.port,
        auth: settings.server.auth.clone(),
        #[cfg(feature = "tls")]
        server_certificate: settings.server.server_certificate.clone(),
        #[cfg(feature = "tls")]
        certificate: settings.server.certificate.clone(),
        #[cfg(feature = "tls")]
        certificate_key: settings.server.certificate_key.clone(),
        #[cfg(feature = "tls")]
        key_password: settings.server.key_password.clone(),
        #[cfg(feature = "encryption")]
        encryption_psk: Some(
            encryption_psk.unwrap_or_else(|| snapcast_proto::DEFAULT_ENCRYPTION_PSK.into()),
        ),
        instance: settings.instance,
        host_id: settings.host_id.clone(),
        latency: settings.player.latency,
        ..ClientConfig::default()
    };
    let rt = tokio::runtime::Runtime::new()?;

    rt.block_on(async {
        let (mut client, events, audio_rx) = SnapClient::new(config);
        let cmd = client.command_sender();

        // Audio output: cpal callback reads from Stream directly
        let player_stream = std::sync::Arc::clone(&client.stream);
        let player_tp = std::sync::Arc::clone(&client.time_provider);
        let player_vol = volume_state.clone();
        tokio::spawn(async move {
            player::play_audio(audio_rx, player_stream, player_tp, player_vol).await;
        });

        // Log events + apply volume
        let event_mixer = mixer.clone();
        let mut events = events;
        tokio::spawn(async move {
            while let Some(event) = events.recv().await {
                match event {
                    ClientEvent::Connected { host, port } => {
                        tracing::info!(host, port, "Connected");
                    }
                    ClientEvent::Disconnected { reason } => {
                        tracing::warn!(reason, "Disconnected");
                    }
                    ClientEvent::ServerSettings { volume, muted, .. } => {
                        tracing::info!(volume, muted, "Initial server settings received");
                        event_mixer.set_volume(volume_percent_to_u8(volume), muted);
                    }
                    ClientEvent::VolumeChanged { volume, muted } => {
                        tracing::info!(volume, muted, "Volume changed");
                        event_mixer.set_volume(volume_percent_to_u8(volume), muted);
                        #[cfg(target_os = "linux")]
                        {
                            let status = format!(
                                "Volume: {}%{}",
                                volume,
                                if muted { " (muted)" } else { "" }
                            );
                            let _ = sd_notify::notify(
                                false,
                                &[sd_notify::NotifyState::Status(&status)],
                            );
                        }
                    }
                    ClientEvent::TimeSyncComplete { diff_ms } => {
                        tracing::info!(diff_ms, "Time sync complete");
                        #[cfg(target_os = "linux")]
                        let _ = sd_notify::notify(false, &[sd_notify::NotifyState::Ready]);
                    }
                    ClientEvent::StreamStarted { codec, format } => {
                        tracing::info!(%codec, %format, "Stream started");
                        #[cfg(target_os = "linux")]
                        {
                            let status = format!(
                                "Playing {} ({} Hz, {} bits, {} ch)",
                                codec,
                                format.rate(),
                                format.bits(),
                                format.channels()
                            );
                            let _ = sd_notify::notify(
                                false,
                                &[sd_notify::NotifyState::Status(&status)],
                            );
                        }
                    }
                    #[cfg(feature = "custom-protocol")]
                    ClientEvent::CustomMessage(msg) => {
                        tracing::info!(type_id = msg.type_id, "Custom message received");
                    }
                }
            }
        });

        let mut run_task = tokio::spawn(async move { client.run().await });
        tokio::select! {
            run_res = &mut run_task => {
                run_res.map_err(|e| anyhow::anyhow!("client task join failed: {e}"))?
            }
            ctrl_c_res = tokio::signal::ctrl_c() => {
                ctrl_c_res.map_err(|e| anyhow::anyhow!("failed to listen for Ctrl-C: {e}"))?;
                tracing::info!("Received Ctrl-C, shutting down");
                if cmd.send(ClientCommand::Stop).await.is_err() {
                    tracing::warn!("failed to send stop command: client command channel closed");
                }
                match tokio::time::timeout(std::time::Duration::from_secs(2), run_task).await {
                    Ok(run_res) => run_res.map_err(|e| anyhow::anyhow!("client task join failed: {e}"))?,
                    Err(_) => anyhow::bail!("timeout waiting for graceful shutdown"),
                }
            }
        }
    })?;

    tracing::info!("snapclient-rs terminated");
    Ok(())
}

fn list_devices(player: &str) {
    let player_name = player.split(':').next().unwrap_or("");
    match player_name {
        #[cfg(target_os = "macos")]
        "coreaudio" | "" => {
            println!("0: Default Output\nCoreAudio default output device\n");
        }
        _ => println!("No device listing available for '{player_name}'"),
    }
}

fn mixer_spec(mode: snapcast_client::config::MixerMode, parameter: &str) -> String {
    match mode {
        snapcast_client::config::MixerMode::Software => "software".to_string(),
        snapcast_client::config::MixerMode::Hardware => format!("hardware:{parameter}"),
        snapcast_client::config::MixerMode::None => "none".to_string(),
        snapcast_client::config::MixerMode::Script => {
            tracing::warn!("Script mixer mode is not implemented in snapclient-rs, falling back to software");
            "software".to_string()
        }
    }
}

fn volume_percent_to_u8(volume: u16) -> u8 {
    if volume > 100 {
        tracing::warn!(volume, "Received out-of-range volume, clamping to 100");
        100
    } else {
        volume as u8
    }
}

#[cfg(unix)]
fn daemonize(daemon: &snapcast_client::config::DaemonSettings) -> anyhow::Result<()> {
    if let Some(priority) = daemon.priority {
        anyhow::ensure!(
            (-20..=19).contains(&priority),
            "invalid daemon priority {priority}; expected range -20..=19"
        );
        unsafe {
            if libc::setpriority(libc::PRIO_PROCESS, 0, priority) != 0 {
                anyhow::bail!(
                    "setpriority({priority}) failed: {}",
                    std::io::Error::last_os_error()
                );
            }
        }
        tracing::info!(priority, "Process priority set");
    }

    if let Some(ref user) = daemon.user {
        tracing::info!(user, "Would drop privileges to user (not yet implemented)");
    }

    unsafe {
        let pid = libc::fork();
        if pid < 0 {
            anyhow::bail!("fork failed");
        }
        if pid > 0 {
            std::process::exit(0);
        }
        if libc::setsid() < 0 {
            anyhow::bail!("setsid failed: {}", std::io::Error::last_os_error());
        }
    }

    tracing::info!("Daemonized");
    Ok(())
}

#[cfg(feature = "mdns")]
fn discover_snapcast() -> anyhow::Result<(String, u16)> {
    use std::time::Duration;
    let mdns = mdns_sd::ServiceDaemon::new()?;
    let service_type = "_snapcast._tcp.local.";
    let receiver = mdns.browse(service_type)?;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);

    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            if let Err(e) = mdns.stop_browse(service_type) {
                tracing::warn!(error = %e, "failed to stop mDNS browse");
            }
            anyhow::bail!("timed out after 5s");
        }
        match receiver.recv_timeout(remaining) {
            Ok(mdns_sd::ServiceEvent::ServiceResolved(info)) => {
                let host = info
                    .get_addresses()
                    .iter()
                    .next()
                    .map(|a| a.to_string())
                    .unwrap_or_else(|| info.get_hostname().trim_end_matches('.').to_string());
                let port = info.get_port();
                tracing::info!(host = %host, port, "Discovered snapserver via mDNS");
                if let Err(e) = mdns.stop_browse(service_type) {
                    tracing::warn!(error = %e, "failed to stop mDNS browse");
                }
                return Ok((host, port));
            }
            Ok(_) => continue,
            Err(_) => {
                if let Err(e) = mdns.stop_browse(service_type) {
                    tracing::warn!(error = %e, "failed to stop mDNS browse");
                }
                anyhow::bail!("mDNS discovery timed out after 5s");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixer_spec_maps_supported_modes() {
        assert_eq!(
            mixer_spec(snapcast_client::config::MixerMode::Software, ""),
            "software"
        );
        assert_eq!(
            mixer_spec(snapcast_client::config::MixerMode::Hardware, "hw:0"),
            "hardware:hw:0"
        );
        assert_eq!(mixer_spec(snapcast_client::config::MixerMode::None, ""), "none");
    }

    #[test]
    fn mixer_spec_script_falls_back_to_software() {
        assert_eq!(
            mixer_spec(snapcast_client::config::MixerMode::Script, "ignored"),
            "software"
        );
    }

    #[test]
    fn volume_percent_to_u8_clamps_to_percentage_range() {
        assert_eq!(volume_percent_to_u8(0), 0);
        assert_eq!(volume_percent_to_u8(42), 42);
        assert_eq!(volume_percent_to_u8(100), 100);
        assert_eq!(volume_percent_to_u8(101), 100);
        assert_eq!(volume_percent_to_u8(u16::MAX), 100);
    }
}
