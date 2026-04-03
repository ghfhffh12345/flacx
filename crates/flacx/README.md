# flacx

High-performance WAV-to-FLAC encoding for Rust.

`flacx` is the publishable library crate in the workspace. It provides the
shared encode pipeline used by both Rust callers and the sibling CLI crate.

## Add to your project

```toml
[dependencies]
flacx = "0.1.0"
```

## Quick start

```rust
use flacx::{EncodeOptions, FlacEncoder, level::Level};

let options = EncodeOptions::default()
    .with_level(Level::Level8)
    .with_threads(4);

FlacEncoder::new(options)
    .encode_file("input.wav", "output.flac")
    .unwrap();
```

## Primary API surface

- `EncodeOptions`
- `FlacEncoder`
- `encode_file`
- `encode_bytes`

## Optional progress feature

Progress support is behind the optional `progress` Cargo feature and is
disabled by default.

```toml
[dependencies]
flacx = { version = "0.1.0", features = ["progress"] }
```

When enabled, the additional progress-specific API surface includes:

- `EncodeProgress`
- `FlacEncoder::encode_with_progress`
- `FlacEncoder::encode_file_with_progress`

## Current scope

- WAV-to-FLAC encoding only
- seekable input/output API
- current supported WAV subset of the encoder

## Out of scope

- decoding
- metadata editing
- non-seekable output
- broader WAV support beyond the current engine envelope

## Workspace note

The CLI lives in a separate workspace crate, `flacx-cli`, and is not bundled
into the publishable library package.

## Progress note

`FlacEncoder` can optionally report real encode progress while keeping the
existing fast encode path intact. The sibling CLI crate explicitly enables this
feature and uses that signal to render a TTY-only progress bar by default.

## Stability note

The package layout and documentation may change independently from the encode
engine. This crate documents the current workspace behavior without relying on
internal milestone labels.
