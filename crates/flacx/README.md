# flacx

High-performance PCM-container/FLAC conversion and FLAC recompression for Rust.

`flacx` is the publishable library crate in this workspace. This README is the
crate-level architecture guide for maintainers and contributors who need to
re-orient themselves around the **current public API surface** quickly.

> Warning: this crate is still experimental. The current `fxmd` layout is the canonical `v1` format, and historical `fxmd` payload variants are not supported.

## Documentation intent

This document is intentionally **not** a beginner tutorial or convenience-first
walkthrough. It is the public-facing architecture companion to the crate rustdoc
in `crates/flacx/src/lib.rs`.

If you want a task-oriented guide for using the crate from application code,
start with [`docs/flacx-user-guide.md`](../../docs/flacx-user-guide.md).

Use it when you need to answer questions like:
- what conceptual surfaces does `flacx` expose?
- where is the explicit core vs the convenience layer?
- which source files currently carry those surfaces?
- which feature gates shape the public contract?

For the larger structural view, see
[`docs/flacx-public-api-architecture.md`](../../docs/flacx-public-api-architecture.md).

## Package surface

```toml
[dependencies]
flacx = "0.8.2"
```

Default feature families:
- `wav` => RIFF/WAVE, RF64, Wave64
- `aiff` => AIFF, AIFC
- `caf` => CAF

Optional feature:
- `progress` => callback-oriented progress reporting

## Public API interface map

```text
flacx
├─ core
│  ├─ EncoderConfig / EncoderBuilder
│  ├─ DecodeConfig / DecodeBuilder
│  ├─ RecompressConfig / RecompressBuilder
│  ├─ Encoder / EncodeSummary
│  ├─ FlacReader / DecodePcmStream / Decoder / DecodeSummary
│  ├─ FlacRecompressSource / Recompressor / RecompressSummary
│  └─ RecompressMode / RecompressPhase / RecompressProgress
│  ├─ PcmReader / AnyPcmStream / PcmStream / PcmStreamSpec / PcmContainer
│  ├─ read_pcm_reader / write_pcm_stream
│  └─ RawPcmDescriptor / RawPcmByteOrder / inspect_raw_pcm_total_samples
├─ inspectors
│  ├─ inspect_pcm_total_samples
│  ├─ inspect_wav_total_samples
│  ├─ inspect_flac_total_samples
│  └─ inspect_raw_pcm_total_samples
├─ builtin
│  ├─ builtin::encode_file / builtin::encode_bytes
│  ├─ builtin::decode_file / builtin::decode_bytes
│  ├─ builtin::recompress_file / builtin::recompress_bytes
│  └─ inspection-helper re-exports
├─ level
└─ progress (feature = "progress")
   ├─ ProgressSnapshot
   ├─ EncodeProgress / DecodeProgress
   └─ progress-enabled encode/decode/recompress methods
```

## Public symbol tree

```text
crate root
├─ modules
│  ├─ core
│  ├─ builtin
│  └─ level
├─ config + builders
│  ├─ EncoderConfig / EncoderBuilder
│  ├─ DecodeConfig / DecodeBuilder
│  └─ RecompressConfig / RecompressBuilder
├─ codec façades
│  ├─ Encoder / EncodeSummary
│  ├─ Decoder / DecodeSummary
│  └─ FlacRecompressSource / Recompressor / RecompressSummary / RecompressMode / RecompressPhase / RecompressProgress
├─ typed PCM + raw PCM boundary
│  ├─ PcmStream / PcmStreamSpec / PcmContainer
│  ├─ read_pcm_reader / write_pcm_stream
│  └─ RawPcmDescriptor / RawPcmByteOrder
├─ inspectors
│  ├─ inspect_wav_total_samples
│  ├─ inspect_pcm_total_samples
│  ├─ inspect_flac_total_samples
│  └─ inspect_raw_pcm_total_samples
└─ optional progress
   ├─ ProgressSnapshot
   ├─ EncodeProgress / DecodeProgress
   └─ progress-enabled methods on Encoder / Decoder / Recompressor
```

## Layer contract

| Layer | Public API surface | Ownership |
| --- | --- | --- |
| Explicit core | `flacx::core`, config/builders, codec façades, reader/session helpers, typed PCM helpers | The source of truth for codec configuration, reader-driven handoff, explicit encode/decode/recompress operations, and summary reporting. |
| Builtin/orchestration | `flacx::builtin` | One-shot file and byte workflows, extension inference, and lightweight routing into the core. |
| Support surfaces | `level`, raw PCM helpers, inspectors, progress types | Supporting concepts that remain public without becoming the main architecture story. |

### Key rule

The architecture should be read **from the explicit core outward**. The
builtin layer is intentionally thin and should not be treated as the
semantic center of the crate.

## Current source structure snapshot

The current source tree that backs the public contract is:

