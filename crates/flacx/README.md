# flacx

High-performance WAV/FLAC conversion for Rust.

`flacx` is the publishable library crate in the workspace. It provides the
shared encode/decode pipeline used by both Rust callers and the sibling CLI
crate.

## Add to your project

```toml
[dependencies]
flacx = "0.3.0"
```

## Quick start

```rust
use flacx::{Encoder, EncoderConfig, level::Level};

let config = EncoderConfig::default()
    .with_level(Level::Level8)
    .with_threads(4);

Encoder::new(config)
    .encode_file("input.wav", "output.flac")
    .unwrap();
```

```rust
use flacx::Decoder;

Decoder::new()
    .decode_file("input.flac", "output.wav")
    .unwrap();
```

## Primary API surface

- `EncoderConfig`
- `Encoder`
- `Decoder`
- `encode_file`
- `encode_bytes`
- `decode_file`
- `decode_bytes`

## Optional progress feature

Progress support is behind the optional `progress` Cargo feature and is
disabled by default.

```toml
[dependencies]
flacx = { version = "0.3.0", features = ["progress"] }
```

When enabled, the additional progress-specific API surface includes:

- `EncodeProgress`
- `Encoder::encode_with_progress`
- `Encoder::encode_file_with_progress`

## Current scope

- WAV-to-FLAC encoding
- FLAC-to-WAV decoding
- seekable input/output API
- current supported narrow encode/decode subset

## Out of scope

- metadata editing
- non-seekable output
- broader transcoding beyond WAV <-> FLAC
- broader format support beyond the current engine envelope

## Workspace note

The CLI lives in a separate workspace crate, `flacx-cli`, and is not bundled
into the publishable library package.

## Progress note

`Encoder` can optionally report real encode progress while keeping the
existing fast encode path intact. The sibling CLI crate explicitly enables this
feature and uses that signal to render a TTY-only progress bar by default.

## Stability note

The package layout and documentation may change independently from the encode
engine. This crate documents the current workspace behavior without relying on
internal milestone labels.
