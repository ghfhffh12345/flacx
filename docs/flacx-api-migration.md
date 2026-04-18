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

The shared public `Metadata` type is now the metadata authoring center for direct source construction:

- `EncodeSource::new(metadata, stream)`
- `DecodeSource::new(metadata, stream)`
- `FlacRecompressSource::new(metadata, stream, expected_streaminfo_md5)`

Example:

```rust
use flacx::{EncodeSource, EncoderConfig, Metadata, PcmStream};

let mut metadata = Metadata::new();
metadata.add_comment("TITLE", "Scratch-authored title");

let stream = PcmStream {
    spec: todo!("your PCM spec"),
    samples: todo!("your interleaved PCM samples"),
};

let mut encoder = EncoderConfig::default().into_encoder(std::io::Cursor::new(Vec::new()));
encoder.encode_source(EncodeSource::new(metadata, stream))?;
```

> Note: mutating reader-derived `Metadata` switches it to semantic-authoring mode. Any opaque preserved metadata carried privately for round-trip fidelity is discarded once you make semantic edits.

The public expert traits remain:

- `EncodePcmStream`
- `DecodePcmStream`

but they are now secondary to the explicit reader/source/session flow.
