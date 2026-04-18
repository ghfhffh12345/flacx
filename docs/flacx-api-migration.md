# flacx API migration notes

This note summarizes the public API shift from the old split metadata/stream
flow to the new explicit reader + owned source flow.

## Encode

| Old | New |
| --- | --- |
| `read_pcm_reader(reader)?` | `PcmReader::new(reader)?` when format choice is dynamic |
| `WavReader::new(reader)?` + `metadata().clone()` + `into_pcm_stream()` | `WavReader::new(reader)?` + `into_source()` |
| `encoder.set_metadata(metadata); encoder.encode(stream)` | `encoder.encode_source(source)` |

Canonical explicit encode path:

```rust
use flacx::{EncoderConfig, WavReader};

let reader = WavReader::new(std::fs::File::open("input.wav")?)?;
let source = reader.into_source();
let mut encoder = EncoderConfig::default()
    .into_encoder(std::fs::File::create("output.flac")?);
encoder.encode_source(source)?;
```

## Decode

| Old | New |
| --- | --- |
| `read_flac_reader(reader)?` + `metadata().clone()` + `into_pcm_stream()` | `FlacReader::new(reader)?` + `into_decode_source()` |
| `decoder.set_metadata(metadata); decoder.decode(stream)` | `decoder.decode_source(source)` |

Canonical explicit decode path:

```rust
use flacx::{DecodeConfig, FlacReader};

let reader = FlacReader::new(std::fs::File::open("input.flac")?)?;
let source = reader.into_decode_source();
let mut decoder = DecodeConfig::default()
    .into_decoder(std::fs::File::create("output.wav")?);
decoder.decode_source(source)?;
```

## Recompress

| Old | New |
| --- | --- |
| `FlacRecompressSource::from_reader(reader)` | `reader.into_recompress_source()` |

Canonical explicit recompress path:

```rust
use flacx::{FlacReader, RecompressConfig};

let reader = FlacReader::new(std::fs::File::open("input.flac")?)?;
let source = reader.into_recompress_source();
let mut recompressor = RecompressConfig::default()
    .into_recompressor(std::fs::File::create("output.flac")?);
recompressor.recompress(source)?;
```

## Advanced/custom streams

If you previously used public metadata staging on `Encoder` / `Decoder`, move
that composition to source construction instead:

- `EncodeSource::new(metadata, stream)`
- `DecodeSource::new(metadata, stream)`

The public expert traits remain:

- `EncodePcmStream`
- `DecodePcmStream`

but they are now secondary to the explicit reader/source/session flow.
