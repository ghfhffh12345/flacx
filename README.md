# flacx

`flacx` converts supported PCM containers to FLAC, decodes FLAC back to PCM
containers, and recompresses existing FLAC streams.

This workspace contains:

- `crates/flacx` — the reusable Rust library
- `crates/flacx-cli` — the command-line interface

> `flacx` is still experimental, so APIs and format support may change.

## Quick start

Add the library crate to a Rust project:

```bash
cargo add flacx
```

The default feature set enables WAV, AIFF, and CAF support. If you want to
choose features explicitly:

```toml
[dependencies]
flacx = { version = "0.9.0", default-features = false, features = ["wav", "progress"] }
```

Use the CLI with the installed or built `flacx` binary:

```bash
flacx --help
```

## Examples

Recommended library workflow:

```rust
use flacx::{EncoderConfig, WavReader};
use std::{
    fs::File,
    io::{BufReader, BufWriter},
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let input = BufReader::new(File::open("input.wav")?);
    let source = WavReader::new(input)?.into_source();

    let output = BufWriter::new(File::create("output.flac")?);
    let mut encoder = EncoderConfig::default().into_encoder(output);
    let summary = encoder.encode_source(source)?;
    println!("encoded {} samples", summary.total_samples);

    Ok(())
}
```

Advanced direct-construction workflow:

```rust
use flacx::{EncodeSource, EncoderConfig, Metadata, WavPcmStream};
use std::{
    fs::File,
    io::{BufReader, BufWriter, Seek, SeekFrom},
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut payload = BufReader::new(File::open("input.wav")?);
    payload.seek(SeekFrom::Start(44))?; // canonical PCM WAV payload offset

    let stream = WavPcmStream::builder(payload)
        .sample_rate(44_100)
        .channels(2)
        .valid_bits_per_sample(16)
        .total_samples(1_024)
        .build()?;

    let source = EncodeSource::new(Metadata::new(), stream);
    let output = BufWriter::new(File::create("output.flac")?);
    let mut encoder = EncoderConfig::default().into_encoder(output);
    encoder.encode_source(source)?;
    Ok(())
}
```

One-shot helper workflow:

```rust
use flacx::builtin::encode_file;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let summary = encode_file("input.wav", "output.flac")?;
    println!("encoded {} samples", summary.total_samples);
    Ok(())
}
```

Decode a FLAC file from the command line:

```bash
flacx decode input.flac -o output.wav
```

Recompress a FLAC file in place:

```bash
flacx recompress input.flac --in-place
```

## Reference

- Library users: <https://docs.rs/flacx>
- CLI users: `flacx --help`
