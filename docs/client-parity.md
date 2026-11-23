# Snapcast client parity matrix

Reference: upstream C++ [`snapclient` in Snapcast v0.35.0][upstream], pinned to
[`f123734`][commit]. The Rust columns describe this repository, not a particular
published crate release. This is a source-based capability comparison, not a
certification of wire compatibility or audio behavior on every platform.

[`snapcast-client`][library] is the embeddable protocol and audio engine; it does
not open an audio device. [`snapclient-rs`][cli] adds the CLI, discovery, device
playback, and service lifecycle. **Supported** means wired into normal use,
not identical behavior; **Partial** means a narrower implementation;
**Experimental** means a transport exists but end-to-end playback is not
supported; **Out of scope** means the application owns the feature.

## Protocol, codecs, and synchronization

| Capability | Upstream `snapclient` | `snapcast-client` | `snapclient-rs` |
| --- | --- | --- | --- |
| TCP audio, Hello, server settings | Supported | Supported: `SnapClient` establishes and manages the session | Supported via `tcp://` |
| Basic credentials in Hello | Supported | Supported via `ClientConfig::auth` | Supported via URL user information; TCP is not encrypted |
| WebSocket audio (`ws://`) | Supported | Supported behind `websocket` feature | Supported behind `websocket` feature |
| TLS WebSocket audio (`wss://`), certificates | Supported in OpenSSL builds, including configurable server verification and client certificates | Supported behind `tls` feature, including optional custom CA and client certificate/key wiring | Supported behind `websocket` + `tls`; TLS flags are accepted only with `wss://` |
| PCM, FLAC, Opus, Ogg/Vorbis decode | Supported; compressed codecs depend on the build | Supported; Ogg/Vorbis uses wire codec name `ogg` | Supported via the library |
| `null` wire codec | Supported | Supported | Supported via the library; `--logsink null` remains unrelated |
| Clock sync, buffered playout, latency compensation | Supported | Supported via shared timing and stream state | Supported via the CPAL player and `--latency`; no claim of identical timing or drift behavior |
| Reconnect and graceful stop | Supported | Supported; reconnect loop and `ClientCommand::Stop` | Supported; stops the network client and audio worker on Ctrl-C or Unix SIGTERM |
| Volume and mute | Applies server settings; can report local changes from a capable player | Emits server settings and volume events; `ClientCommand::SetVolume` can report a local change | Applies server volume and mute; no local mixer change reporting |
| `f32lz4`, encrypted `f32lz4` payloads, custom binary messages | Not implemented in the reference client | Rust extensions behind `f32lz4`, `encryption`, and `custom-protocol`, respectively | `f32lz4` is enabled by default; `encryption` is optional; `custom-protocol` can log received messages but has no CLI send command |
| Embedding and decoded audio | Not the same public API as the Rust crate | Typed commands/events and timestamped, interleaved `f32` audio frames; no device required | Standalone player built on the library, not a separate embedding API |

## CLI, playback, and deployment

| Capability | Upstream `snapclient` | `snapcast-client` | `snapclient-rs` |
| --- | --- | --- | --- |
| Server address, host ID, instance | URL, host ID and instance options; legacy host/port flags | Host, port, host ID and instance in `ClientConfig` | Positional URL, `--hostID`, `--instance`; no legacy `--host` or `--port` |
| mDNS / DNS-SD | Build-dependent service discovery; supports service identifiers in URLs | Out of scope; embedding application supplies the address | Partial: with `mdns` enabled, omitted URL defaults to `_snapcast._tcp` and service identifiers in URL hosts (e.g. `_snapcast._tcp`, `_snapcast-https._tcp`) are resolved; without `mdns`, URL is required |
| Native output backends and device selection | Platform/build-dependent players, including ALSA, PulseAudio, PipeWire, CoreAudio, WASAPI, SDL2, and file output; supports player options and device selection | Out of scope | Partial: CPAL-backed output. `--list` enumerates CPAL devices on Linux; macOS displays a default-output entry. `--soundcard` supports `default`, CPAL index, or CPAL device name; non-CPAL backends and backend parameters are still rejected |
| Sample-format override / resampling | Exposes `--sampleformat` on applicable builds | Optional `resampler` API; does not negotiate device formats itself | Partial: optional `resampler` adapts to a supported `f32` device rate at the same channel count. No manual `--sampleformat` or channel conversion |
| Mixer backends | Software, none, and build-dependent hardware and script mixers | Out of scope for local gain and hardware control | Partial: software, none, and Linux ALSA hardware (falls back to software if initialization fails); script mode warns and uses software |
| Daemon and service integration | Unix daemon mode with priority, user/group and PID-file handling | Out of scope | Partial: Unix daemon mode and priority, but no user/group switch. Linux systemd readiness indicates clock sync; playback status is reported separately |
| Log sinks and filters | Snapcast levels and tags; stdout, stderr, null, file, and platform system logging | Emits `tracing` events; consumer chooses the subscriber | Partial: Snapcast-style flags and `RUST_LOG` override; `system` is an alias for stderr, not syslog |

