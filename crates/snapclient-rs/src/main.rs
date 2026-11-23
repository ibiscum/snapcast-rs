mod cli;
mod logging;
mod mixer;
mod player;

use std::time::Duration;

use anyhow::Context;
use clap::Parser;
use snapcast_client::{ClientCommand, ClientConfig, ClientEvent, SnapClient};

fn main() -> anyhow::Result<()> {
    let cli = cli::Cli::parse();

    logging::init(&cli.logsink, &cli.logfilter)?;

    if cli.list {
        return list_devices(&cli.player);
    }

    #[cfg(feature = "encryption")]
    let encryption_psk = cli.encryption_psk.clone();
    #[cfg(feature = "mdns")]
    let mut settings = cli.into_settings()?;
    #[cfg(not(feature = "mdns"))]
    let settings = cli.into_settings()?;

    #[cfg(unix)]
    let daemon_startup = settings.daemon.as_ref().map(daemonize).transpose()?;

    // mDNS discovery for service identifiers (including default `_snapcast._tcp`)
    #[cfg(feature = "mdns")]
    if let Some(service_type) = mdns_service_type_from_host(&settings.server.host) {
        tracing::info!(service = %service_type, "Resolving mDNS service");
        match discover_snapcast(&service_type) {
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

    let result = rt.block_on(async {
        #[cfg(unix)]
        let (mut interrupt, mut terminate) = {
            use tokio::signal::unix::{SignalKind, signal};
            (
                signal(SignalKind::interrupt()).context("failed to listen for SIGINT")?,
                signal(SignalKind::terminate()).context("failed to listen for SIGTERM")?,
            )
        };
        let shutdown_signal = async {
            #[cfg(unix)]
            {
                tokio::select! {
                    _ = interrupt.recv() => Ok("SIGINT"),
                    _ = terminate.recv() => Ok("SIGTERM"),
                }
            }
            #[cfg(not(unix))]
            {
                tokio::signal::ctrl_c()
                    .await
                    .context("failed to listen for Ctrl-C")?;
                Ok("Ctrl-C")
            }
        };

        #[cfg(unix)]
        if let Some(mut startup) = daemon_startup {
            use std::io::Write as _;
            startup
                .write_all(&[1])
                .context("failed to acknowledge daemon startup")?;
        }

        let (mut client, events, audio_rx) = SnapClient::new(config);
        let cmd = client.command_sender();

        // Audio output: cpal callback reads from Stream directly
        let player_stream = std::sync::Arc::clone(&client.stream);
        let player_tp = std::sync::Arc::clone(&client.time_provider);
        let player_vol = volume_state.clone();
        let soundcard = settings.player.pcm_device.name.clone();
        let (stop_player, player_stop) = tokio::sync::watch::channel(());
        let mut player_task = tokio::spawn(player::play_audio(
            audio_rx,
            player_stream,
            player_tp,
            player_vol,
            soundcard,
            player_stop,
        ));

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
                        notify_status(&format!(
                            "Volume: {}%{}",
                            volume,
                            if muted { " (muted)" } else { "" }
                        ));
                    }
                    ClientEvent::TimeSyncComplete { diff_ms } => {
                        tracing::info!(diff_ms, "Time sync complete");
                        #[cfg(target_os = "linux")]
                        notify_service(sd_notify::NotifyState::Ready);
                    }
                    ClientEvent::StreamStarted { codec, format } => {
                        tracing::info!(%codec, %format, "Stream started");
                    }
                    #[cfg(feature = "custom-protocol")]
                    ClientEvent::CustomMessage(msg) => {
                        tracing::info!(type_id = msg.type_id, "Custom message received");
                    }
                }
            }
        });

        let mut run_task = tokio::spawn(async move { client.run().await });
        let exit = tokio::select! {
            run_res = &mut run_task => Ok(run_res),
            received = shutdown_signal => Err(received),
        };
        drop(stop_player);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        let client_result = async {
            match exit {
                Ok(result) => result.context("client task join failed")?,
                Err(received) => {
                    let received: anyhow::Result<&str> = received;
                    if let Ok(signal) = &received {
                        tracing::info!(signal, "Shutting down");
                    }
                    let result = tokio::time::timeout_at(deadline, async {
                        if cmd.send(ClientCommand::Stop).await.is_err() {
                            tracing::debug!("client command channel already closed");
                        }
                        (&mut run_task).await
                    })
                    .await
                    .context("timeout stopping network client")?
                    .context("client task join failed")?;
                    received?;
                    result
                }
            }
        };
        let player_result = async {
            tokio::time::timeout_at(deadline, &mut player_task)
                .await
                .context("timeout stopping audio player")?
                .context("audio player task join failed")
        };
        let (client_result, player_result) = tokio::join!(client_result, player_result);
        match (client_result, player_result) {
            (Err(client_error), Err(player_error)) => {
                tracing::error!(error = %player_error, "Audio shutdown failed");
                Err(client_error)
            }
            (client_result, player_result) => client_result.and(player_result),
        }
    });
    // Native audio calls may not be interruptible. The player was already given
    // its shutdown deadline; don't wait indefinitely for a stuck backend here.
    rt.shutdown_background();
    result?;

    tracing::info!("snapclient-rs terminated");
    Ok(())
}

