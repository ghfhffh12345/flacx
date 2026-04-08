# flacx

High-performance WAV/FLAC conversion for Rust.

`flacx` is the publishable library crate in this workspace. It exposes the
reusable encode/decode pipeline used by Rust callers and by the sibling
`flacx-cli` crate.

## Add to your project

```toml
[dependencies]
flacx = "0.7.0"
```

If you want live progress reporting from library code, enable the optional
`progress` feature:

```toml
[dependencies]
flacx = { version = "0.7.0", features = ["progress"] }
```

## Public API at a glance

| Area | Main items |
| --- | --- |
| Encoder configuration | `EncoderConfig`, `EncoderBuilder`, `Encoder::builder()` |
| Decoder configuration | `DecodeConfig`, `DecodeBuilder`, `Decoder::builder()` |
| Encoding | `Encoder`, `EncodeSummary`, `encode_file`, `encode_bytes` |
| Decoding | `Decoder`, `DecodeSummary`, `decode_file`, `decode_bytes` |
| Inspection helpers | `inspect_wav_total_samples`, `inspect_flac_total_samples` |
| Compression levels | `level::Level`, `level::LevelProfile` |
| Optional progress | `ProgressSnapshot`, `EncodeProgress`, `DecodeProgress`, progress-enabled methods |

## Quick start: encode and decode files

The file-based API is the simplest entry point when you already have paths on
disk.

```rust,no_run
use flacx::{Decoder, Encoder, EncoderConfig, level::Level};

let encoder = Encoder::new(
    EncoderConfig::builder()
        .level(Level::Level8)
        .threads(4)
        .build(),
);

let encode_summary = encoder.encode_file("input.wav", "output.flac").unwrap();
assert!(encode_summary.total_samples > 0);

let decoder = Decoder::default();
let decode_summary = decoder.decode_file("output.flac", "roundtrip.wav").unwrap();
assert_eq!(encode_summary.total_samples, decode_summary.total_samples);
```

Both `EncodeSummary` and `DecodeSummary` report the frame count, total samples,
block-size information, sample rate, channel count, and bits per sample for the
processed stream. That makes them useful for assertions in tests and for quick
sanity checks after a run.

If you prefer the convenience constructors, `Encoder::builder()` and
`Decoder::builder()` return the same builders as the config types.

```rust,no_run
use flacx::{Decoder, Encoder, level::Level};

let encoder_config = Encoder::builder()
    .level(Level::Level6)
    .threads(8)
    .build();

let decoder_config = Decoder::builder()
    .threads(4)
    .strict_channel_mask_provenance(true)
    .build();

let _encoder = Encoder::new(encoder_config);
let _decoder = Decoder::new(decoder_config);
```

## Configuration

`EncoderConfig` controls how FLAC encoding is planned and executed:

- `level` selects a compression preset from `level::Level`
- `threads` sets the worker count
- `block_size` sets a fixed block size
- `block_schedule` enables a custom block-size schedule for advanced use

`EncoderConfig::default()` uses `Level::Level8` and a thread count derived from
the current machine’s available parallelism.

`DecodeConfig` controls FLAC-to-WAV decoding:

- `threads` sets the worker count
- `strict_channel_mask_provenance` requires explicit provenance before the
  decoder restores non-ordinary channel masks
- `strict_seektable_validation` turns malformed `SEEKTABLE` metadata from a
  tolerated parse-time warning into a decode error

The config types also expose fluent `with_*` methods if you prefer direct
mutation over the builders.

```rust,no_run
use flacx::{DecodeConfig, EncoderConfig, level::Level};

let encoder_config = EncoderConfig::default()
    .with_level(Level::Level4)
    .with_threads(2)
    .with_block_size(1024);

let decoder_config = DecodeConfig::default()
    .with_threads(4)
    .with_strict_channel_mask_provenance(true)
    .with_strict_seektable_validation(true);

assert_eq!(encoder_config.level, Level::Level4);
assert_eq!(decoder_config.threads, 4);
```

Notes:

- `with_threads(0)` clamps to at least one thread.
- `with_level(...)` resets the block size to the preset’s default block size.
- `with_block_size(...)` clears any previously configured block schedule.

## Byte helpers

Use the byte helpers when you already have the whole WAV or FLAC payload in
memory.

```rust,no_run
use flacx::{decode_bytes, encode_bytes};

let wav_bytes = std::fs::read("input.wav").unwrap();
let flac_bytes = encode_bytes(&wav_bytes).unwrap();
let roundtrip_wav_bytes = decode_bytes(&flac_bytes).unwrap();

assert!(!flac_bytes.is_empty());
assert!(!roundtrip_wav_bytes.is_empty());
```