See the Rust [CLI reference](cli-reference.md) and crate manifests
([library][library-features], [binary][cli-features]) for exact flags and
feature defaults.

## Strict upstream option/env parity (`snapclient` vs `snapclient-rs`)

The table below is a strict option-by-option comparison against upstream
[`snapclient.cpp`][upstream-cli] and the current Rust CLI implementation
([`cli.rs`](../crates/snapclient-rs/src/cli.rs),
[`logging.rs`](../crates/snapclient-rs/src/logging.rs)).

### Command-line options

| Upstream option | `snapclient-rs` option | Status | Exact gap |
| --- | --- | --- | --- |
| positional `url` | positional `URL` | Match | Same explicit URI surface (`<scheme>://<host>[:port]`) and same omitted-URL behavior split: with `mdns` enabled, default to `tcp://_snapcast._tcp`; without `mdns`, require an explicit URL. |
| `-h`, `--host` (deprecated) | not available | Gap | Legacy host override is not implemented; `-h` is help in Clap. |
| `-p`, `--port` (deprecated) | not available | Gap | Legacy port override is not implemented. |
| `-i`, `--instance` | `-i`, `--instance` | Match | Same high-level purpose. |
| `--hostID` | `--hostID` | Match | Same high-level purpose. |
| `--cert` | `--cert` | Partial | Accepted only for `wss://`; rejected for `tcp://` and `ws://`. |
| `--cert-key` | `--cert-key` | Partial | Accepted only for `wss://`; rejected for `tcp://` and `ws://`. |
| `--key-password` | `--key-password` | Partial | Parsed for `wss://`, but encrypted private keys are currently unsupported. |
| `--server-cert[=<path>]` | `--server-cert[=<path>]` | Partial | Accepted only for `wss://`; rejected for `tcp://` and `ws://`. |
| `-l`, `--list` | `-l`, `--list` | Partial | Upstream lists backend-specific devices; Rust lists CPAL-facing devices only. |
| `-s`, `--soundcard` | `-s`, `--soundcard` | Match | Same selector semantics (`default`, index, or device name) for the active CPAL playback backend; broader backend parity is tracked under `--player`. |
| `--latency` | `--latency` | Match | Same high-level purpose. |
| `--sampleformat` | `--sampleformat` | Gap | Rust rejects manual sample-format override. |
| `--player` | `--player` | Gap | Upstream supports multiple backends/options; Rust allows only `cpal` without params. |
| `--sharingmode` | not available | Gap | Not implemented in Rust CLI. |
| `--mixer` | `--mixer` | Partial | Present but behavior narrower than upstream backend-dependent implementation. |
| `-d`, `--daemon[=<priority>]` | `-d`, `--daemon[=<priority>]` (Unix) | Partial | Daemon mode exists, but user/group/PID handling is not parity-complete. |
| `--user` | `--user` (rejected) | Gap | Option is parsed but intentionally unsupported in Rust. |
| `--logsink` | `--logsink` | Partial | `system` in Rust maps to stderr, not native system logger behavior. |
| `--logfilter` | `--logfilter` | Partial | Snapcast syntax is mapped to tracing directives; behavior is not identical. |
| `-v`, `--version` | `-V`, `--version` | Partial | Version output exists; short flag differs (`-v` upstream vs Clap `-V`). |
| `--groff` | not available | Gap | Not implemented in Rust CLI. |

### Environment variables

| Variable | Upstream `snapclient` | `snapclient-rs` | Status |
| --- | --- | --- | --- |
| `RUST_LOG` | Not used | Supported (overrides `--logfilter` when nonblank) | Difference |
| Runtime `SNAPCLIENT_*` config vars | No user-facing runtime vars documented in upstream client wiring | None (test-only vars exist in unit tests) | No parity surface |

## Parity implementation milestones and progression

Status values: **Planned** → **In progress** → **Merged** → **Released**.

| ID | Scope | Primary crate | Exit criteria | Status |
| --- | --- | --- | --- | --- |
| M1 | Add `null` wire codec parity | `snapcast-client` | `CodecHeader { codec: "null" }` initializes and chunk handling is covered by focused unit/integration regressions | Merged |
| M2 | Promote `ws://` from experimental to supported playback transport | `snapcast-client` (+ `snapclient-rs` wiring) | `ws://` accepted through connection selection, handshake/audio/reconnect/stop covered by automated tests against a compatible server | Merged |
| M3 | Promote `wss://` with certificate handling parity | `snapcast-client` (+ `snapclient-rs` wiring) | `wss://` session path validated with server verification and client-cert options; regression coverage for success and failure modes | In progress |
| M4 | Optional library-side audio-processing parity improvements | `snapcast-client` | Any added conversion/mixing behavior is feature-gated, documented, and regression-tested without requiring device backends | Planned |
| M5 | Improve CLI playback device-selection parity (`--soundcard`) | `snapclient-rs` | `--soundcard` accepts default device, CPAL index, and CPAL device name with clear out-of-range diagnostics and regression tests | Merged |

