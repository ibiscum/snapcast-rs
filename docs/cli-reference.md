# snapclient-rs and snapserver-rs CLI reference

This document explains all command-line arguments and options for:

- `snapclient-rs`
- `snapserver-rs`

Notes:

- Values shown as defaults are current at the time of writing.
- Some flags are feature-gated and only appear in `--help` if that feature is enabled.

## snapclient-rs

Usage:

```bash
snapclient-rs [OPTIONS] [URL]
```

### Positional argument

- `URL` (optional)
  - Format: `tcp://<host>[:<port>]`
  - Examples:
    - `tcp://192.168.1.10:1704`
    - `tcp://[::1]:1704`
    - `tcp://snapserver.local:1704`
  - If omitted:
    - with mDNS feature: client tries discovery (`_snapcast._tcp`)
    - without mDNS feature: defaults to `tcp://localhost:1704`

### Options

- `-i, --instance <INSTANCE>`
  - Instance id for multiple client instances on one host.
  - Default: `1`

- `--hostID <HOST_ID>`
  - Unique host id (default is MAC-derived when empty).
  - Default: empty string

- `--cert <CERTIFICATE>`
  - Client certificate file (PEM).

- `--cert-key <CERTIFICATE_KEY>`
  - Client private key file (PEM).

- `--key-password <KEY_PASSWORD>`
  - Password for encrypted private key.

- `--server-cert [<SERVER_CERTIFICATE>]`
  - TLS server CA certificate (PEM).
  - Without value: use default certificates.

- `-l, --list`
  - List PCM output devices.

- `-s, --soundcard <SOUNDCARD>`
  - PCM device index or name.
  - Default: `default`

- `--latency <LATENCY>`
  - Additional audio-device latency in milliseconds.
  - Default: `0`

- `--sampleformat <SAMPLEFORMAT>`
  - Requested output format `<rate>:<bits>:<channels>`.
  - Example: `48000:16:2`

- `--player <PLAYER>`
  - Player backend and optional params: `<name>[:<params>]`.
  - Default: empty string

- `--mixer <MIXER>`
  - Mixer mode and optional params: `software|hardware|script|none[:<params>]`.
  - Default: `software`

- `-d, --daemon [<DAEMON>]` (Unix)
  - Daemonize process.
  - Optional value is process priority (nice), valid range `-20..19`.

- `--user <USER>` (Unix)
  - User or user/group to run as in daemon mode (`user[:group]`).

- `--logsink <LOGSINK>`
  - Log output sink: `null|system|stdout|stderr|file:<path>`.
  - Default: `stdout`

- `--logfilter <LOGFILTER>`
  - Log filters: `<tag>:<level>[,<tag>:<level>]*`.
  - Default: `*:info`

- `--encryption-psk <ENCRYPTION_PSK>` (feature: `encryption`)
  - Pre-shared key for `f32lz4e` decryption.
  - If omitted, built-in default key is used.

- `-h, --help`
  - Print help.

- `-V, --version`
  - Print version.

### client examples

```bash
# explicit server
snapclient-rs tcp://192.168.1.50:1704

# IPv6
snapclient-rs "tcp://[fe80::1234%wlan0]:1704"

# list output devices
snapclient-rs --list

# choose device and add latency
snapclient-rs --soundcard default --latency 80 tcp://snapserver.local:1704
```

## snapserver-rs

Usage:

```bash
snapserver-rs [OPTIONS]
```

### Options

- `-c, --config <CONFIG>`
  - Config file path.
  - Default: `/etc/snapserver.conf`

- `--stream-port <STREAM_PORT>`
  - TCP port for binary audio protocol (client connections).

- `--stream-bind-address <STREAM_BIND_ADDRESS>`
  - Bind address for binary audio protocol.

- `--control-port <CONTROL_PORT>`
  - TCP port for JSON-RPC control.

- `--control-bind-address <CONTROL_BIND_ADDRESS>`
  - Bind address for JSON-RPC control.

- `--http-port <HTTP_PORT>`
  - HTTP port for JSON-RPC and Snapweb.

- `--http-bind-address <HTTP_BIND_ADDRESS>`
  - Bind address for HTTP endpoint.