These helpers are useful when you are:

- testing encode/decode behavior in memory
- piping data through a higher-level application buffer
- avoiding temporary output files in a prototype

## Metadata round-trip behavior

When `Decoder` writes WAV output, flacx preserves otherwise-non-roundtrippable
FLAC metadata in a private canonical WAV chunk so a later WAV -> FLAC encode can
reconstruct the original FLAC metadata ordering and payloads. Where WAV has
native compatibility surfaces, flacx also emits derived mirrors such as
`LIST/INFO` and `cue `.

This means decoded WAV output may contain extra metadata chunks compared with a
minimal PCM-only WAV, even when the audio samples are unchanged.

Compatibility note:

- the current crate recognizes only the unified private `fxmd` preservation
  chunk at runtime
- older split private chunks such as `fxvc` / `fxcs` are intentionally no
  longer imported and are treated like unknown WAV chunks

## Sample inspection helpers

`inspect_wav_total_samples` and `inspect_flac_total_samples` are lightweight
metadata probes. They are useful when you want to confirm the total sample count
before committing to a longer encode or decode.

```rust,no_run
use flacx::{inspect_flac_total_samples, inspect_wav_total_samples};
use std::fs::File;

let wav_total_samples = inspect_wav_total_samples(File::open("input.wav").unwrap()).unwrap();
let flac_total_samples = inspect_flac_total_samples(File::open("input.flac").unwrap()).unwrap();

assert!(wav_total_samples > 0);
assert!(flac_total_samples > 0);
```

These helpers do not perform a full transcode; they only inspect the container
metadata needed to report sample counts.

## Compression levels

The `level` module exposes the compression presets used by the encoder.

- `Level::Level0` through `Level::Level8`
- `Level::profile()` returns the corresponding `LevelProfile`
- `LevelProfile` stores the block size and encoder search limits used by that
  preset

```rust,no_run
use flacx::level::Level;

let level = Level::Level8;
let profile = level.profile();

assert_eq!(u8::from(level), 8);
assert_eq!(profile.block_size, 4096);
```

If you need to map from a numeric CLI-style level into the Rust enum, use
`Level::try_from(u8)`.

## Optional progress feature

Progress support is behind the optional `progress` feature and is disabled by
default.

```rust,no_run
use flacx::{Encoder, EncoderConfig, ProgressSnapshot};
use std::io::Cursor;

let encoder = Encoder::new(EncoderConfig::default());
let input = Cursor::new(std::fs::read("input.wav").unwrap());
let mut output = Cursor::new(Vec::new());

let _summary = encoder
    .encode_with_progress(input, &mut output, |progress: ProgressSnapshot| {
        println!(
            "{} / {} samples, {} / {} frames",
            progress.processed_samples,
            progress.total_samples,
            progress.completed_frames,
            progress.total_frames
        );
        Ok(())
    })
    .unwrap();
```

When enabled, the progress surface includes:

- `ProgressSnapshot`
- `EncodeProgress`
- `DecodeProgress`
- `Encoder::encode_with_progress`
- `Encoder::encode_file_with_progress`
- `Decoder::decode_with_progress`
- `Decoder::decode_file_with_progress`

The CLI crate enables this feature and uses it to render its live terminal
progress UI.

## Supported scope

`flacx` is focused on the current WAV ↔ FLAC workflow:

- WAV-to-FLAC encoding
- FLAC-to-WAV decoding
- file-based input/output
- in-memory byte helpers
- sample-count inspection
- optional progress reporting

## Limitations

The library intentionally stays narrow. It does not aim to be a general audio
toolkit.

Out of scope for the current crate:

- metadata editing
- non-seekable streaming APIs
- broader transcoding beyond WAV ↔ FLAC
- broader format support beyond the current engine envelope

## Workspace note

`flacx-cli` lives in the same workspace but remains a separate crate. It is a
thin command-line front end over the same encode/decode pipeline documented
here.

## Practical guidance

- Use the builder types when you want a one-shot configuration setup.
- Use the fluent `with_*` methods when you want to derive a config from an
  existing value.
- Use the file helpers for a simple path-to-path workflow.
- Use the byte helpers or inspection helpers when you need in-memory control or
  preflight metadata checks.
- Use the progress feature only when you need callback-driven progress from Rust
  code.

## Stability note

The package layout and documentation may evolve independently from the encode
engine. This README tracks the current public library surface without relying
on internal milestone labels.
