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
| Convert one file with defaults | `builtin::encode_file`, `builtin::decode_file`, `builtin::recompress_file` |
| Convert in-memory data | `builtin::encode_bytes`, `builtin::decode_bytes`, `builtin::recompress_bytes` |
| Reuse settings across many jobs | `EncoderConfig::into_encoder(...)`, `DecodeConfig::into_decoder(...)`, `RecompressConfig::into_recompressor(...)` |
| Inspect spec/metadata before encoding | explicit family readers such as `WavReader`, or `PcmReader::new(...)` when format choice is dynamic |
| Work with decoded samples directly | `read_flac_reader`, `write_pcm_stream` |
| Show progress in your own UI | `*_with_progress` methods with the `progress` feature |

A practical rule of thumb:

- use `builtin::*` for quick one-shot conversions
- use explicit reader -> owned source -> session when you need explicit control
- for recompress specifically, prefer `FlacReader::new(...) -> into_recompress_source() -> RecompressConfig::into_recompressor(...)`
- use `DecodeConfig::into_decoder(...)` or `RecompressConfig::into_recompressor(...)` when you need reusable decode/recompress sessions

## Encode PCM containers to FLAC

### Quick file-to-file encode

```rust
use flacx::builtin::encode_file;

let summary = encode_file("input.wav", "output.flac")?;
println!("encoded {} samples", summary.total_samples);
```

This is the best starting point when you already have a supported PCM file on
disk.

### Encode in memory

```rust
use flacx::builtin::encode_bytes;

let wav_bytes = std::fs::read("input.wav")?;
let flac_bytes = encode_bytes(&wav_bytes)?;
std::fs::write("output.flac", flac_bytes)?;
```

This is useful for services, tests, or pipelines that already keep data in
memory.

### Encode from custom readers and writers

```rust
use flacx::{EncoderConfig, WavReader};
use std::fs::File;

let reader = WavReader::new(File::open("input.wav")?)?;
let source = reader.into_source();
let mut encoder = EncoderConfig::default().into_encoder(File::create("output.flac")?);
encoder.encode_source(source)?;
```

Use this style when your application already works with `Read + Seek` and
`Write + Seek` values.

### Reuse one configured encoder

```rust
use flacx::{EncoderConfig, WavReader, level::Level};
use std::fs::File;

let config = EncoderConfig::builder()
    .level(Level::Level8)
    .threads(4)
    .build();

for (input_path, output_path) in [("take01.wav", "take01.flac"), ("take02.wav", "take02.flac")] {
    let reader = WavReader::new(File::open(input_path)?)?;
    let source = reader.into_source();
    let mut encoder = config.clone().into_encoder(File::create(output_path)?);
    encoder.encode_source(source)?;
}
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
use flacx::{EncoderConfig, level::Level};

let config = EncoderConfig::default()
    .with_level(Level::Level5)
    .with_threads(8)
    .with_block_size(4096);
```

Guidance:

- start with the default configuration unless you have a reason to tune it
- raise thread count for larger or repeated jobs
- pin block size only when your workflow specifically benefits from it

## Decode FLAC to PCM containers

### Quick FLAC-to-WAV decode

```rust
use flacx::builtin::decode_file;

let summary = decode_file("input.flac", "output.wav")?;
println!("decoded {} frames", summary.frame_count);
```

### Decode in memory

```rust
use flacx::builtin::decode_bytes;

let flac_bytes = std::fs::read("input.flac")?;
let pcm_bytes = decode_bytes(&flac_bytes)?;
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
use flacx::builtin::decode_file;

decode_file("input.flac", "master.aiff")?;
```

### Select the output container in code

If you are decoding to a stream or buffer, or you want the choice to live in
code instead of the filename, configure it explicitly:

```rust
use std::io::Cursor;
use flacx::{DecodeConfig, FlacReader, PcmContainer};

let flac = std::fs::read("input.flac")?;
let reader = FlacReader::new(Cursor::new(flac))?;
let source = reader.into_decode_source();
let mut decoder = DecodeConfig::default()
    .with_output_container(PcmContainer::Wave64)
    .into_decoder(Cursor::new(Vec::new()));
decoder.decode_source(source)?;
```

