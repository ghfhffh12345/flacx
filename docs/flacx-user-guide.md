# flacx crate user guide

`flacx` is a Rust library for converting supported PCM containers to FLAC,
decoding FLAC back to PCM containers, and recompressing existing FLAC streams.

This guide is intentionally **usage-first**. It focuses on how to use the crate
from application code and avoids walking through internal implementation
details.

> `flacx` is still experimental, so APIs and format details may change.

## Add `flacx` to your project

For the default experience:

```toml
[dependencies]
flacx = "0.8.2"
```

Default features enable support for these PCM container families:

| Feature | Enabled by default | Formats |
| --- | --- | --- |
| `wav` | yes | WAV, RF64, Wave64 |
| `aiff` | yes | AIFF, AIFC |
| `caf` | yes | CAF |
| `progress` | no | Progress callbacks |

If you want a smaller dependency surface, disable defaults and opt in only to
what you need:

```toml
[dependencies]
flacx = { version = "0.8.2", default-features = false, features = ["wav", "progress"] }
```

## Pick the right API

`flacx` exposes a few usage styles. Start with the simplest one that matches
your workflow.

| Goal | Recommended API |
| --- | --- |
| Convert one file with defaults | `encode_file`, `decode_file`, `recompress_file` |
| Convert in-memory data | `encode_bytes`, `decode_bytes`, `recompress_bytes` |
| Reuse settings across many jobs | `Encoder`, `Decoder`, `Recompressor` |
| Work with decoded samples directly | `read_pcm_stream`, `write_pcm_stream`, `encode_pcm_stream`, `decode_pcm_stream` |
| Encode raw PCM without a container header | `encode_raw`, `encode_raw_file`, `RawPcmDescriptor` |
| Show progress in your own UI | `*_with_progress` methods with the `progress` feature |

A practical rule of thumb:

- use free functions for quick one-shot conversions
- use `Encoder`, `Decoder`, or `Recompressor` when you need configuration or reuse
- use typed PCM or raw PCM APIs only when you need explicit control

## Encode PCM containers to FLAC

### Quick file-to-file encode

```rust
use flacx::encode_file;

let summary = encode_file("input.wav", "output.flac")?;
println!("encoded {} samples", summary.total_samples);
```

This is the best starting point when you already have a supported PCM file on
disk.

### Encode in memory

```rust
use flacx::Encoder;

let wav_bytes = std::fs::read("input.wav")?;
let flac_bytes = Encoder::default().encode_bytes(&wav_bytes)?;
std::fs::write("output.flac", flac_bytes)?;
```

This is useful for services, tests, or pipelines that already keep data in
memory.

### Encode from custom readers and writers

```rust
use flacx::Encoder;
use std::fs::File;

let input = File::open("input.wav")?;
let output = File::create("output.flac")?;
Encoder::default().encode(input, output)?;
```

Use this style when your application already works with `Read + Seek` and
`Write + Seek` values.

### Reuse one configured encoder

```rust
use flacx::{Encoder, EncoderConfig, level::Level};

let encoder = Encoder::new(
    EncoderConfig::builder()
        .level(Level::Level8)
        .threads(4)
        .build(),
);

encoder.encode_file("take01.wav", "take01.flac")?;
encoder.encode_file("take02.wav", "take02.flac")?;
```

This is the right shape for batch jobs or applications that want consistent
settings across many conversions.

### Supported encode inputs

With default features enabled, `flacx` can encode from these PCM container
inputs:

- `.wav`
- `.rf64`
- `.w64`
- `.aif`
- `.aiff`
- `.aifc`
- `.caf`

If you disable a format-family feature, the related formats stop being
available.

## Tune encoding behavior

The most useful encoder settings are:

- **compression level** via `level::Level`
- **thread count** via `.threads(...)` or `.with_threads(...)`
- **block size** via `.block_size(...)` or `.with_block_size(...)`

Example:

```rust
use flacx::{Encoder, EncoderConfig, level::Level};

let encoder = Encoder::new(
    EncoderConfig::default()
        .with_level(Level::Level5)
        .with_threads(8)
        .with_block_size(4096),
);
```

Guidance:

- start with the default configuration unless you have a reason to tune it
- raise thread count for larger or repeated jobs
- pin block size only when your workflow specifically benefits from it

## Decode FLAC to PCM containers

### Quick FLAC-to-WAV decode

