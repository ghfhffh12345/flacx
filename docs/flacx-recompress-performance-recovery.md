# flacx recompress internal performance recovery notes

This note records the **current internal recompress recovery state** after the
April 13, 2026 checkpoint commit (`61fe821`) and the continuation brief in
`.omx/context/recompress-internal-performance-optimization-continue-20260413T021732Z.md`.

It is a maintainer-facing companion for the active performance lane. The public
API remains unchanged; this document exists to keep the internal authority gate,
current code shape, and review cues visible while the recompress hotspot is
still open work.

## Non-negotiable guardrails

- keep the external recompress API unchanged
- preserve compression behavior, metadata preservation policy, progress
  semantics, and deterministic output
- treat the historical **v0.8.2 CLI compare** as the binding authority gate
- do not treat micro-bench wins or local spot checks as sufficient proof on
  their own

## Current branch state

As of **April 13, 2026**, the current branch has already recovered a large part
of the recompress regression, but it has **not** yet recovered the full
v0.8.2 authority gate.

The continuation context for this lane currently records:

- targeted regression checks green at the checkpoint:
  - `cargo fmt --all`
  - `cargo test -p flacx --test api --test recompress`
  - `cargo build --release -p flacx-cli`
- latest local authority result still failing badly:
  - aggregate geomean: `1.5397`
  - `recompress_single`: `3.278x` baseline
  - `recompress_directory`: `3.142x` baseline
- newer local spot evidence improved the open hotspot materially, but not
  enough:
  - single-file recompress improved to roughly `4.71s`
  - directory recompress improved from roughly `72s` to `43.10s`
  - historical baseline for that directory lane remains roughly `24.62s`
- decode is no longer the main suspect:
  - `FLACX_DECODE_PROFILE` on the single-file lane shows
    `total_take_decoded_samples ~1.00s`
- a representative recompressed output is still byte-identical to `v0.8.2`

## What the current branch changed internally

The current recovery pass keeps recompress on the same public reader/session
story, but changes the internal handoff:

1. `crates/flacx/src/recompress/source.rs`
   - `FlacRecompressSource` now hands recompress into a verifying PCM source
     that can switch to a full decoded-sample fast path when it is safe.
2. `crates/flacx/src/recompress/verify.rs`
   - `VerifyingPcmStream` keeps streaminfo-MD5 verification explicit while still
     preserving the eager `take_decoded_samples()` path for full-stream reads.
3. `crates/flacx/src/recompress/session.rs`
   - `Recompressor` now converts the verified source into a buffered `PcmStream`
     before the encode phase, then reuses the shared encoder machinery.
4. `crates/flacx/src/encoder.rs`
   - `Encoder::encode_buffered_pcm_with_sink(...)` gives recompress a direct
     buffered-PCM entry into the existing encode writer/progress path.
5. `crates/flacx/src/encode_pipeline.rs`
   - the encode pipeline still performs the heavy encode-side frame work, so the
     remaining hotspot is expected to live here or in the adjacent FLAC write
     path rather than in recompress-specific public orchestration.

## Code review findings from the current checkpoint

### 1. Public-surface risk is currently well-guarded

The public recompress contract is still protected by existing tests covering:

- explicit reader-first recompress flow
- builtin vs explicit parity
- deterministic repeat runs
- metadata preservation
- strict-mode validation behavior
- progress phase ordering
- export/API stability

That means the next optimization pass should stay focused on internal
implementation hot spots, not API reshaping.

### 2. The decode-side shortcut is intentional and should stay explicit

The buffered decode fast path is now part of the recompress story on purpose:
it keeps verification explicit while avoiding repeated chunk orchestration when
the encode side already needs the full stream shape. Do not remove that path
unless fresh evidence shows it regresses correctness or authority performance.

### 3. The remaining performance risk has moved into encode-side work

The strongest current hypothesis still matches the evidence: decode is no
longer dominant, and the open gap is likely inside one of these areas:

- frame-level work scheduling in `encode_chunk(...)`
- ordering/collection overhead for encoded frame batches
- write-side frame commit overhead in `write_encoded_chunk(...)`
- allocation and memory-movement cost of the buffered full-stream encode path

### 4. The current internal shape duplicates some encode setup

`encode_stream(...)` and `encode_buffered_pcm_with_sink(...)` now share the same
writer/progress/output contract, but still carry similar setup steps in
different places. That duplication is acceptable for the active recovery lane,
but it is a future cleanup target once the authority gate is green again.

Do **not** start that cleanup before the performance lane is closed; right now
the duplication is easier to measure and reason about than a larger refactor.

## Next measurement targets

Use this order for the next recovery pass:

1. measure encode-side time inside the buffered recompress path before changing
   structure again
2. compare `encode_chunk(...)` worker coordination cost vs actual frame-encode
   cost
3. measure how much time is spent after encoding but before final write
   completion
4. only then decide whether to reduce allocation churn, change batch sizing, or
   restructure write ordering

## Review companions

Keep this note paired with:

- `docs/flacx-ground-up-ownership-map.md`
- `crates/flacx/tests/recompress.rs`
- `crates/flacx/tests/api.rs`
- `.omx/context/recompress-internal-performance-optimization-continue-20260413T021732Z.md`
- generated evidence from:

```bash
python3 scripts/recompress_evidence.py \
  --baseline-worktree /home/ghfhffh12345/flacx/.omx/worktrees/v0.8.2 \
  --out-dir .omx/reports \
  --artifact-stem recompress-internal-performance-optimization
```

and the binding CLI authority compare:

```bash
python3 scripts/cli_perf_compare.py \
  --baseline-worktree /home/ghfhffh12345/flacx/.omx/worktrees/v0.8.2 \
  --corpus /home/ghfhffh12345/flacx/test-wavs \
  --out-dir .omx/reports/cli-perf/recompress-internal-performance-optimization
```

## Exit criteria for this note

This note should shrink or disappear once both conditions are true:

1. the v0.8.2 recompress authority gate is green again
2. the internal encode-side recovery path is either accepted as the new steady
   state or replaced by a simpler implementation with the same verified results