### Progress log

| Date (UTC) | Change | Milestone | Evidence |
| --- | --- | --- | --- |
| 2026-11-22 | Added this matrix and parity baseline against Snapcast v0.35.0 (`f123734`) | M1–M4 baseline | This document, linked from [README](../README.md) |
| 2026-11-22 | Agreed next execution order: `null` codec first, then `ws://`, then `wss://` | M1 → M2 → M3 | User direction in this session |
| 2026-11-22 | Implemented `null` decoder wiring and added controller regression coverage for `CodecHeader { codec: "null" }` | M1 | [null decoder](../crates/snapcast-client/src/decoder/null.rs), [controller test](../crates/snapcast-client/src/controller.rs) |
| 2026-11-22 | Enabled `ws://`/`wss://` scheme selection in client connection path and CLI URL parsing | M2 + M3 | [connection selection](../crates/snapcast-client/src/connection/mod.rs), [CLI parsing](../crates/snapclient-rs/src/cli.rs) |
| 2026-11-22 | Added WSS TLS option wiring for custom CA and client cert/key, with regressions for invalid combinations and encrypted keys | M3 | [WSS transport](../crates/snapcast-client/src/connection/wss.rs), [CLI validation](../crates/snapclient-rs/src/cli.rs) |
| 2026-11-22 | Added ws run-loop regressions for handshake/time-sync, wirechunk audio delivery, and reconnect backoff stop behavior | M2 | [ws run tests](../crates/snapcast-client/src/lib.rs), [ws transport tests](../crates/snapcast-client/src/connection/ws.rs) |
| 2026-11-23 | Added `--soundcard` CPAL selection by default/index/name and regression coverage for selector parsing and out-of-range error shape | M5 | [CLI mapping](../crates/snapclient-rs/src/cli.rs), [CPAL selection + tests](../crates/snapclient-rs/src/player/mod.rs), [CLI reference](./cli-reference.md) |

### Tracking rules

- Update the milestone **Status** when work starts, merges, and releases.
- Add one Progress log row per meaningful parity change (code + tests + docs).
- Keep evidence links concrete (PR, commit, file path, or test target).
- If scope changes, update both the milestone row and the parity matrix rows above.

## Evidence and limits

- **Upstream behavior:** [CLI and daemon wiring][upstream-cli],
  [transport implementation][upstream-transport],
  [codec and session handling][upstream-controller], and
  [build-dependent client features][upstream-build].
- **Rust behavior:** [client API][library-api], [session and codec handling][rust-controller],
  [transport module][rust-transport], and [CLI](../crates/snapclient-rs/src/cli.rs),
  [runtime](../crates/snapclient-rs/src/main.rs), and
  [player](../crates/snapclient-rs/src/player/mod.rs) wiring.
- **Tests:** The [C++ interoperability tests][interop] require an external
  `snapserver` and are ignored by default. They cover Hello, time sync, and
  codec header receipt, not bit-exact audio playback against the C++ server.
  The [Rust integration suite][rust-tests] covers Rust-to-Rust cases such as
  audio, authentication, and reconnects; the [CLI signal test][cli-test]
  exercises shutdown while the handshake is pending. These tests are not
  evidence of WebSocket playback or physical audio-device parity.

[upstream]: https://github.com/snapcast/snapcast/tree/f12373479243e93a97237592d6a3703539ec41d5/client
[commit]: https://github.com/snapcast/snapcast/commit/f12373479243e93a97237592d6a3703539ec41d5
[upstream-cli]: https://github.com/snapcast/snapcast/blob/f12373479243e93a97237592d6a3703539ec41d5/client/snapclient.cpp
[upstream-transport]: https://github.com/snapcast/snapcast/blob/f12373479243e93a97237592d6a3703539ec41d5/client/client_connection.cpp
[upstream-controller]: https://github.com/snapcast/snapcast/blob/f12373479243e93a97237592d6a3703539ec41d5/client/controller.cpp
[upstream-build]: https://github.com/snapcast/snapcast/blob/f12373479243e93a97237592d6a3703539ec41d5/client/CMakeLists.txt
[library]: ../crates/snapcast-client
[cli]: ../crates/snapclient-rs
[library-features]: ../crates/snapcast-client/Cargo.toml
[cli-features]: ../crates/snapclient-rs/Cargo.toml
[library-api]: ../crates/snapcast-client/src/lib.rs
[rust-controller]: ../crates/snapcast-client/src/controller.rs
[rust-transport]: ../crates/snapcast-client/src/connection/mod.rs
[interop]: ../crates/snapcast-client/tests/integration.rs
[rust-tests]: ../crates/snapcast-tests/tests
[cli-test]: ../crates/snapclient-rs/tests/shutdown.rs