### Tune decoding behavior

The settings most users care about are:

- **thread count**
- **default output container**
- optional **strict validation** settings when you want decode to fail instead
  of accepting more tolerant behavior

Example:

```rust
use flacx::{DecodeConfig, PcmContainer};

let config = DecodeConfig::default()
    .with_threads(4)
    .with_output_container(PcmContainer::Wave)
    .with_strict_seektable_validation(true);
```

## Inspect spec and metadata before streaming

If you want to inspect or control both sides explicitly, use the family
reader/source/session flow on encode and the FLAC reader/source/session flow on decode.

```rust
use std::{fs::File, io::Cursor};
use flacx::{DecodeConfig, EncoderConfig, FlacReader, WavReader};

let reader = WavReader::new(File::open("input.wav")?)?;
let spec = reader.spec();
let source = reader.into_source();

let mut encoder = EncoderConfig::default().into_encoder(Cursor::new(Vec::new()));
encoder.encode_source(source)?;

let flac_reader = FlacReader::new(Cursor::new(encoder.into_inner().into_inner()))?;
let decoded_spec = flac_reader.spec();
let decoded_source = flac_reader.into_decode_source();
let mut decoder = DecodeConfig::default().into_decoder(Cursor::new(Vec::new()));
decoder.decode_source(decoded_source)?;

assert!(spec.sample_rate > 0);
assert_eq!(decoded_spec.sample_rate, spec.sample_rate);
```

Use this path when you need to:

- inspect `sample_rate`, `channels`, or `bits_per_sample` before encode or decode streaming starts
- inspect or preserve metadata on the owned source before the session starts
- choose when the sample stream begins flowing on either side
- avoid relying on extension-based behavior

Raw PCM remains inspectable via explicit descriptors:

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
use flacx::{EncoderConfig, ProgressSnapshot, WavReader};
use std::fs::File;

let reader = WavReader::new(File::open("input.wav")?)?;
let source = reader.into_source();
let mut encoder = EncoderConfig::default().into_encoder(File::create("output.flac")?);
encoder.encode_source_with_progress(source, |snapshot: ProgressSnapshot| {
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
use flacx::{DecodeConfig, FlacReader, ProgressSnapshot};

let reader = FlacReader::new(std::fs::File::open("input.flac")?)?;
let source = reader.into_decode_source();
let mut decoder = DecodeConfig::default()
    .into_decoder(std::fs::File::create("output.wav")?);
decoder.decode_source_with_progress(source, |snapshot: ProgressSnapshot| {
    println!("decoded {} / {} frames", snapshot.completed_frames, snapshot.total_frames);
    Ok(())
})?;
```

### Recompress progress

```rust
use flacx::{FlacReader, RecompressConfig, RecompressProgress};

let reader = FlacReader::new(std::fs::File::open("input.flac")?)?;
let source = reader.into_recompress_source();
let mut recompressor = RecompressConfig::default()
    .into_recompressor(std::fs::File::create("output.flac")?);
recompressor.recompress_with_progress(source, |progress: RecompressProgress| {
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
with different settings. The builtin helpers remain thin wrappers, but the
explicit recompress story is intentionally the explicit reader/source/session
path.

```rust
use flacx::{FlacReader, RecompressConfig, level::Level};

let reader = FlacReader::new(std::fs::File::open("input.flac")?)?;
let source = reader.into_recompress_source();
let mut recompressor = RecompressConfig::builder()
    .level(Level::Level5)
    .threads(4)
    .build()
    .into_recompressor(std::fs::File::create("output.flac")?);

let summary = recompressor.recompress(source)?;
println!("recompressed {} samples", summary.total_samples);
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

1. `builtin::encode_file(...)`
2. `builtin::decode_file(...)`
3. `builtin::recompress_file(...)`

### Reuse configured codec objects for batches

If you process many files with the same settings, create one `Encoder`,
`Decoder`, or a writer-owning recompress session and reuse it.

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