#[cfg(target_os = "linux")]
fn notify_service(state: sd_notify::NotifyState<'_>) {
    if let Err(error) = sd_notify::notify(&[state]) {
        tracing::warn!(%error, "Service notification failed");
    }
}

#[cfg(target_os = "linux")]
fn notify_status(status: &str) {
    notify_service(sd_notify::NotifyState::Status(status));
}

fn list_devices(player: &str) -> anyhow::Result<()> {
    let player_name = player.split(':').next().unwrap_or("");

    #[cfg(target_os = "linux")]
    {
        if !matches!(player_name, "" | "cpal" | "alsa" | "pulse" | "pipewire") {
            println!(
                "Unknown player '{player_name}', listing CPAL output devices on Linux instead:\n"
            );
        }
        list_devices_linux()
    }

    #[cfg(target_os = "macos")]
    {
        match player_name {
            "coreaudio" | "" => {
                println!("0: Default Output\nCoreAudio default output device\n");
            }
            _ => println!("No device listing available for '{player_name}'"),
        }
        Ok(())
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        println!("No device listing available for '{player_name}'");
        Ok(())
    }
}

#[cfg(target_os = "linux")]
fn list_devices_linux() -> anyhow::Result<()> {
    use cpal::traits::{DeviceTrait, HostTrait};

    let host = cpal::default_host();
    let default_id = host.default_output_device().and_then(|d| d.id().ok());

    let devices = host
        .output_devices()
        .context("Failed to enumerate output devices");
    list_output_devices(devices, |idx, device| {
        let description = device.description();
        let name = match &description {
            Ok(description) => description.name(),
            Err(error) => {
                tracing::debug!(%error, idx, "Could not read audio device description");
                "<unknown>"
            }
        };
        let is_default = default_id
            .as_ref()
            .is_some_and(|default_id| device.id().is_ok_and(|id| &id == default_id));

        if is_default {
            println!("{idx}: {name} (default)");
        } else {
            println!("{idx}: {name}");
        }

        match device.supported_output_configs() {
            Ok(configs) => {
                let mut has_any = false;
                for cfg in configs {
                    has_any = true;
                    println!(
                        "  - {} ch, {:?}, {}..{} Hz",
                        cfg.channels(),
                        cfg.sample_format(),
                        cfg.min_sample_rate(),
                        cfg.max_sample_rate()
                    );
                }
                if !has_any {
                    println!("  - no supported output configs reported");
                }
            }
            Err(err) => {
                println!("  - failed to read supported output configs: {err}");
            }
        }
    })
}

