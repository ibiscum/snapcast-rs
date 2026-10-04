# Use shairport-sync service as source for snapserver-rs

This guide provides copy-paste-ready systemd files for Raspberry Pi using:

- `shairport-sync.service` as AirPlay receiver
- `snapserver-rs.service` as Snapcast server
- a shared FIFO at `/run/snapcast/shairport-sync.pcm`

It uses systemd drop-ins so package-managed unit files stay untouched.

## 1) Install packages

```bash
sudo apt update
sudo apt install -y shairport-sync
```

## 2) Configure shairport-sync to write PCM to a FIFO

Edit `/etc/shairport-sync.conf` and set these values:

```conf
output_backend = "pipe";

pipe =
{
  name = "/run/snapcast/shairport-sync.pcm";
};
```

## 3) Create snapserver-rs drop-in

Create `/etc/systemd/system/snapserver-rs.service.d/10-shairport-pipe.conf`:

```ini
[Unit]
Wants=shairport-sync.service
After=network-online.target shairport-sync.service

[Service]
# Keep FIFO in /run (recreated at boot)
RuntimeDirectory=snapcast

# Recreate FIFO on each start to avoid stale inode/permissions
ExecStartPre=/usr/bin/rm -f /run/snapcast/shairport-sync.pcm
ExecStartPre=/usr/bin/mkfifo -m 0666 /run/snapcast/shairport-sync.pcm

# Replace source to read the AirPlay PCM stream
# Important: keep sampleformat aligned with shairport output
ExecStart=
ExecStart=/usr/local/bin/snapserver-rs --source "pipe:///run/snapcast/shairport-sync.pcm?name=AirPlay&sampleformat=44100:16:2"
```

Notes:

- The blank `ExecStart=` line is required in a drop-in to reset the original command.
- If your binary path differs, update `/usr/local/bin/snapserver-rs`.

## 3b) Variant: keep existing `ExecStart` untouched

If your current `snapserver-rs.service` already has the correct `--source ...` in
its original command line, use this variant to only prepare the FIFO.

Create `/etc/systemd/system/snapserver-rs.service.d/11-fifo-only.conf`:

```ini
[Unit]
Wants=shairport-sync.service
After=network-online.target shairport-sync.service

[Service]
RuntimeDirectory=snapcast
ExecStartPre=/usr/bin/rm -f /run/snapcast/shairport-sync.pcm
ExecStartPre=/usr/bin/mkfifo -m 0666 /run/snapcast/shairport-sync.pcm
```

Use this only when the existing `ExecStart` already references the same FIFO
path, for example:

```text
pipe:///run/snapcast/shairport-sync.pcm?name=AirPlay&sampleformat=44100:16:2
```

Quick check before restart:

```bash
systemctl cat snapserver-rs.service
```

## 4) Optional shairport ordering drop-in

Most setups work without this. If startup ordering is unreliable, create
`/etc/systemd/system/shairport-sync.service.d/10-snapcast-order.conf`:

```ini
[Unit]
After=network-online.target
Wants=network-online.target
```

## 5) Reload and restart

```bash
sudo systemctl daemon-reload
sudo systemctl restart shairport-sync.service
sudo systemctl restart snapserver-rs.service
```

Enable both on boot:

```bash
sudo systemctl enable shairport-sync.service snapserver-rs.service
```

## 6) Verify

```bash
systemctl status shairport-sync.service
systemctl status snapserver-rs.service
journalctl -u snapserver-rs.service -n 100 --no-pager
journalctl -u shairport-sync.service -n 100 --no-pager
```

## 7) If service names differ

Find your exact unit names and adapt paths above:

```bash
systemctl list-unit-files | grep -E "snapserver|shairport"
```

If your server unit is `snapserver.service` instead of `snapserver-rs.service`,
create the same drop-in under:

- `/etc/systemd/system/snapserver.service.d/10-shairport-pipe.conf`
