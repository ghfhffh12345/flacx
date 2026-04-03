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

## Stability note

The package layout and documentation may change independently from the encode
engine. This crate documents the current workspace behavior without relying on
internal milestone labels.
