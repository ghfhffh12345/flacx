# flacx

`flacx` is a Rust library for converting supported PCM containers to FLAC,
decoding FLAC back to PCM containers, and recompressing existing FLAC streams.

> `flacx` is still experimental, so APIs and format details may change.

## Get Started

Add the crate with Cargo:

```bash
cargo add flacx
```

If you want the default experience, this is enough:

```toml
[dependencies]
flacx = "0.8.2"
```

Default features enable support for these PCM container families:

- `wav` — WAV, RF64, and Wave64
- `aiff` — AIFF and AIFC
- `caf` — CAF

If you want a smaller feature set or progress callbacks, configure features
explicitly:

```toml
[dependencies]
flacx = { version = "0.8.2", default-features = false, features = ["wav", "progress"] }
```

## Examples

### Encode a PCM container to FLAC

Canonical example:

```rust
use flacx::{EncoderConfig, WavReader};
use std::{
    fs::File,
    io::{BufReader, BufWriter},
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let input = BufReader::new(File::open("input.wav")?);
    let reader = WavReader::new(input)?;
    let source = reader.into_source();

    let output = BufWriter::new(File::create("output.flac")?);
    let mut encoder = EncoderConfig::default().into_encoder(output);
    encoder.encode_source(source)?;

    Ok(())
}
```

`builtin` example:

```rust
use flacx::builtin::encode_file;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let summary = encode_file("input.wav", "output.flac")?;
    println!("encoded {} samples", summary.total_samples);
    Ok(())
}
```

If you are migrating from the older split metadata/stream flow, see
[`docs/flacx-api-migration.md`](docs/flacx-api-migration.md).

### Decode FLAC to a PCM container

Canonical example:

```rust
use flacx::{DecodeConfig, FlacReader};
use std::{
    fs::File,
    io::{BufReader, BufWriter},
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let input = BufReader::new(File::open("input.flac")?);
    let reader = FlacReader::new(input)?;
    let source = reader.into_decode_source();

    let output = BufWriter::new(File::create("output.wav")?);
    let mut decoder = DecodeConfig::default().into_decoder(output);
    decoder.decode_source(source)?;

    Ok(())
}
```

`builtin` example:

```rust
use flacx::builtin::decode_file;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let summary = decode_file("input.flac", "output.wav")?;
    println!("decoded {} frames", summary.frame_count);
    Ok(())
}
```

### Recompress an existing FLAC file

Canonical example:

```rust
use flacx::{FlacReader, RecompressConfig};
use std::{
    fs::File,
    io::{BufReader, BufWriter},
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let input = BufReader::new(File::open("input.flac")?);
    let reader = FlacReader::new(input)?;
    let source = reader.into_recompress_source();

    let output = BufWriter::new(File::create("recompressed.flac")?);
    let mut recompressor = RecompressConfig::default().into_recompressor(output);
    let summary = recompressor.recompress(source)?;
    println!("recompressed {} samples", summary.total_samples);

    Ok(())
}
```

`builtin` example:

```rust
use flacx::builtin::recompress_file;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let summary = recompress_file("input.flac", "recompressed.flac")?;
    println!("recompressed {} samples", summary.total_samples);
    Ok(())
}
```

For deeper workflows, including in-memory conversion, reusable codec
configuration, metadata inspection, and progress callbacks, see the
[`flacx` user guide](docs/flacx-user-guide.md).

## More Docs

- [`docs/flacx-user-guide.md`](docs/flacx-user-guide.md) — usage-first guide for
  library users
- [`crates/flacx/README.md`](crates/flacx/README.md) — public API architecture
  and maintainer-oriented crate structure
- [`crates/flacx-cli/README.md`](crates/flacx-cli/README.md) — CLI usage, if you
  want the command-line workflow instead of the library
