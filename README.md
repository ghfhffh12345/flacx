# flacx workspace

High-performance WAV/FLAC conversion in Rust.

The repository is now a Cargo workspace with a publishable **library crate**
and a separate **CLI crate** built on the same encode/decode pipeline.

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
flacx = "0.6.0"
```

Then encode WAV to FLAC from Rust:

```rust
use flacx::{Encoder, EncoderConfig, level::Level};

let config = EncoderConfig::builder()
    .level(Level::Level8)
    .threads(4)
    .build();

Encoder::new(config)
    .encode_file("input.wav", "output.flac")
    .unwrap();
```

And decode FLAC back to WAV:

```rust
use flacx::Decoder;

Decoder::default()
    .decode_file("input.flac", "output.wav")
    .unwrap();
```

See [`crates/flacx/README.md`](crates/flacx/README.md) for the crate-focused usage guide.

### CLI

Build the release binary from the workspace root:

```bash
cargo build --release
```

Then run `flacx` directly from `target/release/` (or after adding that directory to your `PATH`):

```bash
flacx encode input.wav -o output.flac --level 8 --threads 4
flacx encode album-dir -o encoded-album --depth 0
flacx decode input.flac -o output.wav --threads 4
flacx decode encoded-album -o decoded-album --depth 0
```

Supported CLI shape:

- `flacx encode <input> [-o <output-or-dir>] [--depth <depth>]`
- `flacx decode <input> [-o <output-or-dir>] [--depth <depth>]`
- encode-only flags:
  - `--output`
  - `--level`
  - `--threads`
  - `--block-size`
  - `--depth`
- decode-only flags:
  - `--output`
  - `--threads`
  - `--depth`

Encode/decode defaults and folder behavior:

- single-file input with no `-o` writes a sibling `.flac` next to the source WAV
- folder input with no `-o` writes `.flac` siblings next to each discovered WAV
- folder input with `-o <dir>` preserves relative subpaths under the destination root
- decode single-file input with no `-o` writes a sibling `.wav` next to the source FLAC
- decode folder input with no `-o` writes `.wav` siblings next to each discovered FLAC
- decode folder input with `-o <dir>` preserves relative subpaths under the destination root
- `--depth` defaults to `1`, affects directory input only, and uses `0` for unlimited traversal
- encode `--threads` defaults to `8`

Progress display:

- interactive terminals show a live progress line during encode and decode
- redirected or non-interactive runs do not emit progress UI
- single-file runs show the current filename, elapsed time, ETA, and rate
- folder runs show overall batch progress and per-file progress on separate live lines
- batch progress uses exact samples processed across the full planned worklist

See [`crates/flacx-cli/README.md`](crates/flacx-cli/README.md) for CLI usage details.

## Workspace commands

```bash
cargo build --workspace
cargo test --workspace
flacx --help
cargo run -p flacx --release --example benchmark
```

## Performance note

The library and CLI still use the same tuned encode engine, and now add a
matching narrow decode path. The `test-wavs/` benchmark contract remains the
regression baseline for throughput and encoded size on the encode side.

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
