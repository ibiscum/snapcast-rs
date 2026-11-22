# flac-encoder-spike

A standalone verification tool, not part of the snapcast-rs workspace (see
the `[workspace]` marker in `Cargo.toml` — it deliberately roots its own,
separate Cargo workspace so it's invisible to the parent build and CI).

It answers one question: when fed PCM in the same 1152-frame blocks
snapserver-rs's FLAC encoder actually uses, with the header emitted once up
front (matching a live, never-ending stream), do candidate pure-Rust FLAC
encoders produce bitstreams that actually decode correctly?

Runs both aligned and partial-tail inputs (alignment refers to the
1,152-frame input chunks). It reports packets before and during flush, and
how many input chunks produced no packet. OxideAV defaults to 4,096-frame
output blocks, so a bit-exact round-trip does not imply one packet per input.

Symphonia performs a required bit-exact comparison. The optional C
reference check (`flac -t`) is marked skipped if `flac` is not installed
(`brew install flac` / `apt install flac`). Encode or decode errors, PCM
mismatches, and unsuccessful reference checks make the tool exit nonzero.

```bash
cd tools/flac-encoder-spike
cargo run --release
cargo test --locked
```

Full write-up, results, and the resulting decision:
[`docs/flac-encoder-evaluation.md`](../../docs/flac-encoder-evaluation.md).
