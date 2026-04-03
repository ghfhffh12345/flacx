# Migration to v4

v4 keeps the existing WAV-to-FLAC codec scope, but redesigns the public API and adds a `clap`-based CLI.

## What changed

- The primary library surface is now:
  - `EncodeOptions`
  - `FlacEncoder`
  - convenience functions `encode_file` and `encode_bytes`
- The CLI is now available as:
  - `flacx encode <input> <output>`
- The encode engine is still shared between library and CLI.

## Old style

```rust
use flacx::Encoder;

let flac = Encoder::default()
    .with_threads(4)
    .encode_wav_bytes(&wav_bytes)
    .unwrap();
```

## New style

```rust
use flacx::{EncodeOptions, FlacEncoder};

let flac = FlacEncoder::new(EncodeOptions::default().with_threads(4))
    .encode_bytes(&wav_bytes)
    .unwrap();
```

## File-oriented usage

```rust
use flacx::{EncodeOptions, FlacEncoder};

FlacEncoder::new(EncodeOptions::default())
    .encode_file("input.wav", "output.flac")
    .unwrap();
```

## Compatibility note

Strict backward compatibility was intentionally not a v4 goal. Existing callers should migrate to `FlacEncoder` and `EncodeOptions`. The codec behavior, supported format envelope, and benchmark expectations remain the same.