#[cfg(target_os = "linux")]
fn list_output_devices<D>(
    devices: anyhow::Result<impl Iterator<Item = D>>,
    mut print_device: impl FnMut(usize, D),
) -> anyhow::Result<()> {
    let mut devices = devices?.enumerate().peekable();
    println!("Output devices (CPAL):");
    if devices.peek().is_none() {
        println!("No output devices found");
    }
    for (idx, device) in devices {
        print_device(idx, device);
    }
    Ok(())
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
fn daemonize(
    daemon: &snapcast_client::config::DaemonSettings,
) -> anyhow::Result<std::os::unix::net::UnixStream> {
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

    let (mut parent, child) = std::os::unix::net::UnixStream::pair()
        .context("failed to create daemon startup channel")?;
    unsafe {
        let pid = libc::fork();
        if pid < 0 {
            return Err(std::io::Error::last_os_error()).context("fork failed");
        }
        if pid > 0 {
            drop(child);
            wait_for_daemon_startup(&mut parent)?;
            std::process::exit(0);
        }
        drop(parent);
        if libc::setsid() < 0 {
            anyhow::bail!("setsid failed: {}", std::io::Error::last_os_error());
        }
    }

    Ok(child)
}

#[cfg(unix)]
fn wait_for_daemon_startup(mut reader: impl std::io::Read) -> anyhow::Result<()> {
    let mut ack = [0];
    reader
        .read_exact(&mut ack)
        .context("daemon exited before confirming startup")?;
    anyhow::ensure!(ack == [1], "invalid daemon startup acknowledgement");
    Ok(())
}

#[cfg(feature = "mdns")]
fn discover_snapcast(service_type: &str) -> anyhow::Result<(String, u16)> {
    use std::time::Duration;
    let mdns = mdns_sd::ServiceDaemon::new()?;
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

#[cfg(feature = "mdns")]
fn mdns_service_type_from_host(host: &str) -> Option<String> {
    const DEFAULT_SERVICE: &str = "_snapcast._tcp";
    let host = if host.trim().is_empty() {
        DEFAULT_SERVICE
    } else {
        host.trim()
    };
    let host = host.trim_end_matches('.');
    let service = host.strip_suffix(".local").unwrap_or(host);
    if service.starts_with('_') && service.ends_with("._tcp") {
        return Some(format!("{service}.local."));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn daemon_parent_distinguishes_startup_success_from_child_exit() {
        use std::{io::Write, os::unix::net::UnixStream};

        let (parent, child) = UnixStream::pair().unwrap();
        drop(child);
        let error = wait_for_daemon_startup(parent).unwrap_err();
        assert!(error.to_string().contains("before confirming startup"));

        let (parent, mut child) = UnixStream::pair().unwrap();
        child.write_all(&[1]).unwrap();
        wait_for_daemon_startup(parent).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn empty_device_listing_succeeds_but_enumeration_failure_does_not() {
        list_output_devices(Ok(std::iter::empty::<()>()), |_, _| {
            panic!("empty device list should not report a device");
        })
        .unwrap();

        let failed: anyhow::Result<std::iter::Empty<()>> =
            Err(anyhow::anyhow!("enumeration failed"));
        let error = list_output_devices(failed, |_, _| {
            panic!("failed enumeration should not report a device");
        })
        .unwrap_err();
        assert_eq!(error.to_string(), "enumeration failed");
    }

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

    #[cfg(feature = "mdns")]
    #[test]
    fn mdns_service_host_detection_matches_upstream_style_identifiers() {
        assert_eq!(
            mdns_service_type_from_host("_snapcast._tcp").as_deref(),
            Some("_snapcast._tcp.local.")
        );
        assert_eq!(
            mdns_service_type_from_host("_snapcast-https._tcp.local").as_deref(),
            Some("_snapcast-https._tcp.local.")
        );
        assert_eq!(mdns_service_type_from_host("snapserver.local"), None);
    }
}
