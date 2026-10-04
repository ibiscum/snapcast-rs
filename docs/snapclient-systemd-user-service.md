# snapclient-rs as a systemd user service on Raspberry Pi

This guide runs `snapclient-rs` as a user service (not a system service), with
PipeWire and real-time friendly runtime limits.

## 1) Prerequisites

Install audio/runtime packages:

```bash
sudo apt update
sudo apt install -y build-essential pkg-config libasound2-dev libavahi-compat-libdnssd1 libavahi-compat-libdnssd-dev curl ca-certificates pipewire pipewire-pulse wireplumber rtkit
```

Install `snapclient-rs` (for example from this repository build output):

```bash
sudo install -Dm755 /path/to/target/release/snapclient-rs /usr/local/bin/snapclient-rs
```

## 2) Verify user PipeWire services

```bash
systemctl --user status pipewire.service pipewire-pulse.service wireplumber.service
```

If needed:

```bash
systemctl --user enable --now pipewire.service pipewire-pulse.service wireplumber.service
```

## 3) Create user service

Create `~/.config/systemd/user/snapclient-rs.service`:

```ini
[Unit]
Description=snapclient-rs (user)
Wants=pipewire.service pipewire-pulse.service wireplumber.service
After=pipewire.service pipewire-pulse.service wireplumber.service network-online.target

[Service]
Type=simple
ExecStart=/usr/local/bin/snapclient-rs tcp://192.168.1.50:1704
Restart=on-failure
RestartSec=2

# Realtime-friendly limits
LimitRTPRIO=95
LimitMEMLOCK=infinity
Nice=-5
IOSchedulingClass=realtime
IOSchedulingPriority=0

# Hardening (optional)
NoNewPrivileges=yes
PrivateTmp=yes
ProtectSystem=strict
ProtectHome=read-only
ReadWritePaths=%h/.local/state

[Install]
WantedBy=default.target
```

Replace `tcp://192.168.1.50:1704` with your Snapserver address.

## 4) Enable and start

```bash
systemctl --user daemon-reload
systemctl --user enable --now snapclient-rs.service
```

Check status/logs:

```bash
systemctl --user status snapclient-rs.service
journalctl --user -u snapclient-rs.service -f
```

## 5) Start on boot without interactive login

Enable linger for the service user:

```bash
sudo loginctl enable-linger "$USER"
```

This keeps the user manager running after reboot so the user service starts at
boot.

## 6) Real-time troubleshooting

If the service starts but logs scheduling errors, check user limits and rtkit:

```bash
ulimit -r
systemctl status rtkit-daemon.service
```

If your distro/user policy restricts real-time priority, either:

- keep `LimitRTPRIO=95` and ensure PAM/system policy allows it, or
- temporarily remove `Nice` / `IOScheduling*` lines and run with default scheduling.

For ALSA-only deployments (without PipeWire), remove PipeWire units from
`Wants=`/`After=` and configure your ALSA device routing separately.

## 7) Troubleshooting: `Pipe not available` for `pipe:///...`

If `snapserver-rs` logs:

```text
Pipe not available, retrying path="/tmp/snapfifo" error=No such file or directory (os error 2)
```

the configured FIFO does not exist yet.

Create it before starting `snapserver-rs`:

```bash
rm -f /tmp/snapfifo
mkfifo /tmp/snapfifo
snapserver-rs --source "pipe:///tmp/snapfifo?name=Music"
```

Then feed audio from another shell:

```bash
ffmpeg -re -i music.mp3 -f s16le -ar 48000 -ac 2 pipe:1 > /tmp/snapfifo
```

For systemd services, avoid `/tmp` and create the FIFO via `ExecStartPre`:

```ini
RuntimeDirectory=snapcast
ExecStartPre=/usr/bin/rm -f /run/snapcast/snapfifo
ExecStartPre=/usr/bin/mkfifo -m 0666 /run/snapcast/snapfifo
ExecStart=/usr/local/bin/snapserver-rs --source pipe:///run/snapcast/snapfifo?name=Music
```

Why this happens:

- `/tmp` is often cleared on reboot.
- A FIFO is not a regular file; it must be created with `mkfifo`.
- If no writer is attached, stream input can stall.
