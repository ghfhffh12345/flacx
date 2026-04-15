# flacx workspace

High-performance PCM-container/FLAC conversion and FLAC recompression in Rust.

> Warning: flacx is still experimental; APIs, CLI flags, and metadata details may change without notice.

This repository is a Cargo workspace with two user-facing crates:

- `crates/flacx` — the publishable library crate
- `crates/flacx-cli` — the CLI crate built on the same pipeline, but not published

The workspace also includes maintainer docs and local development aids. Public
documentation stays focused on the supported library and CLI workflows.

## Workspace layout

```text
.
├─ crates/
│  ├─ flacx/       # publishable library crate
│  └─ flacx-cli/   # workspace CLI crate
└─ docs/           # maintainer documentation
```

## Quick start

### Library

Add the library crate to your project:

```toml
[dependencies]
flacx = "0.8.2"
```

The library enables its built-in `wav`, `aiff`, and `caf` container families
by default; add `progress` when you want callback-driven progress from Rust.

Then encode a supported PCM container to FLAC from Rust:

```rust
use flacx::{EncoderConfig, read_pcm_reader, level::Level};
use std::{fs::File, io::BufWriter};

let config = EncoderConfig::builder()
    .level(Level::Level8)
    .threads(4)
    .build();

let reader = read_pcm_reader(File::open("input.wav").unwrap()).unwrap();
let metadata = reader.metadata().clone();
let stream = reader.into_pcm_stream();
let writer = BufWriter::new(File::create("output.flac").unwrap());
let mut encoder = config.into_encoder(writer);
encoder.set_metadata(metadata);
encoder.encode(stream).unwrap();
```

And decode FLAC back to a supported PCM container:

```rust
use flacx::{DecodeConfig, read_flac_reader};
use std::{fs::File, io::BufWriter};

let reader = read_flac_reader(File::open("input.flac").unwrap()).unwrap();
let metadata = reader.metadata().clone();
let stream = reader.into_pcm_stream();
let writer = BufWriter::new(File::create("output.wav").unwrap());
let mut decoder = DecodeConfig::default().into_decoder(writer);
decoder.set_metadata(metadata);
decoder.decode(stream).unwrap();
```

And recompress an existing FLAC stream through the same explicit inspect-first
flow:

```rust
use flacx::{FlacRecompressSource, RecompressConfig, read_flac_reader};
use std::{fs::File, io::BufWriter};

let reader = read_flac_reader(File::open("input.flac").unwrap()).unwrap();
let source = FlacRecompressSource::from_reader(reader);
let writer = BufWriter::new(File::create("recompressed.flac").unwrap());
let mut recompressor = RecompressConfig::default().into_recompressor(writer);
recompressor.recompress(source).unwrap();
```

If you want one-shot orchestration instead of the explicit reader/session path,
use `flacx::builtin::{encode_file, encode_bytes, decode_file, decode_bytes,
recompress_file, recompress_bytes}`.
The explicit core remains available under `flacx::core::{read_pcm_reader,
read_flac_reader, write_pcm_stream, EncoderConfig, DecodeConfig, RecompressConfig,
Encoder, Decoder, FlacRecompressSource, Recompressor}`.

See [`docs/flacx-user-guide.md`](docs/flacx-user-guide.md) for the task-oriented
crate usage guide, or [`crates/flacx/README.md`](crates/flacx/README.md) for the
public API architecture guide.

### CLI

Build the release binary from the workspace root:

```bash
cargo build --release
```

Then run `flacx` directly from `target/release/` (or after adding that directory to your `PATH`):

```bash
flacx encode input.wav -o output.flac --level 8 --threads 4
flacx encode album-dir -o encoded-album --depth 0
flacx decode input.flac -o output.aiff --threads 4
flacx decode encoded-album -o decoded-album --depth 0
flacx recompress input.flac -o recompressed.flac --level 5 --threads 4
flacx recompress album-dir -o recompressed-album --depth 0
```

Supported CLI shape:

