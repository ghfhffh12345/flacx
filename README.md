# flacx workspace

High-performance WAV/FLAC conversion in Rust.

This repository is a Cargo workspace with two user-facing crates:

- `crates/flacx` — the publishable library crate
- `crates/flacx-cli` — the CLI crate built on the same pipeline, but not published

The workspace also includes maintainer docs and local development aids. Public
documentation stays focused on the supported library and CLI workflows rather
than ignored local benchmark fixtures.

## Workspace layout

```text
.
├─ crates/
│  ├─ flacx/       # publishable library crate
│  └─ flacx-cli/   # workspace CLI crate
├─ docs/           # maintainer documentation
└─ benchmarks/     # ignored local benchmark tooling
```

## Quick start

### Library crate

Add the library crate to your project:

```toml
[dependencies]
flacx = "0.6.0"
```

Encode WAV to FLAC from Rust:

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

Decode FLAC back to WAV:

```rust
use flacx::Decoder;

Decoder::default()
    .decode_file("input.flac", "output.wav")
    .unwrap();
```

For the complete library user guide, see
[`crates/flacx/README.md`](crates/flacx/README.md).

### CLI crate

Build the release binary from the workspace root:

```bash
cargo build --release
```

Run `flacx` from `target/release/` or after adding that directory to your
`PATH`:

```bash
flacx encode input.wav -o output.flac --level 8 --threads 4
flacx encode album-dir -o encoded-album --depth 0
flacx decode input.flac -o output.wav --threads 4
flacx decode encoded-album -o decoded-album --depth 0
```

The command surface is:

```text
flacx encode <input> [-o <output-or-dir>] [--level <0-8>] [--threads <n>] [--block-size <n>] [--depth <n>]
flacx decode <input> [-o <output-or-dir>] [--threads <n>] [--depth <n>] [--strict-channel-mask-provenance]
```

Common behavior:

- single-file input with no `-o` writes a sibling output file next to the
  source file
- directory input with no `-o` writes sibling outputs next to each discovered
  file
- directory input with `-o <dir>` preserves relative subpaths under the
  destination root
- `--depth` applies only to directory input and uses `0` for unlimited
  traversal
- encode defaults to `--threads 8`
- decode defaults to the library's decode configuration when `--threads` is
  omitted
- interactive terminals show live progress for both encode and decode

See [`crates/flacx-cli/README.md`](crates/flacx-cli/README.md) for the full CLI
guide and flag behavior.

## Workspace commands

```bash
cargo build --workspace
cargo test --workspace
cargo doc --workspace --no-deps
cargo run -p flacx-cli -- --help
```

## Performance note

The library and CLI share the same tuned encode/decode pipeline. Local benchmark
inputs and other ignored development fixtures live outside the published
documentation surface, so normal users do not need them to build or use the
workspace.

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

- [`crates/flacx/README.md`](crates/flacx/README.md) — library user guide and
  API overview
- [`crates/flacx-cli/README.md`](crates/flacx-cli/README.md) — CLI user guide
- [`docs/releasing.md`](docs/releasing.md) — maintainer release workflow