```text
crates/flacx/src/
├─ lib.rs                 # public re-exports and crate contract
├─ config.rs              # EncoderConfig / DecodeConfig + builders
├─ convenience.rs         # implementation backing the public `builtin` module
├─ encoder.rs             # encode façade
├─ decode.rs              # decode façade
├─ recompress/
│  ├─ mod.rs              # public recompress surface + exports
│  ├─ config.rs           # recompress policy + builder
│  ├─ source.rs           # reader-to-session handoff
│  ├─ session.rs          # writer-owning recompress execution
│  ├─ progress.rs         # recompress progress types/adapters
│  └─ verify.rs           # recompress MD5 verification glue
├─ pcm.rs                 # typed PCM boundary
├─ input.rs               # format-family dispatch for PCM ingest
├─ wav_input.rs           # WAV/RF64/Wave64 reader family
├─ wav_output.rs          # WAV-family writer family
├─ decode_output.rs       # decode-side temp output helpers
├─ encode_pipeline.rs     # encode planning helpers
├─ metadata.rs            # public metadata-facing helpers
├─ metadata/
│  ├─ blocks.rs           # metadata block model
│  └─ draft.rs            # metadata drafting/translation helpers
├─ read/
│  ├─ mod.rs              # FLAC read orchestration
│  ├─ frame.rs            # frame parsing/decoding
│  └─ metadata.rs         # FLAC metadata parsing + inspection
├─ write/
│  ├─ mod.rs              # FLAC write orchestration
│  └─ frame.rs            # frame/subframe serialization
└─ progress.rs            # optional progress support
```

This tree is intentionally architectural rather than exhaustive: it highlights
which files anchor the public story instead of documenting every helper module.

## Interface map: outside-in view

```text
supported PCM container family / raw PCM / FLAC
                │
                ▼
   family readers / FLAC reader
                │
                ▼
     spec + metadata handoff
                │
                ▼
        typed PCM boundary
                │
                ▼
  Encoder / Decoder / Recompressor
      │             │            │
      │             │            └─ FLAC reader -> recompress source -> writer-owning session
      │             │
      │             └─ decode output + family writers
      │
      └─ PCM ingest dispatch + encode pipeline
                │
                ▼
 builtin helpers (`builtin::*`) route into the same core
```

## Feature-gated contract

| Feature | Public effect |
| --- | --- |
| `wav` | Enables RIFF/WAVE, RF64, and Wave64 ingest/output surfaces. |
| `aiff` | Enables AIFF and the bounded AIFC surface. |
| `caf` | Enables the bounded CAF surface. |
| `progress` | Enables `ProgressSnapshot`, `EncodeProgress`, `DecodeProgress`, and progress-capable methods. |

## Public surface notes

### Config and builder surfaces
- `EncoderConfig` / `EncoderBuilder`
- `DecodeConfig` / `DecodeBuilder`
- `RecompressConfig` / `RecompressBuilder`

These are the first place to look when the question is “what knobs does the
public API intentionally expose?”

### Codec façades
- `Encoder`
- `Decoder`
- `FlacRecompressSource`
- `Recompressor`
- `RecompressSummary`

These are the stable façade/session types that express the main explicit workflows. Recompress remains public and distinct, but now follows the same inspect-first reader/session story as encode and decode.

### Typed PCM boundary
- `PcmStream`
- `PcmStreamSpec`
- `PcmContainer`
- `read_pcm_reader`
- `write_pcm_stream`

This is the seam between container adapters and the FLAC codec pipeline.

### Builtin/orchestration surface
- `builtin::encode_file`, `builtin::encode_bytes`
- `builtin::decode_file`, `builtin::decode_bytes`
- `builtin::recompress_file`, `builtin::recompress_bytes`

These helpers are important, but they are wrappers around the same explicit
surfaces above rather than a separate architectural center.

## Metadata and preservation note

The public documentation should continue to treat metadata preservation as part
of the crate contract, but not as the top-level orientation story. In
particular:
- the canonical private preservation chunk is the unified `fxmd v1` layout,
- historical `fxmd` payload variants are intentionally unsupported,
- decoded WAV-family output may carry preservation metadata even when the audio
  samples are unchanged.

## Documentation consistency contract

When updating public docs, keep these surfaces aligned:
1. `crates/flacx/src/lib.rs` — crate contract and public re-export map
2. `crates/flacx/README.md` — architecture-at-a-glance guide
3. `docs/flacx-public-api-architecture.md` — expanded structural guide

If one of those changes, the other two should be checked for drift.

## Related docs

- [`crates/flacx/src/lib.rs`](src/lib.rs) — crate rustdoc source
- [`docs/flacx-public-api-architecture.md`](../../docs/flacx-public-api-architecture.md) — expanded architecture guide
- [`docs/flacx-ground-up-ownership-map.md`](../../docs/flacx-ground-up-ownership-map.md) — same-crate ownership map and review cues
- [`docs/flacx-family-parity.md`](../../docs/flacx-family-parity.md) — WAV/AIFF/CAF parity audit
- [`../../README.md`](../../README.md) — workspace overview
- [`../../docs/flacx-major-refactor-review.md`](../../docs/flacx-major-refactor-review.md) — refactor review and maintainer checklist