- `flacx encode <input> [-o <output-or-dir>] [--depth <depth>]`
- `flacx decode <input> [-o <output-or-dir>] [--depth <depth>]`
- `flacx recompress <input> [-o <output-or-dir>] [--in-place] [--level <0-8>] [--threads <n>] [--block-size <samples>] [--mode <loose|default|strict>] [--depth <depth>]`
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
- recompress-only flags:
  - `--output`
  - `--in-place`
  - `--level`
  - `--threads`
  - `--block-size`
  - `--mode`
  - `--depth`

Encode/decode defaults and folder behavior:

- single-file input with no `-o` writes a sibling `.flac` next to the source PCM container
- folder input with no `-o` writes `.flac` siblings next to each discovered `.wav`, `.rf64`, `.w64`, `.aif`, `.aiff`, `.aifc`, or `.caf`
- folder input with `-o <dir>` preserves relative subpaths under the destination root
- decode single-file input with no `-o` writes a sibling `.wav` next to the source FLAC
- decode folder input with no `-o` writes `.wav` siblings next to each discovered FLAC
- decode folder input with `-o <dir>` preserves relative subpaths under the destination root
- decode explicit output paths may target `.wav`, `.rf64`, `.w64`, `.aif`, `.aiff`, `.aifc`, or `.caf`
- decode directory output-family overrides are explicit; without a selector, batch output still defaults to `.wav`
- recompress single-file input with no `-o` writes a sibling `.recompressed.flac`
- recompress directory input with no `-o` writes `.recompressed.flac` siblings next to each discovered FLAC
- recompress `--in-place` is explicit and only rewrites the source after successful output staging
- `--depth` defaults to `1`, affects directory input only, and uses `0` for unlimited traversal
- encode `--threads` defaults to `8`
- recompress `--threads` defaults to `8`
- raw PCM encode is explicit-only via `--raw` plus descriptor flags; generic `.raw` / `.pcm` files are not auto-discovered
- raw PCM remains ingest-only and is not a decode/output family

Progress display:

- interactive terminals show a live progress line during encode and decode
- interactive terminals show decode+encode phase progress during recompress
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

The library and CLI share the same tuned encode/decode pipeline. Benchmarks live
under each crate's `benches/` directory and run with `cargo bench`; normal users
do not need that workflow to build or use the workspace.

## Releases

Tagged releases use `v*` tags:

- final tags publish the `flacx` library crate to crates.io and create a GitHub
  release
- prerelease tags such as `v1.2.3-rc1` create a GitHub prerelease only
- GitHub release pages rely on the built-in tagged source archive; no binaries,
  installers, or separate CLI bundles are attached

See [`docs/releasing.md`](docs/releasing.md) for the release workflow details,
required secret setup, and manual recovery notes.

## Documentation map

- [`crates/flacx/README.md`](crates/flacx/README.md) — crate-level public API
  architecture guide
- [`docs/flacx-user-guide.md`](docs/flacx-user-guide.md) — task-oriented guide
  for using the `flacx` Rust crate
- [`docs/flacx-public-api-architecture.md`](docs/flacx-public-api-architecture.md) —
  expanded maintainer-oriented guide to the current public surface and source
  structure
- [`docs/flacx-ground-up-ownership-map.md`](docs/flacx-ground-up-ownership-map.md) —
  same-crate ownership map for the encode/decode spine and family boundaries
- [`docs/flacx-family-parity.md`](docs/flacx-family-parity.md) —
  review audit for WAV/AIFF/CAF parity and remaining naming caveats
- [`docs/flacx-major-refactor-review.md`](docs/flacx-major-refactor-review.md) —
  maintainer guide for the explicit-core / convenience-layer refactor
- [`docs/flacx-cli-encoding-performance-review.md`](docs/flacx-cli-encoding-performance-review.md) —
  maintainer guide for bounded directory-encode scheduling, progress ordering,
  and verification evidence
- [`crates/flacx-cli/README.md`](crates/flacx-cli/README.md) — CLI user guide
- [`docs/releasing.md`](docs/releasing.md) — maintainer release workflow
