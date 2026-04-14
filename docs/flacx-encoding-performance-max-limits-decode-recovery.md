# flacx encode maximality with decode-recovery guardrails

This maintainer note translates
`.omx/plans/prd-encoding-performance-max-limits-decode-recovery.md` into a
durable in-repo review companion for the active performance lane.

It exists because the plan file is an OMX execution artifact, while reviewers
still need a repo-local document that keeps the coupled encode/decode gates,
hotspot review cues, and artifact obligations visible during implementation.

## Non-negotiable guardrails

- keep the current external encode API stable
- preserve the reader-first encode flow built around `read_pcm_reader(...)`
  and `EncoderConfig::into_encoder(...)`
- keep `flacx::builtin` as a thin convenience layer over the same encode path
- do not add dependencies
- reduce allocations only where measurements justify the change
- recover the decode regression introduced by the latest encode-focused wave
- do not weaken encoder semantics or hide encoded-size fallout to buy speed
- record any encoded-output-shape change that could plausibly affect decode

## Binding performance gates

The authoritative library benchmark remains:

```bash
cargo bench -p flacx --bench throughput -- --noplot
```

Planning baselines captured in the PRD:
- encode baseline on **2026-04-14**: `encode_corpus_throughput` median
  `21.102 MiB/s`
- required encode target: `>= 24.267 MiB/s`
- historical pre-regression decode figure on **2026-04-14**:
  `decode_corpus_throughput` median `43.256 MiB/s`
- post-change decode figure on **2026-04-15**:
  `decode_corpus_throughput` median `38.031 MiB/s`

Completion therefore requires both:
1. a same-machine replay against fresh `HEAD` and a fresh `ceb7cd7` compare
   worktree, and
2. a final `HEAD` result that clears the unchanged encode gate while also
   recovering decode to the confirmed authority baseline.

## Grounded repo findings

### 1. The throughput bench is coupled to encoder output shape, not just encode-core time

Repo evidence from `crates/flacx/benches/throughput.rs`:
- the bench generates WAV fixtures, then pre-encodes the FLAC corpus that the
  decode and recompress runs consume
- the decode benchmark therefore measures both decode-side work and the shape
  of the FLAC corpus produced by the current encoder
- every iteration still includes fresh temp-directory and output-file work

Review implication:
- the new PRD is correct to require `HEAD` vs `ceb7cd7` compare artifacts and
  encoded-corpus-shape notes
- a faster encoder that emits a decode-hostile corpus is not an acceptable win

### 2. Encode still buffers the whole declared stream before frame work begins

Repo evidence from `crates/flacx/src/encode_pipeline.rs`:
- `encode_stream(...)` currently sets `chunk_start = 0` and
  `chunk_end = plan.total_frames`
- it reads `expected_frames_for_chunk(...)` into a single `Vec<i32>`
- `encode_chunk(...)` then fans frame work out from that buffered sample block

Review implication:
- whole-stream materialization remains a first-tier hotspot candidate
- but the winning fix still has to be proven against the binding bench rather
  than assumed from long-stream intuition alone

### 3. Frame analysis is still allocation-heavy even after the latest encode wave

Repo evidence from `crates/flacx/src/model.rs`:
- frame analysis still materializes `Vec`-backed channel extraction,
  transformed stereo channels, warmup buffers, coefficient buffers, and
  residual storage
- those allocations sit inside the frame-analysis path that every encode run
  exercises

Review implication:
- if whole-stream buffering is not the dominant culprit, `model.rs` remains a
  credible next hotspot
- any optimization here should stay measurement-led and narrowly justified

### 4. WAV/raw ingestion already reuses byte buffers, so low-risk wins should preserve stream shape first

Repo evidence:
- `crates/flacx/src/wav_input.rs` keeps `last_chunk_bytes` and reuses it across
  `read_chunk(...)` calls
- `crates/flacx/src/raw.rs` does the same for raw PCM ingestion
- both readers still decode chunk contents into caller-owned sample vectors

Review implication:
- the lowest-risk allocation pass should start by trimming avoidable sample-side
  churn before changing encoded frame layout or scheduling behavior
- ingestion cleanup alone may not be enough, but it is the cleanest place to
  separate shape-preserving wins from shape-changing ones

### 5. Existing API tests already guard the public story and should stay first in the regression lane

Repo evidence from `crates/flacx/tests/api.rs`:
- builtin helpers are compared against the explicit reader/session flow
- encoder configuration behavior is asserted through the public session path
- family-reader coverage already exists behind the feature matrix

Review implication:
- performance work should stay internal unless tests prove public drift
- documentation should continue to describe the reader/session flow as the
  stable conceptual center even while internals change aggressively

### 6. Documentation and artifact production are part of the work, not polish after the fact

The PRD and test spec already require:
- a final performance report under
  `.omx/reports/perf/encoding-performance-max-limits-decode-recovery.*`
- a compare artifact against `ceb7cd7`
- hotspot timing evidence
- encoded-output-shape notes

Review implication:
- implementation is incomplete if faster code lands without the compare trail
- reviewers should reject any “encode is green” claim that does not also show
  decode recovery evidence and corpus-shape context

## Documentation contract for the implementation lane

When this work lands, keep the durable documentation story in this order:

1. **Stable public API first**
   - `read_pcm_reader(...)`
   - `EncoderConfig::into_encoder(...)`
   - `flacx::builtin` remains convenience, not a separate engine
2. **Coupled benchmark authority second**
   - preserve the benchmark command
   - preserve the encode target
   - preserve the `ceb7cd7` replay requirement for decode recovery
3. **Measured hotspot explanation third**
   - say which cost center actually dominated
   - say why that wave was chosen before wider changes
   - distinguish shape-preserving wins from shape-changing wins
4. **Performance evidence fourth**
   - `HEAD` before/after medians
   - fresh `ceb7cd7` authority medians
   - percent deltas
   - encoded-size or corpus-shape notes when relevant
5. **Regression proof fifth**
   - API tests
   - encode/recompress tests
   - feature-matrix checks
   - doc tests

Do not document this lane as “pure encode optimization.” The current request is
explicitly a coupled encode-maximality plus decode-recovery effort.

## Review companions

Keep this note paired with:
- `.omx/plans/prd-encoding-performance-max-limits-decode-recovery.md`
- `.omx/plans/test-spec-encoding-performance-max-limits-decode-recovery.md`
- `crates/flacx/benches/throughput.rs`
- `crates/flacx/tests/api.rs`
- `crates/flacx/src/encode_pipeline.rs`
- `crates/flacx/src/model.rs`
- `crates/flacx/src/wav_input.rs`
- `crates/flacx/src/raw.rs`
- generated artifacts under `.omx/reports/perf/`

## Exit criteria for this note

This note can shrink or disappear once all of the following are true:

1. the public encode/session API is still unchanged and verified
2. the throughput bench clears the `>= 24.267 MiB/s` encode target
3. decode recovers to the confirmed same-machine `ceb7cd7` authority baseline
4. the winning hotspot choice is explained by durable compare/timing artifacts
5. any encoded-size or corpus-shape movement is recorded instead of implied away
