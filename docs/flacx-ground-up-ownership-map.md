# flacx same-crate ownership map

This document records the **current same-crate ownership map** for the
ground-up architecture rebuild described in
`.omx/plans/consensus-flacx-ground-up-architecture-rebuild-deliberate.md`.

It is intentionally grounded in the current tree so review, implementation,
and follow-up refactors can use the same vocabulary.

## Architectural spine

The intended reading order is:

```text
family readers / FLAC reader
            │
            ▼
      owned source handoff
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

This keeps the crate **same-crate**, makes **encode + decode** the center, and
keeps **recompress** downstream of the same reader/source/session substrate instead of
giving it a parallel architecture.

## Current ownership map

| Area | Primary files | Current ownership |
| --- | --- | --- |
| Public contract | `crates/flacx/src/lib.rs`, `crates/flacx/README.md`, `docs/flacx-public-api-architecture.md` | Public layering, exports, and architecture story. |
| Shared typed substrate | `crates/flacx/src/input.rs`, `crates/flacx/src/pcm.rs`, `crates/flacx/src/metadata.rs` | Reader/source handoff, typed PCM seam, and metadata ownership. |
| Encode spine | `crates/flacx/src/encoder.rs`, `crates/flacx/src/encode_pipeline.rs`, `crates/flacx/src/write/*` | Explicit encode sessions, planning, and FLAC write orchestration. |
| Decode spine | `crates/flacx/src/decode.rs`, `crates/flacx/src/read/*`, `crates/flacx/src/decode_output.rs` | Explicit decode sessions, FLAC read orchestration, and output commit flow. |
| Recompress adapter | `crates/flacx/src/recompress/*` | FLAC-reader-driven recompression that reuses the shared PCM/encode substrate while keeping config, source, session, progress, and verification ownership explicit. |
| WAV-family peers | `crates/flacx/src/wav_input.rs`, `crates/flacx/src/wav_output.rs` | RIFF/WAVE, RF64, and Wave64 ingest/output behavior. |
| AIFF-family peers | `crates/flacx/src/aiff.rs`, `crates/flacx/src/aiff_output.rs` | AIFF/AIFC ingest/output behavior behind the `aiff` feature. |
| CAF-family peers | `crates/flacx/src/caf.rs`, `crates/flacx/src/caf_output.rs` | CAF ingest/output behavior behind the `caf` feature. |
| Builtin orchestration | `crates/flacx/src/convenience.rs` | One-shot file/byte helpers layered on top of the explicit sessions. |

## Grounded concentration zones

Current file-size hotspots still show where ownership is most fragile:

- `crates/flacx/src/wav_input.rs` — 1304 LOC
- `crates/flacx/src/wav_output.rs` — 1294 LOC
- `crates/flacx/src/model.rs` — 963 LOC
- `crates/flacx/src/metadata.rs` — 806 LOC
- `crates/flacx/src/read/frame.rs` — 759 LOC
- `crates/flacx/src/aiff_output.rs` — 756 LOC
- `crates/flacx/src/caf_output.rs` — 733 LOC
- `crates/flacx/src/aiff.rs` — 679 LOC
- `crates/flacx/src/write.rs` — 532 LOC
- `crates/flacx/src/caf.rs` — 500 LOC
- `crates/flacx/src/read.rs` — 483 LOC
- `crates/flacx/src/config.rs` — 472 LOC

These are not automatically design failures, but they are the places where
review should keep asking whether a module owns a responsibility or merely
accumulates it.

## What this map says about the rebuild

### 1. Encode and decode are the architectural center

`encoder.rs`, `decode.rs`, and `recompress/` are the most legible explicit
session façades in the crate. The public story should keep routing readers
toward those modules before `convenience.rs`.

### 2. Container families are present as peers

The tree now has explicit WAV, AIFF, and CAF reader/writer modules instead of a
single WAV-only container path. Public docs should acknowledge that symmetry so
future work does not accidentally narrate AIFF/CAF as secondary adapters.

### 3. Recompress is its own public workflow, but not a separate architecture

`FlacRecompressSource` adapts a FLAC reader into `EncodePcmStream`, and
`Recompressor` writes through the same explicit encode-oriented substrate. That
is the correct same-crate shape for this wave.

## Structural review cues

Use these cues when reviewing follow-up changes:

1. Can the new code still be narrated as `reader -> spec/metadata -> typed PCM seam -> processing`?
2. Does the change keep encode/decode clearer than builtin/path helpers?
3. Does a family-specific behavior stay in a family module instead of leaking into shared substrate code?
4. Does recompress consume the spine, or is it trying to define new shared abstractions for itself?

## Recompress follow-up audit companion

When a recompress-specific refactor is in flight, pair this document with the
maintainer note in
[`docs/flacx-recompress-performance-recovery.md`](./flacx-recompress-performance-recovery.md)
and the generated audit artifacts:

```bash
python3 scripts/recompress_evidence.py \
  --baseline-worktree .omx/worktrees/v0.8.2 \
  --out-dir .omx/reports
```

That command refreshes:

- `.omx/reports/architecture/recompress-ownership-map.md`
- `.omx/reports/recompress-benchmark-compare/recompress-logic-refactor.md`
- `.omx/reports/recompress-corpus-diff/recompress-logic-refactor.json`

Use those artifacts to keep the recompress ownership split, byte-level
recompression diff, and v0.8.2 authority prep visible during review.

The recovery note exists specifically for the open April 13, 2026
encode-side hotspot lane; use it to keep the current checkpoint state,
review findings, and next measurement targets attached to the same ownership
map vocabulary.

## Open review notes

- Public naming still carries one legacy WAV-first seam:
  `inspect_wav_total_samples` remains the stable root helper, even though
  `inspect_pcm_total_samples` is re-exported alongside it.
- Internal naming still contains a WAV-shaped writer helper path
  (`wav_output::write_wav_with_metadata_and_md5_with_options`) that accepts a
  generic `PcmContainer`. This is acceptable as an implementation detail for
  now, but it is worth revisiting if future shared-surface naming drifts
  outward.

Those caveats are documented so follow-up cleanup can target them deliberately
instead of rediscovering them ad hoc.
