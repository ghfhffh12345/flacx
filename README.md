# flacx

High-performance WAV-to-FLAC encoding in Rust.

`flacx` keeps a single tuned encode pipeline and exposes it through both a Rust API and a CLI.

## CLI quickstart

```bash
cargo run -- encode input.wav output.flac --level 8 --threads 4
```

Supported CLI shape in v4:

- `flacx encode <input> <output>`
- `--level`
- `--threads`
- `--block-size`

## Library quickstart

```rust
use flacx::{EncodeOptions, FlacEncoder, level::Level};

let options = EncodeOptions::default()
    .with_level(Level::Level8)
    .with_threads(4);

FlacEncoder::new(options)
    .encode_file("input.wav", "output.flac")
    .unwrap();
```

## Performance note

The v4 API/CLI surface is designed to preserve the v3 benchmark contract on `test-wavs/`: keep the same single encode engine, maintain throughput, and avoid compression-ratio regressions.

## Migration

See [`docs/migration-v4.md`](docs/migration-v4.md) for the v4 API redesign and migration notes.