```rust
use flacx::decode_file;

let summary = decode_file("input.flac", "output.wav")?;
println!("decoded {} frames", summary.frame_count);
```

### Decode in memory

```rust
use flacx::Decoder;

let flac_bytes = std::fs::read("input.flac")?;
let pcm_bytes = Decoder::default().decode_bytes(&flac_bytes)?;
std::fs::write("output.wav", pcm_bytes)?;
```

### Let the output path choose the container

When you use `decode_file`, the output extension can choose the destination
container for you:

| Output path | Container written |
| --- | --- |
| `*.wav` | WAV |
| `*.rf64` | RF64 |
| `*.w64` | Wave64 |
| `*.aif`, `*.aiff` | AIFF |
| `*.aifc` | AIFC |
| `*.caf` | CAF |

Example:

```rust
use flacx::Decoder;

Decoder::default().decode_file("input.flac", "master.aiff")?;
```

### Select the output container in code

If you are decoding to a stream or buffer, or you want the choice to live in
code instead of the filename, configure it explicitly:

```rust
use std::io::Cursor;
use flacx::{DecodeConfig, Decoder, PcmContainer};

let flac = std::fs::read("input.flac")?;
let mut output = Cursor::new(Vec::new());

let decoder = Decoder::new(
    DecodeConfig::default().with_output_container(PcmContainer::Wave64),
);

decoder.decode(Cursor::new(flac), &mut output)?;
```

You can also override the target container directly for a single decode:

```rust
use flacx::{Decoder, PcmContainer};
use std::fs::File;

let input = File::open("input.flac")?;
let output = File::create("output.aiff")?;
Decoder::default().decode_as(input, output, PcmContainer::Aiff)?;
```

### Tune decoding behavior

The settings most users care about are:

- **thread count**
- **default output container**
- optional **strict validation** settings when you want decode to fail instead
  of accepting more tolerant behavior

Example:

```rust
use flacx::{DecodeConfig, Decoder, PcmContainer};

let decoder = Decoder::new(
    DecodeConfig::default()
        .with_threads(4)
        .with_output_container(PcmContainer::Wave)
        .with_strict_seektable_validation(true),
);
```

## Work with typed PCM streams

If you want to inspect, transform, or hand off sample data yourself, use the
typed PCM API instead of going directly from file to file.

```rust
use std::io::Cursor;
use flacx::{Decoder, Encoder, PcmContainer, read_pcm_stream, write_pcm_stream};

let wav_bytes = std::fs::read("input.wav")?;
let stream = read_pcm_stream(Cursor::new(&wav_bytes))?;

let mut flac_output = Cursor::new(Vec::new());
Encoder::default().encode_pcm_stream(&stream, &mut flac_output)?;

let decoded = Decoder::default().decode_pcm_stream(Cursor::new(flac_output.into_inner()))?;
let mut wav_output = Cursor::new(Vec::new());
write_pcm_stream(&mut wav_output, &decoded, PcmContainer::Wave)?;
```

Use this path when you need to:

- inspect `sample_rate`, `channels`, or `bits_per_sample`
- run your own processing between decode and re-encode
- choose the output PCM container explicitly
- avoid relying on extension-based behavior

## Encode raw PCM explicitly

If your source is raw PCM rather than WAV, AIFF, or CAF, describe it with
`RawPcmDescriptor`.

```rust
use std::io::Cursor;
use flacx::{Encoder, RawPcmByteOrder, RawPcmDescriptor};

let raw_bytes = std::fs::read("input.pcm")?;
let descriptor = RawPcmDescriptor {
    sample_rate: 44_100,
    channels: 2,
    valid_bits_per_sample: 16,
    container_bits_per_sample: 16,
    byte_order: RawPcmByteOrder::LittleEndian,
    channel_mask: None,
};

let mut output = Cursor::new(Vec::new());
Encoder::default().encode_raw(Cursor::new(raw_bytes), &mut output, descriptor)?;
```

Important raw PCM rules:

- `flacx` does **not** infer raw PCM layout for you
- you must provide sample rate, channel count, bit depth, and byte order
- for 3 to 8 channels, provide a non-zero `channel_mask`

You can also inspect raw PCM before encoding:

