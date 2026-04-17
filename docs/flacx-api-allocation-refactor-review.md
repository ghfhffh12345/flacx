# flacx API allocation refactor review and verification guide

This maintainer note translates
`.omx/plans/prd-flacx-api-allocation-refactor.md` and
`.omx/plans/test-spec-flacx-api-allocation-refactor.md` into a durable
repo-local review companion for the **final verification and report synthesis**
lane.

It exists because the OMX planning artifacts are execution scaffolding, while
the repository still needs a maintainers-first document that keeps the public
approval boundary, benchmark/report obligations, and changed-file-to-evidence
mapping visible during implementation and review.

## Non-negotiable guardrails

- keep the current public `crates/flacx` usage model stable
- keep `crates/flacx-cli` out of scope
- stop for approval before introducing a new public struct or changing/removing
  a public struct in a way that alters the core caller model
- preserve bit-for-bit output identity to the baseline branch across the
  `./test-wavs` round-trip lane
- keep every materially modified area within the `<= 1.05x` slowdown gate
  against the baseline median
- do not claim completion without the report artifacts named in the test spec

## Binding evidence contract

The final lane is only complete when all of the following exist under:

`\.omx/reports/perf/flacx-api-allocation-refactor/`

- `bench-summary.md`
- `bench-summary.json`
- `test-wavs-roundtrip.md`
- `test-wavs-roundtrip.json`
- `public-surface-check.md`

Those artifacts are the durable acceptance surface for the refactor. They must
stay understandable even after the transient OMX plan files are gone.

## What the final report lane must prove

### 1. Public-surface / approval-boundary stability

The review lane must make the approval boundary obvious, not implicit.

Required checks:
- no new public struct landed without approval
- no removed public struct changed the core usage model without approval
- the hotspot contracts remain explicitly reviewed:
  - `crates/flacx/src/input.rs` — `EncodePcmStream`
  - `crates/flacx/src/read.rs` — `DecodePcmStream`
  - `crates/flacx/src/recompress/source.rs` — `FlacRecompressSource`

The `public-surface-check.md` artifact should therefore be treated as a
human-readable audit aid, not as a replacement for the API test suite.

### 2. Benchmark coverage by touched seam

The refactor is intentionally split across distinct internal seams. The final
report must map touched files to the benchmark IDs that now bind them.

| Changed seam | Files that trigger review attention | Required benchmark IDs |
| --- | --- | --- |
| Session orchestration | `convenience.rs`, `encoder.rs`, `decode.rs`, `recompress/source.rs`, `recompress/session.rs` | `builtin_bytes_encode`, `builtin_bytes_decode`, `builtin_bytes_recompress` |
| Metadata / container write | `metadata.rs`, `metadata/blocks.rs`, `wav_output.rs`, `aiff_output.rs`, `caf_output.rs`, `recompress/source.rs` | `metadata_write_path` |
| Buffer lifecycle / decode materialization | `encode_pipeline.rs`, `input.rs`, `read.rs`, `read/frame.rs`, `decode_output.rs` | `decode_frame_materialization` |
| Whole-crate acceptance | any touched `crates/flacx` file | `encode_corpus_throughput`, `decode_corpus_throughput`, `recompress_corpus_throughput`, `test_wavs_roundtrip_throughput` |

If benchmark naming evolves, the final report must map the old names from the
test spec to the new names explicitly so baseline comparisons remain legible.

### 3. Baseline-vs-head byte identity for `./test-wavs`

The user asked for bit-for-bit identity, not just decode-equivalent output.
The round-trip report therefore needs per-file records for:

- input filename
- baseline output path
- head output path
- baseline SHA-256
- head SHA-256
- byte equality result
- baseline bytes
- head bytes
- baseline/head wall time

This is the key reason the report lane exists separately from the main test
lane: the evidence needs to stay reviewable after ephemeral CI logs disappear.

## Repo-local report runner

The final verification lane now has a dedicated synthesis script:

```bash
python3 scripts/flacx_api_allocation_refactor_evidence.py \
  --baseline-worktree .omx/worktrees/flacx-api-allocation-refactor-baseline \
  --out-dir .omx/reports/perf/flacx-api-allocation-refactor
```

What it does:
1. compares `baseline` vs `HEAD` `crates/flacx` source files to build a
   changed-file map
2. reads Criterion estimate artifacts from each worktree and writes
   `bench-summary.md/json`
3. audits the public surface and hotspot blocks into
   `public-surface-check.md`
4. runs the `test-wavs` encode -> decode -> re-encode lane for both worktrees
   and writes `test-wavs-roundtrip.md/json`

Use `--skip-roundtrip` only when you need to inspect incomplete benchmark/API
evidence before the shared corpus is available. It is a progress aid, not a
final-acceptance path.

## Artifact reading order

When all five artifacts exist, read them in this order:

1. `public-surface-check.md`
   - verify the approval boundary stayed intact before spending time on
     performance interpretation
2. `bench-summary.md`
   - confirm every touched seam has a matching benchmark lane
   - check `head / baseline` ratios before scanning raw logs
3. `test-wavs-roundtrip.md`
   - confirm byte equality first
   - use wall-time columns only as supporting context, not as a replacement for
     the Criterion benchmark gate
4. `bench-summary.json` / `test-wavs-roundtrip.json`
   - use the machine-readable artifacts when review tooling or follow-up
     synthesis needs structured data

## Quality gates that still remain binding

The reporting lane does **not** replace the standard quality gates. It sits on
top of them and records the evidence in a durable form.

Run and keep green:

```bash
cargo check -p flacx
cargo test -p flacx
cargo test -p flacx --test api --test encode --test decode --test recompress
cargo clippy -p flacx --all-targets --all-features -- -D warnings
cargo bench -p flacx --bench throughput -- --noplot
```

The final report should reference those commands directly rather than inventing
new verification vocabulary.

## Review cues for maintainers

When the final artifacts are generated, review them in this order:

1. `public-surface-check.md`
   - did anything cross the approval boundary?
2. `bench-summary.md`
   - which changed files bound which benchmark IDs?
   - which IDs are still pending or incomplete?
3. `test-wavs-roundtrip.md`
   - are all rows byte-equal?
   - did any path show suspicious size or timing movement?
4. the normal test/bench command output
   - do the durable artifacts match the live verification logs?

## Exit criteria for this review note

This note can shrink or disappear once all of the following are true:

1. the refactor lands with the public usage model unchanged or explicitly
   approved otherwise
2. the generated report artifacts remain part of the normal review story
3. the changed-file benchmark mapping is obvious from the evidence
4. the `test-wavs` byte-identity report is reproducible on demand
5. maintainers no longer need the transient OMX plan files to understand the
   acceptance contract
