# flacx workspace

High-performance WAV-to-FLAC encoding in Rust.

The repository is now a Cargo workspace with a publishable **library crate**
and a separate **CLI crate** built on the same encode pipeline.

## Workspace layout

```text
.
├─ crates/
│  ├─ flacx/      # publishable library crate
│  └─ flacx-cli/  # non-published CLI crate
├─ docs/
├─ test-wavs/
└─ benchmarks/
```

## Quick start

### Library

Add the library crate to your project:

```toml
[dependencies]
flacx = "0.1.0"
```

Then encode WAV to FLAC from Rust:

```rust
use flacx::{EncodeOptions, FlacEncoder, level::Level};

let options = EncodeOptions::default()
    .with_level(Level::Level8)
    .with_threads(4);

FlacEncoder::new(options)
    .encode_file("input.wav", "output.flac")
    .unwrap();
```

See [`crates/flacx/README.md`](crates/flacx/README.md) for the crate-focused
usage guide.

### CLI

Run the workspace CLI crate:

```bash
cargo run -p flacx-cli -- encode input.wav output.flac --level 8 --threads 4
```

Supported CLI shape:

- `flacx encode <input> <output>`
- `--level`
- `--threads`
- `--block-size`

## Workspace commands

```bash
cargo build --workspace
cargo test --workspace
cargo run -p flacx-cli -- --help
cargo run -p flacx --release --example benchmark
```

## What changed in v5

- single combined package → Cargo workspace
- library and CLI moved into separate crates
- only the library crate is prepared for crates.io publication
- package/layout cleanup only; codec behavior is unchanged

## Performance note

The library and CLI still use the same tuned encode engine. The `test-wavs/`
benchmark contract remains the regression baseline for throughput and encoded
size.

## Migration

- [`docs/migration-v4.md`](docs/migration-v4.md) — v4 API redesign
- [`docs/migration-v5.md`](docs/migration-v5.md) — v5 workspace/package split
