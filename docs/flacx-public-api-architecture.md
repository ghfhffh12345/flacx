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

`flacx` is organized around a small public story:

```text
explicit configs + typed boundaries
                │
                ▼
      Encoder / Decoder / Recompressor
                │
                ▼
      container readers / writers
                │
                ▼
           FLAC read/write core
                │
                ▼
   convenience helpers route into the same core
```

The key architectural distinction is:
- **explicit core** = the semantic center of the crate
- **convenience/orchestration** = wrappers that route into that core

The documentation should preserve that reading order.

## 2. Public interface map

```text
flacx
├─ modules
│  ├─ core
│  ├─ convenience
│  └─ level
├─ config/builders
│  ├─ EncoderConfig / EncoderBuilder
│  ├─ DecodeConfig / DecodeBuilder
│  └─ RecompressConfig / RecompressBuilder
├─ codec façades
│  ├─ Encoder / EncodeSummary
│  ├─ Decoder / DecodeSummary
│  └─ Recompressor / RecompressMode / RecompressPhase / RecompressProgress
├─ typed boundary
│  ├─ PcmStream / PcmStreamSpec / PcmContainer
│  ├─ read_pcm_stream / write_pcm_stream
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
| Explicit core | `core`, configs/builders, `Encoder`, `Decoder`, `Recompressor`, typed PCM helpers | configuration, typed handoff, explicit encode/decode/recompress operations, summaries | a path-oriented convenience story |
| Convenience/orchestration | `convenience`, flat `*_file` / `*_bytes` helpers | one-shot path/byte routing and extension-driven ergonomics | a duplicate policy engine |
| Container adaptation | public typed boundary plus family-specific behavior behind the scenes | container parsing/writing and family-specific translation | the place where top-level architecture is explained first |
| Support surfaces | `level`, inspector helpers, raw PCM helpers, progress types | supporting concepts adjacent to the core | the primary conceptual center |

## 4. Current source tree snapshot

This is the structural snapshot that currently supports the public API story:

```text
crates/flacx/src/
├─ lib.rs
├─ config.rs
├─ convenience.rs
├─ encoder.rs
├─ decode.rs
├─ recompress.rs
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
- `config.rs`, `encoder.rs`, `decode.rs`, `recompress.rs`, and `pcm.rs` are
  the fastest way to orient yourself around the exported architecture.
- `input.rs`, `wav_input.rs`, `wav_output.rs`, `read/`, and `write/` show how
  the container-facing and FLAC-facing edges were separated during the refactor.
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
recompress façade                   recompress.rs
typed PCM boundary                  pcm.rs + input.rs
WAV-family ingest/output            wav_input.rs + wav_output.rs
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
- do **not** lead with tutorial prose when the real need is orientation,
- do **not** over-explain internal execution paths in public-facing docs.

## 8. Docs synchronization checklist

When the public surface changes, verify all of the following together:
- `crates/flacx/src/lib.rs`
- `crates/flacx/README.md`
- `docs/flacx-public-api-architecture.md`
- any workspace-level documentation map that points readers to those files

## 9. Verification cues

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