```rust
use std::io::Cursor;
use flacx::{RawPcmByteOrder, RawPcmDescriptor, inspect_raw_pcm_total_samples};

let raw = std::fs::read("input.pcm")?;
let descriptor = RawPcmDescriptor {
    sample_rate: 48_000,
    channels: 2,
    valid_bits_per_sample: 24,
    container_bits_per_sample: 24,
    byte_order: RawPcmByteOrder::LittleEndian,
    channel_mask: None,
};

let total_samples = inspect_raw_pcm_total_samples(Cursor::new(raw), descriptor)?;
println!("{total_samples}");
```

## Show progress in your own UI

Enable the feature first:

```toml
[dependencies]
flacx = { version = "0.8.2", features = ["progress"] }
```

Then use the progress-aware methods.

### Encode progress

```rust
use flacx::{Encoder, ProgressSnapshot};

Encoder::default().encode_file_with_progress("input.wav", "output.flac", |snapshot: ProgressSnapshot| {
    println!(
        "encoded {} / {} samples",
        snapshot.processed_samples,
        snapshot.total_samples
    );
    Ok(())
})?;
```

### Decode progress

```rust
use flacx::{Decoder, ProgressSnapshot};

Decoder::default().decode_file_with_progress("input.flac", "output.wav", |snapshot: ProgressSnapshot| {
    println!(
        "decoded {} / {} frames",
        snapshot.completed_frames,
        snapshot.total_frames
    );
    Ok(())
})?;
```

### Recompress progress

```rust
use flacx::{RecompressProgress, Recompressor};

Recompressor::default().recompress_file_with_progress("input.flac", "output.flac", |progress: RecompressProgress| {
    println!(
        "{}: {} / {} samples",
        progress.phase.as_str(),
        progress.phase_processed_samples,
        progress.phase_total_samples
    );
    Ok(())
})?;
```

## Recompress existing FLAC files

Use recompression when the input is already FLAC and you want a new FLAC output
with different settings.

```rust
use flacx::{RecompressConfig, Recompressor, level::Level};

let recompressor = Recompressor::new(
    RecompressConfig::builder()
        .level(Level::Level5)
        .threads(4)
        .build(),
);

recompressor.recompress_file("input.flac", "output.flac")?;
```

If you need stricter or looser behavior, `RecompressMode` lets you choose
between `Loose`, `Default`, and `Strict`. Most users should start with the
default mode.

## Inspect files before converting them

Sometimes you only need sample counts for planning, validation, or progress
estimation.

```rust
use std::fs::File;
use flacx::{inspect_flac_total_samples, inspect_wav_total_samples};

let pcm_samples = inspect_wav_total_samples(File::open("input.wav")?)?;
let flac_samples = inspect_flac_total_samples(File::open("input.flac")?)?;

println!("pcm samples: {pcm_samples}");
println!("flac samples: {flac_samples}");
```

These helpers are useful for:

- preflight checks
- estimating work for larger batches
- validating inputs before starting a full conversion

## Practical recommendations

### Start simple

For many applications, these three entry points are enough:

1. `encode_file(...)`
2. `decode_file(...)`
3. `recompress_file(...)`

### Reuse configured codec objects for batches

If you process many files with the same settings, create one `Encoder`,
`Decoder`, or `Recompressor` and reuse it.

### Prefer explicit output selection when format matters

If a downstream system requires a specific PCM family, set `PcmContainer`
explicitly or choose an unambiguous output extension.

### Use typed PCM only when you need the extra control

`PcmStream` is excellent for advanced workflows, but file-based APIs are the
fastest way to get started.

### Use raw PCM only when there is no container metadata

Raw PCM support is powerful, but it is also the most explicit path. Make sure
the descriptor matches the real input layout exactly.

## Common pitfalls

- **A format is rejected:** check your enabled Cargo features
- **Progress methods are missing:** enable the `progress` feature
- **Decoded output is not the container you expected:** check the output path
  extension or set `PcmContainer` explicitly
- **Raw PCM validation fails:** double-check sample format, byte order, and
  channel mask
- **A tuned compression level changed the default block size:** set an explicit
  block size if you want it fixed
- **A streaming pipe does not work directly:** many APIs require seekable
  input/output, so use files or in-memory buffers when needed

## Related docs

- [`../README.md`](../README.md) — workspace overview
- [`../crates/flacx/README.md`](../crates/flacx/README.md) — architecture-oriented crate guide
- [`../crates/flacx-cli/README.md`](../crates/flacx-cli/README.md) — CLI usage guide