- `--doc-root <DOC_ROOT>`
  - Path to Snapweb static files.

- `--buffer <BUFFER>`
  - Audio buffer size in milliseconds.

- `--codec <CODEC>`
  - Default codec.
  - Supported values include: `f32lz4`, `f32lz4e`, `pcm`, `flac`, `opus`, `ogg`.

- `--sampleformat <SAMPLEFORMAT>`
  - Default sample format (`<rate>:<bits>:<channels>`).

- `--encryption-psk <ENCRYPTION_PSK>` (feature: `encryption`)
  - Pre-shared key for `f32lz4e` encryption.

- `--source <SOURCES>`
  - Stream source URI (repeatable).
  - Can be passed multiple times.

- `--auth`
  - Require authentication on control/HTTP/WebSocket APIs.

- `--auth-secret <AUTH_SECRET>`
  - Secret to sign/verify control API auth tokens.
  - Required if `--auth` is enabled.

- `--mdns-disable` (feature: `mdns`)
  - Disable mDNS advertisement.

- `--mdns-name <MDNS_NAME>` (feature: `mdns`)
  - mDNS service name (default service name is Snapserver).

- `--logfilter <LOGFILTER>`
  - Log filter string.
  - Default: `info`

- `-h, --help`
  - Print help.

- `-V, --version`
  - Print version.

## source URI reference for --source

Format:

```text
scheme:///<path-or-endpoint>?key=value&key=value
```

Common query params:

- `name=<stream-name>` stream name shown in status/control APIs
- `sampleformat=<rate>:<bits>:<channels>` source format override

### pipe source

- Scheme: `pipe`
- Path: FIFO path
- Example:

```text
pipe:///run/snapcast/snapfifo?name=Music&sampleformat=48000:16:2
```

### file source

- Scheme: `file`
- Path: PCM/WAV file path
- Reader loops on EOF and paces playback in real time.
- Example:

```text
file:///home/pi/music.raw?name=File&sampleformat=48000:16:2
```

### process source

- Scheme: `process`
- Path: executable path
- Query params:
  - `params=<arg string>` process argument string
- Example:

```text
process:///usr/bin/ffmpeg?name=Tone&sampleformat=48000:16:2&params=-f lavfi -i sine=frequency=1000:sample_rate=48000 -f s16le -ac 2 -ar 48000 -
```

### tcp source

- Scheme: `tcp`
- Endpoint: `tcp://<host>[:port]`
- Default port if omitted: `4953`
- Example:

```text
tcp://0.0.0.0:4953?name=TcpIn&sampleformat=48000:16:2
```

### librespot source

- Scheme: `librespot`
- Starts `librespot` internally and reads stdout PCM.
- Query params:
  - `devicename` default `Snapcast`
  - `bitrate` default `320`
  - `username`
  - `password`
  - `cache`
  - `volume` default `100`
  - `normalize` `true|false` default `false`
  - `autoplay` `true|false` default `false`
- Example:

```text
librespot:///librespot?name=Spotify&sampleformat=48000:16:2&devicename=LivingRoom&bitrate=320
```

### airplay source

- Scheme: `airplay`
- Starts `shairport-sync` internally and reads stdout PCM.
- Query params:
  - `devicename` default `Snapcast`
  - `port` default `5000`
  - `password` optional
- Example:

```text
airplay:///shairport-sync?name=AirPlay&sampleformat=44100:16:2&devicename=Kitchen&port=5000
```

## Practical server examples

```bash
# minimal
snapserver-rs --source "pipe:///tmp/snapfifo?name=Music"

# multiple sources
snapserver-rs \
  --source "pipe:///run/snapcast/radio.pcm?name=Radio&sampleformat=48000:16:2" \
  --source "process:///usr/bin/ffmpeg?name=Tone&sampleformat=48000:16:2&params=-f lavfi -i sine=frequency=1000:sample_rate=48000 -f s16le -ac 2 -ar 48000 -"

# secure control API
snapserver-rs --auth --auth-secret "change-me" --source "pipe:///tmp/snapfifo?name=Music"
```

## Notes on precedence

- `snapserver-rs` loads config file first, then applies CLI overrides.
- Repeatable `--source` values override config sources when provided on CLI.
