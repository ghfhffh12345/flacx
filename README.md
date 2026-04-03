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

See [`crates/flacx-cli/README.md`](crates/flacx-cli/README.md) for CLI usage
details.

## Workspace commands

```bash
cargo build --workspace
cargo test --workspace
cargo run -p flacx-cli -- --help
cargo run -p flacx --release --example benchmark
```

## Performance note

The library and CLI still use the same tuned encode engine. The `test-wavs/`
benchmark contract remains the regression baseline for throughput and encoded
size.

## Releases

Tagged releases use `v*` tags:

- final tags publish the `flacx` library crate to crates.io and create a
  GitHub release
- prerelease tags such as `v1.2.3-rc1` create a GitHub prerelease only
- GitHub release pages rely on GitHub's built-in tagged source archive, so no
  binaries or extra source bundles are attached

See [`docs/releasing.md`](docs/releasing.md) for the release workflow details,
required crates.io secret setup, and manual recovery notes.

## Documentation

- [`crates/flacx/README.md`](crates/flacx/README.md) — library crate usage
- [`crates/flacx-cli/README.md`](crates/flacx-cli/README.md) — CLI crate usage
- [`docs/releasing.md`](docs/releasing.md) — tag-driven release automation
