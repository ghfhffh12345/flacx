# flacx public API architecture guide

This guide explains the **current `flacx` architecture from the perspective of
the externally exposed API**. It is aimed at maintainers and contributors who
need to understand the crate at a glance without dropping directly into the
implementation.

It complements:
- `crates/flacx/src/lib.rs` — the crate contract and rustdoc-facing public map
- `crates/flacx/README.md` — the crate-level architecture summary
- `docs/flacx-major-refactor-review.md` — the refactor review checklist and
  maintainer migration notes

## 1. Architecture summary

`flacx` is organized around a same-crate public story:

```text
family readers / FLAC reader
            │
            ▼
     spec + metadata handoff
            │
            ▼
      typed PCM stream seam
            │
            ▼
 Encoder / Decoder / Recompressor
            │
            ▼
 family writers / FLAC writer
            │
            ▼
 builtin helpers route into the same spine
```

The key architectural distinction is:
- **encode/decode spine** = the semantic center of the crate
- **family peers** = WAV, AIFF, and CAF remain first-class around that spine
- **builtin/orchestration** = wrappers that route into that same core

The documentation should preserve that reading order.

## 2. Public interface map

```text
flacx
├─ modules
│  ├─ core
│  ├─ builtin
│  └─ level
├─ config/builders
│  ├─ EncoderConfig / EncoderBuilder
│  ├─ DecodeConfig / DecodeBuilder
│  └─ RecompressConfig / RecompressBuilder
├─ codec façades
│  ├─ Encoder / EncodeSummary
│  ├─ FlacReader / DecodePcmStream / Decoder / DecodeSummary
│  └─ FlacRecompressSource / Recompressor / RecompressSummary / RecompressMode / RecompressPhase / RecompressProgress
├─ typed boundary
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
   └─ progress-enabled methods on the façade types
```

## 3. Layer ownership map

| Layer | Public entry points | What it owns | What it should not become |
| --- | --- | --- | --- |
| Encode/decode spine | `core`, configs/builders, `Encoder`, `FlacReader`, `Decoder`, `FlacRecompressSource`, `Recompressor`, reader/session helpers | configuration, reader-driven handoff, typed PCM seam, explicit encode/decode/recompress operations, summaries | a path-oriented builtin story |
| Family peers | public typed boundary plus WAV/AIFF/CAF behavior behind the scenes | container parsing/writing and family-specific translation | a hidden WAV-default compatibility layer |
| Builtin/orchestration | `builtin`, namespaced `*_file` / `*_bytes` helpers | one-shot path/byte routing and extension-driven ergonomics | a duplicate policy engine |
| Support surfaces | `level`, inspector helpers, raw PCM helpers, progress types | supporting concepts adjacent to the spine | the primary conceptual center |

## 4. Current source tree snapshot

This is the structural snapshot that currently supports the public API story:

```text
crates/flacx/src/
├─ lib.rs
├─ aiff.rs
├─ aiff_output.rs
├─ caf.rs
├─ caf_output.rs
├─ config.rs
├─ convenience.rs         # implementation backing the public `builtin` module
├─ encoder.rs
├─ decode.rs
├─ recompress/
│  ├─ mod.rs
│  ├─ config.rs
│  ├─ source.rs
│  ├─ session.rs
│  ├─ progress.rs
│  └─ verify.rs
├─ pcm.rs
├─ input.rs
├─ wav_input.rs
├─ wav_output.rs
├─ decode_output.rs
├─ encode_pipeline.rs
├─ metadata.rs
├─ metadata/
│  ├─ blocks.rs
│  └─ draft.rs
├─ read/
│  ├─ mod.rs
│  ├─ frame.rs
│  └─ metadata.rs
├─ write/
│  ├─ mod.rs
│  └─ frame.rs
├─ raw.rs
├─ level.rs
├─ progress.rs
└─ ... supporting modules omitted here
```

### Reading the tree
- `lib.rs` is the public contract surface.
- `config.rs`, `encoder.rs`, `decode.rs`, `recompress/`, and `pcm.rs` are
  the fastest way to orient yourself around the exported architecture.
- `input.rs`, `wav_input.rs`, `aiff.rs`, `caf.rs`, `wav_output.rs`,
  `aiff_output.rs`, `caf_output.rs`, `read/`, and `write/` show how the
  family-facing and FLAC-facing edges were separated without splitting crates.
- `metadata/` and `decode_output.rs` exist to keep major responsibilities out of
  the top-level façades.

## 5. Interface-to-structure map

```text
Public surface                      Main structural anchors
──────────────────────────────────  ───────────────────────────────────────────
crate contract                      lib.rs
config/builders                     config.rs
explicit encode façade              encoder.rs + encode_pipeline.rs
explicit decode façade              decode.rs + decode_output.rs
explicit recompress session         recompress/
typed PCM boundary                  pcm.rs + input.rs
WAV-family ingest/output            wav_input.rs + wav_output.rs
AIFF-family ingest/output           aiff.rs + aiff_output.rs
CAF-family ingest/output            caf.rs + caf_output.rs
FLAC read/write internals           read/ + write/
metadata model / translation        metadata.rs + metadata/
optional progress                   progress.rs
```

This map is intentionally shallow: it is for orientation, not for explaining
full call graphs or internal execution traces.

## 6. Feature-gated contract

| Feature | Architectural effect |
| --- | --- |
| `wav` | Enables the RIFF/WAVE, RF64, and Wave64 family surfaces. |
| `aiff` | Enables AIFF and the bounded AIFC surface. |
| `caf` | Enables the bounded CAF surface. |
| `progress` | Enables the optional callback-oriented progress surface. |

## 7. Narrative priorities for future documentation edits

When public docs are edited, keep this order:
1. public architecture and layering
2. public interface grouping
3. feature-gated contract
4. subordinate support surfaces
5. practical usage only when it does not displace the architecture story

In particular:
- do **not** let convenience helpers become the main conceptual model,
- do **not** describe AIFF/CAF as second-tier routes behind a WAV core,
- do **not** lead with tutorial prose when the real need is orientation,
- do **not** over-explain internal execution paths in public-facing docs.

## 8. Architecture audit companions

These review-oriented docs keep the same-crate rebuild evidence close to the
public story:

- `docs/flacx-ground-up-ownership-map.md` — current ownership map and review
  cues for the encode/decode spine
- `docs/flacx-family-parity.md` — family-parity audit across WAV, AIFF, and CAF

Use them when you need grounded review notes rather than the lighter public
surface summary in this guide.

## 9. Docs synchronization checklist

When the public surface changes, verify all of the following together:
- `crates/flacx/src/lib.rs`
- `crates/flacx/README.md`
- `docs/flacx-public-api-architecture.md`
- `docs/flacx-ground-up-ownership-map.md`
- `docs/flacx-family-parity.md`
- any workspace-level documentation map that points readers to those files

## 10. Verification cues

Useful checks when updating these docs:

```bash
cargo check -p flacx
cargo test -p flacx --test api --test decode
cargo test --workspace
find crates/flacx/src -maxdepth 2 -type f | sort
rg -n "core|convenience|architecture|flacx-public-api-architecture" \
  crates/flacx/src/lib.rs crates/flacx/README.md README.md docs/flacx-public-api-architecture.md
```

These checks do not prove prose quality, but they do help catch structural
staleness and naming drift.
